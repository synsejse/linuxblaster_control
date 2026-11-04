//! The thread that owns the X4 and runs every read and write.
use soundblaster_x4::{
    Equalizer, Error, Feature, Output, Port, Result, State, SxfiMode, X4, open,
};
use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

/// How often to read the device's state while it is connected.
const CONNECTED_POLL: Duration = Duration::from_secs(3);
/// How often to look for the device while it is not.
const DISCONNECTED_POLL: Duration = Duration::from_secs(5);

fn rejected(message: &str) -> Error {
    Error::Invalid(message.into())
}

/// EQ belongs to an output and processing/profile context. A UI draft must not
/// silently become a write to another context after a phone or button change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EqContext {
    output: Output,
    sxfi: Option<SxfiMode>,
    profile: Option<u8>,
    pub direct: bool,
}
impl EqContext {
    pub fn from_state(state: &State) -> Self {
        Self {
            output: state.output,
            sxfi: state.sxfi,
            profile: state.active_eq_profile,
            direct: state.feature(Feature::DirectMode),
        }
    }
    pub fn check(self, state: &State) -> Result<()> {
        if self != Self::from_state(state) {
            return Err(rejected(
                "The output, sound mode or EQ profile changed. Refresh and review the settings before applying them.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    Refresh,
    Output(Output),
    Feature(Feature, bool),
    Sxfi(EqContext, SxfiMode),
    Profile(EqContext, u8),
    Eq(EqContext, Equalizer),
    MixerLevel(u8, f32),
    MixerMute(u8, bool),
    MicrophoneBoost(bool),
}
impl Command {
    fn writes(&self) -> bool {
        !matches!(self, Self::Refresh)
    }
}

/// What led to a state report.
#[derive(Clone, Copy)]
pub enum Completion {
    Poll,
    Refresh,
    Control,
    EqApplied,
}
impl Completion {
    fn of(command: Option<&Command>) -> Self {
        match command {
            None => Self::Poll,
            Some(Command::Refresh) => Self::Refresh,
            Some(Command::Eq(..)) => Self::EqApplied,
            Some(_) => Self::Control,
        }
    }
}

pub enum Event {
    State(State, Completion),
    Error(String),
}

/// Start the worker for `port`, or for the only X4 attached.
pub fn spawn(port: Option<String>) -> (Sender<Command>, Receiver<Event>) {
    let (tx, commands) = mpsc::channel();
    let (events, rx) = mpsc::channel();
    thread::spawn(move || run(port.as_deref(), &commands, &events));
    (tx, rx)
}

fn run(
    port: Option<&str>,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
) {
    let mut device = None;
    loop {
        let poll = if device.is_some() {
            CONNECTED_POLL
        } else {
            DISCONNECTED_POLL
        };
        let command = match commands.recv_timeout(poll) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let command = command.as_ref();
        let event = match serve(&mut device, port, command) {
            Ok(state) => Event::State(state, Completion::of(command)),
            Err(error) => {
                // A partial write is never replayed after reconnect.
                device = None;
                let prefix = if command.is_some_and(Command::writes) {
                    "Change failed; some settings may have changed. Read device state before trying again. "
                } else {
                    ""
                };
                Event::Error(format!("{prefix}{error}"))
            }
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

/// Run `command`, if any, connecting first if need be, then read the state.
fn serve(
    device: &mut Option<X4<Port>>,
    port: Option<&str>,
    command: Option<&Command>,
) -> Result<State> {
    let device = match device {
        Some(device) => device,
        None if command.is_some_and(Command::writes) => {
            return Err(rejected(
                "Connection changed; refresh before changing settings",
            ));
        }
        None => device.insert(open(port)?),
    };
    match command {
        None | Some(Command::Refresh) => {}
        Some(Command::Output(output)) => device.set_output(*output)?,
        Some(Command::Feature(feature, enabled)) => {
            device.set_feature(*feature, *enabled)?;
        }
        Some(Command::Sxfi(context, mode)) => {
            context.check(&device.state()?)?;
            device.set_sxfi(*mode)?;
        }
        Some(Command::Profile(context, index)) => {
            let current = device.state()?;
            context.check(&current)?;
            if !current.eq_profile_available(*index) {
                return Err(rejected(
                    "This EQ profile is unavailable in the current sound mode",
                ));
            }
            device.set_eq_profile(*index)?;
        }
        Some(Command::Eq(context, eq)) => {
            context.check(&device.state()?)?;
            if context.direct {
                return Err(rejected("Turn off Direct Mode to apply EQ"));
            }
            device.apply_eq(context.output, eq)?;
        }
        Some(Command::MixerLevel(index, db)) => {
            device.set_mixer_level(*index, *db)?;
        }
        Some(Command::MixerMute(index, muted)) => {
            device.set_mixer_mute(*index, *muted)?;
        }
        Some(Command::MicrophoneBoost(enabled)) => {
            device.set_microphone_boost(*enabled)?;
        }
    }
    device.state()
}
