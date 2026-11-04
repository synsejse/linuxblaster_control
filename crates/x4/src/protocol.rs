//! CDC framing, session initialization and serialized request/reply handling.
//!
//! Frames are `5a command length:u8 payload` or
//! `5b command length:u16le payload`; lengths exclude the header. There is no
//! framing checksum. [`Parser`] accepts both forms and arbitrary read boundaries;
//! [`Session`] sends `5a` and negotiates a payload limit of at most 255 bytes.
//!
//! Queries normally reply with the same command. Command `02` is an ACK:
//! `original_command status associated_data[8]`. Status `00` is success;
//! SXFI mode selection also emits a short status `01` pending ACK before its
//! terminal ten-byte ACK. Unmatched frames are queued as notifications.
//!
//! Session deadlines require a transport whose reads have finite timeouts.
//! Failed exchanges invalidate the session; reconnect instead of replaying an
//! uncertain write. `docs/PROTOCOL.md` documents command-specific payloads.
use crate::error::{Error, Result, invalid};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    time::{Duration, Instant},
};

/// Fixed part of the unlock key; the challenge supplies the bytes either side.
const UNLOCK_KEY: [u8; 28] = [
    0xd3, 0x1a, 0x21, 0x27, 0x9b, 0xe3, 0x46, 0xf0, 0x99, 0x9d, 0x6e, 0xc4,
    0xc3, 0xfe, 0xbe, 0x98, 0x90, 0x18, 0x69, 0xc1, 0x18, 0xfb, 0xb1, 0x25,
    0x6e, 0x0c, 0xe0, 0x7b,
];
/// Largest number of queued notifications or retained transfer replies.
const QUEUE_LIMIT: usize = 128;

/// The little-endian word at `at`; callers have checked the reply's length.
pub(crate) fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
/// Serial reads that return without data; deadlines are checked by callers.
fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
    )
}

/// Decoded command, payload and original framing marker; length bytes omitted.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Original framing marker: 0x5a or 0x5b.
    pub marker: u8,
    pub command: u8,
    pub payload: Vec<u8>,
}

/// Incremental `5a`/`5b` decoder; skips bytes before a recognized marker.
/// Incomplete frames remain buffered; payloads over 4096 bytes are rejected.
#[derive(Default)]
pub struct Parser {
    buffer: Vec<u8>,
}
impl Parser {
    /// Append bytes and return every complete frame available so far.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            let start = self
                .buffer
                .iter()
                .position(|b| *b == 0x5a || *b == 0x5b)
                .unwrap_or(self.buffer.len());
            self.buffer.drain(..start);
            let Some(&marker) = self.buffer.first() else {
                break;
            };
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
                marker,
                command: self.buffer[1],
                payload: self.buffer[header..header + length].to_vec(),
            });
            self.buffer.drain(..header + length);
        }
        Ok(frames)
    }
}

