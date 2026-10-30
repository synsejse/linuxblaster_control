use crate::style;
use iced::widget::{
    Space, button, column, container, row, scrollable, slider, text,
};
use iced::{Element, Subscription, Task};
use soundblaster_x4::{
    Equalizer, Feature, Output, State, SxfiMode,
    mixer::{MixerControl, MixerGroup},
    open,
    protocol::invalid,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
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
    busy: bool,
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
            busy: false,
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
        if self.busy {
            return;
        }
        match self.tx.send(command) {
            Ok(()) => {
                self.busy = true;
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
                    if !matches!(completed, Completion::Poll) {
                        self.busy = false;
                    }
                }
                Event::Error(error) => {
                    self.error = Some(error);
                    self.state = None;
                    self.busy = false;
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
    use soundblaster_x4::{EqProfileInfo, mixer::MixerRange};

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
        assert!(app.busy);
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
        assert!(!app.busy);
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
fn action(
    label: impl Into<String>,
    message: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let label = label.into();
    let message = enabled.then_some(message);
    crate::keyboard_button::wrap(
        label.clone(),
        button(text(label).font(style::SEMIBOLD).size(12))
            .padding([9, 13])
            .style(style::primary)
            .on_press_maybe(message.clone()),
        message,
    )
}

fn choice(
    label: impl Into<String>,
    message: Message,
    selected: bool,
    enabled: bool,
) -> Element<'static, Message> {
    let label = label.into();
    let message = enabled.then_some(message);
    crate::keyboard_button::wrap(
        label.clone(),
        button(text(label).font(style::SEMIBOLD).size(12))
            .padding([9, 13])
            .style(style::choice(selected))
            .on_press_maybe(message.clone()),
        message,
    )
}
fn icon(glyph: char) -> iced::widget::Text<'static> {
    text(glyph.to_string()).font(style::ICONS).size(16)
}
fn eyebrow(label: &'static str) -> iced::widget::Text<'static> {
    text(label)
        .font(style::SEMIBOLD)
        .size(10)
        .color(style::MUTED)
}
fn divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .height(1)
        .width(iced::Fill)
        .style(style::divider)
        .into()
}
fn secondary(
    label: &'static str,
    glyph: char,
    message: Message,
    enabled: bool,
) -> Element<'static, Message> {
    let message = enabled.then_some(message);
    crate::keyboard_button::wrap(
        label,
        button(
            row![icon(glyph).size(13), text(label).size(12)]
                .spacing(7)
                .align_y(iced::Center),
        )
        .padding([9, 12])
        .style(style::secondary)
        .on_press_maybe(message.clone()),
        message,
    )
}
fn toggle(
    label: &'static str,
    value: bool,
    change: impl Fn(bool) -> Message + 'static,
    enabled: bool,
) -> Element<'static, Message> {
    toggle_id(label, label, value, change, enabled)
}
fn toggle_id(
    id: impl Into<iced::advanced::widget::Id>,
    label: &'static str,
    value: bool,
    change: impl Fn(bool) -> Message + 'static,
    enabled: bool,
) -> Element<'static, Message> {
    let message = enabled.then(|| change(!value));
    crate::keyboard_button::wrap(
        id,
        iced::widget::toggler(value)
            .label(label)
            .size(17)
            .text_size(12)
            .spacing(9)
            .on_toggle_maybe(enabled.then_some(change)),
        message,
    )
}

fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(20)
        .width(iced::Fill)
        .style(style::card)
        .into()
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
            Message::Tick => self.events(),
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
        let connection = if self.offline {
            "Demo device"
        } else if self.state.is_some() {
            "Connected via USB"
        } else {
            "Not connected"
        };
        let device_tile = container(
            row![
                container(icon('\u{e0f1}').size(22).color(style::ACCENT))
                    .center_x(40)
                    .center_y(40)
                    .style(style::chip),
                column![
                    text("Sound Blaster X4").font(style::SEMIBOLD).size(13),
                    text(connection).size(11).color(style::MUTED)
                ]
                .spacing(5),
            ]
            .spacing(10)
            .align_y(iced::Center),
        )
        .padding(12)
        .width(iced::Fill)
        .style(style::well);
        let nav = |page, glyph, label| {
            let message = Message::Page(page);
            crate::keyboard_button::wrap(
                label,
                button(
                    row![
                        icon(glyph),
                        text(label).size(13),
                        Space::new().width(iced::Fill),
                        icon('\u{e06f}').size(13)
                    ]
                    .spacing(10)
                    .align_y(iced::Center),
                )
                .width(iced::Fill)
                .padding([11, 12])
                .style(style::choice(self.page == page))
                .on_press(message.clone()),
                Some(message),
            )
        };
        let mut footer = column![
            divider(),
            text("SETTINGS ON DEVICE")
                .size(9)
                .font(style::SEMIBOLD)
                .color(style::MUTED),
            text("Changes stay with your X4.")
                .size(11)
                .color(style::MUTED)
        ]
        .spacing(10);
        if let Some(state) = &self.state
            && !self.offline
        {
            footer = footer.push(
                text(format!("Firmware {}", state.firmware))
                    .size(10)
                    .color(style::MUTED),
            );
        }
        let sidebar = container(
            column![
                row![
                    container(
                        text("X4")
                            .size(16)
                            .font(style::SEMIBOLD)
                            .color(style::ACCENT)
                    )
                    .center_x(34)
                    .center_y(34)
                    .style(style::chip),
                    text("Linuxblaster").font(style::SEMIBOLD).size(16)
                ]
                .spacing(10)
                .align_y(iced::Center),
                Space::new().height(16),
                text("DEVICE")
                    .size(10)
                    .font(style::SEMIBOLD)
                    .color(style::MUTED),
                device_tile,
                Space::new().height(16),
                text("CONTROLS")
                    .size(10)
                    .font(style::SEMIBOLD)
                    .color(style::MUTED),
                nav(Page::Equalizer, '\u{e0f1}', "Equalizer"),
                nav(Page::Mixer, '\u{e154}', "Mixer"),
                Space::new().height(iced::Fill),
                footer,
            ]
            .spacing(9),
        )
        .padding([24, 14])
        .width(218)
        .height(iced::Fill)
        .style(style::sidebar);
        let (title, subtitle) = match self.page {
            Page::Equalizer => (
                "Sound controls",
                "Shape your sound with the X4’s onboard equalizer.",
            ),
            Page::Mixer => (
                "Mixer",
                "Balance playback, direct monitoring and recording.",
            ),
        };
        let badge = if self.offline {
            "OFFLINE DEMO"
        } else if self.busy {
            "UPDATING"
        } else if self.state.is_some() {
            "LIVE"
        } else {
            "DISCONNECTED"
        };
        let mut body = column![
            row![
                column![
                    row![
                        text(title).font(style::SEMIBOLD).size(25),
                        container(text(badge).size(9).font(style::SEMIBOLD))
                            .padding([4, 7])
                            .style(if self.offline || self.state.is_none() {
                                style::neutral_chip
                            } else if self.busy {
                                style::pending
                            } else {
                                style::chip
                            })
                    ]
                    .spacing(12)
                    .align_y(iced::Center),
                    text(subtitle).size(12).color(style::MUTED)
                ]
                .spacing(7),
                Space::new().width(iced::Fill),
                secondary(
                    "Refresh",
                    '\u{e145}',
                    Message::Command(Command::Refresh),
                    !self.busy && !self.offline
                ),
            ]
            .spacing(12)
            .align_y(iced::Center)
        ]
        .spacing(18);
        if let Some(error) = &self.error {
            body = body.push(
                container(
                    row![icon('\u{e077}'), text(error).size(12)].spacing(10),
                )
                .padding(12)
                .width(iced::Fill)
                .style(style::error),
            );
        }
        if let Some(notice) = &self.notice {
            body = body.push(
                container(
                    row![icon('\u{e0f9}').size(14), text(notice).size(12)]
                        .spacing(10),
                )
                .padding([10, 12])
                .width(iced::Fill)
                .style(style::pending),
            );
        }
        if let Some(state) = &self.state {
            if self.page == Page::Equalizer {
                body = body
                    .push(self.device_view(state))
                    .push(self.eq_view(state));
            } else {
                body = body.push(self.mixer_view(state));
            }
        } else {
            body = body.push(container(column![
                container(icon('\u{e37f}').size(32).color(style::MUTED)).center_x(72).center_y(72).style(style::well),
                text("Connect your Sound Blaster X4").size(21).font(style::SEMIBOLD).align_x(iced::Center),
                text("Plug in your device and close any other controllers.").size(13).color(style::MUTED).align_x(iced::Center),
                text("We’ll reconnect automatically. Your settings won’t be replayed.").size(12).color(style::MUTED).align_x(iced::Center),
            ].spacing(16).align_x(iced::Center)).padding([70,20]).center_x(iced::Fill).style(style::card));
        }
        row![
            sidebar,
            scrollable(
                container(body.max_width(1060))
                    .padding(24)
                    .center_x(iced::Fill)
            )
            .height(iced::Fill)
        ]
        .into()
    }
    fn device_view(&self, state: &State) -> Element<'_, Message> {
        let enabled = !self.busy && !self.offline;
        let context = EqContext::from_state(state);
        let mut output = row![].spacing(6);
        for (value, label) in [
            (Output::Speaker, "Speakers"),
            (Output::Headphone, "Headphones"),
        ] {
            output = output.push(choice(
                label,
                Message::Command(Command::Output(value)),
                state.output == value,
                enabled,
            ));
        }
        let mut modes = row![].spacing(6);
        for (value, label) in [
            (SxfiMode::Off, "Off"),
            (SxfiMode::On, "On"),
            (SxfiMode::Battle, "Battle"),
        ] {
            modes = modes.push(choice(
                label,
                Message::Command(Command::Sxfi(context, value)),
                state.sxfi == Some(value),
                enabled
                    && state.sxfi.is_some()
                    && state.output == Output::Headphone
                    && !context.direct,
            ));
        }
        let mut body = column![
            row![
                column![eyebrow("OUTPUT"), output]
                    .spacing(10)
                    .width(iced::Fill),
                column![eyebrow("SUPER X-FI"), modes]
                    .spacing(10)
                    .width(iced::Fill)
            ]
            .spacing(22),
            divider(),
            row![
                toggle(
                    "Direct mode",
                    state.feature(Feature::DirectMode),
                    |v| Message::Command(Command::Feature(
                        Feature::DirectMode,
                        v
                    )),
                    enabled && state.writable_feature(Feature::DirectMode)
                ),
                Space::new().width(iced::Fill),
                toggle(
                    "High headphone gain",
                    state.feature(Feature::HeadphoneHighGain),
                    |v| {
                        Message::Command(Command::Feature(
                            Feature::HeadphoneHighGain,
                            v,
                        ))
                    },
                    enabled
                        && state.writable_feature(Feature::HeadphoneHighGain)
                        && state.output == Output::Headphone
                )
            ]
            .spacing(16)
            .align_y(iced::Center),
        ]
        .spacing(16);
        if context.direct || state.output == Output::Speaker {
            body = body.push(
                text(if context.direct {
                    "Direct mode bypasses EQ and Super X-Fi."
                } else {
                    "Switch to headphones to use Super X-Fi."
                })
                .size(12)
                .color(style::WARNING),
            );
        }
        card(body)
    }
    fn eq_view<'a>(&'a self, state: &'a State) -> Element<'a, Message> {
        let enabled = !self.busy && !state.feature(Feature::DirectMode);
        let context = EqContext::from_state(state);
        let mut profiles = row![].spacing(6);
        for p in &state.eq_profiles {
            let selected = state.active_eq_profile == Some(p.index);
            let active =
                enabled && !self.offline && profile_available(state, p.index);
            let message = active.then_some(Message::Command(Command::Profile(
                context, p.index,
            )));
            let control = button(
                row![
                    text(format!("{:02}", p.index + 1)).size(10).color(
                        if selected {
                            style::ACCENT
                        } else {
                            style::MUTED
                        }
                    ),
                    text(&p.name).size(12)
                ]
                .spacing(8)
                .height(iced::Fill)
                .align_y(iced::Center),
            )
            .padding([10, 12])
            .width(iced::Fill)
            .height(48)
            .style(style::choice(selected))
            .on_press_maybe(message.clone());
            let control = crate::keyboard_button::wrap(
                format!("profile-{}", p.index),
                control,
                message,
            );
            let hint = if self.offline {
                "Preview only; device writes are disabled."
            } else if context.direct {
                "Turn off Direct mode to select a profile."
            } else if self.busy {
                "Waiting for the device."
            } else if !profile_available(state, p.index) {
                if p.index == 3 {
                    "Enable Super X-Fi to use this profile."
                } else {
                    "Turn off Super X-Fi to use this profile."
                }
            } else {
                "Load this profile from the X4."
            };
            profiles = profiles.push(
                iced::widget::tooltip(
                    control,
                    container(text(format!("{} · {hint}", p.name)).size(12))
                        .padding(10)
                        .style(style::well),
                    iced::widget::tooltip::Position::Bottom,
                )
                .gap(8),
            );
        }
        let mut body = column![
            row![
                icon('\u{e154}').color(style::ACCENT),
                column![
                    text("Equalizer").font(style::SEMIBOLD).size(17),
                    text("10 bands · profiles stored on your X4")
                        .size(11)
                        .color(style::MUTED)
                ]
                .spacing(4),
                Space::new().width(iced::Fill),
                toggle(
                    "Enabled",
                    self.draft.as_ref().is_some_and(|e| e.enabled),
                    Message::EqEnabled,
                    enabled && self.draft.is_some()
                )
            ]
            .spacing(12)
            .align_y(iced::Center),
            profiles,
        ]
        .spacing(14);
        if let Some(eq) = &self.draft {
            body = body.push(
                container(
                    iced::widget::canvas(crate::curve::Curve(
                        eq.bands_db,
                        eq.enabled && !state.feature(Feature::DirectMode),
                    ))
                    .width(iced::Fill)
                    .height(200),
                )
                .padding([8, 10])
                .style(style::well),
            );
            let mut bands = row![].spacing(6);
            for (index, label) in [
                "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
            ]
            .iter()
            .enumerate()
            {
                let fader = iced::widget::vertical_slider(
                    -12.0..=12.0,
                    eq.bands_db[index],
                    move |v| {
                        if enabled {
                            Message::Band(index, v)
                        } else {
                            Message::Tick
                        }
                    },
                )
                .step(0.1)
                .shift_step(1.0_f32)
                .default(0.0_f32)
                .height(80)
                .width(24)
                .style(style::fader(enabled));
                let value = iced_aw::NumberInput::new(
                    &eq.bands_db[index],
                    -12.0..=12.0,
                    move |v| Message::Band(index, v),
                )
                .on_input_maybe(
                    enabled.then_some(move |v| Message::Band(index, v)),
                )
                .ignore_buttons(true)
                .ignore_scroll(true)
                .step(0.1)
                .width(iced::Fill)
                .padding([6, 2])
                .set_size(11)
                .align_x(iced::Center)
                .font(style::REGULAR)
                .style(style::number_input)
                .input_style(style::input);
                bands = bands.push(
                    column![
                        text(*label).size(11).color(style::MUTED),
                        fader,
                        value
                    ]
                    .align_x(iced::Center)
                    .spacing(9)
                    .width(iced::Fill),
                );
            }
            body = body.push(bands).push(row![
                text("Frequency · Hz").size(10).color(style::MUTED),
                Space::new().width(iced::Fill),
                text("Gain · dB").size(10).color(style::MUTED)
            ]);
            let preamp = iced_audio::HSlider::new(
                iced_audio::NormalParam::new((eq.preamp_db + 12.0) / 24.0, 0.5),
            )
            .enabled(enabled)
            .height(iced::Length::Fixed(8.0))
            .style(style::AudioSlider)
            .config(&iced_audio::virtual_slider::Config {
                wheel_scalar: 0.0,
                ..Default::default()
            })
            .on_gesture(|gesture| {
                Message::Preamp(
                    gesture
                        .new_normal()
                        .map(|n| n.as_f32() * 24.0 - 12.0)
                        .unwrap_or(f32::NAN),
                )
            });
            // Hide floating-point readback noise without changing the EQ draft.
            let preamp_display_db = (eq.preamp_db * 100.0).round() / 100.0;
            body = body.push(divider()).push(
                row![
                    column![
                        text("Preamp").size(13).font(style::SEMIBOLD),
                        text("Overall EQ gain").size(10).color(style::MUTED)
                    ]
                    .spacing(3)
                    .width(120),
                    container(preamp).width(iced::Fill),
                    iced_aw::NumberInput::new(
                        &preamp_display_db,
                        -12.0..=12.0,
                        Message::Preamp
                    )
                    .on_input_maybe(enabled.then_some(Message::Preamp))
                    .ignore_buttons(true)
                    .ignore_scroll(true)
                    .step(0.1)
                    .width(64)
                    .set_size(12)
                    .padding([7, 8])
                    .style(style::number_input)
                    .input_style(style::input),
                    text("dB").size(11).color(style::MUTED),
                ]
                .spacing(14)
                .align_y(iced::Center),
            );
            let status = if self.busy {
                "Applying changes…"
            } else if self.dirty {
                "Unapplied changes"
            } else if self.offline {
                "Preview · device writes disabled"
            } else {
                "Synced with your device"
            };
            let footer = row![
                icon(if self.dirty { '\u{e0f9}' } else { '\u{e06c}' })
                    .size(14)
                    .color(if self.dirty {
                        style::WARNING
                    } else {
                        style::MUTED
                    }),
                text(status).size(11).color(if self.dirty {
                    style::WARNING
                } else {
                    style::MUTED
                }),
                Space::new().width(iced::Fill),
                secondary(
                    "Revert",
                    '\u{e148}',
                    Message::Revert,
                    self.dirty && !self.busy
                ),
                action(
                    "Apply EQ",
                    Message::Apply,
                    enabled
                        && !self.offline
                        && self.dirty
                        && eq.validate().is_ok()
                )
            ]
            .spacing(10)
            .align_y(iced::Center);
            body = body.push(
                container(footer).padding([12, 14]).width(iced::Fill).style(
                    if self.dirty {
                        style::pending
                    } else {
                        style::well
                    },
                ),
            );
        } else {
            body = body.push(
                text("EQ is unavailable in the current device state.")
                    .color(style::MUTED),
            );
        }
        card(body)
    }
    fn mixer_view<'a>(&'a self, state: &'a State) -> Element<'a, Message> {
        let route = if state.output == Output::Headphone {
            "Headphones"
        } else {
            "Speakers"
        };
        let mut groups = column![
            row![
                container(
                    row![
                        icon('\u{e0f1}').size(13),
                        text(format!("Output · {route}")).size(12)
                    ]
                    .spacing(8)
                )
                .padding([7, 10])
                .style(style::chip),
                Space::new().width(iced::Fill),
                text("Levels apply on confirmation; mute is immediate.")
                    .size(11)
                    .color(style::MUTED)
            ]
            .align_y(iced::Center)
        ]
        .spacing(16);
        for (group, title, detail, glyph) in [
            (
                MixerGroup::Playback,
                "Playback",
                "What you hear",
                '\u{e0f1}',
            ),
            (
                MixerGroup::Monitoring,
                "Monitoring",
                "Inputs heard directly through the X4",
                '\u{e145}',
            ),
            (
                MixerGroup::Recording,
                "Recording",
                "Inputs sent to recording applications",
                '\u{e154}',
            ),
            (
                MixerGroup::Other,
                "Other controls",
                "Additional device controls",
                '\u{e0f9}',
            ),
        ] {
            let matching: Vec<_> =
                state.mixer.iter().filter(|c| c.group == group).collect();
            if matching.is_empty() {
                continue;
            }
            let mut controls = column![
                row![
                    container(icon(glyph).color(style::ACCENT))
                        .center_x(36)
                        .center_y(36)
                        .style(style::chip),
                    column![
                        text(title).font(style::SEMIBOLD).size(16),
                        text(detail).size(11).color(style::MUTED)
                    ]
                    .spacing(3)
                ]
                .spacing(12)
                .align_y(iced::Center)
            ]
            .spacing(16);
            for control in matching {
                let index = control.index;
                let boost = control.is_microphone_boost();
                controls = controls.push(divider());
                let mut header = row![
                    text(&control.name).size(13).font(style::SEMIBOLD),
                    Space::new().width(iced::Fill)
                ]
                .align_y(iced::Center);
                if boost {
                    if let Some(enabled) = control.microphone_boost {
                        header = header.push(toggle_id(
                            "microphone-boost",
                            "Enabled",
                            enabled,
                            |enabled| {
                                Message::Command(Command::MicrophoneBoost(
                                    enabled,
                                ))
                            },
                            !self.busy && !self.offline,
                        ));
                    }
                } else if let Some(muted) = control.muted {
                    if mixer_mute_available(control) {
                        header = header.push(toggle_id(
                            format!("mute-{index}"),
                            "Mute",
                            muted,
                            move |v| {
                                Message::Command(Command::MixerMute(index, v))
                            },
                            !self.busy && !self.offline,
                        ));
                    } else {
                        header = header.push(
                            text("Mute unavailable")
                                .size(10)
                                .color(style::MUTED),
                        );
                    }
                }
                let mut entry = column![header].spacing(10);
                if let (Some(range), Some(actual)) =
                    (&control.range, control.level_db)
                {
                    let value = self
                        .mixer_drafts
                        .get(&index)
                        .copied()
                        .unwrap_or(actual);
                    let enabled = !self.busy && !self.offline;
                    let changed = (value - actual).abs() > 0.001;
                    entry = entry.push(
                        row![
                            text(format!("{:.0}", range.min_db))
                                .size(10)
                                .color(style::MUTED)
                                .width(28),
                            slider(
                                range.min_db..=range.max_db,
                                value,
                                move |v| if enabled {
                                    Message::Mixer(index, v)
                                } else {
                                    Message::Tick
                                }
                            )
                            .step(range.step_db.max(0.01))
                            .height(22)
                            .style(style::level),
                            text(format!("{:.0}", range.max_db))
                                .size(10)
                                .color(style::MUTED)
                                .width(22),
                            container(
                                text(format!("{value:.1} dB")).size(12).color(
                                    if changed {
                                        style::WARNING
                                    } else {
                                        style::TEXT
                                    }
                                )
                            )
                            .center_x(82)
                            .padding([7, 0])
                            .style(style::well),
                            action(
                                "Apply",
                                Message::MixerApply(index),
                                enabled && changed
                            )
                        ]
                        .spacing(12)
                        .align_y(iced::Center),
                    );
                } else {
                    entry = entry.push(
                        text(if boost {
                            if control.microphone_boost.is_some() {
                                "Applies immediately"
                            } else {
                                "Boost state unavailable"
                            }
                        } else {
                            "No adjustable level"
                        })
                        .size(11)
                        .color(style::MUTED),
                    );
                }
                controls = controls.push(entry);
            }
            groups = groups.push(card(controls));
        }
        groups.into()
    }
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
        for bytes in style::FONTS {
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
        ] {
            app.page = page;
            app.state = fixture.clone();
            app.offline = matches!(name, "equalizer" | "mixer");
            app.dirty = name == "pending";
            if name == "disconnected" {
                app.state = None;
            }
            if name == "direct" {
                app.state.as_mut().unwrap().features |= 1 << 5;
            }
            for (width, height) in [(1120, 900), (860, 640)] {
                let mut renderer =
                    pollster::block_on(<iced::Renderer as Headless>::new(
                        style::REGULAR,
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
                let theme = style::theme(&app);
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
                        format!("{directory}/{name}-{width}.png"),
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
        assert!(!app.busy);
        assert!(app.error.is_some());
    }
}
