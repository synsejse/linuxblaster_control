//! Numeric SXFI selectors retain unresolved fields without inventing acoustic meaning.
use crate::{
    X4,
    controls::Information,
    protocol::{Result, invalid},
};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeToken {
    Movie,
    Fps,
    Pcg,
    Music,
}
impl ModeToken {
    pub fn bytes(self) -> [u8; 4] {
        match self {
            Self::Movie => *b"MV  ",
            Self::Fps => *b"FPS ",
            Self::Pcg => *b"PCG ",
            Self::Music => *b"MSC ",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum SxfiQuery {
    Capabilities,
    Selectors,
    Modes,
    State,
    Identifiers,
    Identifier,
    State09,
    CurrentMode,
    Capability0e,
    Flag0f,
    State10,
    Capability12,
    Entries13,
}
impl SxfiQuery {
    fn selector(self) -> u8 {
        match self {
            Self::Capabilities => 0,
            Self::Selectors => 1,
            Self::Modes => 3,
            Self::State => 5,
            Self::Identifiers => 6,
            Self::Identifier => 8,
            Self::State09 => 9,
            Self::CurrentMode => 13,
            Self::Capability0e => 14,
            Self::Flag0f => 15,
            Self::State10 => 16,
            Self::Capability12 => 18,
            Self::Entries13 => 19,
        }
    }
}
impl<T: Read + Write> X4<T> {
    /// Runtime gates can reject queries; identifiers never establish personalization activation.
    pub fn sxfi_query(&mut self, query: SxfiQuery) -> Result<Information> {
        let s = query.selector();
        let p = self.selector(0x6f, &[s])?;
        let n = match s {
            0 => Some(8),
            5 | 16 => Some(2),
            6 => Some(65),
            8 => Some(33),
            9 | 11 | 15 | 22 => Some(3),
            13 => Some(11),
            14 => Some(6),
            21 => Some(9),
            _ => None,
        };
        if n.is_some_and(|n| p.len() != n) || p.is_empty() {
            return Err(invalid("SXFI response length"));
        }
        let width = match s {
            3 => Some(4),
            18 => Some(1),
            19 => Some(7),
            _ => None,
        };
        if let Some(w) = width
            && (p.len() < 2 || p.len() != 2 + w * p[1] as usize)
        {
            return Err(invalid("SXFI counted response"));
        }
        Ok(Information(p))
    }
    /// Returns after final ACK and matching current token, without enabling the button.
    pub fn set_sxfi_token(&mut self, token: ModeToken) -> Result<()> {
        self.select_sxfi_token(token)?;
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            let p = self.sxfi_query(SxfiQuery::CurrentMode)?.0;
            if p[1..5] == token.bytes() {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(self
                    .session
                    .mismatch("SXFI token activation readback timeout"));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    pub fn set_sxfi_flag_0a(&mut self, value: bool) -> Result<()> {
        self.session.set(0x6f, &[10, u8::from(value)])
    }
    pub fn set_sxfi_flag_11(&mut self, value: bool) -> Result<()> {
        self.session.set(0x6f, &[17, u8::from(value)])
    }
    /// Known helper action. Argument semantics remain unresolved.
    pub fn sxfi_action_07(&mut self) -> Result<()> {
        self.session.set(0x6f, &[7])
    }
    /// Counted four-byte mode tokens. Runtime capability and busy gates apply.
    pub fn sxfi_configure_modes_02(
        &mut self,
        modes: &[ModeToken],
    ) -> Result<()> {
        if modes.is_empty() || modes.len() > 4 {
            return Err(invalid("SXFI mode count"));
        }
        let mut p = vec![2, modes.len() as u8];
        for mode in modes {
            p.extend(mode.bytes());
        }
        self.session.set(0x6f, &p)
    }
    /// Low-level helper query: explicit count and opaque data; no inferred field meaning.
    pub fn sxfi_value_0b(&mut self, count: u8, data: &[u8]) -> Result<u16> {
        self.sxfi_counted_value(11, count, data)
    }
    pub fn sxfi_value_16(&mut self, count: u8, data: &[u8]) -> Result<u16> {
        self.sxfi_counted_value(22, count, data)
    }
    fn sxfi_counted_value(
        &mut self,
        selector: u8,
        count: u8,
        data: &[u8],
    ) -> Result<u16> {
        if count == 0 || data.is_empty() {
            return Err(invalid("SXFI helper requires explicit counted data"));
        }
        let mut p = vec![selector, count];
        p.extend(data);
        let r = self.fixed_selector(0x6f, &p, 3)?;
        Ok(u16::from_le_bytes([r[1], r[2]]))
    }
    /// Explicit seven-byte capability/type/token key, returned fields remain numeric.
    pub fn sxfi_result_15(&mut self, key: [u8; 7]) -> Result<Information> {
        let mut p = vec![21];
        p.extend(key);
        Ok(Information(self.fixed_selector(0x6f, &p, 9)?))
    }
    /// Experimental compound selector14: seven bytes before on/off at offset8.
    pub fn sxfi_compound_14(
        &mut self,
        fields: [u8; 7],
        enabled: bool,
    ) -> Result<()> {
        let mut p = vec![20];
        p.extend(fields);
        p.push(u8::from(enabled));
        self.session.set(0x6f, &p)
    }
    /// Toggle only the SXFI button and verify its state bit. Unlike
    /// [`X4::set_sxfi`], this skips output, Direct Mode and EQ-slot checks.
    pub fn set_sxfi_button(&mut self, enabled: bool) -> Result<()> {
        let bit = self
            .sxfi_button_bit()?
            .ok_or_else(|| invalid("SXFI button absent"))?;
        self.write_sxfi_button(enabled)?;
        if self.button_enabled(bit)? != enabled {
            return Err(self.session.mismatch("Button readback mismatch"));
        }
        Ok(())
    }
}
