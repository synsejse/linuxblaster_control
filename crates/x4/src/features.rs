//! Device EQ slots and Super X-Fi mode selection.
//!
//! Hardware slots are Music (0), Movie (1), Footsteps (2) and SXFI (3).
//! Names of Creative app presets assigned to those slots are separate app data.
//! EQ enable uses DSP `96:09`, not the active-index high bit. Slot 3 becomes
//! active with SXFI; its initial flat curve is not a separate EQ bypass mode.
//! This module does not upload head maps or headphone-compensation assets.
use crate::{
    Feature, Output, X4,
    dsp::{ProfileBank, ProfileField, ProfileSource},
    error::{Result, invalid},
    protocol::word,
    sxfi::{ModeToken, SxfiQuery},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    thread,
    time::Duration,
};

/// SXFI enable plus processing mode. `Off` preserves the latent selected mode;
/// `On` selects ASCII `MV  `, `Battle` selects `PCG ` before enabling button 30.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SxfiMode {
    Off,
    On,
    Battle,
}
impl SxfiMode {
    /// Processing token selected before enabling; `None` for `Off`.
    fn token(self) -> Option<ModeToken> {
        match self {
            Self::Off => None,
            Self::On => Some(ModeToken::Movie),
            Self::Battle => Some(ModeToken::Pcg),
        }
    }
    /// Whether EQ slot `index` is the one this mode can use.
    pub(crate) fn uses_slot(self, index: u8) -> bool {
        (index == SXFI_SLOT) == (self != Self::Off)
    }
}
/// How long the device may take to load a slot or change processing mode.
const LOAD_TIMEOUT: Duration = Duration::from_secs(4);
const LOAD_POLL: Duration = Duration::from_millis(80);
/// Button ID of the SXFI on/off switch in the command-`26` inventory.
const SXFI_BUTTON: u16 = 30;
/// Hardware EQ slot that becomes active with Super X-Fi.
const SXFI_SLOT: u8 = 3;
/// On-device slot identity and UTF-16-decoded firmware name, not an app preset label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqProfileInfo {
    pub index: u8,
    pub name: String,
}

