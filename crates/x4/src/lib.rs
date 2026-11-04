//! Userspace Sound Blaster X4 controls over CDC ACM (`041e:3278`).
//!
//! [`open`] discovers and exclusively opens the serial port; [`X4`] exposes
//! synchronous queries and verified writes. Audio remains with `snd-usb-audio`.
//! [`protocol`] contains framing, unlock and serialized request handling.
//! `docs/PROTOCOL.md` in the crate documents wire layouts and known gaps.
//!
//! ```no_run
//! use soundblaster_x4::{Result, open};
//! fn main() -> Result<()> {
//!     let mut device = open(None)?;
//!     let state = device.state()?;
//!     println!("{}: {:?}", state.firmware, state.output);
//!     Ok(())
//! }
//! ```
//!
//! Reads are sequential, not an atomic device snapshot. Setters can partially
//! succeed before an error; reconnect and read actual state after a failed
//! exchange. Mixer GET and SET always carry an explicit channel.
#![expect(
    clippy::cast_possible_truncation,
    reason = "Wire fields are narrowed only after their bounds are checked"
)]

pub mod controls;
pub mod dsp;
mod error;
pub mod features;
pub mod mixer;
pub mod protocol;
pub mod software;
pub mod sxfi;
pub mod transfer;
use error::invalid;
pub use error::{Error, Result};
pub use features::{EqProfileInfo, SxfiMode};
pub use mixer::{MixerControl, MixerGroup, MixerRange};
use protocol::{Session, word};
use serde::{Deserialize, Serialize};
use serialport::SerialPort;
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

/// The transport [`open`] returns.
pub type Port = serialport::TTYPort;

/// Creative USB vendor ID.
pub const VENDOR_ID: u16 = 0x041e;
/// Sound Blaster X4 USB product ID; G6 devices are not supported.
pub const PRODUCT_ID: u16 = 0x3278;

/// Output selector: speaker is wire value `1`, headphone is `4` (u32 LE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Speaker,
    Headphone,
}

impl Output {
    fn wire(self) -> u32 {
        match self {
            Self::Speaker => 1,
            Self::Headphone => 4,
        }
    }

    fn from_wire(value: u32) -> Option<Self> {
        [Self::Speaker, Self::Headphone]
            .into_iter()
            .find(|output| output.wire() == value)
    }
}

/// Established command-`39` feature bits. Names come from Creative's managed
/// enum; only Direct Mode and high gain have verified acoustic effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Bit 5: bypasses EQ and Super X-Fi processing.
    DirectMode,
    /// Bit 6: high-gain headphone amplifier.
    HeadphoneHighGain,
    /// Bit 15.
    SpdifPassthrough,
    /// Bit 20.
    SaveHeadphoneHighGain,
    /// Bit 21.
    HeadphoneVirtualization,
    /// Bit 26.
    Uac2,
}

impl Feature {
    /// Bit number used by the feature masks and the SET payload.
    pub fn bit(self) -> u8 {
        match self {
            Self::DirectMode => 5,
            Self::HeadphoneHighGain => 6,
            Self::SpdifPassthrough => 15,
            Self::SaveHeadphoneHighGain => 20,
            Self::HeadphoneVirtualization => 21,
            Self::Uac2 => 26,
        }
    }

    fn mask(self) -> u32 {
        1 << self.bit()
    }
}

/// DSP module holding the hardware EQ.
const EQ_MODULE: u8 = 0x96;
/// EQ enable parameter (`0.0`/`1.0`); preamp follows, then ten bands.
const EQ_ENABLE: u8 = 0x09;
const EQ_PREAMP: u8 = 0x0a;
const EQ_FIRST_BAND: u8 = 0x0b;
const EQ_LAST_PARAMETER: u8 = 0x14;
/// Readback tolerance for f32 EQ values.
const EQ_TOLERANCE: f32 = 0.011;
/// Largest EQ gain in either direction, in dB, that this API writes.
pub const EQ_LIMIT_DB: f32 = 12.0;
/// Bytes in a stored EQ profile: a u32 count, then twelve DSP tuples.
const PROFILE_SIZE: usize = 76;
/// How long output and feature writes may take to read back.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(3);
const SETTLE_POLL: Duration = Duration::from_millis(40);

