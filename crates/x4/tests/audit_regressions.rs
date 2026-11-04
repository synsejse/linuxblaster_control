//! Synthetic fixtures derived from the 2026-10-30 firmware audits. No new live observations.
#![expect(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    reason = "Scripts build small fixed frames, and a bad one should fail the test"
)]

use soundblaster_x4::{
    Error, X4, dsp::*, protocol::Session, software::*, sxfi::ModeToken,
    transfer::*,
};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
};
#[derive(Default)]
struct Script {
    steps: VecDeque<(Vec<u8>, Vec<u8>)>,
    rx: VecDeque<u8>,
    max_read: usize,
}
impl Read for Script {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.rx.is_empty() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let n = out.len().min(self.rx.len()).min(if self.max_read == 0 {
            usize::MAX
        } else {
            self.max_read
        });
        for x in &mut out[..n] {
            *x = self.rx.pop_front().unwrap();
        }
        Ok(n)
    }
}
impl Write for Script {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let (want, reply) = self.steps.pop_front().expect("unexpected write");
        assert_eq!(data, want);
        self.rx.extend(reply);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn frame(c: u8, p: &[u8]) -> Vec<u8> {
    let mut v = vec![0x5a, c, p.len() as u8];
    v.extend(p);
    v
}
fn dev(steps: Vec<(Vec<u8>, Vec<u8>)>) -> X4<Script> {
    X4 {
        session: Session::new(Script {
            steps: steps.into(),
            ..Default::default()
        }),
        firmware: "static fixture".into(),
    }
}
#[test]
fn factory_and_current_bank_profiles_preserve_request_identity() {
    let mut data = 12u32.to_le_bytes().to_vec();
    for parameter in 9..=20 {
        data.extend([0x96, parameter]);
        let value: f32 = if parameter == 9 { 1.0 } else { 0.0 };
        data.extend(value.to_le_bytes());
    }
    for (bank, source, flags) in [
        (ProfileBank::Speaker, ProfileSource::Factory, 1),
        (ProfileBank::Headphone, ProfileSource::Factory, 3),
        (ProfileBank::Current, ProfileSource::Writable, 14),
        (ProfileBank::Current, ProfileSource::Factory, 15),
    ] {
        let mut first = vec![1, 2, 0, flags, 76, 0, 128];
        first.extend(&data[..52]);
        let mut second = vec![1, 2, 0, flags, 76, 0, 1];
        second.extend(&data[52..]);
        let mut replies = frame(0x17, &first);
        replies.extend(frame(0x17, &second));
        let mut device = dev(vec![(frame(0x17, &[1, 2, 0, flags]), replies)]);
        let eq = device.read_eq_profile(bank, source, 0).unwrap();
        assert!(eq.enabled);
        assert_eq!(eq.bands_db, [0.0; 10]);
    }
    let mut device = dev(vec![(
        frame(0x1d, &[1, 2, 3]),
        frame(0x1d, &[1, 0, 2, 3, 1, 1, 0x8e, 0x40, 0x8c, 0]),
    )]);
    assert_eq!(
        device
            .profile_inventory(
                soundblaster_x4::Output::Headphone,
                ProfileSource::Factory
            )
            .unwrap(),
        vec![[0x8e, 0x40, 0x8c, 0]],
    );
}

#[test]
fn profile_listing_accepts_fragmented_32_character_names() {
    let name = b"0123456789abcdefghijklmnopqrstuv";
    let mut first = vec![1, 2, 0, 128 | 32, 26, 0];
    first.extend(name[..26].iter().flat_map(|b| [*b, 0]));
    let mut second = vec![1, 2, 0, 32, 6, 26];
    second.extend(name[26..].iter().flat_map(|b| [*b, 0]));
    let mut replies = frame(0x18, &first);
    replies.extend(frame(0x18, &second));
    let mut device = dev(vec![
        (
            frame(0x1d, &[1, 2, 2]),
            frame(0x1d, &[1, 0, 2, 2, 1, 1, 14, 64, 140, 0]),
        ),
        (frame(0x18, &[1, 2, 0]), replies),
    ]);
    let profiles = device
        .eq_profiles(soundblaster_x4::Output::Headphone)
        .unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name.as_bytes(), name);
}
#[test]
fn mixer_explicit_channel_and_wrong_echo() {
    for ch in [0, 1] {
        let mut d = dev(vec![(
            frame(0x23, &[1, 0, 0, 0, 0]),
            frame(0x23, &[0, 0, 244, ch]),
        )]);
        assert_eq!(d.mixer_channel_level(0, 0).is_ok(), ch == 0);
    }
}
#[test]
fn all_channel_shape_duplicate_and_missing() {
    for bad in 0..3 {
        let mut p = vec![2];
        for ch in 0..3 {
            p.extend([0, 0, if bad == 1 { 0 } else { ch }]);
        }
        if bad == 2 {
            p.pop();
        }
        let mut d =
            dev(vec![(frame(0x23, &[1, 2, 0, 0, 255]), frame(0x23, &p))]);
        assert_eq!(d.mixer_channels(2).is_ok(), bad == 0);
    }
}
#[test]
fn dsp_fragment_boundaries_bits_and_unrelated_frames() {
    let params: Vec<_> = (9..21)
        .map(|parameter| Parameter {
            module: 0x96,
            parameter,
        })
        .collect();
    let mut q = vec![12];
    for p in &params {
        q.extend([p.module, p.parameter]);
    }
    let mut reply = frame(0x70, &[2, b'e', b'n']);
    for (i, chunk) in params.chunks(9).enumerate() {
        let mut p = vec![chunk.len() as u8, u8::from(i == 0)];
        for v in chunk {
            p.extend([v.module, v.parameter]);
            p.extend(0x8000_0000_u32.to_le_bytes());
        }
        reply.extend(frame(0x11, &p));
    }
    let mut d = dev(vec![(frame(0x11, &q), reply)]);
    let v = d.dsp_read(&params, false).unwrap();
    assert_eq!(v.len(), 12);
    assert!(v.iter().all(|v| v.bits == 0x8000_0000));
    assert_eq!(d.session.notifications.front().unwrap().command, 0x70);
    assert!(decode_fragment(&[10, 0]).is_err());
}
#[test]
fn codecs_crc_resume_bitmap_and_processing() {
    assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(crc32(0, b"1234"), b"56789"), 0xcbf4_3926);
    let info = TransferInfo {
        total: 400,
        offset: 20,
        token: 7,
        slots: 4,
        resume: true,
        packet_size: 1024,
        crc: Some(5),
    };
    assert_eq!(TransferInfo::decode(&info.encode().unwrap()).unwrap(), info);
    let mut p = info.encode().unwrap();
    p[14] = 1;
    assert!(TransferInfo::decode(&p).is_err());
    let r = WindowReply::decode(&[4, 129, 242, 255, 255, 255, 0, 0, 0, 0], 4)
        .unwrap();
    assert_eq!(r.missing, 2);
    let p = [9, 20, 129, 7, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        ProcessingResult::decode(&p, 20, 7).unwrap().status,
        ProcessingStatus::Failed(129)
    );
    assert!(ProcessingResult::decode(&p, 20, 8).is_err());
    assert!(data_payload(FrameWidth::Short, 32, &[1]).is_err());
    assert!(data_payload(FrameWidth::Short, 0, &[1; 253]).is_err());
}
#[test]
fn room_stale_stack_sanitized_and_private_debug() {
    let info = sanitize_download(DownloadKind::RoomDescriptor, vec![0xaa; 154])
        .unwrap();
    assert_eq!(&info.0[..8], &[0; 8]);
    assert_eq!(&info.0[146..], &[0; 8]);
    assert_eq!(&info.0[8..146], &[0xaa; 138]);
    assert_eq!(format!("{info:?}"), "Information([redacted])");
}
#[test]
fn software_semantics() {
    assert_eq!(ModeToken::Movie.bytes(), *b"MV  ");
    assert_eq!(ModeToken::Fps.bytes(), *b"FPS ");
    assert_eq!(ModeToken::Music.bytes(), *b"MSC ");
    assert_eq!(PercentTable::Playback.encode(94).unwrap(), -231);
    assert_eq!(PercentTable::Playback.decode(0), 100);
    assert_eq!(tone_curve(0, 0, 12.).unwrap(), [0.; 10]);
    assert_eq!(
        adjust_tone([1.; 10], (0, 0), (0, 0), 12.).unwrap(),
        [1.; 10]
    );
}
fn upload_script(status: u8, ack_offset: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
    let w = FrameWidth::Extended;
    let data = [1, 2, 3];
    let start = frame(0x98, &[2, 0, 0, 0, 0, 7, 0, 0, 0, 1, 32, 0]);
    let info = TransferInfo {
        total: 3,
        offset: 0,
        token: 7,
        slots: 2,
        resume: false,
        packet_size: 32,
        crc: Some(crc32(0, &data)),
    };
    let mut progress = vec![4, 0];
    progress.extend(0xffff_fffe_u32.to_le_bytes());
    progress.extend(3u32.to_le_bytes());
    let mut ack = vec![0x98, 0, 5];
    ack.extend(ack_offset.to_le_bytes());
    ack.extend([0; 3]);
    let mut final_reply = frame(2, &ack);
    final_reply.extend(frame(0x98, &[9, 22, 1, 7, 0, 0, 0, 0, 0, 0, 0]));
    final_reply.extend(frame(0x98, &[9, 22, status, 7, 0, 0, 0, 0, 0, 0, 0]));
    vec![
        (frame(0x98, &[2, 3, 0, 0, 0, 22]), start),
        (
            w.frame(&info.encode().unwrap()).unwrap(),
            frame(2, &[0x98, 0, 3, 1, 0, 0, 0, 0, 0, 0]),
        ),
        (
            w.frame(&data_payload(w, 0, &data).unwrap()).unwrap(),
            vec![],
        ),
        (w.frame(&[6, 128, 0]).unwrap(), frame(0x98, &progress)),
        (w.frame(&[5, 3, 0, 0, 0]).unwrap(), final_reply),
    ]
}
#[test]
fn upload_waits_processing_and_rejects_crc_failure_or_wrong_shift_ack() {
    for (status, offset, ok) in [(0, 3, true), (129, 3, false), (0, 2, false)] {
        let mut d = dev(upload_script(status, offset));
        let asset =
            PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
        assert_eq!(d.upload_prepared(&asset).is_ok(), ok);
        assert_eq!(d.session.usable, ok);
    }
}
#[test]
fn upload_resume_is_explicit_error() {
    let mut d = dev(vec![(
        frame(0x98, &[2, 1, 0, 0, 0, 22]),
        frame(0x98, &[2, 0, 0, 0, 0, 0, 0, 0, 0, 128, 32, 0]),
    )]);
    let a = PreparedUpload::new(UploadKind::Metadata, vec![1]).unwrap();
    let e = d.upload_prepared(&a).unwrap_err();
    assert!(matches!(e, Error::Unsupported(_)));
}
#[test]
fn upload_missing_mask_resends_only_requested_slot_and_checks_offset() {
    for offset in [0u32, 1] {
        let mut steps = upload_script(0, 3);
        let mut recovery = vec![4, 0x81];
        recovery.extend(u32::MAX.to_le_bytes());
        recovery.extend(offset.to_le_bytes());
        let marker = steps[3].clone();
        steps[3].1 = frame(0x98, &recovery);
        steps.insert(4, steps[2].clone());
        steps.insert(5, marker);
        let mut d = dev(steps);
        let a =
            PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
        assert_eq!(d.upload_prepared(&a).is_ok(), offset == 0);
    }
    let mut steps = upload_script(0, 3);
    let final_marker = steps[3].clone();
    let mut recovery = vec![4, 0x81];
    recovery.extend(0xffff_fffd_u32.to_le_bytes());
    recovery.extend(0u32.to_le_bytes());
    steps[3].1 = frame(0x98, &recovery);
    steps.insert(4, steps[2].clone());
    steps.insert(5, final_marker);
    let mut d = dev(steps);
    let a = PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
    assert!(d.upload_prepared(&a).is_ok());
}
#[test]
fn download_checks_complete_window_and_crc_before_sanitizing() {
    for missing in [false, true] {
        let w = FrameWidth::Extended;
        let bytes = vec![0xa5; 154];
        let info = TransferInfo {
            total: 154,
            offset: 0,
            token: 0,
            slots: 32,
            resume: false,
            packet_size: 59,
            crc: Some(crc32(0, &bytes)),
        };
        let mut frames = vec![];
        for (i, c) in bytes.chunks(51).enumerate() {
            if missing && i == 1 {
                continue;
            }
            frames.extend(
                w.frame(&data_payload(w, i as u8, c).unwrap()).unwrap(),
            );
        }
        frames.extend(w.frame(&[6, 128, 3]).unwrap());
        let mut start = vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 31];
        start.extend(59u16.to_le_bytes());
        let mut progress = vec![4, 0];
        progress.extend(0xffff_fff0_u32.to_le_bytes());
        progress.extend(154u32.to_le_bytes());
        let mut steps = vec![
            (
                w.frame(&[2, 0, 0, 0, 0, 0x93]).unwrap(),
                frame(2, &[0x98, 0, 2, 0, 0, 0, 0, 0x13, 0, 0]),
            ),
            (
                w.frame(&start).unwrap(),
                w.frame(&info.encode().unwrap()).unwrap(),
            ),
            (
                vec![0x5b, 2, 10, 0, 0x98, 0, 3, 1, 0, 0, 0, 0, 0, 0],
                frames,
            ),
            (
                w.frame(&progress).unwrap(),
                w.frame(&[5, 154, 0, 0, 0]).unwrap(),
            ),
            (
                vec![0x5b, 2, 10, 0, 0x98, 0, 5, 0, 0, 0, 0, 0, 0, 0],
                vec![],
            ),
        ];
        if missing {
            let mut recovery = vec![4, 0x81];
            recovery.extend(0xffff_fff2_u32.to_le_bytes());
            recovery.extend(0u32.to_le_bytes());
            let mut recovered = w
                .frame(&data_payload(w, 1, &bytes[51..102]).unwrap())
                .unwrap();
            recovered.extend(w.frame(&[6, 128, 3]).unwrap());
            steps.insert(3, (w.frame(&recovery).unwrap(), recovered));
        }
        let mut d = dev(steps);
        let result = d.download_asset(DownloadKind::RoomDescriptor);
        assert!(result.is_ok(), "{result:?}");
        if let Ok(info) = result {
            assert_eq!(&info.0[..8], &[0; 8]);
            assert_eq!(&info.0[146..], &[0; 8]);
        }
    }
}
#[test]
fn metadata_roundtrip_and_truncated_or_private_fields() {
    let m = UserMetadata {
        user_id: b"synthetic".to_vec(),
        description: vec![],
        timestamp: 42,
        parent_profile_id: vec![],
        generation: b"1".to_vec(),
    };
    let p = m.encode().unwrap();
    let restored = UserMetadata::decode(&p).unwrap();
    assert_eq!(restored.timestamp, 42);
    assert_eq!(restored.user_id, b"synthetic");
    assert_eq!(format!("{restored:?}"), "UserMetadata([redacted])");
    for n in 0..p.len() {
        assert!(UserMetadata::decode(&p[..n]).is_err());
    }
    let mut invalid = p.clone();
    invalid.push(0);
    assert!(UserMetadata::decode(&invalid).is_err());
    assert!(
        TransferStart::ExperimentalFileUpload {
            file_type: 2,
            total: 1
        }
        .encode()
        .is_err()
    );
    assert_eq!(
        TransferStart::Download(DownloadKind::RoomDescriptor)
            .encode()
            .unwrap(),
        [2, 0, 0, 0, 0, 0x93]
    );
}

