//! What the app is doing to the device, and what to say about it.
use super::worker::Command;
use soundblaster_x4::{Feature, Output, State, SxfiMode};
use std::time::{Duration, Instant};

/// How long a finished change keeps its check mark.
pub const CONFIRMATION: Duration = Duration::from_millis(1600);

/// The control a write belongs to, so progress can appear on that control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Refresh,
    Apply,
    Output(Output),
    Feature(Feature),
    Sxfi,
    Profile(u8),
    Eq,
    MixerLevel(u8),
    MixerMute(u8),
    MicrophoneBoost,
}

/// A command in flight, and what to say about it before and after.
#[derive(Debug)]
pub struct Activity {
    pub control: Control,
    pub progress: String,
    pub done: String,
}

/// A finished change, shown until [`CONFIRMATION`] has passed.
#[derive(Debug)]
pub struct Confirmed {
    pub control: Control,
    pub message: String,
    pub at: Instant,
}

pub fn output_name(output: Output) -> &'static str {
    match output {
        Output::Speaker => "Speakers",
        Output::Headphone => "Headphones",
    }
}
pub fn sxfi_name(mode: SxfiMode) -> &'static str {
    match mode {
        SxfiMode::Off => "Off",
        SxfiMode::On => "On",
        SxfiMode::Battle => "Battle",
    }
}

impl Activity {
    fn new(
        control: Control,
        progress: impl Into<String>,
        done: impl Into<String>,
    ) -> Self {
        Self {
            control,
            progress: progress.into(),
            done: done.into(),
        }
    }
    fn switch(control: Control, name: &str, on: bool) -> Self {
        let state = if on { "on" } else { "off" };
        Self::new(
            control,
            format!("Turning {name} {state}…"),
            format!("{name} {state}"),
        )
    }
    /// How `command` reads, using names from the last known `state`.
    pub fn describe(command: &Command, state: Option<&State>) -> Self {
        let mixer_name = |index: u8| {
            state
                .and_then(|s| s.mixer.iter().find(|c| c.index == index))
                .map_or("Level", |c| c.name.as_str())
        };
        match command {
            Command::Apply(..) => Self::new(
                Control::Apply,
                "Applying changes…",
                "Changes applied",
            ),
            Command::Refresh if state.is_none() => Self::new(
                Control::Refresh,
                "Connecting…",
                "Connected to your X4",
            ),
            Command::Refresh => Self::new(
                Control::Refresh,
                "Reading the X4…",
                "Device state updated",
            ),
            Command::Output(output) => {
                let name = output_name(*output);
                Self::new(
                    Control::Output(*output),
                    format!("Switching to {name}…"),
                    format!("Now on {name}"),
                )
            }
            Command::Feature(feature, on) => {
                let name = match feature {
                    Feature::DirectMode => "Direct mode",
                    Feature::HeadphoneHighGain => "High headphone gain",
                    _ => "Feature",
                };
                Self::switch(Control::Feature(*feature), name, *on)
            }
            Command::Sxfi(_, mode) => {
                let name = sxfi_name(*mode);
                Self::new(
                    Control::Sxfi,
                    format!("Setting Super X-Fi to {name}…"),
                    format!("Super X-Fi set to {name}"),
                )
            }
            Command::Profile(_, index) => {
                let name = state
                    .and_then(|s| {
                        s.eq_profiles.iter().find(|p| p.index == *index)
                    })
                    .map_or("Profile", |p| p.name.as_str());
                Self::new(
                    Control::Profile(*index),
                    format!("Loading {name}…"),
                    format!("{name} loaded"),
                )
            }
            Command::Eq(..) => {
                let name = state
                    .and_then(State::active_profile)
                    .map_or("your X4", |p| p.name.as_str());
                Self::new(
                    Control::Eq,
                    format!("Writing EQ to {name}…"),
                    format!("EQ saved to {name}"),
                )
            }
            Command::MixerLevel(index, db) => {
                let name = mixer_name(*index);
                Self::new(
                    Control::MixerLevel(*index),
                    format!("Setting {name} to {db:.1} dB…"),
                    format!("{name} set to {db:.1} dB"),
                )
            }
            Command::MixerMute(index, muted) => {
                let name = mixer_name(*index);
                let (progress, done) = if *muted {
                    (format!("Muting {name}…"), format!("{name} muted"))
                } else {
                    (format!("Unmuting {name}…"), format!("{name} unmuted"))
                };
                Self::new(Control::MixerMute(*index), progress, done)
            }
            Command::MicrophoneBoost(on) => {
                Self::switch(Control::MicrophoneBoost, "Microphone Boost", *on)
            }
        }
    }
}
