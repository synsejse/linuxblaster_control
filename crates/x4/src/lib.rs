//! Userspace Sound Blaster X4 controls over CDC ACM (`041e:3278`).
//!
//! [`open`] discovers and exclusively opens the serial port; [`X4`] exposes
//! synchronous queries and verified writes. Audio remains with `snd-usb-audio`.
//! [`protocol`] contains framing, unlock and serialized request handling.
//! `docs/PROTOCOL.md` in the crate documents wire layouts and known gaps.
//!
//! ```no_run
//! use soundblaster_x4::{open, protocol::Result};
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
pub mod controls;
pub mod dsp;
pub mod features;
pub mod mixer;
pub mod protocol;
pub mod software;
pub mod sxfi;
pub mod transfer;
pub use features::{EqProfileInfo, SxfiMode};
pub use mixer::{MixerControl, MixerGroup, MixerRange};
pub use protocol::{Error, Result};
use protocol::{Session, invalid};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    time::Duration,
};

/// Creative USB vendor ID.
pub const VENDOR_ID: u16 = 0x041e;
/// Sound Blaster X4 USB product ID; G6 devices are not supported.
pub const PRODUCT_ID: u16 = 0x3278;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Output selector: speaker is wire value `1`, headphone is `4` (u32 LE).
pub enum Output {
    Speaker,
    Headphone,
}
impl Output {
    fn value(self) -> u32 {
        match self {
            Self::Speaker => 1,
            Self::Headphone => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Established command-`39` feature bits. Names come from Creative's managed
/// enum; only Direct Mode and high gain have verified acoustic effects.
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
const PRESET_FORMAT: &str = "soundblaster-x4-eq-v1";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Active hardware EQ: enable, preamp and ten gains in device band order.
/// Band centres are 31, 62, 125, 250, 500 Hz, 1, 2, 4, 8 and 16 kHz.
/// Wire values are IEEE-754 f32 LE, module `96`, parameters `09..14` (hex).
pub struct Equalizer {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; 10],
}
impl Equalizer {
    /// Compare within device readback precision.
    pub fn approx_eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && std::iter::once(self.preamp_db)
                .chain(self.bands_db)
                .zip(std::iter::once(other.preamp_db).chain(other.bands_db))
                .all(|(a, b)| (a - b).abs() <= EQ_TOLERANCE)
    }
    /// Reject nonfinite gains and gains outside this API's -12..12 dB limit.
    pub fn validate(&self) -> Result<()> {
        for value in
            std::iter::once(&self.preamp_db).chain(self.bands_db.iter())
        {
            if !value.is_finite() || !(-12.0..=12.0).contains(value) {
                return Err(invalid(
                    "EQ gains must be finite and within the application's -12..12 dB limit",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Sequential control snapshot, checked for route/SXFI/slot/Direct Mode changes.
/// It does not include individual mixer channels or personalization assets.
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
        self.supported_features & feature.mask() != 0
            && self.reset_features & feature.mask() == 0
    }
}

/// Local EQ preset, deliberately incompatible with G6 profiles.
/// Loading is local; applying updates the selected device EQ slot.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub format: String,
    pub output: Output,
    pub eq: Equalizer,
}
impl Preset {
    /// Construct the versioned serializable value; performs no file or device I/O.
    pub fn new(output: Output, eq: Equalizer) -> Self {
        Self {
            format: PRESET_FORMAT.into(),
            output,
            eq,
        }
    }
    /// Check the format, expected output and gain limits without applying it.
    pub fn validate(&self, output: Output) -> Result<()> {
        if self.format != PRESET_FORMAT {
            return Err(invalid("This is not an X4 EQ preset"));
        }
        if self.output != output {
            return Err(invalid(
                "Preset output differs from the active output; switch outputs first",
            ));
        }
        self.eq.validate()
    }
}

/// Discover one X4, or validate an explicit port against its USB VID/PID.
/// Opens exclusively at 115200 baud, asserts DTR/RTS, then initializes CDC.
/// Multiple devices require an explicit path; serial access permissions are
/// supplied by the caller's environment, not installed by this crate.
pub fn open(port: Option<&str>) -> Result<X4<serialport::TTYPort>> {
    let ports = serialport::available_ports()?;
    let candidates: Vec<_> = ports.iter().filter(|p| matches!(&p.port_type,
        serialport::SerialPortType::UsbPort(info) if info.vid == VENDOR_ID && info.pid == PRODUCT_ID)).collect();
    let path = match port {
        Some(path) => {
            let canonical = std::fs::canonicalize(path)?;
            if !candidates.iter().any(|p| {
                std::fs::canonicalize(&p.port_name).ok().as_ref()
                    == Some(&canonical)
            }) {
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
    let mut serial = serialport::new(&path, 115200).timeout(Duration::from_millis(20))
        .flow_control(serialport::FlowControl::None).open_native()
        .map_err(|e| invalid(format!("Cannot open {path}: {e}. Install the X4 udev rule and replug; close other controllers.")))?;
    serial.set_exclusive(true)?;
    use serialport::SerialPort;
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
        match u32::from_le_bytes(p[1..5].try_into()?) {
            1 => Ok(Output::Speaker),
            4 => Ok(Output::Headphone),
            _ => Err(invalid("Unknown output")),
        }
    }
    /// GET `39 [02]`; return `(supported_mask, reset_required_mask)` from
    /// reply `02 supported:u32le reset:u32le`.
    pub fn feature_support(&mut self) -> Result<(u32, u32)> {
        let p = self.selector(0x39, &[2])?;
        if p.len() != 9 {
            return Err(invalid("Invalid feature support"));
        }
        Ok((
            u32::from_le_bytes(p[1..5].try_into()?),
            u32::from_le_bytes(p[5..9].try_into()?),
        ))
    }
    /// GET `39 [01 ff ff ff ff]`; reply `01 current_mask:u32le`.
    pub fn features(&mut self) -> Result<u32> {
        let p = self.selector(0x39, &[1, 255, 255, 255, 255])?;
        if p.len() != 5 {
            return Err(invalid("Invalid feature state"));
        }
        Ok(u32::from_le_bytes(p[1..].try_into()?))
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
        let value = f32::from_le_bytes(p[4..].try_into()?);
        if !value.is_finite() {
            return Err(invalid("Nonfinite EQ response"));
        }
        Ok(value)
    }
    /// Read all twelve active EQ parameters; enable must be exactly 0 or 1.
    pub fn eq(&mut self) -> Result<Equalizer> {
        let enabled = self.eq_parameter(EQ_ENABLE)?;
        if enabled != 0.0 && enabled != 1.0 {
            return Err(invalid("Invalid EQ enable state"));
        }
        let preamp_db = self.eq_parameter(EQ_PREAMP)?;
        let mut bands_db = [0.0; 10];
        for (i, band) in bands_db.iter_mut().enumerate() {
            *band = self.eq_parameter(EQ_FIRST_BAND + i as u8)?;
        }
        Ok(Equalizer {
            enabled: enabled == 1.0,
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
    /// then require matching readback.
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
            .any(|v| *v == output.value().to_le_bytes())
        {
            return Err(invalid("Output not supported"));
        }
        let mut payload = vec![0];
        payload.extend(output.value().to_le_bytes());
        self.session.set(0x2c, &payload)?;
        if self.output()? != output {
            return Err(self.session.mismatch("Output readback mismatch"));
        }
        Ok(())
    }
    /// SET `39 [00 bit enabled]`. Reject unsupported/reset-required features
    /// and poll readback for three seconds.
    pub fn set_feature(
        &mut self,
        feature: Feature,
        enabled: bool,
    ) -> Result<()> {
        let (supported, reset) = self.feature_support()?;
        if supported & feature.mask() == 0 || reset & feature.mask() != 0 {
            return Err(invalid("Feature unavailable or requires reset"));
        }
        self.session
            .set(0x39, &[0, feature.bit(), u8::from(enabled)])?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if self.feature(feature)? == enabled {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(self
                    .session
                    .mismatch("Feature readback mismatch after settling"));
            }
            std::thread::sleep(Duration::from_millis(40));
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
        let desired = std::iter::once(eq.preamp_db).chain(eq.bands_db);
        let previous =
            std::iter::once(current.preamp_db).chain(current.bands_db);
        // Re-check context before every write: a button press can change
        // output, slot or Direct Mode between writes.
        for (i, (value, old)) in desired.zip(previous).enumerate() {
            if (value - old).abs() > 0.001 {
                self.check_eq_context(output, profile, sxfi)?;
                self.set_eq_parameter(EQ_PREAMP + i as u8, value)?;
            }
        }
        if eq.enabled != current.enabled {
            self.check_eq_context(output, profile, sxfi)?;
            self.set_eq_parameter(
                EQ_ENABLE,
                if eq.enabled { 1.0 } else { 0.0 },
            )?;
        }
        Ok(())
    }
    /// GET `17 [01 02 index flags]`, flags speaker `00`/headphone `02`.
    /// Assemble numbered blocks (`block & 80` means more) and decode exactly
    /// twelve `[96 parameter value:f32le]` records after a u32 LE record count.
    pub fn stored_eq_profile(
        &mut self,
        output: Output,
        index: u8,
    ) -> Result<Equalizer> {
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
        if index > 3 {
            return Err(invalid("Profile index outside 0..3"));
        }
        let flags = bank.flags() | source.flag();
        let prefix = [1, 2, index, flags];
        let mut p = self
            .session
            .get(0x17, &prefix, |f| f.payload.starts_with(&prefix))?;
        let mut data = Vec::new();
        let mut total = None;
        let mut complete = false;
        for block in 0..128 {
            if p.len() < 7 || p[..4] != prefix {
                return Err(invalid("Invalid profile header"));
            }
            let size = u16::from_le_bytes([p[4], p[5]]) as usize;
            if size != 76
                || *total.get_or_insert(size) != size
                || p[6] & 0x7f != block
            {
                return Err(invalid("Profile block order/size mismatch"));
            }
            data.extend_from_slice(&p[7..]);
            if data.len() > size {
                return Err(invalid("Oversized profile"));
            }
            if p[6] & 0x80 == 0 {
                complete = true;
                break;
            }
            p = self.session.next_profile_block(&prefix)?;
        }
        if !complete || Some(data.len()) != total || data.len() < 4 {
            return Err(invalid("Incomplete profile"));
        }
        let count = u32::from_le_bytes(data[..4].try_into()?) as usize;
        if count != 12 || data.len() != 4 + count * 6 {
            return Err(invalid("Unsupported profile layout"));
        }
        let mut values = [None; 12];
        for tuple in data[4..].as_chunks::<6>().0 {
            if tuple[0] != EQ_MODULE
                || !(EQ_ENABLE..=EQ_LAST_PARAMETER).contains(&tuple[1])
            {
                return Err(invalid("Unexpected profile parameter"));
            }
            let slot = &mut values[(tuple[1] - EQ_ENABLE) as usize];
            if slot.is_some() {
                return Err(invalid("Duplicate profile parameter"));
            }
            let value = f32::from_le_bytes(tuple[2..].try_into()?);
            if !value.is_finite() {
                return Err(invalid("Nonfinite profile value"));
            }
            *slot = Some(value);
        }
        let values = values.map(|v| v.expect("12 unique parameters checked"));
        if values[0] != 0.0 && values[0] != 1.0 {
            return Err(invalid("Invalid profile enable value"));
        }
        Ok(Equalizer {
            enabled: values[0] == 1.0,
            preamp_db: values[1],
            bands_db: values[2..].try_into()?,
        })
    }
}

#[cfg(test)]
mod tests;