#[test]
fn early_processing_is_retained_only_for_current_transfer() {
    for status in [0, 129] {
        for before in [1, 3, 4] {
            let mut steps = upload_script(status, 3);
            let early = frame(0x98, &[9, 22, status, 7, 0, 0, 0, 0, 0, 0, 0]);
            steps[4].1.truncate(13); // shift ACK only
            let mut response = frame(0x70, &[1]);
            response.extend(early);
            response.extend(&steps[before].1);
            steps[before].1 = response;
            let mut d = dev(steps);
            let a = PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3])
                .unwrap();
            assert_eq!(d.upload_prepared(&a).is_ok(), status == 0);
            assert_eq!(d.session.notifications.front().unwrap().command, 0x70);
        }
    }
}

#[test]
fn processing_polls_identity_and_host_deadline() {
    use std::time::Duration;
    let policy = ProcessingPolicy {
        // Successful polling must tolerate scheduler delays during parallel builds.
        // The separate case below verifies deadline expiry with no reply.
        timeout: Duration::from_secs(1),
        poll_interval: Duration::from_millis(2),
    };
    let mut steps = upload_script(0, 3);
    steps[4].1.truncate(13);
    steps[4]
        .1
        .extend(frame(0x98, &[9, 22, 0, 8, 0, 0, 0, 0, 0, 0, 0])); // wrong token
    steps[4]
        .1
        .extend(frame(0x98, &[9, 20, 0, 7, 0, 0, 0, 0, 0, 0, 0])); // wrong type
    steps.push((
        frame(0x98, &[9, 22, 7, 0, 0, 0]),
        frame(0x98, &[9, 22, 0, 7, 0, 0, 0, 0, 0, 0, 0]),
    ));
    let a = PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
    assert!(dev(steps).upload_prepared_with_policy(&a, policy).is_ok());

    let mut steps = upload_script(0, 3);
    steps[4].1.truncate(13);
    // Equal polling interval/deadline: no query after host timeout.
    let policy = ProcessingPolicy {
        timeout: Duration::from_millis(2),
        poll_interval: Duration::from_millis(2),
    };
    let mut d = dev(steps);
    d.session
        .notifications
        .push_back(soundblaster_x4::protocol::Frame {
            marker: 0x5a,
            command: 0x98,
            payload: vec![9, 22, 0, 7, 0, 0, 0, 0, 0, 0, 0],
        });
    assert!(d.upload_prepared_with_policy(&a, policy).is_err());
    assert!(!d.session.usable);
}

