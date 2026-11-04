use iced::widget::{Space, column, container, row, slider};
use iced::{Element, Subscription, Task};
use iced_t3::{Tone, chrome, color, font, icon, style, widget as t3};
use soundblaster_x4::{
    EqProfileInfo, Equalizer, Feature, Output, State, SxfiMode,
    mixer::{MixerControl, MixerGroup},
    open,
    protocol::invalid,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        LazyLock,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

/// EQ belongs to an output and processing/profile context. A UI draft must not
/// silently become a write to another context after a phone or button change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EqContext {
    output: Output,
    sxfi: Option<SxfiMode>,
    profile: Option<u8>,
    direct: bool,
}
impl EqContext {
    fn from_state(state: &State) -> Self {
        Self {
            output: state.output,
            sxfi: state.sxfi,
            profile: state.active_eq_profile,
            direct: state.feature(Feature::DirectMode),
        }
    }
    fn check(self, state: &State) -> soundblaster_x4::protocol::Result<()> {
        if self != Self::from_state(state) {
            return Err(invalid(
                "The output, sound mode or EQ profile changed. Refresh and review the settings before applying them.",
            ));
        }
        Ok(())
    }
}

fn profile_available(state: &State, index: u8) -> bool {
    !state.feature(Feature::DirectMode)
        && match state.sxfi {
            Some(SxfiMode::Off) => index < 3,
            Some(SxfiMode::On | SxfiMode::Battle) => index == 3,
            None => false,
        }
}