impl<T: Read + Write> X4<T> {
    /// Position of the SXFI button in `26 [05]`, i.e. its state-mask bit.
    pub(crate) fn sxfi_button_bit(&mut self) -> Result<Option<u8>> {
        let p = self.selector(0x26, &[5])?;
        if p.len() < 4
            || p.len() != 4 + p[2] as usize * 2
            || p[1] != 0
            || p[3] != 0
        {
            return Err(invalid("Unsupported button capability layout"));
        }
        let bit = p[4..]
            .as_chunks::<2>()
            .0
            .iter()
            .position(|v| u16::from_le_bytes(*v) == SXFI_BUTTON);
        match bit {
            Some(bit) if bit >= 64 => {
                Err(invalid("SXFI button index exceeds state mask"))
            }
            bit => Ok(bit.map(|b| b as u8)),
        }
    }
    /// GET `26 [08 ff ff]`; reply `08 ff ff state:u64le`.
    pub(crate) fn button_enabled(&mut self, bit: u8) -> Result<bool> {
        let buttons = self.selector(0x26, &[8, 255, 255])?;
        if buttons.len() != 11 || buttons[1..3] != [255, 255] {
            return Err(invalid("Malformed button state"));
        }
        let state =
            u64::from(word(&buttons, 3)) | u64::from(word(&buttons, 7)) << 32;
        Ok(state & (1 << bit) != 0)
    }
    /// SET `26 [07 1e 00 enabled 00]`; acknowledgement only.
    pub(crate) fn write_sxfi_button(&mut self, enabled: bool) -> Result<()> {
        let [low, high] = SXFI_BUTTON.to_le_bytes();
        self.session
            .set(0x26, &[7, low, high, u8::from(enabled), 0])
    }
    /// Check `6f [03]` advertises `token`, then SET `6f [0c token]` through
    /// its pending ACK. Does not touch the SXFI button.
    pub(crate) fn select_sxfi_token(&mut self, token: ModeToken) -> Result<()> {
        let modes = self.sxfi_query(SxfiQuery::Modes)?.0;
        if !modes[2..].as_chunks::<4>().0.contains(&token.bytes()) {
            return Err(invalid("SXFI mode is not supported"));
        }
        let mut payload = vec![0x0c];
        payload.extend(token.bytes());
        self.session.set_pending(0x6f, &payload, 0x0c)
    }
    /// Resolve button ID 30 in `26 [05]`, then read enable via `26 [08 ff ff]`.
    /// Enabled devices also read mode via `6f [0d]`. Return `None` when the
    /// button is not advertised; a latent `PCG ` mode alone does not mean Battle is on.
    pub fn sxfi(&mut self) -> Result<Option<SxfiMode>> {
        let Some(bit) = self.sxfi_button_bit()? else {
            return Ok(None);
        };
        if !self.button_enabled(bit)? {
            return Ok(Some(SxfiMode::Off));
        }
        let mode = self.sxfi_query(SxfiQuery::CurrentMode)?.0;
        match &mode[1..5] {
            b"MV  " => Ok(Some(SxfiMode::On)),
            b"PCG " => Ok(Some(SxfiMode::Battle)),
            _ => Err(invalid("Unknown SXFI processing mode")),
        }
    }
    /// GET `1a [01 02 00]`; reply `02 raw_index`. Return `raw_index & 7f`.
    /// Bit7 copies current cached record byte35 bit0; it is not EQ enable.
    pub fn active_eq_profile(&mut self) -> Result<u8> {
        let p = self
            .session
            .get(0x1a, &[1, 2, 0], |f| f.payload.first() == Some(&2))?;
        if p.len() != 2 {
            return Err(invalid("Invalid active EQ profile reply"));
        }
        Ok(p[1] & 0x7f)
    }
    /// GET `1d [01 02 flags]`: bit0 selects factory entries, bit1 the output bank.
    /// Returns the four-byte entries unchanged; these describe cached profile
    /// metadata, not whether the current mode permits selecting the slot.
    pub fn profile_inventory(
        &mut self,
        output: Output,
        source: ProfileSource,
    ) -> Result<Vec<[u8; 4]>> {
        let flags = ProfileBank::from(output).flags() | source.flag();
        let info = self.session.get(0x1d, &[1, 2, flags], |f| {
            f.payload.starts_with(&[1, 0, 2, flags])
        })?;
        if info.len() < 6
            || info[4] != info[5]
            || info[4] > 4
            || info.len() != 6 + info[5] as usize * 4
        {
            return Err(invalid("Unsupported EQ profile capability layout"));
        }
        Ok(info[6..].as_chunks::<4>().0.to_vec())
    }
    /// GET inventory `1d [01 02 flags]` and names `18 [01 02 index]`.
    /// Names may span replies; the firmware zero-extends stored bytes into
    /// two-byte wire characters. Stored EQ values are read separately.
    pub fn eq_profiles(
        &mut self,
        output: Output,
    ) -> Result<Vec<EqProfileInfo>> {
        let info = self.profile_inventory(output, ProfileSource::Writable)?;
        let mut profiles = Vec::new();
        for index in 0..info.len() as u8 {
            let p = self.profile_field(index, ProfileField::Name)?;
            let units: Vec<u16> = p
                .as_chunks::<2>()
                .0
                .iter()
                .map(|v| u16::from_le_bytes(*v))
                .collect();
            let name = String::from_utf16(&units)
                .map_err(|_| invalid("EQ profile name is not UTF-16"))?
                .trim_end_matches('\0')
                .to_owned();
            profiles.push(EqProfileInfo { index, name });
        }
        Ok(profiles)
    }
    /// Device sends ACK before loading a profile and then emits EQ notifications.
    /// Match active and stored values so callers never receive an intermediate EQ.
    fn wait_eq_loaded(&mut self, output: Output, index: u8) -> Result<()> {
        thread::sleep(LOAD_POLL);
        self.settle(
            LOAD_TIMEOUT,
            LOAD_POLL,
            "Device EQ profile did not settle; reconnect and read state",
            |x4| {
                if x4.output()? != output {
                    return Err(invalid(
                        "Output changed during processing transition",
                    ));
                }
                if x4.active_eq_profile()? != index {
                    return Ok(false);
                }
                let expected = x4.stored_eq_profile(output, index)?;
                Ok(expected.approx_eq(&x4.eq()?))
            },
        )
    }
    pub(crate) fn check_eq_context(
        &mut self,
        output: Output,
        index: u8,
        sxfi: Option<SxfiMode>,
    ) -> Result<()> {
        if self.output()? != output
            || self.active_eq_profile()? != index
            || self.sxfi()? != sxfi
            || self.feature(Feature::DirectMode)?
        {
            return Err(invalid(
                "EQ context changed during apply; refresh before continuing",
            ));
        }
        Ok(())
    }
    /// SET `1a [00 02 index]`, then wait for active EQ to match the stored slot.
    /// Reject Direct Mode, unadvertised slots, slot 3 with SXFI off, and slots
    /// 0..2 with SXFI on. This does not automatically change SXFI mode.
    pub fn set_eq_profile(&mut self, index: u8) -> Result<()> {
        if self.feature(Feature::DirectMode)? {
            return Err(invalid("Turn off Direct Mode before selecting EQ"));
        }
        let output = self.output()?;
        if !self.eq_profiles(output)?.iter().any(|p| p.index == index) {
            return Err(invalid("EQ profile not advertised"));
        }
        if !self.sxfi()?.unwrap_or(SxfiMode::Off).uses_slot(index) {
            return Err(invalid(
                "SXFI EQ requires Super X-Fi; Music, Movie and Footsteps require SXFI off",
            ));
        }
        self.session.set(0x1a, &[0, 2, index])?;
        self.wait_eq_loaded(output, index)
    }
    /// Select `6f [0c mode[4]]` with pending-ACK handling when enabling, then
    /// SET button `26 [07 1e 00 enabled 00]` and verify mode/slot/EQ settling.
    /// Requires headphone output and Direct Mode off. Disabling only clears
    /// the button, leaving the selected processing mode available for next time.
    pub fn set_sxfi(&mut self, mode: SxfiMode) -> Result<()> {
        let output = self.output()?;
        if output != Output::Headphone {
            return Err(invalid("Super X-Fi requires headphone output"));
        }
        if self.feature(Feature::DirectMode)? {
            return Err(invalid(
                "Turn off Direct Mode before changing Super X-Fi",
            ));
        }
        if self.sxfi_button_bit()?.is_none() {
            return Err(invalid("Super X-Fi is not advertised"));
        }
        if let Some(token) = mode.token() {
            self.select_sxfi_token(token)?;
        }
        self.write_sxfi_button(mode != SxfiMode::Off)?;
        thread::sleep(LOAD_POLL);
        let mut index = 0;
        self.settle(
            LOAD_TIMEOUT,
            LOAD_POLL,
            "SXFI mode readback did not settle",
            |x4| {
                index = x4.active_eq_profile()?;
                Ok(x4.sxfi()? == Some(mode) && mode.uses_slot(index))
            },
        )?;
        self.wait_eq_loaded(output, index)
    }
}

#[cfg(test)]
mod tests;