#[test]
fn metadata_download_variable_lengths_and_invalid_strings() {
    for n in [0, 1, 54, 200, 496] {
        let m = UserMetadata {
            user_id: vec![b'a'; n],
            description: vec![],
            timestamp: 0,
            parent_profile_id: vec![],
            generation: vec![],
        };
        let p = m.encode().unwrap();
        assert!(
            sanitize_download(DownloadKind::UserMetadata, p.clone()).is_ok()
        );
        for end in 0..p.len() {
            assert!(
                sanitize_download(
                    DownloadKind::UserMetadata,
                    p[..end].to_vec()
                )
                .is_err()
            );
        }
        let mut bad = p;
        bad[4..].fill(1);
        assert!(sanitize_download(DownloadKind::UserMetadata, bad).is_err());
    }
    let m = UserMetadata {
        user_id: vec![0],
        description: vec![],
        timestamp: 0,
        parent_profile_id: vec![],
        generation: vec![],
    };
    assert!(m.encode().is_err());
}

fn download_log_script(width: FrameWidth, bad: u8) -> Vec<(Vec<u8>, Vec<u8>)> {
    let w = FrameWidth::Extended;
    let stride = if width == FrameWidth::Short { 53 } else { 51 };
    let data: Vec<u8> = (0..=stride * 3).map(|i| i as u8).collect();
    let info = TransferInfo {
        total: data.len() as u32,
        offset: 0,
        token: 0,
        slots: 2,
        resume: false,
        packet_size: 59,
        crc: Some(crc32(0, &data) ^ u32::from(bad == 1)),
    };
    let mut first = width
        .frame(&data_payload(width, 1, &data[stride..2 * stride]).unwrap())
        .unwrap();
    // Flagged sequence byte must be masked to five bits.
    let mut p = data_payload(width, 0, &data[..stride]).unwrap();
    p[1] |= 0x80;
    first.extend(width.frame(&p).unwrap());
    let mut second = width
        .frame(&data_payload(width, 1, &data[3 * stride..]).unwrap())
        .unwrap();
    second.extend(
        width
            .frame(
                &data_payload(width, 0, &data[2 * stride..3 * stride]).unwrap(),
            )
            .unwrap(),
    );
    if bad == 2 {
        let corrupt = vec![0; stride];
        second.extend(
            width
                .frame(&data_payload(width, 0, &corrupt).unwrap())
                .unwrap(),
        );
    }
    second.extend(width.frame(&[6, 128, 1]).unwrap());
    let ack = |sel| vec![0x5b, 2, 10, 0, 0x98, 0, sel, 0, 0, 0, 0, 0, 0, 0];
    let progress = |n: usize| {
        let mut p = vec![4, 0];
        p.extend(0xffff_fffc_u32.to_le_bytes());
        p.extend((n as u32).to_le_bytes());
        w.frame(&p).unwrap()
    };
    let shift = |n: usize| {
        let mut p = vec![5];
        p.extend((n as u32).to_le_bytes());
        w.frame(&p).unwrap()
    };
    vec![
        (
            w.frame(&[2, 0, 0, 0, 0, 255]).unwrap(),
            frame(2, &[0x98, 0, 2, 0, 0, 0, 0, 127, 0, 0]),
        ),
        (
            w.frame(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 31, 59, 0]).unwrap(),
            w.frame(&info.encode().unwrap()).unwrap(),
        ),
        (vec![0x5b, 2, 10, 0, 0x98, 0, 3, 1, 0, 0, 0, 0, 0, 0], first),
        (progress(stride * 2), shift(stride * 2)),
        (ack(5), second),
        (progress(data.len()), shift(data.len())),
        (ack(5), vec![]),
    ]
}
#[test]
fn multiwindow_download_both_markers_reordered_and_crc_or_duplicate_errors() {
    for width in [FrameWidth::Short, FrameWidth::Extended] {
        for bad in 0..3 {
            let mut d = dev(download_log_script(width, bad));
            let result = d.download_asset(DownloadKind::Log);
            assert_eq!(result.is_ok(), bad == 0, "{result:?}");
            if let Ok(bytes) = result {
                assert!(bytes.0.iter().enumerate().all(|(i, b)| *b == i as u8));
            }
        }
    }
}

