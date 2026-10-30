//! X4 hardware mixer. Q8.8 conversion follows the observed dB descriptor flag.
//! Preserve raw values because the playback controls' advertised steps are unusual.
//!
//! All level requests contain explicit channels. Master changes shift lookup
//! steps with clipping; full-channel restoration writes masters first.
use crate::{
    X4,
    protocol::{Result, invalid},
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Presentation grouping inferred from the descriptor's low-six-bit control type.
pub enum MixerGroup {
    Playback,
    Monitoring,
    Recording,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Advertised signed Q8.8 min/max and unsigned Q8.8 step, also expressed in dB.
/// Firmware quantization does not necessarily follow a `min + N * step` grid.
pub struct MixerRange {
    pub min_db: f32,
    pub max_db: f32,
    pub step_db: f32,
    pub min_raw: i16,
    pub max_raw: i16,
    pub step_raw: u16,
}
impl MixerRange {
    /// Encode the requested level at wire precision. The advertised step is
    /// not a min + N*step grid: live X4 writes quantize internally instead.
    pub fn encode(&self, db: f32) -> Result<i16> {
        if !db.is_finite() || db < self.min_db || db > self.max_db {
            return Err(invalid("Mixer level is outside the advertised range"));
        }
        if self.step_raw == 0 || self.min_raw > self.max_raw {
            return Err(invalid("Invalid mixer range/step"));
        }
        Ok((db * 256.0).round() as i16)
    }

    fn accepts_readback(&self, requested: i16, actual: i16) -> bool {
        // Live loopback writes differ by up to 101 Q8.8 units despite a
        // reported 64-unit step. Half a dB covers the observed quantization;
        // larger advertised steps remain relevant to playback controls.
        let tolerance = i32::from(self.step_raw.max(128));
        actual >= self.min_raw
            && actual <= self.max_raw
            && (i32::from(actual) - i32::from(requested)).abs() <= tolerance
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One advertised control and its explicit master level/flag readback.
/// Descriptor bits: type `003f`, volume `0040`, flag `0080`, dB `0800`;
/// the upper nibble counts physical channels (plus one master).
pub struct MixerControl {
    pub index: u8,
    pub name: String,
    pub group: MixerGroup,
    pub descriptor: u16,
    pub has_volume: bool,
    pub is_db: bool,
    pub range: Option<MixerRange>,
    pub level_db: Option<f32>,
    pub raw_level: Option<i16>,
    pub muted: Option<bool>,
    /// Type-63 Microphone Boost state. The wire flag is inverted (raw `0`
    /// means enabled); this field is already the enabled state.
    pub microphone_boost: Option<bool>,
}
impl MixerControl {
    /// Low six descriptor bits: the control type used for identity.
    pub fn kind(&self) -> u16 {
        self.descriptor & DESCRIPTOR_KIND
    }
    /// Whether this is the type-63 Microphone Boost flag control.
    pub fn is_microphone_boost(&self) -> bool {
        self.kind() == KIND_MICROPHONE_BOOST
    }
}

const DESCRIPTOR_KIND: u16 = 0x3f;
const DESCRIPTOR_VOLUME: u16 = 0x40;
const DESCRIPTOR_FLAG: u16 = 0x80;
const DESCRIPTOR_DB: u16 = 0x800;
/// Descriptor type of the Microphone Boost flag control.
const KIND_MICROPHONE_BOOST: u16 = 63;
/// Volume controls `0..10` have explicit channels; see [`channel_count`].
const VOLUME_CONTROLS: u8 = 10;

fn identity(kind: u16) -> (String, MixerGroup) {
    use MixerGroup::*;
    let (name, group) = match kind {
        1 => ("Speakers", Playback),
        2 => ("Headphones", Playback),
        14 => ("S/PDIF output", Playback),
        15 => ("Microphone", Monitoring),
        16 => ("Line-in", Monitoring),
        17 => ("S/PDIF", Monitoring),
        3 => ("Microphone", Recording),
        4 => ("Line-in", Recording),
        5 => ("What U Hear", Recording),
        9 => ("S/PDIF", Recording),
        KIND_MICROPHONE_BOOST => ("Microphone Boost", Other),
        _ => return (format!("Unknown control {kind}"), Other),
    };
    (name.into(), group)
}
/// Physical channels plus master for a volume control index.
fn channel_count(index: u8) -> Result<u8> {
    match index {
        0 | 1 => Ok(9),
        2..VOLUME_CONTROLS => Ok(3),
        _ => Err(invalid("Invalid volume index")),
    }
}
fn parse_channels(index: u8, n: usize, p: &[u8]) -> Result<Vec<i16>> {
    if p.len() != 1 + 3 * n || p[0] != index {
        return Err(invalid("Malformed all-channel reply"));
    }
    let mut values = vec![None; n];
    for t in p[1..].as_chunks::<3>().0.iter() {
        let ch = t[2] as usize;
        if ch >= n || values[ch].is_some() {
            return Err(invalid("Duplicate/invalid mixer channel"));
        }
        values[ch] = Some(i16::from_le_bytes([t[0], t[1]]));
    }
    values
        .into_iter()
        .map(|v| v.ok_or_else(|| invalid("Missing channel")))
        .collect()
}

impl<T: Read + Write> X4<T> {
    /// GET `21 [00]`; return `(index, descriptor)` for every control.
    pub fn mixer_descriptors(&mut self) -> Result<Vec<(u8, u16)>> {
        let p = self.session.get(0x21, &[0], |_| true)?;
        if p.len() < 4
            || p[0] != p[1]
            || p[2] != 0
            || p[3] != 0
            || p.len() < 4 + p[1] as usize * 2
        {
            return Err(invalid(
                "Unsupported fragmented or malformed mixer capabilities",
            ));
        }
        // Known firmware appends four undocumented bytes after the descriptors.
        Ok(p[4..4 + p[1] as usize * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .map(|(index, bytes)| (index as u8, u16::from_le_bytes(*bytes)))
            .collect())
    }
    fn mixer_descriptor(&mut self, index: u8) -> Result<u16> {
        self.mixer_descriptors()?
            .into_iter()
            .find(|(i, _)| *i == index)
            .map(|(_, descriptor)| descriptor)
            .ok_or_else(|| {
                invalid("Mixer control not advertised by this device")
            })
    }
    /// GET `22 [01 index 00...]`; advertised Q8.8 range and step.
    pub fn mixer_range(&mut self, index: u8) -> Result<MixerRange> {
        let mut request = [0; 33];
        request[0] = 1;
        request[1] = index;
        let p = self.session.get(0x22, &request, |f| {
            f.payload.len() >= 3 && f.payload[2] == index
        })?;
        if p.len() != 9 || p[..3] != [1, 0, index] {
            return Err(invalid("Malformed mixer range response"));
        }
        let max_raw = i16::from_le_bytes([p[3], p[4]]);
        let min_raw = i16::from_le_bytes([p[5], p[6]]);
        let step_raw = u16::from_le_bytes([p[7], p[8]]);
        if min_raw > max_raw || step_raw == 0 {
            return Err(invalid("Invalid mixer range or step"));
        }
        Ok(MixerRange {
            min_db: min_raw as f32 / 256.0,
            max_db: max_raw as f32 / 256.0,
            step_db: step_raw as f32 / 256.0,
            min_raw,
            max_raw,
            step_raw,
        })
    }
    /// Explicit channel read. Channels 0..8 for indices 0/1, 0..2 for 2..9.
    pub fn mixer_channel_level(
        &mut self,
        index: u8,
        channel: u8,
    ) -> Result<i16> {
        if channel >= channel_count(index)? {
            return Err(invalid("Invalid mixer channel"));
        }
        let p = self.session.get(0x23, &[1, index, 0, 0, channel], |f| {
            f.payload.first() == Some(&index)
        })?;
        if p.len() != 4 || p[0] != index || p[3] != channel {
            return Err(invalid("Mixer index/channel echo mismatch"));
        }
        Ok(i16::from_le_bytes([p[1], p[2]]))
    }
    /// FF is read-only. Returns master first, then physical channels; rejects holes/duplicates.
    pub fn mixer_channels(&mut self, index: u8) -> Result<Vec<i16>> {
        let n = channel_count(index)? as usize;
        let p = self.session.get(0x23, &[1, index, 0, 0, 255], |f| {
            f.payload.first() == Some(&index)
        })?;
        parse_channels(index, n, &p)
    }
    /// SET `23 [00 index raw:i16le channel]` and verify quantized readback.
    /// A mismatch invalidates the session.
    fn write_mixer_channel(
        &mut self,
        index: u8,
        channel: u8,
        raw: i16,
        range: &MixerRange,
    ) -> Result<()> {
        let mut p = vec![0, index];
        p.extend(raw.to_le_bytes());
        p.push(channel);
        self.session.set(0x23, &p)?;
        if !range
            .accepts_readback(raw, self.mixer_channel_level(index, channel)?)
        {
            return Err(self.session.mismatch(
                "Mixer level readback mismatch; reconnect and read actual state",
            ));
        }
        Ok(())
    }
    /// Exact Q8.8 request, checked against advertised range; readback can quantize.
    /// A master write shifts lookup steps with clipping, not fixed dB offsets.
    pub fn set_mixer_channel_raw(
        &mut self,
        index: u8,
        channel: u8,
        raw: i16,
    ) -> Result<()> {
        if channel >= channel_count(index)? {
            return Err(invalid("Invalid mixer channel"));
        }
        let range = self.mixer_range(index)?;
        if raw < range.min_raw || raw > range.max_raw {
            return Err(invalid("Mixer level outside range"));
        }
        self.write_mixer_channel(index, channel, raw, &range)
    }
    /// Read every channel of all volume controls (42 levels on the X4).
    pub fn capture_mixer_channels(&mut self) -> Result<Vec<Vec<i16>>> {
        (0..VOLUME_CONTROLS)
            .map(|i| self.mixer_channels(i))
            .collect()
    }
    /// Restores all masters before physical channels to avoid clobbering balance.
    pub fn restore_mixer_channels(
        &mut self,
        levels: &[Vec<i16>],
    ) -> Result<()> {
        if levels.len() != VOLUME_CONTROLS as usize
            || levels.iter().zip(0..).any(|(v, i)| {
                channel_count(i).is_ok_and(|n| v.len() != n as usize)
            })
        {
            return Err(invalid("Expected all 42 mixer levels"));
        }
        // Validate the complete snapshot before the first mutation.
        let mut ranges = Vec::with_capacity(levels.len());
        for (v, i) in levels.iter().zip(0..) {
            let r = self.mixer_range(i)?;
            if v.iter().any(|x| *x < r.min_raw || *x > r.max_raw) {
                return Err(invalid("Snapshot level outside range"));
            }
            ranges.push(r);
        }
        for ((v, r), i) in levels.iter().zip(&ranges).zip(0..) {
            self.write_mixer_channel(i, 0, v[0], r)?;
        }
        for ((v, r), i) in levels.iter().zip(&ranges).zip(0..) {
            for (raw, ch) in v.iter().zip(0..).skip(1) {
                self.write_mixer_channel(i, ch, *raw, r)?;
            }
        }
        Ok(())
    }
    /// Index of the advertised type-63 flag control.
    fn microphone_boost_index(&mut self) -> Result<u8> {
        self.mixer_descriptors()?
            .into_iter()
            .find(|(_, d)| {
                d & DESCRIPTOR_KIND == KIND_MICROPHONE_BOOST
                    && d & DESCRIPTOR_FLAG != 0
            })
            .map(|(index, _)| index)
            .ok_or_else(|| invalid("Microphone Boost is not advertised"))
    }
    /// GET `24 [01 index]` for the type-63 control; raw `0` means enabled.
    pub fn microphone_boost(&mut self) -> Result<bool> {
        let index = self.microphone_boost_index()?;
        Ok(!self.mixer_flag(index)?)
    }
    /// SET `24 [00 index !enabled]` on the type-63 control and verify readback.
    pub fn set_microphone_boost(&mut self, enabled: bool) -> Result<()> {
        let index = self.microphone_boost_index()?;
        self.session.set(0x24, &[0, index, u8::from(!enabled)])?;
        if self.mixer_flag(index)? == enabled {
            return Err(self.session.mismatch(
                "Microphone Boost readback mismatch; reconnect and read actual state",
            ));
        }
        Ok(())
    }
    /// GET `24 [01 index]`; reply `index flag`.
    fn mixer_flag(&mut self, index: u8) -> Result<bool> {
        let p = self
            .session
            .get(0x24, &[1, index], |f| f.payload.first() == Some(&index))?;
        if p.len() != 2 || p[1] > 1 {
            return Err(invalid("Malformed mixer flag response"));
        }
        Ok(p[1] == 1)
    }
    /// Read descriptors, ranges, explicit master levels and mute flags.
    /// Use `capture_mixer_channels` for all 42 channel values.
    pub fn mixer(&mut self) -> Result<Vec<MixerControl>> {
        let descriptors = self.mixer_descriptors()?;
        let mut controls = Vec::with_capacity(descriptors.len());
        for (index, descriptor) in descriptors {
            let kind = descriptor & DESCRIPTOR_KIND;
            let (name, group) = identity(kind);
            let has_volume = descriptor & DESCRIPTOR_VOLUME != 0;
            let is_db = descriptor & DESCRIPTOR_DB != 0;
            let range = if has_volume && is_db {
                Some(self.mixer_range(index)?)
            } else {
                None
            };
            let raw_level = if has_volume {
                Some(self.mixer_channel_level(index, 0)?)
            } else {
                None
            };
            let flag = if descriptor & DESCRIPTOR_FLAG != 0 {
                Some(self.mixer_flag(index)?)
            } else {
                None
            };
            controls.push(MixerControl {
                index,
                name,
                group,
                descriptor,
                has_volume,
                is_db,
                range,
                level_db: raw_level.filter(|_| is_db).map(|r| r as f32 / 256.0),
                raw_level,
                muted: None,
                microphone_boost: None,
            });
            let control = controls.last_mut().expect("just pushed");
            if control.is_microphone_boost() {
                control.microphone_boost = flag.map(|raw| !raw);
            } else {
                control.muted = flag;
            }
        }
        Ok(controls)
    }
    /// Write explicit master channel zero and verify quantized readback.
    pub fn set_mixer_level(&mut self, index: u8, level_db: f32) -> Result<()> {
        // Reject nonfinite values even before issuing a capability query.
        if !level_db.is_finite() {
            return Err(invalid("Mixer level must be finite"));
        }
        let descriptor = self.mixer_descriptor(index)?;
        if descriptor & (DESCRIPTOR_VOLUME | DESCRIPTOR_DB)
            != DESCRIPTOR_VOLUME | DESCRIPTOR_DB
            || identity(descriptor & DESCRIPTOR_KIND).1 == MixerGroup::Other
        {
            return Err(invalid(
                "This mixer control does not support known dB volume writes",
            ));
        }
        let range = self.mixer_range(index)?;
        let target = range.encode(level_db)?;
        self.write_mixer_channel(index, 0, target, &range)
    }
    /// SET `24 [00 index muted]` and check `index flag` readback; reject type 63.
    /// Live playback mutes are coupled: indices 0/1 change together, while the
    /// S/PDIF index-2 setter changes them but its own getter remains false.
    /// Index 2 writes are rejected before I/O; use the shared playback mute.
    pub fn set_mixer_mute(&mut self, index: u8, muted: bool) -> Result<()> {
        if index == 2 {
            return Err(crate::protocol::unsupported(
                "SPDIF playback has no independent mute; use playback index0/1",
            ));
        }
        let descriptor = self.mixer_descriptor(index)?;
        if descriptor & DESCRIPTOR_FLAG == 0
            || identity(descriptor & DESCRIPTOR_KIND).1 == MixerGroup::Other
        {
            return Err(invalid("This control is not a supported mixer mute"));
        }
        self.session.set(0x24, &[0, index, u8::from(muted)])?;
        if self.mixer_flag(index)? != muted {
            return Err(self.session.mismatch(
                "Mixer mute readback mismatch; reconnect and read actual state",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Session;
    use std::{
        collections::{HashMap, VecDeque},
        io,
        sync::{Arc, Mutex},
    };
    #[derive(Default)]
    struct Fake {
        replies: HashMap<Vec<u8>, Vec<u8>>,
        rx: VecDeque<u8>,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
    }
    impl Read for Fake {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.rx.is_empty() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let n = out.len().min(2).min(self.rx.len());
            for b in &mut out[..n] {
                *b = self.rx.pop_front().unwrap();
            }
            Ok(n)
        }
    }
    impl Write for Fake {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes.lock().unwrap().push(bytes.to_vec());
            if let Some(reply) = self.replies.get(bytes) {
                self.rx.extend(reply);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn wire(command: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x5a, command, payload.len() as u8];
        out.extend(payload);
        out
    }
    fn ack(command: u8, status: u8) -> Vec<u8> {
        wire(2, &[command, status, 0, 0, 0, 0, 0, 0, 0, 0])
    }
    fn fake(descriptors: &[u16]) -> Fake {
        let mut caps =
            vec![descriptors.len() as u8, descriptors.len() as u8, 0, 0];
        for d in descriptors {
            caps.extend(d.to_le_bytes());
        }
        caps.extend([1, 0, 0, 0]); // Unknown trailing bytes from the live capture.
        Fake {
            replies: [(wire(0x21, &[0]), wire(0x21, &caps))].into(),
            ..Default::default()
        }
    }
    fn range_reply(fake: &mut Fake, index: u8, min: i16, max: i16, step: u16) {
        let mut query = [0; 33];
        query[0] = 1;
        query[1] = index;
        let mut p = vec![1, 0, index];
        p.extend(max.to_le_bytes());
        p.extend(min.to_le_bytes());
        p.extend(step.to_le_bytes());
        fake.replies.insert(wire(0x22, &query), wire(0x22, &p));
    }
    fn device(port: Fake) -> X4<Fake> {
        X4 {
            session: Session::new(port),
            firmware: "fixture".into(),
        }
    }
    #[test]
    fn captured_mixer_values_and_boost_are_distinct() {
        let mut fake = fake(&[0x89c1, 0x00bf]);
        range_reply(&mut fake, 0, -20493, 0, 409);
        fake.replies.insert(
            wire(0x23, &[1, 0, 0, 0, 0]),
            wire(0x23, &[0, 0x4c, 0xf8, 0]),
        );
        fake.replies
            .insert(wire(0x24, &[1, 0]), wire(0x24, &[0, 0]));
        fake.replies
            .insert(wire(0x24, &[1, 1]), wire(0x24, &[1, 1]));
        let controls = device(fake).mixer().unwrap();
        assert_eq!(controls[0].level_db, Some(-7.703125));
        assert_eq!(controls[0].range.as_ref().unwrap().min_raw, -20493);
        assert_eq!(controls[0].group, MixerGroup::Playback);
        assert_eq!(controls[1].muted, None);
        assert_eq!(controls[1].name, "Microphone Boost");
        assert_eq!(controls[1].microphone_boost, Some(false));
    }
    #[test]
    fn wire_encoding_checks_range_and_preserves_precision() {
        let range = MixerRange {
            min_raw: -24576,
            max_raw: 2304,
            step_raw: 256,
            min_db: -96.0,
            max_db: 9.0,
            step_db: 1.0,
        };
        assert_eq!(range.encode(-5.6).unwrap(), -1434);
        assert_eq!(range.encode(9.0).unwrap(), 9 * 256);
        assert!(range.encode(f32::NAN).is_err());
        assert!(range.encode(10.0).is_err());
    }
    #[test]
    fn level_write_requires_ack_and_independent_readback() {
        for (status, actual, succeeds) in [
            (0, -6_i16 * 256, true),
            (0, -2 * 256, false),
            (0x81, -6 * 256, false),
        ] {
            let mut fake = fake(&[0x29cf]);
            range_reply(&mut fake, 0, -24576, 2304, 256);
            let mut set = vec![0, 0];
            set.extend((-1434_i16).to_le_bytes());
            set.push(0);
            fake.replies.insert(wire(0x23, &set), ack(0x23, status));
            let mut p = vec![0];
            p.extend(actual.to_le_bytes());
            p.push(0);
            fake.replies
                .insert(wire(0x23, &[1, 0, 0, 0, 0]), wire(0x23, &p));
            let writes = fake.writes.clone();
            let mut dev = device(fake);
            assert_eq!(dev.set_mixer_level(0, -5.6).is_ok(), succeeds);
            assert_eq!(dev.session.usable, succeeds);
            assert_eq!(
                writes
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|w| **w == wire(0x23, &set))
                    .count(),
                1
            );
        }
    }
    #[test]
    fn mute_write_rejects_boost_and_checks_readback() {
        let fake = fake(&[0x00bf]);
        let writes = fake.writes.clone();
        assert!(device(fake).set_mixer_mute(0, true).is_err());
        assert_eq!(writes.lock().unwrap().len(), 1);
        for actual in [0, 1] {
            let mut fake = self::fake(&[0x29cf]);
            fake.replies.insert(wire(0x24, &[0, 0, 1]), ack(0x24, 0));
            fake.replies
                .insert(wire(0x24, &[1, 0]), wire(0x24, &[0, actual]));
            assert_eq!(
                device(fake).set_mixer_mute(0, true).is_ok(),
                actual == 1
            );
        }
    }
    #[test]
    fn microphone_boost_inverts_flag_and_requires_readback() {
        // Live X4 layout: ten volume controls, then type 63 at index 10.
        let mut descriptors = [0x29cf; 11];
        descriptors[10] = 0x00bf;
        for enabled in [false, true] {
            for actual in [0, 1] {
                let mut fake = fake(&descriptors);
                fake.replies.insert(
                    wire(0x24, &[0, 10, u8::from(!enabled)]),
                    ack(0x24, 0),
                );
                fake.replies
                    .insert(wire(0x24, &[1, 10]), wire(0x24, &[10, actual]));
                let mut dev = device(fake);
                let succeeds = enabled == (actual == 0);
                assert_eq!(dev.set_microphone_boost(enabled).is_ok(), succeeds);
                assert_eq!(dev.session.usable, succeeds);
            }
        }
    }
    #[test]
    fn microphone_boost_requires_advertised_type_63() {
        let fake = fake(&[0x29cf; 11]);
        let writes = fake.writes.clone();
        assert!(device(fake).set_microphone_boost(true).is_err());
        assert!(writes.lock().unwrap().iter().all(|w| w[1] != 0x24));
    }
    #[test]
    fn invalid_level_sends_nothing() {
        let fake = Fake::default();
        let writes = fake.writes.clone();
        assert!(device(fake).set_mixer_level(0, f32::INFINITY).is_err());
        assert!(writes.lock().unwrap().is_empty());
    }
    #[test]
    fn live_loopback_quantization_exceeds_reported_step() {
        let range = MixerRange {
            min_raw: -20493,
            max_raw: 0,
            step_raw: 64,
            min_db: -20493.0 / 256.0,
            max_db: 0.0,
            step_db: 0.25,
        };
        assert!(range.accepts_readback(-3252, -3151));
        assert!(!range.accepts_readback(-3252, -2996));
        assert!(!range.accepts_readback(0, 1));
    }
}
