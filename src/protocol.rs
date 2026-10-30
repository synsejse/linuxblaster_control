//! X4 CDC wire protocol. See docs/PROTOCOL.md for evidence and limitations.
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    time::{Duration, Instant},
};

pub type Result<T> =
    std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn invalid(
    message: impl Into<String>,
) -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::new(io::ErrorKind::InvalidData, message.into()).into()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub command: u8,
    pub payload: Vec<u8>,
}

#[derive(Default)]
pub struct Parser {
    buffer: Vec<u8>,
}
impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(&marker) = self.buffer.first() {
            if marker != 0x5a && marker != 0x5b {
                self.buffer.remove(0);
                continue;
            }
            let header = if marker == 0x5a { 3 } else { 4 };
            if self.buffer.len() < header {
                break;
            }
            let length = if header == 3 {
                self.buffer[2] as usize
            } else {
                u16::from_le_bytes([self.buffer[2], self.buffer[3]]) as usize
            };
            if length > 4096 {
                return Err(invalid("Oversized CDC frame"));
            }
            if self.buffer.len() < header + length {
                break;
            }
            frames.push(Frame {
                command: self.buffer[1],
                payload: self.buffer[header..header + length].to_vec(),
            });
            self.buffer.drain(..header + length);
        }
        Ok(frames)
    }
}

pub fn unlock(challenge: &[u8], random: [u8; 16]) -> Result<Vec<u8>> {
    if challenge.len() != 36 {
        return Err(invalid("Invalid challenge length"));
    }
    let middle = [
        0xd3, 0x1a, 0x21, 0x27, 0x9b, 0xe3, 0x46, 0xf0, 0x99, 0x9d, 0x6e, 0xc4,
        0xc3, 0xfe, 0xbe, 0x98, 0x90, 0x18, 0x69, 0xc1, 0x18, 0xfb, 0xb1, 0x25,
        0x6e, 0x0c, 0xe0, 0x7b,
    ];
    let mut key = Vec::from(&challenge[..2]);
    key.extend(middle);
    key.extend_from_slice(&challenge[2..4]);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| invalid("AES key length"))?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&random[..12]), &challenge[4..])
        .map_err(|_| invalid("AES-GCM unlock failed"))?;
    let mut message = b"unlock".to_vec();
    message.extend(random);
    message.extend(encrypted);
    message.extend(b"\r\n");
    Ok(message)
}