/// Build the ASCII `unlock` response from the 36-byte seed/challenge body.
/// Uses AES-256-GCM, empty AAD, nonce `random[..12]`, a 16-byte tag, and CRLF.
/// The caller must supply a fresh random 16-byte field for each challenge.
pub fn unlock(challenge: &[u8], random: [u8; 16]) -> Result<Vec<u8>> {
    if challenge.len() != 36 {
        return Err(invalid("Invalid challenge length"));
    }
    let mut key = Vec::from(&challenge[..2]);
    key.extend(UNLOCK_KEY);
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

/// The challenge, or the reply that says no challenge is coming, once `data`
/// holds all of it.
fn challenge_reply(data: &[u8]) -> Result<Option<Vec<u8>>> {
    if let Some(start) = data.windows(9).position(|s| s == b"whoareyou") {
        let Some(reply) = data.get(start..start + 47) else {
            return Ok(None);
        };
        if &reply[45..] != b"\r\n" {
            return Err(invalid("Malformed challenge"));
        }
        return Ok(Some(reply.to_vec()));
    }
    Ok([b"NotYet\r\n".as_slice(), b"Unknown command\r\n"]
        .into_iter()
        .find(|reply| data.ends_with(reply))
        .map(<[u8]>::to_vec))
}

/// A single owner serializes all exchanges. A failed exchange invalidates this session.
/// Notifications are retained separately, never used as acknowledgements.
pub struct Session<T> {
    port: T,
    parser: Parser,
    pending: VecDeque<Frame>,
    retained: VecDeque<Frame>,
    transfer_active: bool,
    /// Up to 128 unmatched frames, including asynchronous device notifications.
    pub notifications: VecDeque<Frame>,
    timeout: Duration,
    /// False after an exchange failure; do not manually reset to retry a write.
    pub usable: bool,
    /// Negotiated outgoing payload limit (59 until initialization).
    pub max_payload: usize,
}
impl<T: Read + Write> Session<T> {
    /// Wrap a transport; call [`Self::connect`] before normal device operations.
    pub fn new(port: T) -> Self {
        Self {
            port,
            parser: Parser::default(),
            pending: VecDeque::new(),
            retained: VecDeque::new(),
            transfer_active: false,
            notifications: VecDeque::new(),
            timeout: Duration::from_secs(3),
            usable: true,
            max_payload: 59,
        }
    }
    /// Set the host response timeout; transport reads must also be bounded.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }
    /// Mark the device state uncertain and return `message` as the error.
    /// Used when an acknowledged write fails its readback check.
    pub(crate) fn mismatch(&mut self, message: impl Into<String>) -> Error {
        self.usable = false;
        invalid(message)
    }
    /// Run one exchange; any failure invalidates the session.
    fn guarded<R>(
        &mut self,
        exchange: impl FnOnce(&mut Self) -> Result<R>,
    ) -> Result<R> {
        let result = exchange(self);
        if result.is_err() {
            self.usable = false;
        }
        result
    }
    fn ensure_usable(&self) -> Result<()> {
        if self.usable {
            Ok(())
        } else {
            Err(invalid(
                "Session invalid; reconnect before issuing commands",
            ))
        }
    }
    /// Queue already-arrived frames as notifications before a new request.
    fn drain_input(&mut self) -> Result<()> {
        let end = Instant::now() + Duration::from_millis(100);
        loop {
            while let Some(f) = self.pending.pop_front() {
                self.notify(f)?;
            }
            if Instant::now() >= end || !self.receive()? {
                return Ok(());
            }
        }
    }
    fn notify(&mut self, frame: Frame) -> Result<()> {
        // Bounded: physical controls can be active indefinitely.
        if self.notifications.len() == QUEUE_LIMIT {
            self.pending.push_front(frame);
            return Err(invalid(
                "Notification queue full; drain notifications before continuing",
            ));
        }
        self.notifications.push_back(frame);
        Ok(())
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
            Err(e) if transient(&e) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    pub(crate) fn wait(
        &mut self,
        matcher: impl Fn(&Frame) -> bool,
    ) -> Result<Frame> {
        self.wait_until(Instant::now() + self.timeout, matcher)
    }
    pub(crate) fn wait_until(
        &mut self,
        end: Instant,
        matcher: impl Fn(&Frame) -> bool,
    ) -> Result<Frame> {
        loop {
            while let Some(frame) = self.pending.pop_front() {
                if self.transfer_active
                    && frame.command == 2
                    && frame.payload.first() == Some(&0x98)
                    && frame.payload.get(1).is_some_and(|status| *status != 0)
                {
                    return Err(invalid("Transfer rejected by device ACK"));
                }
                if matcher(&frame) {
                    return Ok(frame);
                }
                if self.transfer_active && frame.command == 0x98 {
                    if self.retained.len() >= QUEUE_LIMIT {
                        return Err(invalid("Transfer response queue full"));
                    }
                    self.retained.push_back(frame);
                } else {
                    self.notify(frame)?;
                }
            }
            if Instant::now() >= end {
                return Err(Error::Timeout);
            }
            self.receive()?;
        }
    }
    pub(crate) fn prepare(
        &mut self,
        command: u8,
        payload: &[u8],
    ) -> Result<()> {
        self.ensure_usable()?;
        if payload.len() > self.max_payload.min(255) {
            return Err(invalid("Command exceeds negotiated payload limit"));
        }
        // Keep earlier notifications for the caller; they are never replies.
        self.drain_input()?;
        let mut wire = vec![0x5a, command, payload.len() as u8];
        wire.extend_from_slice(payload);
        self.port.write_all(&wire)?;
        Ok(())
    }
    /// Send a query and return the same-command payload matching `matcher`.
    /// Any command-`02` ACK for this query is an error, including a success ACK:
    /// a query requires data. Matching and waiting have a three-second deadline.
    pub fn get(
        &mut self,
        command: u8,
        payload: &[u8],
        matcher: impl Fn(&Frame) -> bool,
    ) -> Result<Vec<u8>> {
        self.guarded(|s| {
            s.prepare(command, payload)?;
            let frame = s.wait(|f| {
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
        })
    }
    /// Send a write and require a ten-byte ACK with matching command/status `00`.
    /// This verifies acknowledgement only, not resulting state. Use
    /// [`Self::set_pending`] for commands with an asynchronous pending phase.
    pub fn set(&mut self, command: u8, payload: &[u8]) -> Result<()> {
        self.guarded(|s| {
            s.prepare(command, payload)?;
            let frame = s.wait(|f| {
                f.command == 2 && f.payload.first() == Some(&command)
            })?;
            if frame.payload.len() != 10 || frame.payload[1] != 0 {
                return Err(invalid(format!(
                    "X4 rejected command 0x{command:02x}: {:?}",
                    frame.payload
                )));
            }
            Ok(())
        })
    }
    /// Wait up to ten seconds through status-`01` ACKs, requiring a final
    /// ten-byte status-`00` ACK whose associated byte matches `selector`.
    /// SXFI `6f [0c mode[4]]` uses this path; pending associated byte `0a`
    /// differs from the final `0c` and is deliberately not selector-filtered.
    pub fn set_pending(
        &mut self,
        command: u8,
        payload: &[u8],
        selector: u8,
    ) -> Result<()> {
        self.guarded(|s| {
            s.prepare(command, payload)?;
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                let frame = s.wait_until(end, |f| {
                    f.command == 2 && f.payload.first() == Some(&command)
                })?;
                if frame.payload.len() < 2 {
                    return Err(invalid("Malformed pending ACK"));
                }
                match frame.payload[1] {
                    1 if frame.payload.len() == 4 => {}
                    0 if frame.payload.len() == 10
                        && frame.payload.get(2) == Some(&selector) =>
                    {
                        return Ok(());
                    }
                    _ => {
                        return Err(invalid(format!(
                            "X4 rejected asynchronous command: {:?}",
                            frame.payload
                        )));
                    }
                }
            }
        })
    }
    /// Receive a continuation without draining unrelated queued frames.
    pub fn continuation(
        &mut self,
        command: u8,
        matcher: impl Fn(&Frame) -> bool,
    ) -> Result<Vec<u8>> {
        let end = Instant::now() + self.timeout;
        self.guarded(|s| {
            s.continuation_frame_until(command, end, matcher)
                .map(|f| f.payload)
        })
    }
    pub(crate) fn continuation_frame_until(
        &mut self,
        command: u8,
        end: Instant,
        matcher: impl Fn(&Frame) -> bool,
    ) -> Result<Frame> {
        self.ensure_usable()?;
        if let Some(i) = self
            .retained
            .iter()
            .position(|f| f.command == command && matcher(f))
        {
            return Ok(self.retained.remove(i).expect("located frame"));
        }
        self.wait_until(end, |f| f.command == command && matcher(f))
    }
    /// Locally abandon this session. No cancel packet is established; device
    /// timeout/cleanup may retain resumable buffers or staging files.
    pub fn abort(&mut self) {
        self.usable = false;
        self.transfer_active = false;
        self.retained.clear();
    }
    pub(crate) fn begin_transfer(&mut self) -> Result<()> {
        self.ensure_usable()?;
        if self.transfer_active {
            return Err(invalid("Transfer already active"));
        }
        self.drain_input()?;
        // An incomplete old frame cannot become a new operation's response.
        self.parser = Parser::default();
        self.retained.clear();
        self.transfer_active = true;
        Ok(())
    }
    pub(crate) fn end_transfer(&mut self) {
        self.transfer_active = false;
        self.retained.clear();
    }
    /// Send an already framed transfer packet without draining in-flight responses.
    pub(crate) fn transfer_send(&mut self, wire: &[u8]) -> Result<()> {
        self.ensure_usable()?;
        self.guarded(|s| Ok(s.port.write_all(wire)?))
    }
    /// Read until `reply` recognizes a complete ASCII reply. Reads one byte at
    /// a time to leave any following binary frames in the port.
    fn ascii(
        &mut self,
        reply: impl Fn(&[u8]) -> Result<Option<Vec<u8>>>,
    ) -> Result<Vec<u8>> {
        let end = Instant::now() + self.timeout;
        let mut data = Vec::new();
        while Instant::now() < end && data.len() <= 4096 {
            let mut byte = [0];
            match self.port.read(&mut byte) {
                Ok(0) => return Err(invalid("X4 disconnected during unlock")),
                Ok(_) => data.push(byte[0]),
                Err(e) if transient(&e) => continue,
                Err(e) => return Err(e.into()),
            }
            if let Some(reply) = reply(&data)? {
                return Ok(reply);
            }
        }
        Err(invalid("No valid X4 unlock response"))
    }
    /// Probe `03 []` for binary readiness, falling back on timeout to
    /// `whoareyou.MyApp8`, challenge/unlock and `SW_MODE1` initialization.
    /// Negotiate the payload limit, then return firmware from `09 [02]`.
    pub fn connect(&mut self) -> Result<String> {
        self.guarded(Self::initialize)
    }
    fn initialize(&mut self) -> Result<String> {
        let payload = match self.get(3, &[], |_| true) {
            Ok(p) => p,
            Err(e) if e.is_timeout() => {
                self.unlock_channel()?;
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
        let firmware = std::str::from_utf8(&fw[2..])
            .map_err(|_| invalid("Firmware version is not UTF-8"))?;
        Ok(firmware.trim_end_matches('\0').into())
    }
    /// The ASCII handshake a cold X4 needs before it accepts binary frames.
    fn unlock_channel(&mut self) -> Result<()> {
        self.usable = true;
        self.parser = Parser::default();
        self.pending.clear();
        let mut response = Vec::new();
        for _ in 0..3 {
            self.port.write_all(b"\r\nwhoareyou.MyApp8\r\n")?;
            response = self.ascii(challenge_reply)?;
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
            self.ascii(|data| {
                Ok(data.ends_with(b"unlock_OK\r\n").then(|| data.to_vec()))
            })?;
        } else if response != b"Unknown command\r\n" {
            return Err(invalid("X4 is still booting"));
        }
        self.port.write_all(b"SW_MODE1\r\n")?;
        let ack =
            self.wait(|f| f.command == 2 && f.payload.first() == Some(&0x6d))?;
        if ack.payload.len() != 10 || ack.payload[1] != 0 {
            return Err(invalid("SW_MODE1 failed"));
        }
        Ok(())
    }
}
