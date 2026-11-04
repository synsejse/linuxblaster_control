use super::*;
use crate::{
    protocol::{Frame, Parser, Session, unlock},
    testing::{Fake, ack, device, fake, hex, wire},
};

#[test]
fn every_fragment_boundary_and_coalesced_frames() {
    let bytes =
        hex("5a03023b005b020a006d0000000000000000005a11080100960a000010c1");
    let expected = vec![
        Frame {
            marker: 0x5a,
            command: 3,
            payload: hex("3b00"),
        },
        Frame {
            marker: 0x5b,
            command: 2,
            payload: hex("6d000000000000000000"),
        },
        Frame {
            marker: 0x5a,
            command: 0x11,
            payload: hex("0100960a000010c1"),
        },
    ];
    for cut in 0..=bytes.len() {
        let mut parser = Parser::default();
        let mut frames = parser.feed(&bytes[..cut]).unwrap();
        frames.extend(parser.feed(&bytes[cut..]).unwrap());
        assert_eq!(frames, expected);
    }
    assert!(Parser::default().feed(&hex("5b02ffff")).is_err());
    assert_eq!(Parser::default().feed(&hex("5b020a")).unwrap(), Vec::new());
}
#[test]
fn real_unlock_vector() {
    let rows: serde_json::Value = serde_json::from_str(
        include_str!("../tests/fixtures/unlock.json")
            .trim_start_matches('\u{feff}'),
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    let challenge = hex(rows
        .iter()
        .find(|r| {
            r["direction"] == "rx"
                && r["hex"].as_str().unwrap().starts_with("77686f617265796f75")
        })
        .unwrap()["hex"]
        .as_str()
        .unwrap());
    let sent = hex(rows
        .iter()
        .find(|r| {
            r["direction"] == "tx"
                && r["hex"].as_str().unwrap().starts_with("756e6c6f636b")
        })
        .unwrap()["hex"]
        .as_str()
        .unwrap());
    assert_eq!(
        unlock(&challenge[9..45], sent[6..22].try_into().unwrap()).unwrap(),
        sent
    );
}
#[test]
fn cold_session_handles_binary_challenge_with_embedded_crlf() {
    let mut challenge = b"whoareyou".to_vec();
    challenge.extend(hex("1e047832"));
    challenge.extend(b"\r\n");
    challenge.extend([0; 30]);
    challenge.extend(b"\r\n");
    let mut port = fake(vec![
        (b"\r\nwhoareyou.MyApp8\r\n".to_vec(), challenge),
        (
            b"SW_MODE1\r\n".to_vec(),
            hex("5b020a006d000000000000000000"),
        ),
        (
            hex("5a090102"),
            hex("5a09120210312e372e3235303332342e3039313000"),
        ),
    ]);
    port.cold = true;
    let writes = port.writes.clone();
    let mut session = Session::new(port);
    session.set_timeout(Duration::from_millis(10));
    assert_eq!(session.connect().unwrap(), "1.7.250324.0910");
    assert_eq!(session.max_payload, 59);
    assert_eq!(writes.lock().unwrap().len(), 6);
}
#[test]
fn warm_session_skips_unlock() {
    let port = fake(vec![
        (hex("5a0300"), hex("5a03023b00")),
        (
            hex("5a090102"),
            hex("5a09120210312e372e3235303332342e3039313000"),
        ),
    ]);
    let writes = port.writes.clone();
    assert!(X4::connect(port).is_ok());
    assert_eq!(writes.lock().unwrap().len(), 2);
}
#[test]
fn timeout_invalidates_session_and_never_retries_write() {
    let port = Fake::default();
    let writes = port.writes.clone();
    let mut session = Session::new(port);
    session.set_timeout(Duration::from_millis(1));
    assert!(session.set(0x39, &[0, 5, 1]).is_err());
    assert!(session.set(0x39, &[0, 5, 1]).is_err());
    assert_eq!(writes.lock().unwrap().len(), 1);
}
#[test]
fn failed_or_malformed_ack_is_not_success() {
    for response in [ack(0x39, 0x81), wire(2, &[0x39, 0])] {
        let mut session =
            Session::new(fake(vec![(wire(0x39, &[0, 5, 1]), response)]));
        assert!(session.set(0x39, &[0, 5, 1]).is_err());
        assert!(!session.usable);
    }
}
#[test]
fn unrelated_notifications_are_retained() {
    let mut response = wire(0x2c, &[1, 4, 0, 0, 0]);
    response.extend(hex("5a11080100960a000010c1"));
    let mut dev = device(fake(vec![(hex("5a110301960a"), response)]));
    assert_eq!(dev.eq_parameter(10).unwrap(), -9.0);
    assert_eq!(dev.session.notifications.front().unwrap().command, 0x2c);
}
#[test]
fn eq_and_profile_values_are_validated_before_any_io() {
    let port = Fake::default();
    let writes = port.writes.clone();
    let mut dev = device(port);
    for value in [f32::NAN, f32::INFINITY, -13.0, 13.0] {
        let eq = Equalizer {
            enabled: true,
            preamp_db: value,
            bands_db: [0.0; 10],
        };
        assert!(dev.apply_eq(Output::Headphone, &eq).is_err());
    }
    assert!(writes.lock().unwrap().is_empty());
}
#[test]
fn feature_change_requires_support_ack_and_matching_readback() {
    for (actual, succeeds) in [(0x20, true), (0, false)] {
        let port = fake(vec![
            (
                wire(0x39, &[2]),
                wire(0x39, &[2, 0x60, 0, 0, 0, 0, 0, 0, 0]),
            ),
            (wire(0x39, &[0, 5, 1]), ack(0x39, 0)),
            (
                wire(0x39, &[1, 255, 255, 255, 255]),
                wire(0x39, &[1, actual, 0, 0, 0]),
            ),
        ]);
        let writes = port.writes.clone();
        let mut dev = device(port);
        assert_eq!(
            dev.set_feature(Feature::DirectMode, true).is_ok(),
            succeeds
        );
        assert_eq!(
            writes
                .lock()
                .unwrap()
                .iter()
                .filter(|w| **w == wire(0x39, &[0, 5, 1]))
                .count(),
            1
        );
    }
}
#[test]
fn output_change_polls_until_the_firmware_reports_the_new_route() {
    // The X4 ACKs the switch first; speaker output reads back late.
    let headphone = wire(0x2c, &[1, 4, 0, 0, 0]);
    let speaker = wire(0x2c, &[1, 1, 0, 0, 0]);
    for (readback, settled, succeeds) in [
        (vec![headphone.clone(), headphone.clone()], &speaker, true),
        (vec![], &headphone, false),
    ] {
        let mut port = fake(vec![
            (
                wire(0x2c, &[2]),
                wire(0x2c, &[2, 0x82, 1, 0, 0, 0, 4, 0, 0, 0]),
            ),
            (wire(0x2c, &[0, 1, 0, 0, 0]), ack(0x2c, 0)),
            (wire(0x2c, &[1]), settled.clone()),
        ]);
        port.sequences.insert(wire(0x2c, &[1]), readback.into());
        let writes = port.writes.clone();
        let mut dev = device(port);
        assert_eq!(dev.set_output(Output::Speaker).is_ok(), succeeds);
        assert_eq!(dev.session.usable, succeeds);
        // Waiting must only re-read: the switch itself is never replayed.
        assert_eq!(
            writes
                .lock()
                .unwrap()
                .iter()
                .filter(|w| **w == wire(0x2c, &[0, 1, 0, 0, 0]))
                .count(),
            1
        );
    }
}
#[test]
fn recorded_eq_response() {
    let rows: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/control-reads.json"
    ))
    .unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "dsp_96_0a")
        .unwrap();
    let mut dev = device(fake(vec![(
        hex(row["tx"].as_str().unwrap()),
        hex(row["rx"].as_str().unwrap()),
    )]));
    assert_eq!(dev.eq_parameter(10).unwrap(), -9.0);
}
#[test]
fn profile_assembles_captured_blocks_and_rejects_wrong_order() {
    let rows: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/capabilities.json"
    ))
    .unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "device_profile_page2")
        .unwrap();
    let tx = hex(row["tx"].as_str().unwrap());
    let rx = hex(row["rx"].as_str().unwrap());
    let mut dev = device(fake(vec![(tx.clone(), rx.clone())]));
    let eq = dev.stored_eq_profile(Output::Headphone, 0).unwrap();
    assert_eq!(eq.preamp_db, -9.0);
    assert_eq!(eq.bands_db[1], 9.0);
    let mut frames = Parser::default().feed(&rx).unwrap();
    frames[1].payload[6] = 2;
    let bad = frames
        .iter()
        .flat_map(|f| wire(f.command, &f.payload))
        .collect();
    assert!(
        device(fake(vec![(tx, bad)]))
            .stored_eq_profile(Output::Headphone, 0)
            .is_err()
    );
}