#[test]
fn upload_already_present_and_partial_validator_failures() {
    for status in [0x10, 0x82, 0x83, 0x84] {
        let mut steps = upload_script(0, 3);
        steps[3].1 = frame(0x98, &[4, status, 255, 255, 255, 255, 0, 0, 0, 0]);
        steps.truncate(4);
        let a =
            PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
        let result = dev(steps).upload_prepared(&a);
        if status == 0x10 {
            assert_eq!(result.unwrap(), UploadOutcome::AlreadyPresent);
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn profile_fields_follow_total_more_count_offset_and_both_pair_widths() {
    for field in [ProfileField::Name, ProfileField::Auxiliary] {
        let selector = if matches!(field, ProfileField::Name) {
            1
        } else {
            4
        };
        let mut d = dev(vec![(
            frame(0x18, &[selector, 2, 0]),
            frame(0x18, &[selector, 2, 0, 2, 2, 0, b'a', 0, b'b', 0]),
        )]);
        assert_eq!(d.profile_field(0, field).unwrap(), [b'a', 0, b'b', 0]);
    }
    let mut d = dev(vec![
        (
            frame(0x18, &[0, 2, 0, 130, 1, 0, b'a', 0]),
            frame(2, &[0x18, 0x80, 0, 0, 0, 0, 0, 0, 0, 0]),
        ),
        (
            frame(0x18, &[0, 2, 0, 2, 1, 1, b'b', 0]),
            frame(2, &[0x18, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        ),
    ]);
    d.session.max_payload = 8;
    d.write_profile_field(0, ProfileField::Name, b"ab").unwrap();
}

#[test]
fn split_reads_and_resume_checkpoint_validation() {
    let a = PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
    for split in [1, 2, 3, 7, 512] {
        let mut d = X4 {
            session: Session::new(Script {
                steps: upload_script(0, 3).into(),
                max_read: split,
                ..Default::default()
            }),
            firmware: "synthetic".into(),
        };
        assert!(d.upload_prepared(&a).is_ok());
    }
    for token in [7, 8] {
        let mut steps = upload_script(0, 3);
        steps[0].1 = frame(0x98, &[2, 1, 0, 0, 0, 7, 0, 0, 0, 129, 32, 0]);
        let w = FrameWidth::Extended;
        let info = TransferInfo {
            total: 3,
            offset: 1,
            token: 7,
            slots: 2,
            resume: true,
            packet_size: 32,
            crc: Some(crc32(0, &[1, 2, 3])),
        };
        steps[1].0 = w.frame(&info.encode().unwrap()).unwrap();
        steps[2].0 = w.frame(&data_payload(w, 0, &[2, 3]).unwrap()).unwrap();
        let checkpoint = UploadCheckpoint {
            file_type: 22,
            total: 3,
            token,
            offset: 1,
        };
        let mut d = dev(steps);
        assert_eq!(
            d.resume_prepared(&a, checkpoint, ProcessingPolicy::default())
                .is_ok(),
            token == 7
        );
    }
}
#[test]
fn local_cancel_returns_checkpoint_and_staging_is_distinct() {
    let a = PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
    let mut d = dev(upload_script(0, 3));
    let mut saved = None;
    assert!(
        d.upload_prepared_with_progress(&a, ProcessingPolicy::default(), |p| {
            saved = Some(p);
            false
        })
        .is_err()
    );
    assert_eq!(saved.unwrap().token, 7);
    assert!(!d.session.usable);

    let mut steps = upload_script(0, 3);
    steps[0].0 = frame(0x98, &[2, 3, 0, 0, 0, 1]);
    steps[4].1 = frame(2, &[0x98, 0, 5, 3, 0, 0, 0, 0, 0, 0]);
    steps[4]
        .1
        .extend(frame(0x98, &[9, 1, 0, 7, 0, 0, 0, 0, 0, 0, 0]));
    let a =
        PreparedUpload::new(UploadKind::FirmwareStage, vec![1, 2, 3]).unwrap();
    assert!(matches!(
        dev(steps).upload_prepared(&a).unwrap(),
        UploadOutcome::Staged(_)
    ));
}

#[test]
fn full_upload_window_requires_all_bits_clear_and_repeats_final_marker() {
    let w = FrameWidth::Extended;
    let data = vec![42; 48];
    for missing in [0u32, 2] {
        let mut steps = upload_script(0, 48);
        steps[0].0 = frame(0x98, &[2, 48, 0, 0, 0, 22]);
        let info = TransferInfo {
            total: 48,
            offset: 0,
            token: 7,
            slots: 2,
            resume: false,
            packet_size: 32,
            crc: Some(crc32(0, &data)),
        };
        steps[1].0 = w.frame(&info.encode().unwrap()).unwrap();
        steps[2].0 =
            w.frame(&data_payload(w, 0, &data[..24]).unwrap()).unwrap();
        steps[3].0 = w.frame(&[6, 128, 1]).unwrap();
        let mut reply = vec![4, 0];
        reply.extend((0xffff_fffc_u32 | missing).to_le_bytes());
        reply.extend(48u32.to_le_bytes());
        steps[3].1 = frame(0x98, &reply);
        steps[4].0 = w.frame(&[5, 48, 0, 0, 0]).unwrap();
        steps.insert(
            3,
            (
                w.frame(&data_payload(w, 1, &data[24..]).unwrap()).unwrap(),
                vec![],
            ),
        );
        let asset =
            PreparedUpload::new(UploadKind::Metadata, data.clone()).unwrap();
        assert_eq!(dev(steps).upload_prepared(&asset).is_ok(), missing == 0);
    }
}
#[test]
fn old_notifications_cannot_supply_ack_and_wrong_download_shift_fails() {
    use soundblaster_x4::protocol::Frame;
    use std::time::Duration;
    let mut steps = upload_script(0, 3);
    steps[1].1.clear();
    let mut d = dev(steps);
    d.session.set_timeout(Duration::from_millis(1));
    d.session.notifications.push_back(Frame {
        marker: 0x5a,
        command: 2,
        payload: vec![0x98, 0, 3, 1, 0, 0, 0, 0, 0, 0],
    });
    let asset =
        PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
    assert!(d.upload_prepared(&asset).is_err());
    assert!(!d.session.usable);
    let mut steps = download_log_script(FrameWidth::Extended, 0);
    steps[3].1 = FrameWidth::Extended.frame(&[5, 0, 0, 0, 0]).unwrap();
    assert!(dev(steps).download_asset(DownloadKind::Log).is_err());
}

#[test]
fn error_ack_cannot_be_hidden_by_later_window_success() {
    let mut steps = upload_script(0, 3);
    let mut response = frame(2, &[0x98, 0x80, 4, 0, 0, 0, 0, 0, 0, 0]);
    response.extend(&steps[3].1);
    steps[3].1 = response;
    let a = PreparedUpload::new(UploadKind::Metadata, vec![1, 2, 3]).unwrap();
    let mut d = dev(steps);
    assert!(d.upload_prepared(&a).is_err());
    assert!(!d.session.usable);
}

#[test]
fn nonfinal_missing_slot_recovers_after_host_interval() {
    let w = FrameWidth::Extended;
    let mut steps = download_log_script(w, 0);
    let missing = steps[2].1.drain(..59).collect::<Vec<_>>();
    let mut response = vec![4, 0x81];
    response.extend(0xffff_fffe_u32.to_le_bytes());
    response.extend(0u32.to_le_bytes());
    steps.insert(3, (w.frame(&response).unwrap(), missing));
    assert!(dev(steps).download_asset(DownloadKind::Log).is_ok());
}