/// Active hardware EQ: enable, preamp and ten gains in device band order.
/// Band centres are 31, 62, 125, 250, 500 Hz, 1, 2, 4, 8 and 16 kHz.
/// Wire values are IEEE-754 f32 LE, module `96`, parameters `09..14` (hex).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Equalizer {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; 10],
}

impl Equalizer {
    /// The preamp, then each band: the order of parameters `0a..14`.
    fn gains(&self) -> impl Iterator<Item = f32> {
        std::iter::once(self.preamp_db).chain(self.bands_db)
    }

    /// Compare within device readback precision.
    pub fn approx_eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self
                .gains()
                .zip(other.gains())
                .all(|(a, b)| (a - b).abs() <= EQ_TOLERANCE)
    }

    /// Reject nonfinite gains and gains outside this API's -12..12 dB limit.
    pub fn validate(&self) -> Result<()> {
        if self.gains().all(|gain| gain.abs() <= EQ_LIMIT_DB) {
            Ok(())
        } else {
            Err(invalid(
                "EQ gains must be finite and within the application's -12..12 dB limit",
            ))
        }
    }
}

/// Sequential control snapshot, checked for route/SXFI/slot/Direct Mode changes.
/// It does not include individual mixer channels or personalization assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub firmware: String,
    pub output: Output,
    pub supported_features: u32,
    pub reset_features: u32,
    pub features: u32,
    pub eq_supported: bool,
    pub eq: Option<Equalizer>,
    pub sxfi: Option<SxfiMode>,
    pub eq_profiles: Vec<EqProfileInfo>,
    pub active_eq_profile: Option<u8>,
    pub mixer: Vec<MixerControl>,
}

impl State {
    /// Whether `feature` is currently enabled.
    pub fn feature(&self, feature: Feature) -> bool {
        self.features & feature.mask() != 0
    }

    /// Whether `feature` is supported without a reset requirement.
    pub fn writable_feature(&self, feature: Feature) -> bool {
        self.supported_features & feature.mask() != 0 && self.reset_features & feature.mask() == 0
    }

    /// The slot the device has active.
    pub fn active_profile(&self) -> Option<&EqProfileInfo> {
        self.eq_profiles
            .iter()
            .find(|p| self.active_eq_profile == Some(p.index))
    }

    /// Whether the device accepts EQ slot `index` right now: slot 3 only with
    /// Super X-Fi on, the others only with it off, and none in Direct Mode.
    pub fn eq_profile_available(&self, index: u8) -> bool {
        !self.feature(Feature::DirectMode) && self.sxfi.is_some_and(|mode| mode.uses_slot(index))
    }
}

/// Discover one X4, or validate an explicit port against its USB VID/PID.
/// Opens exclusively at 115200 baud, asserts DTR/RTS, then initializes CDC.
/// Multiple devices require an explicit path; serial access permissions are
/// supplied by the caller's environment, not installed by this crate.
pub fn open(port: Option<&str>) -> Result<X4<Port>> {
    let ports = serialport::available_ports()?;
    let candidates: Vec<_> = ports
        .iter()
        .filter(|p| {
            matches!(&p.port_type, serialport::SerialPortType::UsbPort(info)
                if info.vid == VENDOR_ID && info.pid == PRODUCT_ID)
        })
        .collect();
    let path = match port {
        Some(path) => {
            let canonical = std::fs::canonicalize(path)?;
            if !candidates
                .iter()
                .any(|p| std::fs::canonicalize(&p.port_name).ok().as_ref() == Some(&canonical))
            {
                return Err(invalid(
                    "Selected port is not a Sound Blaster X4 (041e:3278)",
                ));
            }
            path.to_owned()
        }
        None if candidates.len() == 1 => candidates[0].port_name.clone(),
        None => {
            return Err(invalid(format!(
                "Expected one X4, found {}; select a port explicitly",
                candidates.len()
            )));
        }
    };
    let mut serial = serialport::new(&path, 115_200)
        .timeout(Duration::from_millis(20))
        .flow_control(serialport::FlowControl::None)
        .open_native()
        .map_err(|e| {
            invalid(format!(
                "Cannot open {path}: {e}. Install the X4 udev rule and replug; close other controllers."
            ))
        })?;
    serial.set_exclusive(true)?;
    serial.write_data_terminal_ready(true)?;
    serial.write_request_to_send(true)?;
    X4::connect(serial)
}

