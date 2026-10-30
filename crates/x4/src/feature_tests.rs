//! Captures are from backups/before-feature-research/probe-trace.jsonl.
use crate::{Output, SxfiMode, X4, protocol::Session};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    sync::{Arc, Mutex},
};

type Writes = Arc<Mutex<Vec<Vec<u8>>>>;
#[derive(Default)]
struct Fake {
    replies: HashMap<Vec<u8>, Vec<u8>>,
    rx: VecDeque<u8>,
    writes: Writes,
}
impl Read for Fake {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.rx.is_empty() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let n = out.len().min(2).min(self.rx.len());
        for byte in &mut out[..n] {
            *byte = self.rx.pop_front().unwrap();
        }
        Ok(n)
    }
}
impl Write for Fake {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes.lock().unwrap().push(bytes.to_vec());
        let reply = self.replies.get(bytes).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Unexpected test request {bytes:02x?}"),
            )
        })?;
        self.rx.extend(reply);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn wire(command: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x5a, command, payload.len() as u8];
    frame.extend(payload);
    frame
}
fn device(fake: Fake) -> X4<Fake> {
    X4 {
        session: Session::new(fake),
        firmware: "fixture".into(),
    }
}
fn profiles_fake() -> Fake {
    let mut fake = Fake::default();
    fake.replies.insert(
        hex("5a1d03010202"),
        hex("5a1d160100020204040e408c0003408c0002408c0002408c00"),
    );
    for (tx, rx) in [
        ("5a1803010200", "5a18100102000505004d007500730069006300"),
        ("5a1803010201", "5a18100102010505004d006f00760069006500"),
        (
            "5a1803010202",
            "5a182a01020212120046006f006f00740073007400650070007300200045006e00680061006e00630065007200",
        ),
        ("5a1803010203", "5a180e0102030404005300580046004900"),
    ] {
        fake.replies.insert(hex(tx), hex(rx));
    }
    fake
}
fn add_context(fake: &mut Fake, output: Output, direct: bool, sxfi: bool) {
    fake.replies.insert(
        wire(0x2c, &[1]),
        wire(
            0x2c,
            &[1, if output == Output::Headphone { 4 } else { 1 }, 0, 0, 0],
        ),
    );
    fake.replies.insert(
        wire(0x39, &[1, 255, 255, 255, 255]),
        wire(0x39, &[1, if direct { 0x20 } else { 0 }, 0, 0, 0]),
    );
    fake.replies
        .insert(wire(0x26, &[5]), hex("5a2606050001001e00"));
    fake.replies.insert(
        wire(0x26, &[8, 255, 255]),
        wire(0x26, &[8, 255, 255, u8::from(sxfi), 0, 0, 0, 0, 0, 0, 0]),
    );
    fake.replies
        .insert(wire(0x6f, &[0x0d]), hex("5a6f0b0d4d562020303030322e30"));
}
fn assert_no_setting_writes(writes: &Writes) {
    for frame in writes.lock().unwrap().iter() {
        let is_write = match frame.get(1).copied() {
            Some(0x1a) => frame.get(3) == Some(&0),
            Some(0x26) => frame.get(3) == Some(&7),
            Some(0x6f) => frame.get(3) == Some(&0x0c),
            _ => false,
        };
        assert!(!is_write, "Unexpected setting write {frame:02x?}");
    }
}