#[test]
fn eq_write_requires_independent_readback_and_poison_on_mismatch() {
    for (actual, succeeds) in [(-9.5_f32, true), (-9.0, false)] {
        let mut set = vec![1, 0x96, 10];
        set.extend((-9.5_f32).to_le_bytes());
        let mut response = vec![1, 0, 0x96, 10];
        response.extend(actual.to_le_bytes());
        let port = fake(vec![
            (wire(0x12, &set), ack(0x12, 0)),
            (wire(0x11, &[1, 0x96, 10]), wire(0x11, &response)),
        ]);
        let writes = port.writes.clone();
        let mut dev = device(port);
        assert_eq!(dev.set_eq_parameter(10, -9.5).is_ok(), succeeds);
        assert_eq!(writes.lock().unwrap().len(), 2);
        assert_eq!(dev.session.usable, succeeds);
    }
}

#[test]
fn apply_eq_rejects_output_change_before_writes() {
    let port = fake(vec![
        (wire(0x10, &[]), wire(0x10, &[0x40, 0, 0, 0, 0, 0, 0, 0])),
        (wire(0x2c, &[1]), wire(0x2c, &[1, 1, 0, 0, 0])),
    ]);
    let writes = port.writes.clone();
    let mut dev = device(port);
    let eq = Equalizer {
        enabled: true,
        preamp_db: -9.0,
        bands_db: [0.0; 10],
    };
    assert!(dev.apply_eq(Output::Headphone, &eq).is_err());
    assert!(writes.lock().unwrap().iter().all(|w| w[1] != 0x12));
}
