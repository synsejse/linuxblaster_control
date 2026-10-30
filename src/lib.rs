//! Sound Blaster X4 controls over CDC ACM; audio remains with snd-usb-audio.
pub mod protocol;
use protocol::{Result, Session, invalid};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    time::Duration,
};

pub const VENDOR_ID: u16 = 0x041e;
pub const PRODUCT_ID: u16 = 0x3278;
pub const UDEV_RULE: &str =
    include_str!("../packaging/70-linuxblaster-control.rules");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    fn genre(self) -> u8 {
        match self {
            Self::Speaker => 0,
            Self::Headphone => 2,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Equalizer {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; 10],
}
impl Equalizer {
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
pub struct State {
    pub firmware: String,
    pub output: Output,
    pub supported_features: u32,
    pub reset_features: u32,
    pub features: u32,
    pub eq_supported: bool,
    pub eq: Option<Equalizer>,
}
impl State {
    pub fn feature(&self, bit: u8) -> bool {
        self.features & (1 << bit) != 0
    }
    pub fn writable_feature(&self, bit: u8) -> bool {
        self.supported_features & (1 << bit) != 0
            && self.reset_features & (1 << bit) == 0
    }
}

/// Local EQ preset, deliberately incompatible with G6 profiles. Never writes stored device profiles.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub format: String,
    pub output: Output,
    pub eq: Equalizer,
}
impl Preset {
    pub fn new(output: Output, eq: Equalizer) -> Self {
        Self {
            format: "soundblaster-x4-eq-v1".into(),
            output,
            eq,
        }
    }
    pub fn validate(&self, output: Output) -> Result<()> {
        if self.format != "soundblaster-x4-eq-v1" {
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
                "Expected one X4, found {}; use --port for multiple devices",
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

pub struct X4<T> {
    pub session: Session<T>,
    pub firmware: String,
}
impl<T: Read + Write> X4<T> {
    pub fn connect(port: T) -> Result<Self> {
        let mut session = Session::new(port);
        let firmware = session.connect()?;
        Ok(Self { session, firmware })
    }
    fn selector(&mut self, command: u8, payload: &[u8]) -> Result<Vec<u8>> {
        self.session
            .get(command, payload, |f| f.payload.first() == payload.first())
    }
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
    pub fn features(&mut self) -> Result<u32> {
        let p = self.selector(0x39, &[1, 255, 255, 255, 255])?;
        if p.len() != 5 {
            return Err(invalid("Invalid feature state"));
        }
        Ok(u32::from_le_bytes(p[1..].try_into()?))
    }
    pub fn eq_supported(&mut self) -> Result<bool> {
        let p = self.session.get(0x10, &[], |_| true)?;
        if p.len() != 8 {
            return Err(invalid("Invalid DSP capability response"));
        }
        Ok(p[0] & 0x40 != 0)
    }
    pub fn eq_parameter(&mut self, parameter: u8) -> Result<f32> {
        if !(9..=20).contains(&parameter) {
            return Err(invalid("Invalid EQ parameter"));
        }
        let p = self.session.get(0x11, &[1, 0x96, parameter], |f| {
            f.payload.len() >= 4 && f.payload[2..4] == [0x96, parameter]
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
    pub fn eq(&mut self) -> Result<Equalizer> {
        let enabled = self.eq_parameter(9)?;
        if enabled != 0.0 && enabled != 1.0 {
            return Err(invalid("Invalid EQ enable state"));
        }
        let preamp_db = self.eq_parameter(10)?;
        let mut bands_db = [0.0; 10];
        for (i, band) in bands_db.iter_mut().enumerate() {
            *band = self.eq_parameter(11 + i as u8)?;
        }
        Ok(Equalizer {
            enabled: enabled == 1.0,
            preamp_db,
            bands_db,
        })
    }
    pub fn state(&mut self) -> Result<State> {
        // Check route before and after: do not present EQ from a different output.
        let output = self.output()?;
        let (supported_features, reset_features) = self.feature_support()?;
        let features = self.features()?;
        let eq_supported = self.eq_supported()?;
        let eq = if eq_supported { Some(self.eq()?) } else { None };
        if self.output()? != output {
            return Err(invalid(
                "Output changed during refresh; refresh again",
            ));
        }
        self.session.notifications.clear();
        Ok(State {
            firmware: self.firmware.clone(),
            output,
            supported_features,
            reset_features,
            features,
            eq_supported,
            eq,
        })
    }
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
            self.session.usable = false;
            return Err(invalid("Output readback mismatch"));
        }
        Ok(())
    }
    pub fn set_feature(&mut self, bit: u8, enabled: bool) -> Result<()> {
        if ![5, 6].contains(&bit) {
            return Err(invalid(
                "Only Direct Mode and headphone gain may be changed",
            ));
        }
        let (supported, reset) = self.feature_support()?;
        if supported & (1 << bit) == 0 || reset & (1 << bit) != 0 {
            return Err(invalid("Feature unavailable or requires reset"));
        }
        self.session.set(0x39, &[0, bit, u8::from(enabled)])?;
        if (self.features()? & (1 << bit) != 0) != enabled {
            self.session.usable = false;
            return Err(invalid("Feature readback mismatch"));
        }
        Ok(())
    }
    fn set_eq_parameter(&mut self, parameter: u8, value: f32) -> Result<()> {
        let mut payload = vec![1, 0x96, parameter];
        payload.extend(value.to_le_bytes());
        self.session.set(0x12, &payload)?;
        if (self.eq_parameter(parameter)? - value).abs() > 0.011 {
            self.session.usable = false;
            return Err(invalid("EQ readback mismatch"));
        }
        Ok(())
    }
    pub fn apply_eq(&mut self, output: Output, eq: &Equalizer) -> Result<()> {
        eq.validate()?; // Validate every value before the first device access/write.
        if !self.eq_supported()? {
            return Err(invalid("Hardware EQ not advertised"));
        }
        if self.output()? != output {
            return Err(invalid("Output changed; refresh before applying EQ"));
        }
        let current = self.eq()?;
        let desired = std::iter::once(eq.preamp_db).chain(eq.bands_db);
        let previous =
            std::iter::once(current.preamp_db).chain(current.bands_db);
        for (i, (value, old)) in desired.zip(previous).enumerate() {
            if (value - old).abs() > 0.001 {
                if self.output()? != output {
                    return Err(invalid(
                        "Output changed during EQ apply; some settings may have changed",
                    ));
                }
                self.set_eq_parameter(10 + i as u8, value)?;
            }
        }
        if eq.enabled != current.enabled {
            if self.output()? != output {
                return Err(invalid("Output changed before EQ enable"));
            }
            self.set_eq_parameter(9, if eq.enabled { 1.0 } else { 0.0 })?;
        }
        Ok(())
    }
    pub fn stored_profile(&mut self, output: Output) -> Result<Equalizer> {
        let prefix = [1, 2, 0, output.genre()];
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
            if *total.get_or_insert(size) != size || p[6] & 0x7f != block {
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
            if tuple[0] != 0x96 || !(9..=20).contains(&tuple[1]) {
                return Err(invalid("Unexpected profile parameter"));
            }
            let slot = &mut values[(tuple[1] - 9) as usize];
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
