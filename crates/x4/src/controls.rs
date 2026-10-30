//! Firmware 1.7 numeric controls. ACK means accepted, not acoustic activation.
//! Commands `29`/`4b` (speaker configuration) have no X4 firmware handler
//! and are deliberately not exposed.
use crate::{
    X4,
    protocol::{Result, invalid, unsupported},
};
use std::io::{Read, Write};
#[derive(Debug, Clone, Copy)]
pub enum CapabilityWord {
    General,
    Audio,
    Hardware,
    Transfer,
}
#[derive(Debug, Clone, Copy)]
pub enum DeviceInfo {
    Identity,
    Manufacturer,
    Product,
    Firmware,
    Serial,
    Capabilities,
    Name,
    Version,
}
/// May contain private strings. Debug intentionally redacts bytes.
pub struct Information(pub Vec<u8>);
impl std::fmt::Debug for Information {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Information([redacted])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterSelection {
    Id1 = 1,
    Id2 = 2,
    Id4 = 4,
    Id5 = 5,
    Id6 = 6,
}
impl TryFrom<u16> for FilterSelection {
    type Error = crate::Error;
    fn try_from(v: u16) -> Result<Self> {
        match v {
            1 => Ok(Self::Id1),
            2 => Ok(Self::Id2),
            4 => Ok(Self::Id4),
            5 => Ok(Self::Id5),
            6 => Ok(Self::Id6),
            _ => Err(invalid("Filter ID unsupported")),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum PromptLanguage {
    English,
    Chinese,
    Japanese,
}
impl PromptLanguage {
    fn bytes(self) -> [u8; 2] {
        match self {
            Self::English => *b"en",
            Self::Chinese => *b"zh",
            Self::Japanese => *b"ja",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum ResetAction {
    Action1 = 1,
    Action2 = 2,
    Action3 = 3,
}
impl<T: Read + Write> X4<T> {
    pub fn protocol_version(&mut self) -> Result<[u8; 2]> {
        self.session
            .get(1, &[0], |_| true)?
            .try_into()
            .map_err(|_| invalid("Version length"))
    }
    pub fn protocol_format(&mut self) -> Result<u8> {
        let p = self.session.get(0x0b, &[], |_| true)?;
        if p.len() != 1 {
            return Err(invalid("Format length"));
        }
        Ok(p[0])
    }
    pub fn capability_word(&mut self, kind: CapabilityWord) -> Result<u32> {
        let (c, q) = match kind {
            CapabilityWord::General => (5, vec![]),
            CapabilityWord::Audio => (0x20, vec![]),
            CapabilityWord::Hardware => (0x30, vec![]),
            CapabilityWord::Transfer => (0x90, vec![0]),
        };
        let p = self.session.get(c, &q, |_| true)?;
        let data = if c == 0x90 {
            if p.first() != Some(&0) {
                return Err(invalid("Capability selector"));
            }
            &p[1..]
        } else {
            &p
        };
        Ok(u32::from_le_bytes(
            data.try_into().map_err(|_| invalid("Capability length"))?,
        ))
    }
    pub fn device_information(
        &mut self,
        kind: DeviceInfo,
    ) -> Result<Information> {
        let q = match kind {
            DeviceInfo::Identity => vec![0],
            DeviceInfo::Manufacturer => vec![1, 0],
            DeviceInfo::Product => vec![1, 1],
            DeviceInfo::Firmware => vec![2],
            DeviceInfo::Serial => vec![3],
            DeviceInfo::Capabilities => vec![4],
            DeviceInfo::Name => vec![5],
            DeviceInfo::Version => vec![6],
        };
        let p = self.selector(9, &q)?;
        let fixed = match kind {
            DeviceInfo::Identity => Some(17),
            DeviceInfo::Capabilities => Some(2),
            DeviceInfo::Name => Some(65),
            DeviceInfo::Version => Some(5),
            _ => None,
        };
        if fixed.is_some_and(|n| p.len() != n) || p.is_empty() {
            return Err(invalid("Device info length"));
        }
        Ok(Information(p))
    }
    /// Query can invoke the firmware mode setter; not guaranteed passive.
    pub fn connection_mode(&mut self) -> Result<u8> {
        let p = self.selector(0x0c, &[1])?;
        if p.len() != 2 {
            return Err(invalid("Connection mode length"));
        }
        Ok(p[1])
    }
    pub fn set_connection_mode_2(&mut self) -> Result<()> {
        self.session.set(0x0c, &[2])
    }
    /// Destructive connection reset. No automatic reconnection/update fallback.
    pub fn reset_connection(&mut self) -> Result<()> {
        self.session.set(0x0c, &[0])
    }
    pub fn operation_mode(&mut self) -> Result<u8> {
        let p = self.selector(0x6d, &[1])?;
        if p.len() != 2 {
            return Err(invalid("Operation mode length"));
        }
        Ok(p[1])
    }
    pub fn set_operation_mode(&mut self) -> Result<()> {
        self.session.set(0x6d, &[0, 1])
    }
    /// Cached selection only; does not read the DSP.
    pub fn filter_selection(&mut self) -> Result<FilterSelection> {
        let p = self.selector(0x6c, &[1])?;
        if p.len() != 3 {
            return Err(invalid("Filter length"));
        }
        u16::from_le_bytes([p[1], p[2]]).try_into()
    }
    /// ACK confirms the cached numeric request only. The peripheral path may
    /// perform no register write for IDs4/5/6; hardware variants differ.
    pub fn set_filter_selection(&mut self, id: FilterSelection) -> Result<()> {
        self.session.set(0x6c, &[0, id as u8, 0])
    }
    pub fn filter_inventory(&mut self) -> Result<Vec<u16>> {
        let p = self.selector(0x6c, &[2])?;
        if p.len() < 2 || p.len() != 2 + 2 * p[1] as usize {
            return Err(invalid("Filter inventory length"));
        }
        Ok(p[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|x| u16::from_le_bytes([x[0], x[1]]))
            .collect())
    }
    pub fn echo(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let mut p = vec![0];
        p.extend(data);
        let reply = self.selector(0x54, &p)?;
        if reply != p {
            return Err(invalid("Echo mismatch"));
        }
        Ok(reply[1..].to_vec())
    }
    pub fn prompt_operations(&mut self) -> Result<Vec<u8>> {
        self.fixed_selector(0x70, &[0], 5)
    }
    pub fn prompt_language(&mut self) -> Result<[u8; 2]> {
        let p = self.fixed_selector(0x70, &[2], 3)?;
        Ok([p[1], p[2]])
    }
    pub fn prompt_languages(&mut self) -> Result<Vec<[u8; 2]>> {
        let p = self.selector(0x70, &[3])?;
        if p.len() < 2 || p.len() != 2 + 2 * p[1] as usize {
            return Err(invalid("Language list length"));
        }
        Ok(p[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|x| [x[0], x[1]])
            .collect())
    }
    pub fn set_prompt_language(
        &mut self,
        language: PromptLanguage,
    ) -> Result<()> {
        let b = language.bytes();
        self.session.set(0x70, &[1, b[0], b[1]])
    }
    pub fn transport_option_operations(&mut self) -> Result<Vec<u8>> {
        self.fixed_selector(0x72, &[0], 5)
    }
    pub fn transport_option(&mut self) -> Result<bool> {
        let p = self.fixed_selector(0x72, &[1], 2)?;
        if p[1] > 1 {
            return Err(invalid("Option boolean"));
        }
        Ok(p[1] != 0)
    }
    pub fn set_transport_option(&mut self, value: bool) -> Result<()> {
        self.session.set(0x72, &[2, u8::from(value)])
    }
    /// Known LED subset; color ordering unresolved. Value is shifted right 8 by firmware.
    pub fn set_led_word(&mut self, reserved: u8, word: u32) -> Result<()> {
        let mut p = vec![0x0a, 0, 3, 8, reserved];
        p.extend(word.to_le_bytes());
        self.session.set(0x3a, &p)
    }
    pub fn reset_actions(&mut self) -> Result<Vec<u8>> {
        self.fixed_selector(0x52, &[2], 8)
    }
    /// Destructive. Sends action only; no claim of restoration or reconnection.
    pub fn reset_action(&mut self, action: ResetAction) -> Result<()> {
        self.session.set(0x52, &[0, action as u8, 0])
    }
    /// Destructive; firmware requires runtime state 6.
    pub fn factory_reset(&mut self) -> Result<()> {
        self.session.set(0x9b, &[1])
    }
    pub fn upgrade_inventory(&mut self) -> Result<Vec<u8>> {
        self.fixed_selector(0x53, &[2], 3)
    }
    /// Destructive update-event submission, NOT a complete flashing workflow.
    pub fn request_upgrade(&mut self) -> Result<()> {
        self.session.set(0x53, &[0, 1])
    }
    /// Firmware echoes this context; no proven module-mask storage.
    pub fn upgrade_context(&mut self, word: u32) -> Result<()> {
        let mut p = vec![1];
        p.extend(word.to_le_bytes());
        self.session.set(0x53, &p)
    }
    pub(crate) fn fixed_selector(
        &mut self,
        c: u8,
        q: &[u8],
        n: usize,
    ) -> Result<Vec<u8>> {
        let p = self.selector(c, q)?;
        if p.len() != n {
            return Err(invalid("Unexpected reply length"));
        }
        Ok(p)
    }
}
#[derive(Debug, Clone, Copy)]
pub enum LowLevelControl {
    Id18 = 0x18,
    Id4c = 0x4c,
    Id30 = 0x30,
}
impl<T: Read + Write> X4<T> {
    /// Numeric register-like controls; physical semantics unresolved.
    pub fn low_level_read(
        &mut self,
        id: LowLevelControl,
        address: u8,
    ) -> Result<u8> {
        if matches!(id, LowLevelControl::Id30) {
            return Err(unsupported(
                "37 ID30 GET returns failure on this firmware",
            ));
        }
        let p = self.session.get(
            0x37,
            &[0x80, id as u8, address, 0, 0, 0, 1],
            |f| f.payload.first() == Some(&0x81),
        )?;
        if p.len() != 4 || p[1] != id as u8 || p[2] != 1 {
            return Err(invalid("Low-level read shape"));
        }
        Ok(p[3])
    }
    pub fn low_level_write(
        &mut self,
        id: LowLevelControl,
        address: u8,
        value: u8,
    ) -> Result<()> {
        self.session.set(0x37, &[0, id as u8, 0, address, value])
    }
}