/// Synchronous controller over one owned [`Read`] + [`Write`] transport.
/// The public session permits raw exchanges; these bypass high-level guards.
pub struct X4<T> {
    pub session: Session<T>,
    pub firmware: String,
}

impl<T: Read + Write> X4<T> {
    /// Initialize an already configured transport; does not configure its baud
    /// rate or timeout. Reads must return periodically for deadlines to work.
    pub fn connect(port: T) -> Result<Self> {
        let mut session = Session::new(port);
        let firmware = session.connect()?;
        Ok(Self { session, firmware })
    }

    fn selector(&mut self, command: u8, payload: &[u8]) -> Result<Vec<u8>> {
        self.session
            .get(command, payload, |f| f.payload.first() == payload.first())
    }

    /// GET `2c [01]`; reply `01 output:u32le`.
    pub fn output(&mut self) -> Result<Output> {
        let p = self.selector(0x2c, &[1])?;
        if p.len() != 5 {
            return Err(invalid("Invalid output reply"));
        }
        Output::from_wire(word(&p, 1)).ok_or_else(|| invalid("Unknown output"))
    }

    /// GET `39 [02]`; return `(supported_mask, reset_required_mask)` from
    /// reply `02 supported:u32le reset:u32le`.
    pub fn feature_support(&mut self) -> Result<(u32, u32)> {
        let p = self.selector(0x39, &[2])?;
        if p.len() != 9 {
            return Err(invalid("Invalid feature support"));
        }
        Ok((word(&p, 1), word(&p, 5)))
    }

    /// GET `39 [01 ff ff ff ff]`; reply `01 current_mask:u32le`.
    pub fn features(&mut self) -> Result<u32> {
        let p = self.selector(0x39, &[1, 255, 255, 255, 255])?;
        if p.len() != 5 {
            return Err(invalid("Invalid feature state"));
        }
        Ok(word(&p, 1))
    }

    /// Read one feature bit from [`Self::features`].
    pub fn feature(&mut self, feature: Feature) -> Result<bool> {
        Ok(self.features()? & feature.mask() != 0)
    }

    /// GET `10 []`; inspect bit 6 of the eight-byte DSP capability reply.
    pub fn eq_supported(&mut self) -> Result<bool> {
        let p = self.session.get(0x10, &[], |_| true)?;
        if p.len() != 8 {
            return Err(invalid("Invalid DSP capability response"));
        }
        Ok(p[0] & 0x40 != 0)
    }

    /// GET `11 [01 96 parameter]`; reply `01 00 96 parameter value:f32le`.
    /// Accepted parameters: `09` enable, `0a` preamp, `0b..14` band gains.
    pub fn eq_parameter(&mut self, parameter: u8) -> Result<f32> {
        if !(EQ_ENABLE..=EQ_LAST_PARAMETER).contains(&parameter) {
            return Err(invalid("Invalid EQ parameter"));
        }
        let p = self.session.get(0x11, &[1, EQ_MODULE, parameter], |f| {
            f.payload.len() == 8 && f.payload[2..4] == [EQ_MODULE, parameter]
        })?;
        if p.len() != 8 || p[..2] != [1, 0] {
            return Err(invalid("Invalid EQ response"));
        }
        let value = f32::from_bits(word(&p, 4));
        if !value.is_finite() {
            return Err(invalid("Nonfinite EQ response"));
        }
        Ok(value)
    }

    /// Read all twelve active EQ parameters; enable must be exactly 0 or 1.
    pub fn eq(&mut self) -> Result<Equalizer> {
        let enabled = enable_flag(self.eq_parameter(EQ_ENABLE)?)
            .ok_or_else(|| invalid("Invalid EQ enable state"))?;
        let preamp_db = self.eq_parameter(EQ_PREAMP)?;
        let mut bands_db = [0.0; 10];
        for (i, band) in bands_db.iter_mut().enumerate() {
            *band = self.eq_parameter(EQ_FIRST_BAND + i as u8)?;
        }
        Ok(Equalizer {
            enabled,
            preamp_db,
            bands_db,
        })
    }