#[test]
fn captured_profile_count_and_utf16_names() {
    let fake = profiles_fake();
    let writes = fake.writes.clone();
    let profiles = device(fake).eq_profiles(Output::Headphone).unwrap();
    assert_eq!(
        profiles
            .iter()
            .map(|p| (p.index, p.name.as_str()))
            .collect::<Vec<_>>(),
        [
            (0, "Music"),
            (1, "Movie"),
            (2, "Footsteps Enhancer"),
            (3, "SXFI")
        ]
    );
    assert_eq!(writes.lock().unwrap().len(), 5);
}
#[test]
fn profile_name_decoder_handles_utf16_surrogate_pairs() {
    let mut fake = profiles_fake();
    let mut p = vec![1, 2, 0, 3, 3, 0];
    for unit in "Č🎵".encode_utf16() {
        p.extend(unit.to_le_bytes());
    }
    fake.replies.insert(hex("5a1803010200"), wire(0x18, &p));
    assert_eq!(
        device(fake).eq_profiles(Output::Headphone).unwrap()[0].name,
        "Č🎵"
    );
}
#[test]
fn profile_count_and_name_lengths_are_validated() {
    let mut fake = profiles_fake();
    fake.replies.insert(
        hex("5a1d03010202"),
        hex("5a1d160100020204030e408c0003408c0002408c0002408c00"),
    );
    let writes = fake.writes.clone();
    assert!(device(fake).eq_profiles(Output::Headphone).is_err());
    assert_eq!(writes.lock().unwrap().len(), 1);
    let mut fake = profiles_fake();
    fake.replies
        .insert(hex("5a1803010200"), hex("5a18080102000505004d00"));
    assert!(device(fake).eq_profiles(Output::Headphone).is_err());
}
#[test]
fn sxfi_state_uses_capability_order_and_decodes_modes() {
    for (mask, mode, expected) in [
        (4, "0d4d562020303030322e30", SxfiMode::On),
        (4, "0d50434720303030372e31", SxfiMode::Battle),
        (1, "0d50434720303030372e31", SxfiMode::Off),
    ] {
        let mut fake = Fake::default();
        // SXFI ID=30 occupies bit 2, not bit 30 or the first bit.
        fake.replies.insert(
            wire(0x26, &[5]),
            wire(0x26, &[5, 0, 3, 0, 1, 0, 2, 0, 30, 0]),
        );
        fake.replies.insert(
            wire(0x26, &[8, 255, 255]),
            wire(0x26, &[8, 255, 255, mask, 0, 0, 0, 0, 0, 0, 0]),
        );
        fake.replies
            .insert(wire(0x6f, &[0x0d]), wire(0x6f, &hex(mode)));
        let writes = fake.writes.clone();
        assert_eq!(device(fake).sxfi().unwrap(), Some(expected));
        assert_eq!(
            writes.lock().unwrap().len(),
            if expected == SxfiMode::Off { 2 } else { 3 }
        );
    }
}
#[test]
fn sxfi_unknown_mode_is_not_reported_as_on() {
    let mut fake = Fake::default();
    add_context(&mut fake, Output::Headphone, false, true);
    fake.replies
        .insert(wire(0x6f, &[0x0d]), wire(0x6f, b"\x0dXXXX0002.0"));
    assert!(device(fake).sxfi().is_err());
}
#[test]
fn captured_pending_ack_then_final_ack_does_not_resend() {
    let command = hex("5a6f050c4d562020");
    let mut response = hex("5a02046f010a00");
    response.extend(hex("5a260b08ffff0100000000000000"));
    response.extend(hex("5a020a6f000c00000000000000"));
    let mut fake = Fake::default();
    fake.replies.insert(command.clone(), response);
    let writes = fake.writes.clone();
    let mut session = Session::new(fake);
    session.set_pending(0x6f, b"\x0cMV  ", 0x0c).unwrap();
    assert_eq!(*writes.lock().unwrap(), vec![command]);
    assert!(session.usable);
    assert_eq!(session.notifications.front().unwrap().command, 0x26);
}
#[test]
fn pending_failure_or_wrong_terminal_selector_invalidates_session() {
    for final_payload in [
        hex("6f810c00000000000000"),
        hex("6f000d00000000000000"),
        hex("6f000c"),
    ] {
        let command = hex("5a6f050c4d562020");
        let mut response = hex("5a02046f010a00");
        response.extend(wire(2, &final_payload));
        let mut fake = Fake::default();
        fake.replies.insert(command, response);
        let writes = fake.writes.clone();
        let mut session = Session::new(fake);
        assert!(session.set_pending(0x6f, b"\x0cMV  ", 0x0c).is_err());
        assert!(!session.usable);
        assert!(session.set_pending(0x6f, b"\x0cMV  ", 0x0c).is_err());
        assert_eq!(writes.lock().unwrap().len(), 1);
    }
}
#[test]
fn invalid_profile_index_or_sxfi_context_never_writes_settings() {
    for (index, sxfi) in [(4, false), (3, false), (0, true)] {
        let mut fake = profiles_fake();
        add_context(&mut fake, Output::Headphone, false, sxfi);
        let writes = fake.writes.clone();
        assert!(device(fake).set_eq_profile(index).is_err());
        assert_no_setting_writes(&writes);
    }
    let mut fake = Fake::default();
    add_context(&mut fake, Output::Headphone, true, false);
    let writes = fake.writes.clone();
    assert!(device(fake).set_eq_profile(0).is_err());
    assert_no_setting_writes(&writes);
}
#[test]
fn sxfi_write_requires_headphone_output_and_direct_mode_off() {
    for (output, direct) in
        [(Output::Speaker, false), (Output::Headphone, true)]
    {
        let mut fake = Fake::default();
        add_context(&mut fake, output, direct, false);
        let writes = fake.writes.clone();
        assert!(device(fake).set_sxfi(SxfiMode::On).is_err());
        assert_no_setting_writes(&writes);
    }
}
#[test]
fn sxfi_button_write_checks_state_bit_and_poisons_on_mismatch() {
    for (actual, succeeds) in [(1, true), (0, false)] {
        let mut fake = Fake::default();
        fake.replies
            .insert(wire(0x26, &[5]), wire(0x26, &[5, 0, 2, 0, 1, 0, 30, 0]));
        fake.replies.insert(
            wire(0x26, &[7, 30, 0, 1, 0]),
            wire(2, &[0x26, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        );
        // SXFI is the second advertised button: state bit 1.
        fake.replies.insert(
            wire(0x26, &[8, 255, 255]),
            wire(0x26, &[8, 255, 255, actual << 1, 0, 0, 0, 0, 0, 0, 0]),
        );
        let mut dev = device(fake);
        assert_eq!(dev.set_sxfi_button(true).is_ok(), succeeds);
        assert_eq!(dev.session.usable, succeeds);
    }
}