fn mixer_mute_available(control: &MixerControl) -> bool {
    // Tested X4 firmware ACKs playback S/PDIF mute without changing its state.
    // Other S/PDIF controls (monitoring and recording) have distinct kinds.
    control.muted.is_some() && control.kind() != 14
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
#[derive(Clone, Copy)]
enum Completion {
    Poll,
    Refresh,
    Control,
    EqApplied,
}
enum Event {
    State(State, Completion),
    Error(String),
}
/// The control a write belongs to, so progress can appear on that control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Refresh,
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
struct Activity {
    control: Control,
    progress: String,
    done: String,
}
/// How long a finished change keeps its check mark.
const CONFIRMATION: Duration = Duration::from_millis(1600);

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Page {
    #[default]
    Equalizer,
    Mixer,
}

pub struct X4App {
    offline: bool,
    tx: Sender<Command>,
    rx: Receiver<Event>,
    state: Option<State>,
    draft: Option<Equalizer>,
    draft_context: Option<EqContext>,
    dirty: bool,
    pending: Option<Activity>,
    confirmed: Option<(Control, String, Instant)>,
    error: Option<String>,
    notice: Option<String>,
    page: Page,
    mixer_drafts: BTreeMap<u8, f32>,
    mixer_editing: BTreeSet<u8>,
}
impl X4App {
    pub fn new(port: Option<String>) -> Self {
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut device = None;
            loop {
                let command = match commands.recv_timeout(Duration::from_secs(
                    if device.is_some() { 3 } else { 5 },
                )) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let completion = match command {
                    None => Completion::Poll,
                    Some(Command::Refresh) => Completion::Refresh,
                    Some(Command::Eq(_, _)) => Completion::EqApplied,
                    _ => Completion::Control,
                };
                let result = (|| {
                    if device.is_none() {
                        if command.as_ref().is_some_and(Command::writes) {
                            return Err(invalid(
                                "Connection changed; refresh before changing settings",
                            ));
                        }
                        device = Some(open(port.as_deref())?);
                    }
                    let dev = device.as_mut().unwrap();
                    match &command {
                        Some(Command::Output(output)) => {
                            dev.set_output(*output)?
                        }
                        Some(Command::Feature(feature, enabled)) => {
                            dev.set_feature(*feature, *enabled)?
                        }
                        Some(Command::Sxfi(context, mode)) => {
                            context.check(&dev.state()?)?;
                            dev.set_sxfi(*mode)?;
                        }
                        Some(Command::Profile(context, index)) => {
                            let current = dev.state()?;
                            context.check(&current)?;
                            if !profile_available(&current, *index) {
                                return Err(invalid(
                                    "This EQ profile is unavailable in the current sound mode",
                                ));
                            }
                            dev.set_eq_profile(*index)?;
                        }
                        Some(Command::Eq(context, eq)) => {
                            context.check(&dev.state()?)?;
                            if context.direct {
                                return Err(invalid(
                                    "Turn off Direct Mode to apply EQ",
                                ));
                            }
                            dev.apply_eq(context.output, eq)?;
                        }
                        Some(Command::MixerLevel(index, db)) => {
                            dev.set_mixer_level(*index, *db)?;
                        }
                        Some(Command::MixerMute(index, muted)) => {
                            dev.set_mixer_mute(*index, *muted)?;
                        }
                        Some(Command::MicrophoneBoost(enabled)) => {
                            dev.set_microphone_boost(*enabled)?;
                        }
                        _ => (),
                    }
                    dev.state()
                })();
                let event = match result {
                    Ok(state) => Event::State(state, completion),
                    Err(error) => {
                        // A partial write is never replayed after reconnect.
                        device = None;
                        let prefix = if command
                            .as_ref()
                            .is_some_and(Command::writes)
                        {
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
        });
        let mut app = Self::with_channels(tx, rx);
        app.send(Command::Refresh);
        app
    }
    fn with_channels(tx: Sender<Command>, rx: Receiver<Event>) -> Self {
        Self {
            offline: false,
            tx,
            rx,
            state: None,
            draft: None,
            draft_context: None,
            dirty: false,
            pending: None,
            confirmed: None,
            error: None,
            notice: None,
            page: Page::default(),
            mixer_drafts: BTreeMap::new(),
            mixer_editing: BTreeSet::new(),
        }
    }
    fn send(&mut self, command: Command) {
        if self.offline {
            return;
        }
        // Also guard against several widgets firing in the same frame.
        if self.busy() {
            return;
        }
        let activity = self.activity(&command);
        match self.tx.send(command) {
            Ok(()) => {
                self.pending = Some(activity);
                self.confirmed = None;
                self.error = None;
                self.notice = None;
            }
            Err(_) => {
                self.error = Some(
                    "Device worker stopped. Restart the application.".into(),
                )
            }
        }
    }
    fn busy(&self) -> bool {
        self.pending.is_some()
    }
    fn in_flight(&self, control: Control) -> bool {
        self.pending.as_ref().is_some_and(|a| a.control == control)
    }
    fn activity(&self, command: &Command) -> Activity {
        let state = self.state.as_ref();
        let mixer_name = |index: u8| {
            state
                .and_then(|s| s.mixer.iter().find(|c| c.index == index))
                .map_or("Level", |c| c.name.as_str())
        };
        let profile_name = |index: u8| {
            state
                .and_then(|s| s.eq_profiles.iter().find(|p| p.index == index))
                .map_or("Profile", |p| p.name.as_str())
        };
        let switch = |on: bool| if on { "on" } else { "off" };
        let (control, progress, done) = match command {
            Command::Refresh if state.is_none() => (
                Control::Refresh,
                "Connecting…".into(),
                "Connected to your X4".into(),
            ),
            Command::Refresh => (
                Control::Refresh,
                "Reading the X4…".into(),
                "Device state updated".into(),
            ),
            Command::Output(output) => {
                let name = output_name(*output);
                (
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
                (
                    Control::Feature(*feature),
                    format!("Turning {name} {}…", switch(*on)),
                    format!("{name} {}", switch(*on)),
                )
            }
            Command::Sxfi(_, mode) => {
                let name = sxfi_name(*mode);
                (
                    Control::Sxfi,
                    format!("Setting Super X-Fi to {name}…"),
                    format!("Super X-Fi set to {name}"),
                )
            }
            Command::Profile(_, index) => {
                let name = profile_name(*index);
                (
                    Control::Profile(*index),
                    format!("Loading {name}…"),
                    format!("{name} loaded"),
                )
            }
            Command::Eq(..) => {
                let name = state
                    .and_then(active_profile)
                    .map_or("your X4", |p| p.name.as_str());
                (
                    Control::Eq,
                    format!("Writing EQ to {name}…"),
                    format!("EQ saved to {name}"),
                )
            }
            Command::MixerLevel(index, db) => {
                let name = mixer_name(*index);
                (
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
                (Control::MixerMute(*index), progress, done)
            }
            Command::MicrophoneBoost(on) => (
                Control::MicrophoneBoost,
                format!("Turning Microphone Boost {}…", switch(*on)),
                format!("Microphone Boost {}", switch(*on)),
            ),
        };
        Activity {
            control,
            progress,
            done,
        }
    }
    fn events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::State(state, completed) => {
                    let context = EqContext::from_state(&state);
                    let context_changed =
                        self.draft_context.is_some_and(|old| old != context);
                    let reset = !self.dirty
                        || context_changed
                        || matches!(
                            completed,
                            Completion::Refresh | Completion::EqApplied
                        );
                    if context_changed && self.dirty {
                        self.notice = Some("The output, sound mode or profile changed. Unsaved EQ edits were discarded; the current device EQ is shown.".into());
                    }
                    if reset {
                        self.draft = state.eq.clone();
                        self.draft_context = Some(context);
                        self.dirty = false;
                    }
                    for control in &state.mixer {
                        if !self.mixer_editing.contains(&control.index)
                            && let Some(level) = control.level_db
                        {
                            self.mixer_drafts.insert(control.index, level);
                        }
                    }
                    self.state = Some(state);
                    if !matches!(completed, Completion::Poll)
                        && let Some(activity) = self.pending.take()
                    {
                        self.confirmed = Some((
                            activity.control,
                            activity.done,
                            Instant::now(),
                        ));
                    }
                }
                Event::Error(error) => {
                    self.error = Some(error);
                    self.state = None;
                    self.pending = None;
                    self.confirmed = None;
                    self.draft = None;
                    self.draft_context = None;
                    self.dirty = false;
                    self.mixer_drafts.clear();
                    self.mixer_editing.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundblaster_x4::mixer::MixerRange;

    pub(super) fn state() -> State {
        State {
            firmware: "test".into(),
            output: Output::Headphone,
            supported_features: 0x60,
            reset_features: 0,
            features: 0x40,
            eq_supported: true,
            eq: Some(Equalizer {
                enabled: true,
                preamp_db: -9.0,
                bands_db: [0.0; 10],
            }),
            sxfi: Some(SxfiMode::Off),
            eq_profiles: vec![EqProfileInfo {
                index: 0,
                name: "Music".into(),
            }],
            active_eq_profile: Some(0),
            mixer: vec![MixerControl {
                index: 1,
                name: "Headphones".into(),
                group: MixerGroup::Playback,
                descriptor: 0x89c2,
                has_volume: true,
                is_db: true,
                range: Some(MixerRange {
                    min_db: -80.0,
                    max_db: 0.0,
                    step_db: 1.0,
                    min_raw: -20480,
                    max_raw: 0,
                    step_raw: 256,
                }),
                level_db: Some(-6.0),
                raw_level: Some(-1536),
                muted: Some(false),
                microphone_boost: None,
            }],
        }
    }
    pub(super) fn app() -> (X4App, Sender<Event>, Receiver<Command>) {
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        (X4App::with_channels(tx, rx), events, commands)
    }
    #[test]
    fn mixer_refresh_preserves_eq_edits_but_context_change_discards_them() {
        let (mut app, events, _) = app();
        events
            .send(Event::State(state(), Completion::Refresh))
            .unwrap();
        app.events();
        app.draft.as_mut().unwrap().preamp_db = -6.0;
        app.dirty = true;
        events
            .send(Event::State(state(), Completion::Control))
            .unwrap();
        app.events();
        assert!(app.dirty);
        assert_eq!(app.draft.as_ref().unwrap().preamp_db, -6.0);
        let mut changed = state();
        changed.active_eq_profile = Some(1);
        events
            .send(Event::State(changed, Completion::Poll))
            .unwrap();
        app.events();
        assert!(!app.dirty);
        assert_eq!(app.draft.as_ref().unwrap().preamp_db, -9.0);
        assert!(app.notice.is_some());
    }
    #[test]
    fn stale_context_rejected_for_mode_profile_output_and_direct_changes() {
        let original = state();
        let context = EqContext::from_state(&original);
        assert!(context.check(&original).is_ok());
        let mut changed = original.clone();
        changed.sxfi = Some(SxfiMode::Battle);
        assert!(context.check(&changed).is_err());
        changed = original.clone();
        changed.active_eq_profile = Some(1);
        assert!(context.check(&changed).is_err());
        changed = original.clone();
        changed.output = Output::Speaker;
        assert!(context.check(&changed).is_err());
        changed = original;
        changed.features |= 1 << Feature::DirectMode.bit();
        assert!(context.check(&changed).is_err());
    }
    #[test]
    fn poll_preserves_an_active_mixer_edit_then_refreshes_after_release() {
        let (mut app, events, _) = app();
        events
            .send(Event::State(state(), Completion::Refresh))
            .unwrap();
        app.events();
        app.mixer_drafts.insert(1, -12.0);
        app.mixer_editing.insert(1);
        events
            .send(Event::State(state(), Completion::Poll))
            .unwrap();
        app.events();
        assert_eq!(app.mixer_drafts[&1], -12.0);
        app.mixer_editing.remove(&1);
        events
            .send(Event::State(state(), Completion::Poll))
            .unwrap();
        app.events();
        assert_eq!(app.mixer_drafts[&1], -6.0);
    }
    #[test]
    fn profile_availability_follows_sxfi_and_direct_mode() {
        let mut current = state();
        assert!(profile_available(&current, 0));
        assert!(profile_available(&current, 2));
        assert!(!profile_available(&current, 3));
        for mode in [SxfiMode::On, SxfiMode::Battle] {
            current.sxfi = Some(mode);
            assert!(profile_available(&current, 3));
            assert!(!profile_available(&current, 0));
        }
        current.features |= 1 << Feature::DirectMode.bit();
        assert!(!profile_available(&current, 3));
        current.features &= !(1 << Feature::DirectMode.bit());
        current.sxfi = None;
        assert!(!profile_available(&current, 0));
        assert!(!profile_available(&current, 3));
    }
    #[test]
    fn spdif_playback_mute_is_disabled_without_disabling_other_mutes() {
        let mut control = state().mixer.remove(0);
        assert!(mixer_mute_available(&control));
        control.index = 2;
        control.descriptor = 0x29ce;
        control.group = MixerGroup::Playback;
        assert!(!mixer_mute_available(&control));
        control.descriptor = 0x29d1;
        control.group = MixerGroup::Monitoring;
        assert!(mixer_mute_available(&control));
        control.descriptor = 0x2ac9;
        control.group = MixerGroup::Recording;
        assert!(mixer_mute_available(&control));
        control.muted = None;
        assert!(!mixer_mute_available(&control));
    }
    #[test]
    fn boost_waits_for_confirmed_state_and_preserves_eq_draft() {
        let (mut app, events, commands) = app();
        let mut current = X4App::demo().state.unwrap();
        let boost = current.mixer.iter_mut().find(|c| c.index == 10).unwrap();
        assert!(boost.is_microphone_boost());
        boost.microphone_boost = Some(false);
        events
            .send(Event::State(current.clone(), Completion::Refresh))
            .unwrap();
        app.events();
        let _ = app.update(Message::Band(0, 3.0));
        let _ = app.update(Message::Command(Command::MicrophoneBoost(true)));
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::MicrophoneBoost(true)
        ));
        assert!(app.busy());
        assert_eq!(
            app.state
                .as_ref()
                .unwrap()
                .mixer
                .last()
                .unwrap()
                .microphone_boost,
            Some(false)
        );
        current.mixer.last_mut().unwrap().microphone_boost = Some(true);
        events
            .send(Event::State(current, Completion::Control))
            .unwrap();
        app.events();
        assert!(!app.busy());
        assert_eq!(
            app.state
                .as_ref()
                .unwrap()
                .mixer
                .last()
                .unwrap()
                .microphone_boost,
            Some(true)
        );
        assert!(app.dirty);
        assert_eq!(app.draft.as_ref().unwrap().bands_db[0], 3.0);
    }
    #[test]
    fn progress_stays_on_its_control_until_the_write_is_confirmed() {
        let (mut app, events, _commands) = app();
        events
            .send(Event::State(state(), Completion::Refresh))
            .unwrap();
        app.events();
        app.confirmed = None;
        app.send(Command::Output(Output::Speaker));
        let control = Control::Output(Output::Speaker);
        assert!(app.in_flight(control));
        assert!(!app.in_flight(Control::Output(Output::Headphone)));
        assert_eq!(
            app.pending.as_ref().unwrap().progress,
            "Switching to Speakers…"
        );
        // A background poll is not the write's confirmation.
        events
            .send(Event::State(state(), Completion::Poll))
            .unwrap();
        app.events();
        assert!(app.in_flight(control));
        assert!(app.confirmed.is_none());
        events
            .send(Event::State(state(), Completion::Control))
            .unwrap();
        app.events();
        assert!(!app.busy());
        let (confirmed, done, _) = app.confirmed.as_ref().unwrap();
        assert_eq!(*confirmed, control);
        assert_eq!(done, "Now on Speakers");
    }
    #[test]
    fn confirmation_expires_and_a_failure_never_confirms() {
        let (mut app, events, _commands) = app();
        app.confirmed =
            Some((Control::Sxfi, "Done".into(), Instant::now() - CONFIRMATION));
        let _ = app.update(Message::Tick);
        assert!(app.confirmed.is_none());
        app.send(Command::Refresh);
        assert!(app.busy());
        events.send(Event::Error("Read failed".into())).unwrap();
        app.events();
        assert!(!app.busy());
        assert!(app.confirmed.is_none());
    }
    #[test]
    fn controls_ignore_input_during_a_write_without_looking_disabled() {
        let (mut app, _events, _commands) = app();
        assert!(matches!(
            app.press(true, Message::Revert),
            Some(Message::Revert)
        ));
        assert!(app.press(false, Message::Revert).is_none());
        app.send(Command::Refresh);
        assert!(matches!(
            app.press(true, Message::Revert),
            Some(Message::Tick)
        ));
    }
    #[test]
    fn only_one_command_is_queued_while_busy() {
        let (mut app, _, commands) = app();
        app.send(Command::Refresh);
        app.send(Command::Feature(Feature::DirectMode, true));
        assert!(matches!(commands.try_recv().unwrap(), Command::Refresh));
        assert!(commands.try_recv().is_err());
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Window(chrome::Action),
    Page(Page),
    Command(Command),
    EqEnabled(bool),
    Band(usize, f32),
    Preamp(f32),
    Apply,
    Revert,
    Mixer(u8, f32),
    MixerApply(u8),
    Key(iced::keyboard::Key, iced::keyboard::Modifiers),
}
/// Width of the page column (T3 Code's `max-w-4xl` less its gutters).
const PAGE_WIDTH: f32 = 832.0;
/// Display size of the app icon by the title.
const ICON_SIZE: u32 = 22;
/// The app icon, scaled once to double size so it stays sharp on HiDPI screens.
static APP_ICON: LazyLock<iced::widget::image::Handle> = LazyLock::new(|| {
    let icon = image::load_from_memory(include_bytes!(
        "../../../packaging/linuxblaster-control.png"
    ))
    .expect("bundled app icon")
    .into_rgba8();
    let size = ICON_SIZE * 2;
    let icon = image::imageops::thumbnail(&icon, size, size);
    iced::widget::image::Handle::from_rgba(size, size, icon.into_raw())
});

fn number_input(
    _: &iced::Theme,
    _: iced_aw::style::Status,
) -> iced_aw::style::number_input::Style {
    iced_aw::style::number_input::Style {
        button_background: None,
        icon_color: color::MUTED,
    }
}
fn output_name(output: Output) -> &'static str {
    match output {
        Output::Speaker => "Speakers",
        Output::Headphone => "Headphones",
    }
}
fn sxfi_name(mode: SxfiMode) -> &'static str {
    match mode {
        SxfiMode::Off => "Off",
        SxfiMode::On => "On",
        SxfiMode::Battle => "Battle",
    }
}
impl X4App {
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(100))
                .map(|_| Message::Tick),
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) => Some(Message::Key(key, modifiers)),
                _ => None,
            }),
        ])
    }
    pub fn demo() -> Self {
        let (tx, _) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let mut app = Self::with_channels(tx, rx);
        app.offline = true;
        let mut state: State =
            serde_json::from_str(include_str!("../fixtures/demo.json"))
                .expect("offline fixture");
        state.firmware = "Offline preview".into();
        events
            .send(Event::State(state, Completion::Refresh))
            .unwrap();
        app.events();
        app
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                self.events();
                if self
                    .confirmed
                    .as_ref()
                    .is_some_and(|(.., at)| at.elapsed() >= CONFIRMATION)
                {
                    self.confirmed = None;
                }
            }
            Message::Window(action) => return chrome::perform(action),
            Message::Key(
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                modifiers,
            ) => {
                return if modifiers.shift() {
                    iced::widget::operation::focus_previous()
                } else {
                    iced::widget::operation::focus_next()
                };
            }
            Message::Key(iced::keyboard::Key::Character(key), modifiers)
                if modifiers.control() && key.eq_ignore_ascii_case("r") =>
            {
                self.send(Command::Refresh)
            }
            Message::Key(_, _) => (),
            Message::Page(page) => self.page = page,
            Message::Command(command) => self.send(command),
            Message::EqEnabled(value) => {
                if let Some(eq) = &mut self.draft {
                    eq.enabled = value;
                    self.dirty = true;
                }
            }
            Message::Band(index, value) => {
                if let Some(eq) = &mut self.draft {
                    eq.bands_db[index] = value;
                    self.dirty = true;
                }
            }
            Message::Preamp(value) => {
                if value.is_finite()
                    && let Some(eq) = &mut self.draft
                {
                    eq.preamp_db = value;
                    self.dirty = true;
                }
            }
            Message::Apply => {
                if let (Some(context), Some(eq)) =
                    (self.draft_context, self.draft.clone())
                {
                    self.send(Command::Eq(context, eq));
                }
            }
            Message::Revert => {
                if let Some(state) = &self.state {
                    self.draft = state.eq.clone();
                    self.dirty = false;
                }
            }
            Message::Mixer(index, value) => {
                self.mixer_editing.insert(index);
                self.mixer_drafts.insert(index, value);
            }
            Message::MixerApply(index) => {
                self.mixer_editing.remove(&index);
                if let Some(value) = self.mixer_drafts.get(&index) {
                    self.send(Command::MixerLevel(index, *value));
                }
            }
        }
        Task::none()
    }
    pub fn view(&self) -> Element<'_, Message> {
        let mut page = column![].spacing(24).max_width(PAGE_WIDTH);
        if let Some(error) = &self.error {
            page =
                page.push(t3::alert(icon::CIRCLE_ALERT, error, Tone::Danger));
        }
        if let Some(notice) = &self.notice {
            page = page.push(t3::alert(
                icon::TRIANGLE_ALERT,
                notice,
                Tone::Warning,
            ));
        }
        page = match &self.state {
            Some(state) if self.page == Page::Equalizer => {
                page.push(self.device_view(state)).push(self.eq_view(state))
            }
            Some(state) => page.push(self.mixer_view(state)),
            None if self.busy() => page.push(t3::empty_state(
                t3::dots(),
                "Connecting to your X4",
                [],
            )),
            None => page.push(t3::empty_state(
                t3::icon_tile(icon::UNPLUG, 36),
                "Connect your Sound Blaster X4",
                [
                    "Plug in your device and close any other controllers.",
                    "We’ll reconnect automatically.",
                ],
            )),
        };
        let content = t3::scroll(
            container(page)
                .padding(iced::Padding::from([24.0, t3::PAGE_GUTTER]).top(8))
                .center_x(iced::Fill),
        )
        .height(iced::Fill);
        let mut main = column![self.titlebar(), content];
        if self.page == Page::Equalizer
            && let (Some(state), Some(eq)) = (&self.state, &self.draft)
        {
            main = main.push(self.apply_bar(state, eq));
        }
        chrome::frame(
            row![self.sidebar(), t3::sidebar_divider(), main],
            Message::Window,
        )
    }
    /// Controls keep their look during a write; only the one in flight shows
    /// progress. Everything else ignores input until the device answers.
    fn press(&self, allowed: bool, message: Message) -> Option<Message> {
        allowed.then(|| if self.busy() { Message::Tick } else { message })
    }
    /// [`Self::press`] for switches.
    fn on_toggle<F: Fn(bool) -> Message>(
        &self,
        allowed: bool,
        change: F,
    ) -> Option<impl Fn(bool) -> Message + use<F>> {
        let busy = self.busy();
        allowed.then_some(
            move |value| {
                if busy { Message::Tick } else { change(value) }
            },
        )
    }
    /// Dots while `control` is in flight, then a check once the X4 confirms it.
    fn progress<'a>(&self, control: Control) -> Option<Element<'a, Message>> {
        if self.in_flight(control) {
            Some(t3::dots())
        } else if self
            .confirmed
            .as_ref()
            .is_some_and(|(confirmed, ..)| *confirmed == control)
        {
            Some(
                t3::icon(icon::CIRCLE_CHECK)
                    .size(14)
                    .color(color::SUCCESS)
                    .into(),
            )
        } else {
            None
        }
    }
    /// `content` with the progress of `control` beside it.
    fn with_progress<'a>(
        &self,
        control: Control,
        content: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let mut with_status = row![].spacing(10).align_y(iced::Center);
        if let Some(progress) = self.progress(control) {
            with_status = with_status.push(progress);
        }
        with_status.push(content).into()
    }
    fn sidebar(&self) -> Element<'_, Message> {
        let header = chrome::drag_region(
            container(
                row![
                    iced::widget::image(APP_ICON.clone())
                        .width(ICON_SIZE)
                        .height(ICON_SIZE),
                    Space::new().width(4),
                    t3::text("Linuxblaster", 14).font(font::SEMIBOLD),
                    t3::text("Control", 14).color(color::MUTED),
                ]
                .spacing(5)
                .align_y(iced::Center),
            )
            .padding([0, 10])
            .center_y(44)
            .width(iced::Fill),
            Message::Window,
        );
        let nav = |page, glyph, label| {
            t3::sidebar_nav(
                label,
                glyph,
                label,
                self.page == page,
                Some(Message::Page(page)),
            )
        };
        let mut body = column![
            nav(Page::Equalizer, icon::AUDIO_LINES, "Equalizer"),
            nav(Page::Mixer, icon::SLIDERS_HORIZONTAL, "Mixer"),
        ]
        .spacing(2);
        if let Some(state) = &self.state {
            body = body.push(Space::new().height(10)).push(t3::sidebar_group(
                "Output",
                [
                    self.output_choice(
                        state,
                        Output::Headphone,
                        icon::HEADPHONES,
                    ),
                    self.output_choice(state, Output::Speaker, icon::SPEAKER),
                ],
            ));
            if !state.eq_profiles.is_empty() {
                body =
                    body.push(Space::new().height(6)).push(t3::sidebar_group(
                        "EQ profile",
                        state
                            .eq_profiles
                            .iter()
                            .map(|profile| self.profile_choice(state, profile)),
                    ));
            }
        }
        let detail = match &self.state {
            _ if self.offline => "Demo device".to_owned(),
            Some(state) => format!("Firmware {}", state.firmware),
            None => "Not connected".to_owned(),
        };
        let device = row![
            t3::icon_tile(icon::RADIO_RECEIVER, 32),
            column![
                t3::text("Sound Blaster X4", 13).font(font::MEDIUM),
                t3::muted(detail).size(11),
            ]
            .spacing(2),
        ]
        .spacing(10)
        .align_y(iced::Center);
        t3::sidebar(
            header,
            body,
            column![t3::divider(), container(device).padding([12, 8])],
        )
    }
    fn output_choice<'a>(
        &'a self,
        state: &'a State,
        output: Output,
        glyph: char,
    ) -> Element<'a, Message> {
        let selected = state.output == output;
        let control = Control::Output(output);
        t3::sidebar_choice(
            output_name(output),
            Some(glyph),
            output_name(output),
            self.progress(control)
                .unwrap_or_else(|| t3::radio(selected)),
            selected,
            self.press(
                !self.offline,
                Message::Command(Command::Output(output)),
            ),
        )
    }
    fn profile_choice<'a>(
        &'a self,
        state: &'a State,
        profile: &'a EqProfileInfo,
    ) -> Element<'a, Message> {
        let selected = state.active_eq_profile == Some(profile.index);
        let available =
            !self.offline && profile_available(state, profile.index);
        let choice = t3::sidebar_choice(
            format!("profile-{}", profile.index),
            None,
            profile.name.clone(),
            self.progress(Control::Profile(profile.index))
                .unwrap_or_else(|| t3::radio(selected)),
            selected,
            self.press(
                available,
                Message::Command(Command::Profile(
                    EqContext::from_state(state),
                    profile.index,
                )),
            ),
        );
        // Only an unavailable profile has anything to explain.
        let hint = if self.offline || available {
            return choice;
        } else if state.feature(Feature::DirectMode) {
            "Turn off Direct mode to select a profile."
        } else if profile.index == 3 {
            "Enable Super X-Fi to use this profile."
        } else {
            "Turn off Super X-Fi to use this profile."
        };
        t3::tooltip(choice, hint, t3::Position::Right)
    }
    /// What the app is doing or just finished, in words. EQ writes report in
    /// the action bar instead while it is on screen.
    fn activity_line(&self) -> Option<Element<'_, Message>> {
        let on_page =
            |control| control == Control::Eq && self.page == Page::Equalizer;
        if let Some(activity) = &self.pending {
            return (!on_page(activity.control)).then(|| {
                row![t3::dots(), t3::muted(&activity.progress)]
                    .spacing(8)
                    .align_y(iced::Center)
                    .into()
            });
        }
        let (control, done, _) = self.confirmed.as_ref()?;
        (!on_page(*control)).then(|| {
            row![
                t3::icon(icon::CIRCLE_CHECK).size(14).color(color::SUCCESS),
                t3::text(done, 12).color(color::SUCCESS),
            ]
            .spacing(6)
            .align_y(iced::Center)
            .into()
        })
    }
    fn titlebar(&self) -> Element<'_, Message> {
        let (glyph, title) = match self.page {
            Page::Equalizer => (icon::AUDIO_LINES, "Equalizer"),
            Page::Mixer => (icon::SLIDERS_HORIZONTAL, "Mixer"),
        };
        let mut leading =
            row![t3::breadcrumb(glyph, ["Sound Blaster X4"], title)]
                .spacing(16)
                .align_y(iced::Center);
        if let Some(line) = self.activity_line() {
            leading = leading.push(line);
        }
        let refresh = if self.in_flight(Control::Refresh) {
            container(t3::spinner()).center_x(26).center_y(26).into()
        } else {
            t3::tooltip(
                t3::icon_button(
                    "refresh",
                    icon::REFRESH_CW,
                    self.press(
                        !self.offline,
                        Message::Command(Command::Refresh),
                    ),
                ),
                "Refresh · Ctrl+R",
                t3::Position::Bottom,
            )
        };
        chrome::bar(leading, Some(refresh), Message::Window)
    }
    fn device_view(&self, state: &State) -> Element<'_, Message> {
        let context = EqContext::from_state(state);
        let sxfi_allowed = !self.offline
            && state.sxfi.is_some()
            && state.output == Output::Headphone
            && !context.direct;
        let modes =
            [SxfiMode::Off, SxfiMode::On, SxfiMode::Battle].map(|mode| {
                let label = sxfi_name(mode);
                t3::segment(
                    label,
                    label,
                    state.sxfi == Some(mode),
                    self.press(
                        sxfi_allowed,
                        Message::Command(Command::Sxfi(context, mode)),
                    ),
                )
            });
        let sxfi_detail = if context.direct {
            t3::warning("Turn off Direct mode to use Super X-Fi.")
        } else if state.output == Output::Speaker {
            t3::warning("Switch to headphones to use Super X-Fi.")
        } else {
            t3::muted("Spatial audio for headphones.")
        };
        let feature_switch = |id, feature, allowed| {
            self.with_progress(
                Control::Feature(feature),
                t3::switch(
                    id,
                    None,
                    state.feature(feature),
                    self.on_toggle(allowed, move |value| {
                        Message::Command(Command::Feature(feature, value))
                    }),
                ),
            )
        };
        t3::section(
            t3::section_title("Device"),
            None,
            [
                t3::setting(
                    "Super X-Fi",
                    sxfi_detail,
                    self.with_progress(Control::Sxfi, t3::segmented(modes)),
                ),
                t3::setting(
                    "Direct mode",
                    if context.direct {
                        t3::warning("EQ and Super X-Fi are bypassed.")
                    } else {
                        t3::muted("Bypass EQ and Super X-Fi.")
                    },
                    feature_switch(
                        "direct-mode",
                        Feature::DirectMode,
                        !self.offline
                            && state.writable_feature(Feature::DirectMode),
                    ),
                ),
                t3::setting(
                    "High headphone gain",
                    t3::muted("Available on headphone output."),
                    feature_switch(
                        "headphone-gain",
                        Feature::HeadphoneHighGain,
                        !self.offline
                            && state
                                .writable_feature(Feature::HeadphoneHighGain)
                            && state.output == Output::Headphone,
                    ),
                ),
            ],
        )
    }
    fn eq_view<'a>(&'a self, state: &'a State) -> Element<'a, Message> {
        let enabled = !state.feature(Feature::DirectMode);
        let busy = self.busy();
        let band = move |index| {
            move |value| {
                if enabled && !busy {
                    Message::Band(index, value)
                } else {
                    Message::Tick
                }
            }
        };
        let preamp = move |value| {
            if enabled && !busy {
                Message::Preamp(value)
            } else {
                Message::Tick
            }
        };
        // These change which curve is on the device, so the one shown is stale.
        let reloading = self.pending.as_ref().is_some_and(|a| {
            matches!(
                a.control,
                Control::Profile(_)
                    | Control::Output(_)
                    | Control::Sxfi
                    | Control::Feature(Feature::DirectMode)
            )
        });
        let mut rows = Vec::new();
        if let Some(eq) = &self.draft {
            let mut bands = row![];
            for (index, label) in [
                "31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz",
                "2 kHz", "4 kHz", "8 kHz", "16 kHz",
            ]
            .iter()
            .enumerate()
            {
                let fader = iced::widget::vertical_slider(
                    -12.0..=12.0,
                    eq.bands_db[index],
                    band(index),
                )
                .step(0.1)
                .shift_step(1.0_f32)
                .default(0.0_f32)
                .height(96)
                .width(18)
                .style(style::slider(enabled));
                let value = iced_aw::NumberInput::new(
                    &eq.bands_db[index],
                    -12.0..=12.0,
                    band(index),
                )
                .on_input_maybe(enabled.then_some(band(index)))
                .ignore_buttons(true)
                .ignore_scroll(true)
                .step(0.1)
                .width(iced::Fill)
                .padding([5, 2])
                .set_size(11)
                .line_height(t3::line_height(11))
                .align_x(iced::Center)
                .font(font::REGULAR)
                .style(number_input)
                .input_style(style::input_quiet);
                // No row spacing: the curve centres its points in equal tenths.
                bands = bands.push(
                    container(
                        column![t3::muted(*label).size(11), fader, value]
                            .align_x(iced::Center)
                            .spacing(8),
                    )
                    .padding([0, 3])
                    .width(iced::Fill),
                );
            }
            rows.push(
                container(
                    column![
                        iced::widget::canvas(crate::curve::Curve(
                            eq.bands_db,
                            eq.enabled
                                && !state.feature(Feature::DirectMode)
                                && !reloading,
                        ))
                        .width(iced::Fill)
                        .height(196),
                        bands,
                    ]
                    .spacing(12),
                )
                .padding([14, 16])
                .into(),
            );
            // Hide floating-point readback noise without changing the EQ draft.
            let preamp_display_db = (eq.preamp_db * 100.0).round() / 100.0;
            rows.push(t3::setting(
                "Preamp",
                t3::muted("Overall EQ gain."),
                row![
                    slider(-12.0..=12.0, eq.preamp_db, preamp)
                        .step(0.1_f32)
                        .shift_step(1.0_f32)
                        .default(0.0_f32)
                        .width(240)
                        .height(22)
                        .style(style::slider(enabled)),
                    iced_aw::NumberInput::new(
                        &preamp_display_db,
                        -12.0..=12.0,
                        preamp
                    )
                    .on_input_maybe(enabled.then_some(preamp))
                    .ignore_buttons(true)
                    .ignore_scroll(true)
                    .step(0.1)
                    .width(64)
                    .set_size(12)
                    .line_height(t3::line_height(12))
                    .padding([6, 8])
                    .style(number_input)
                    .input_style(style::input),
                    t3::muted("dB"),
                ]
                .spacing(12)
                .align_y(iced::Center),
            ));
        } else {
            rows.push(t3::setting(
                "Bands",
                t3::muted("EQ is unavailable in the current device state."),
                Space::new(),
            ));
        }
        t3::section(
            match active_profile(state) {
                Some(profile) => {
                    t3::section_heading("Equalizer", &profile.name)
                }
                None => t3::section_title("Equalizer").into(),
            },
            Some(t3::switch(
                "eq-enabled",
                Some("Enabled"),
                self.draft.as_ref().is_some_and(|e| e.enabled),
                self.on_toggle(
                    enabled && self.draft.is_some(),
                    Message::EqEnabled,
                ),
            )),
            rows,
        )
    }
    /// EQ edits are drafts; the bar keeps their state and Apply in view while
    /// the page scrolls.
    fn apply_bar<'a>(
        &'a self,
        state: &'a State,
        eq: &'a Equalizer,
    ) -> Element<'a, Message> {
        let saving = self.in_flight(Control::Eq);
        let saved = self
            .confirmed
            .as_ref()
            .filter(|(control, ..)| *control == Control::Eq);
        let (status, tint) =
            if let Some(activity) = self.pending.as_ref().filter(|_| saving) {
                (activity.progress.as_str(), color::MUTED)
            } else if self.dirty {
                ("Unapplied changes", color::WARNING)
            } else if let Some((_, done, _)) = saved {
                (done.as_str(), color::SUCCESS)
            } else if self.busy() {
                ("Waiting for the device", color::MUTED)
            } else if self.offline {
                ("Preview · device writes disabled", color::MUTED)
            } else {
                ("Synced with your device", color::MUTED)
            };
        let marker: Element<'a, Message> = if saving {
            t3::dots()
        } else if self.dirty {
            t3::icon(icon::INFO).size(14).color(tint).into()
        } else if self.busy() {
            t3::icon(icon::CLOCK_4).size(14).color(tint).into()
        } else {
            t3::icon(icon::CIRCLE_CHECK).size(14).color(tint).into()
        };
        let can_apply = !self.busy()
            && !state.feature(Feature::DirectMode)
            && !self.offline
            && self.dirty
            && eq.validate().is_ok();
        t3::composer(
            row![
                marker,
                t3::text(status, 13).color(tint).width(iced::Fill),
                t3::ghost(
                    icon::UNDO_2,
                    "Revert",
                    (self.dirty && !self.busy()).then_some(Message::Revert),
                ),
                if saving {
                    t3::primary_busy("Apply EQ")
                } else {
                    t3::primary("Apply EQ", can_apply.then_some(Message::Apply))
                },
            ]
            .spacing(8)
            .align_y(iced::Center),
            PAGE_WIDTH,
            self.dirty,
        )
    }
    fn mixer_view<'a>(&'a self, state: &'a State) -> Element<'a, Message> {
        let mut groups = column![].spacing(24);
        for (group, title, detail) in [
            (MixerGroup::Playback, "Playback", Some("What you hear")),
            (
                MixerGroup::Monitoring,
                "Monitoring",
                Some("Inputs heard directly through the X4"),
            ),
            (
                MixerGroup::Recording,
                "Recording",
                Some("Inputs sent to recording applications"),
            ),
            (MixerGroup::Other, "Other controls", None),
        ] {
            let rows: Vec<_> = state
                .mixer
                .iter()
                .filter(|c| c.group == group)
                .map(|control| self.mixer_row(control))
                .collect();
            if rows.is_empty() {
                continue;
            }
            let heading = match detail {
                Some(detail) => t3::section_heading(title, detail),
                None => t3::section_title(title).into(),
            };
            groups = groups.push(t3::section(heading, None, rows));
        }
        groups.into()
    }
    fn mixer_row<'a>(
        &'a self,
        control: &'a MixerControl,
    ) -> Element<'a, Message> {
        let index = control.index;
        let boost = control.is_microphone_boost();
        let enabled = !self.offline;
        let busy = self.busy();
        let trailing: Element<'a, Message> = if boost {
            match control.microphone_boost {
                Some(value) => t3::switch(
                    "microphone-boost",
                    Some("Enabled"),
                    value,
                    self.on_toggle(enabled, |v| {
                        Message::Command(Command::MicrophoneBoost(v))
                    }),
                ),
                None => Space::new().into(),
            }
        } else if let Some(muted) = control.muted {
            let available = mixer_mute_available(control);
            let name = &control.name;
            t3::tooltip(
                t3::icon_toggle(
                    format!("mute-{index}"),
                    if muted {
                        icon::VOLUME_X
                    } else {
                        icon::VOLUME_2
                    },
                    muted,
                    self.press(
                        enabled && available,
                        Message::Command(Command::MixerMute(index, !muted)),
                    ),
                ),
                if !available {
                    "Mute unavailable".to_owned()
                } else if muted {
                    format!("Unmute {name}")
                } else {
                    format!("Mute {name}")
                },
                t3::Position::Left,
            )
        } else {
            Space::new().into()
        };
        let (Some(range), Some(actual)) = (&control.range, control.level_db)
        else {
            let trailing = self.with_progress(
                if boost {
                    Control::MicrophoneBoost
                } else {
                    Control::MixerMute(index)
                },
                trailing,
            );
            return if !boost {
                t3::setting(
                    &control.name,
                    t3::muted("No adjustable level"),
                    trailing,
                )
            } else if control.microphone_boost.is_none() {
                t3::setting(
                    &control.name,
                    t3::muted("Boost state unavailable"),
                    trailing,
                )
            } else {
                t3::setting_titled(&control.name, trailing)
            };
        };
        let value = self.mixer_drafts.get(&index).copied().unwrap_or(actual);
        let changed = (value - actual).abs() > 0.001;
        // This slot keeps columns aligned: Apply for a pending level, otherwise
        // the progress of this row's level or mute.
        let level = Control::MixerLevel(index);
        let action: Element<'a, Message> = if self.in_flight(level) {
            t3::primary_small_busy("Apply")
        } else if changed {
            t3::primary_small(
                "Apply",
                self.press(enabled, Message::MixerApply(index)),
            )
        } else {
            self.progress(level)
                .or_else(|| self.progress(Control::MixerMute(index)))
                .unwrap_or_else(|| Space::new().into())
        };
        container(
            row![
                column![
                    t3::text(&control.name, 14).font(font::MEDIUM),
                    t3::muted(format!(
                        "{:.0} to {:.0} dB",
                        range.min_db, range.max_db
                    ))
                ]
                .spacing(2)
                .width(136),
                slider(range.min_db..=range.max_db, value, move |v| {
                    if enabled && !busy {
                        Message::Mixer(index, v)
                    } else {
                        Message::Tick
                    }
                })
                .step(range.step_db.max(0.01))
                .height(22)
                .style(style::slider(enabled)),
                container(t3::text(format!("{value:.1} dB"), 12).color(
                    if changed {
                        color::WARNING
                    } else {
                        color::FOREGROUND
                    }
                ))
                .align_right(60),
                container(action).align_right(54),
                container(trailing).align_right(if boost { 96 } else { 28 }),
            ]
            .spacing(12)
            .align_y(iced::Center),
        )
        .padding([12, 16])
        .width(iced::Fill)
        .into()
    }
}

