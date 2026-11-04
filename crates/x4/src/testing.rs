//! A scripted serial port shared by the unit tests.
use crate::{X4, protocol::Session};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct Fake {
    pub responses: HashMap<Vec<u8>, Vec<u8>>,
    /// Replies used once each, in order, before falling back to `responses`.
    pub sequences: HashMap<Vec<u8>, VecDeque<Vec<u8>>>,
    pub rx: VecDeque<u8>,
    pub writes: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Answer the ASCII unlock handshake.
    pub cold: bool,
    /// Fail a request with no scripted reply instead of leaving it unanswered.
    pub strict: bool,
}
impl Read for Fake {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.rx.is_empty() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let n = buffer.len().min(2).min(self.rx.len());
        for byte in &mut buffer[..n] {
            *byte = self.rx.pop_front().unwrap();
        }
        Ok(n)
    }
}
impl Write for Fake {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes.lock().unwrap().push(bytes.to_vec());
        if self.cold && bytes.starts_with(b"unlock") {
            self.rx.extend(b"unlock_OK\r\n");
        }
        if self.cold && bytes == b"SW_MODE1\r\n" {
            self.responses.insert(hex("5a0300"), hex("5a03023b00"));
        }
        if let Some(response) =
            self.sequences.get_mut(bytes).and_then(VecDeque::pop_front)
        {
            self.rx.extend(response);
        } else if let Some(response) = self.responses.get(bytes) {
            self.rx.extend(response);
        } else if self.strict {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Unexpected test request {bytes:02x?}"),
            ));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn fake(responses: Vec<(Vec<u8>, Vec<u8>)>) -> Fake {
    Fake {
        responses: responses.into_iter().collect(),
        ..Default::default()
    }
}
pub fn device(port: Fake) -> X4<Fake> {
    X4 {
        session: Session::new(port),
        firmware: "fixture".into(),
    }
}
pub fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
pub fn wire(command: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x5a, command, payload.len() as u8];
    bytes.extend(payload);
    bytes
}
pub fn ack(command: u8, status: u8) -> Vec<u8> {
    wire(2, &[command, status, 0, 0, 0, 0, 0, 0, 0, 0])
}