/// A single owner serializes all exchanges. A failed exchange invalidates this session.
/// Notifications are retained separately, never used as acknowledgements.
pub struct Session<T> {
    port: T,
    parser: Parser,
    pending: VecDeque<Frame>,
    pub notifications: VecDeque<Frame>,
    timeout: Duration,
    pub usable: bool,
    pub max_payload: usize,
}
impl<T: Read + Write> Session<T> {
    pub fn new(port: T) -> Self {
        Self {
            port,
            parser: Parser::default(),
            pending: VecDeque::new(),
            notifications: VecDeque::new(),
            timeout: Duration::from_secs(3),
            usable: true,
            max_payload: 59,
        }
    }
    #[cfg(test)]
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }
    fn notify(&mut self, frame: Frame) {
        // Bounded: physical controls can be active indefinitely.
        if self.notifications.len() == 128 {
            self.notifications.pop_front();
        }
        self.notifications.push_back(frame);
    }
    fn receive(&mut self) -> Result<bool> {
        let mut buffer = [0; 512];
        match self.port.read(&mut buffer) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "X4 disconnected",
            )
            .into()),
            Ok(n) => {
                self.pending.extend(self.parser.feed(&buffer[..n])?);
                Ok(true)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
    fn wait(&mut self, matcher: impl Fn(&Frame) -> bool) -> Result<Frame> {
        let end = Instant::now() + self.timeout;
        loop {
            while let Some(frame) = self.pending.pop_front() {
                if matcher(&frame) {
                    return Ok(frame);
                }
                self.notify(frame);
            }
            if Instant::now() >= end {
                return Err(io::Error::new(io::ErrorKind::TimedOut,
                "X4 response timed out; reopen and read state before another write").into());
            }
            self.receive()?;
        }
    }
    fn prepare(&mut self, command: u8, payload: &[u8]) -> Result<()> {
        if !self.usable {
            return Err(invalid(
                "Session invalid; reconnect before issuing commands",
            ));
        }
        if payload.len() > self.max_payload {
            return Err(invalid("Command exceeds negotiated payload limit"));
        }
        // Drain previous notifications before sending a query. Keep them for the caller.
        let end = Instant::now() + Duration::from_millis(100);
        loop {
            while let Some(f) = self.pending.pop_front() {
                self.notify(f);
            }
            if Instant::now() >= end || !self.receive()? {
                break;
            }
        }
        let mut wire = vec![0x5a, command, payload.len() as u8];
        wire.extend_from_slice(payload);
        self.port.write_all(&wire)?;
        Ok(())
    }
    pub fn get(
        &mut self,
        command: u8,
        payload: &[u8],
        matcher: impl Fn(&Frame) -> bool,
    ) -> Result<Vec<u8>> {
        let result = (|| {
            self.prepare(command, payload)?;
            let frame = self.wait(|f| {
                (f.command == command && matcher(f))
                    || (f.command == 2 && f.payload.first() == Some(&command))
            })?;
            if frame.command == 2 {
                return Err(invalid(format!(
                    "Query 0x{command:02x} returned ACK {:?}",
                    frame.payload
                )));
            }
            Ok(frame.payload)
        })();
        if result.is_err() {
            self.usable = false;
        }
        result
    }
    pub fn set(&mut self, command: u8, payload: &[u8]) -> Result<()> {
        let result = (|| {
            self.prepare(command, payload)?;
            let frame = self.wait(|f| {
                f.command == 2 && f.payload.first() == Some(&command)
            })?;
            if frame.payload.len() != 10 || frame.payload[1] != 0 {
                return Err(invalid(format!(
                    "X4 rejected command 0x{command:02x}: {:?}",
                    frame.payload
                )));
            }
            Ok(())
        })();
        if result.is_err() {
            self.usable = false;
        }
        result
    }
    fn ascii(&mut self, challenge: bool) -> Result<Vec<u8>> {
        let end = Instant::now() + self.timeout;
        let mut data = Vec::new();
        // Read one byte to leave any following binary frames in the port.
        while Instant::now() < end && data.len() <= 4096 {
            let mut byte = [0];
            match self.port.read(&mut byte) {
                Ok(0) => return Err(invalid("X4 disconnected during unlock")),
                Ok(_) => data.push(byte[0]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            if challenge {
                if let Some(start) =
                    data.windows(9).position(|s| s == b"whoareyou")
                {
                    if data.len() >= start + 47 {
                        let reply = &data[start..start + 47];
                        if &reply[45..] != b"\r\n" {
                            return Err(invalid("Malformed challenge"));
                        }
                        return Ok(reply.to_vec());
                    }
                } else {
                    for reply in
                        [b"NotYet\r\n".as_slice(), b"Unknown command\r\n"]
                    {
                        if data.ends_with(reply) {
                            return Ok(reply.to_vec());
                        }
                    }
                }
            } else if data.ends_with(b"unlock_OK\r\n") {
                return Ok(data);
            }
        }
        Err(invalid("No valid X4 unlock response"))
    }
    pub fn connect(&mut self) -> Result<String> {
        let result = self.initialize();
        if result.is_err() {
            self.usable = false;
        }
        result
    }
    fn initialize(&mut self) -> Result<String> {
        let payload = match self.get(3, &[], |_| true) {
            Ok(p) => p,
            Err(e)
                if e.downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::TimedOut) =>
            {
                self.usable = true;
                self.parser = Parser::default();
                self.pending.clear();
                let mut response = Vec::new();
                for _ in 0..3 {
                    self.port.write_all(b"\r\nwhoareyou.MyApp8\r\n")?;
                    response = self.ascii(true)?;
                    if response != b"NotYet\r\n" {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                if response.starts_with(b"whoareyou") {
                    let mut random = [0; 16];
                    getrandom::getrandom(&mut random)
                        .map_err(|e| invalid(e.to_string()))?;
                    self.port.write_all(&unlock(&response[9..45], random)?)?;
                    self.ascii(false)?;
                } else if response != b"Unknown command\r\n" {
                    return Err(invalid("X4 is still booting"));
                }
                self.port.write_all(b"SW_MODE1\r\n")?;
                let ack = self.wait(|f| {
                    f.command == 2 && f.payload.first() == Some(&0x6d)
                })?;
                if ack.payload.len() != 10 || ack.payload[1] != 0 {
                    return Err(invalid("SW_MODE1 failed"));
                }
                self.get(3, &[], |_| true)?
            }
            Err(e) => return Err(e),
        };
        if payload.len() != 2 {
            return Err(invalid("Invalid maximum payload response"));
        }
        self.max_payload =
            u16::from_le_bytes([payload[0], payload[1]]) as usize;
        if !(8..=255).contains(&self.max_payload) {
            return Err(invalid("Unsupported command payload limit"));
        }
        let fw = self.get(9, &[2], |f| f.payload.first() == Some(&2))?;
        if fw.len() < 2 || fw.len() != fw[1] as usize + 2 {
            return Err(invalid("Malformed firmware response"));
        }
        Ok(std::str::from_utf8(&fw[2..])?.trim_end_matches('\0').into())
    }
    pub fn next_profile_block(&mut self, prefix: &[u8]) -> Result<Vec<u8>> {
        let result =
            self.wait(|f| f.command == 0x17 && f.payload.starts_with(prefix));
        if result.is_err() {
            self.usable = false;
        }
        result.map(|f| f.payload)
    }
}