    /// Read the supported control snapshot and reject processing-context changes.
    /// Unrelated notifications remain available; this is not a full backup.
    pub fn state(&mut self) -> Result<State> {
        // Check route before and after: do not present EQ from a different output.
        let output = self.output()?;
        let (supported_features, reset_features) = self.feature_support()?;
        let features = self.features()?;
        let eq_supported = self.eq_supported()?;
        let sxfi = self.sxfi()?;
        let active_eq_profile = Some(self.active_eq_profile()?);
        let eq_profiles = self.eq_profiles(output)?;
        let eq = if eq_supported { Some(self.eq()?) } else { None };
        let mixer = self.mixer()?;
        if self.output()? != output
            || Some(self.active_eq_profile()?) != active_eq_profile
            || self.sxfi()? != sxfi
            || (self.features()? ^ features) & Feature::DirectMode.mask() != 0
        {
            return Err(invalid(
                "Output or processing mode changed during refresh; refresh again",
            ));
        }
        Ok(State {
            firmware: self.firmware.clone(),
            output,
            supported_features,
            reset_features,
            features,
            eq_supported,
            eq,
            sxfi,
            eq_profiles,
            active_eq_profile,
            mixer,
        })
    }

    /// Validate advertised outputs (`2c [02]`), SET `2c [00 output:u32le]`,
    /// then poll readback for three seconds. The firmware ACKs before the route
    /// changes, and switching to speakers can read back late.
    pub fn set_output(&mut self, output: Output) -> Result<()> {
        let support = self.selector(0x2c, &[2])?;
        if support.len() < 2
            || support[1] & 0x80 == 0
            || support.len() != 2 + (support[1] & 0x7f) as usize * 4
        {
            return Err(invalid("Invalid output capability response"));
        }
        if !support[2..]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|v| *v == output.wire().to_le_bytes())
        {
            return Err(invalid("Output not supported"));
        }
        let mut payload = vec![0];
        payload.extend(output.wire().to_le_bytes());
        self.session.set(0x2c, &payload)?;
        self.settle(
            SETTLE_TIMEOUT,
            SETTLE_POLL,
            "Output readback mismatch after settling",
            |x4| Ok(x4.output()? == output),
        )
    }

    /// SET `39 [00 bit enabled]`. Reject unsupported/reset-required features
    /// and poll readback for three seconds.
    pub fn set_feature(&mut self, feature: Feature, enabled: bool) -> Result<()> {
        let (supported, reset) = self.feature_support()?;
        if supported & feature.mask() == 0 || reset & feature.mask() != 0 {
            return Err(invalid("Feature unavailable or requires reset"));
        }
        self.session
            .set(0x39, &[0, feature.bit(), u8::from(enabled)])?;
        self.settle(
            SETTLE_TIMEOUT,
            SETTLE_POLL,
            "Feature readback mismatch after settling",
            |x4| Ok(x4.feature(feature)? == enabled),
        )
    }

    /// Poll `settled` until it holds. Firmware ACKs asynchronous changes before
    /// they land; running out of time means the device state is uncertain.
    pub(crate) fn settle(
        &mut self,
        timeout: Duration,
        poll: Duration,
        message: &str,
        mut settled: impl FnMut(&mut Self) -> Result<bool>,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if settled(self)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(self.session.mismatch(message));
            }
            std::thread::sleep(poll);
        }
    }

    fn set_eq_parameter(&mut self, parameter: u8, value: f32) -> Result<()> {
        let mut payload = vec![1, EQ_MODULE, parameter];
        payload.extend(value.to_le_bytes());
        self.session.set(0x12, &payload)?;
        if (self.eq_parameter(parameter)? - value).abs() > EQ_TOLERANCE {
            return Err(self.session.mismatch("EQ readback mismatch"));
        }
        Ok(())
    }

    /// Apply changed values via `12 [01 96 parameter value:f32le]` and verify
    /// each one. Writes update the active stored slot; no separate save is sent.
    /// Reject Direct Mode and processing-context changes. This is a sequence
    /// of writes, not a transaction: errors can leave earlier values applied.
    pub fn apply_eq(&mut self, output: Output, eq: &Equalizer) -> Result<()> {
        eq.validate()?; // Validate every value before the first device access/write.
        if !self.eq_supported()? {
            return Err(invalid("Hardware EQ not advertised"));
        }
        if self.output()? != output {
            return Err(invalid("Output changed; refresh before applying EQ"));
        }
        let profile = self.active_eq_profile()?;
        let sxfi = self.sxfi()?;
        self.check_eq_context(output, profile, sxfi)?;
        let current = self.eq()?;
        // Re-check context before every write: a button press can change
        // output, slot or Direct Mode between writes.
        for (parameter, (value, old)) in (EQ_PREAMP..).zip(eq.gains().zip(current.gains())) {
            if (value - old).abs() > 0.001 {
                self.check_eq_context(output, profile, sxfi)?;
                self.set_eq_parameter(parameter, value)?;
            }
        }
        if eq.enabled != current.enabled {
            self.check_eq_context(output, profile, sxfi)?;
            self.set_eq_parameter(EQ_ENABLE, f32::from(u8::from(eq.enabled)))?;
        }
        Ok(())
    }

    /// GET `17 [01 02 index flags]`, flags speaker `00`/headphone `02`.
    /// Assemble numbered blocks (`block & 80` means more) and decode exactly
    /// twelve `[96 parameter value:f32le]` records after a u32 LE record count.
    pub fn stored_eq_profile(&mut self, output: Output, index: u8) -> Result<Equalizer> {
        self.read_eq_profile(output.into(), dsp::ProfileSource::Writable, index)
    }

    /// `17 [01 02 index flags]`: bit0 selects factory data; bits1..3 select
    /// bank0/1/current7. Writable enable09 reflects global enable, even for an
    /// inactive slot. The reply describes cached values, not DSP consumption.
    pub fn read_eq_profile(
        &mut self,
        bank: dsp::ProfileBank,
        source: dsp::ProfileSource,
        index: u8,
    ) -> Result<Equalizer> {
        dsp::check_profile_index(index)?;
        let flags = bank.flags() | source.flag();
        let prefix = [1, 2, index, flags];
        let mut p = self
            .session
            .get(0x17, &prefix, |f| f.payload.starts_with(&prefix))?;
        let mut data = Vec::with_capacity(PROFILE_SIZE);
        for block in 0..128 {
            if p.len() < 7 || p[..4] != prefix {
                return Err(invalid("Invalid profile header"));
            }
            if u16::from_le_bytes([p[4], p[5]]) as usize != PROFILE_SIZE || p[6] & 0x7f != block {
                return Err(invalid("Profile block order/size mismatch"));
            }
            data.extend_from_slice(&p[7..]);
            if data.len() > PROFILE_SIZE {
                return Err(invalid("Oversized profile"));
            }
            if p[6] & 0x80 == 0 {
                return decode_profile(&data);
            }
            p = self
                .session
                .continuation(0x17, |f| f.payload.starts_with(&prefix))?;
        }
        Err(invalid("Incomplete profile"))
    }
}