fn active_profile(state: &State) -> Option<&EqProfileInfo> {
    state
        .eq_profiles
        .iter()
        .find(|p| state.active_eq_profile == Some(p.index))
}

#[cfg(test)]
mod iced_tests {
    use super::*;
    use iced::advanced::{
        Layout, layout,
        renderer::{Headless, Style},
        widget::Tree,
    };
    #[test]
    fn offline_render_and_state_flows() {
        iced::advanced::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(iced_aw::ICED_AW_FONT_BYTES.into());
        for bytes in font::ALL {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes.into());
        }
        let mut app = X4App::demo();
        assert!(app.offline);
        let confirmed = app.state.as_ref().unwrap().eq.clone();
        let _ = app.update(Message::Band(2, 3.0));
        assert!(app.dirty);
        assert_eq!(app.state.as_ref().unwrap().eq, confirmed);
        let _ = app.update(Message::Revert);
        assert!(!app.dirty);
        assert_eq!(app.draft, confirmed);
        let fixture = app.state.clone();
        for (page, name) in [
            (Page::Equalizer, "equalizer"),
            (Page::Mixer, "mixer"),
            (Page::Equalizer, "pending"),
            (Page::Equalizer, "disconnected"),
            (Page::Equalizer, "direct"),
            (Page::Equalizer, "loading"),
            (Page::Equalizer, "confirmed"),
            (Page::Equalizer, "saving"),
            (Page::Mixer, "applying"),
        ] {
            app.page = page;
            app.state = fixture.clone();
            app.offline = matches!(name, "equalizer" | "mixer");
            app.dirty = name == "pending";
            app.notice = (name == "pending")
                .then(|| "The output, sound mode or profile changed.".into());
            app.error = None;
            let activity = |control, progress: &str| Activity {
                control,
                progress: progress.into(),
                done: String::new(),
            };
            app.pending = match name {
                "loading" => {
                    Some(activity(Control::Profile(1), "Loading Movie…"))
                }
                "saving" => Some(activity(Control::Eq, "Writing EQ to Music…")),
                "applying" => {
                    app.mixer_drafts.insert(0, -20.0);
                    let mixer = &mut app.state.as_mut().unwrap().mixer;
                    mixer[1].muted = Some(true);
                    Some(activity(
                        Control::MixerLevel(0),
                        "Setting Headphones to -20.0 dB…",
                    ))
                }
                _ => None,
            };
            app.confirmed = (name == "confirmed").then(|| {
                (Control::Profile(0), "Music loaded".into(), Instant::now())
            });
            if name == "disconnected" {
                app.state = None;
                app.error = Some("No Sound Blaster X4 found.".into());
            }
            if name == "direct" {
                app.state.as_mut().unwrap().features |= 1 << 5;
            }
            for (width, height) in [(1120, 900), (860, 640), (860, 1240)] {
                let mut renderer =
                    pollster::block_on(<iced::Renderer as Headless>::new(
                        font::REGULAR,
                        14.0.into(),
                        Some("tiny-skia"),
                    ))
                    .expect("software renderer");
                let mut view = app.view();
                let mut tree = Tree::new(&view);
                let size = iced::Size::new(width as f32, height as f32);
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(size, size),
                );
                let theme = iced_t3::theme();
                let mut messages = Vec::new();
                view.as_widget_mut().update(
                    &mut tree,
                    &iced::Event::Window(iced::window::Event::RedrawRequested(
                        std::time::Instant::now(),
                    )),
                    Layout::new(&node),
                    iced::mouse::Cursor::Unavailable,
                    &renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut iced::advanced::Shell::new(&mut messages),
                    &iced::Rectangle::with_size(size),
                );
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &Style {
                        text_color: theme.palette().text,
                    },
                    Layout::new(&node),
                    iced::mouse::Cursor::Unavailable,
                    &iced::Rectangle::with_size(size),
                );
                let pixels = renderer.screenshot(
                    iced::Size::new(width, height),
                    1.0,
                    theme.palette().background,
                );
                assert_eq!(pixels.len(), (width * height * 4) as usize);
                if let Ok(directory) = std::env::var("X4_UI_CAPTURE_DIR") {
                    std::fs::create_dir_all(&directory).unwrap();
                    image::save_buffer(
                        format!("{directory}/{name}-{width}x{height}.png"),
                        &pixels,
                        width,
                        height,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        }
    }
    /// Heights of the step buttons `NumberInput` lays out even with
    /// `ignore_buttons`, which skips drawing them.
    const HIDDEN_STEPPERS: [f32; 3] = [8.9375, 9.75, 20.5];
    fn off_grid(layout: Layout<'_>, found: &mut Vec<iced::Rectangle>) {
        let bounds = layout.bounds();
        if (bounds.y.fract() != 0.0 || bounds.height.fract() != 0.0)
            && !HIDDEN_STEPPERS.contains(&bounds.height)
        {
            found.push(bounds);
        }
        for child in layout.children() {
            off_grid(child, found);
        }
    }
    /// Iced truncates fractional positions, so one off-grid box puts everything
    /// inside it out of line.
    #[test]
    fn every_box_sits_on_the_pixel_grid_vertically() {
        for bytes in font::ALL {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes.into());
        }
        let renderer = pollster::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            14.0.into(),
            Some("tiny-skia"),
        ))
        .expect("software renderer");
        let mut app = X4App::demo();
        let fixture = app.state.clone();
        for page in [Page::Equalizer, Page::Mixer] {
            for connected in [true, false] {
                app.page = page;
                app.state = if connected { fixture.clone() } else { None };
                for (width, height) in [(1120.0, 900.0), (860.0, 640.0)] {
                    let size = iced::Size::new(width, height);
                    let mut view = app.view();
                    let mut tree = Tree::new(&view);
                    let node = view.as_widget_mut().layout(
                        &mut tree,
                        &renderer,
                        &layout::Limits::new(size, size),
                    );
                    let mut found = Vec::new();
                    off_grid(Layout::new(&node), &mut found);
                    assert!(
                        found.is_empty(),
                        "{page:?} at {width}x{height}: {found:#?}"
                    );
                }
            }
        }
    }
    /// Clicks down the column of mute icons and returns the first mute sent.
    fn click_mute(app: &X4App) -> Option<(u8, bool)> {
        let renderer = pollster::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            14.0.into(),
            Some("tiny-skia"),
        ))
        .expect("software renderer");
        let size = iced::Size::new(1120.0, 900.0);
        let mut view = app.view();
        let mut tree = Tree::new(&view);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(size, size),
        );
        let x = 1120.0 - t3::PAGE_GUTTER - 16.0 - 13.0;
        let mut messages = Vec::new();
        for y in (60..400).step_by(6) {
            let position = iced::Point::new(x, y as f32);
            for event in [
                iced::mouse::Event::CursorMoved { position },
                iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left),
                iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left),
            ] {
                view.as_widget_mut().update(
                    &mut tree,
                    &iced::Event::Mouse(event),
                    Layout::new(&node),
                    iced::mouse::Cursor::Available(position),
                    &renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut iced::advanced::Shell::new(&mut messages),
                    &iced::Rectangle::with_size(size),
                );
            }
        }
        messages.into_iter().find_map(|message| match message {
            Message::Command(Command::MixerMute(index, muted)) => {
                Some((index, muted))
            }
            _ => None,
        })
    }
    #[test]
    fn mute_icon_asks_for_the_opposite_of_the_confirmed_state() {
        let (mut app, events, _commands) = tests::app();
        app.page = Page::Mixer;
        for muted in [false, true] {
            let mut state = tests::state();
            state.mixer[0].muted = Some(muted);
            events
                .send(Event::State(state, Completion::Refresh))
                .unwrap();
            app.events();
            assert_eq!(click_mute(&app), Some((1, !muted)));
        }
    }
    #[test]
    fn failed_operation_discards_unconfirmed_state() {
        let (mut app, events, _) = tests::app();
        events
            .send(Event::State(tests::state(), Completion::Refresh))
            .unwrap();
        app.events();
        let _ = app.update(Message::Band(0, 5.0));
        events.send(Event::Error("Write failed".into())).unwrap();
        app.events();
        assert!(app.state.is_none());
        assert!(app.draft.is_none());
        assert!(!app.busy());
        assert!(app.error.is_some());
    }
}