/// `0.0` or `1.0` as a bool; the device reports no other enable value.
#[expect(clippy::float_cmp, reason = "The device sends these exact values")]
fn enable_flag(value: f32) -> Option<bool> {
    (value == 0.0 || value == 1.0).then_some(value == 1.0)
}

/// Decode a u32 record count and twelve `[96 parameter value:f32le]` tuples.
fn decode_profile(data: &[u8]) -> Result<Equalizer> {
    if data.len() != PROFILE_SIZE {
        return Err(invalid("Incomplete profile"));
    }
    if word(data, 0) != 12 {
        return Err(invalid("Unsupported profile layout"));
    }
    let mut values = [None; 12];
    for tuple in data[4..].as_chunks::<6>().0 {
        if tuple[0] != EQ_MODULE || !(EQ_ENABLE..=EQ_LAST_PARAMETER).contains(&tuple[1]) {
            return Err(invalid("Unexpected profile parameter"));
        }
        let slot = &mut values[(tuple[1] - EQ_ENABLE) as usize];
        if slot.is_some() {
            return Err(invalid("Duplicate profile parameter"));
        }
        let value = f32::from_bits(word(tuple, 2));
        if !value.is_finite() {
            return Err(invalid("Nonfinite profile value"));
        }
        *slot = Some(value);
    }
    let [enabled, preamp_db, bands_db @ ..] =
        values.map(|v| v.expect("12 unique parameters checked"));
    Ok(Equalizer {
        enabled: enable_flag(enabled).ok_or_else(|| invalid("Invalid profile enable value"))?,
        preamp_db,
        bands_db,
    })
}

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
