//! Application state and message handling.
#[cfg(test)]
pub(crate) mod tests;

use crate::{
    activity::{Activity, CONFIRMATION, Confirmed, Control},
    worker,
};
use iced::{
    Subscription, Task,
    keyboard::{self, Key, Modifiers, key::Named},
};
use iced_t3::chrome;
use soundblaster_x4::{Equalizer, State};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use worker::{Command, Completion, EqContext, Event};

/// How often to look for worker events and expire confirmations.
const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Page {
    #[default]
    Equalizer,
    Mixer,
    Device,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    /// Input to a control that looks live but must not act yet.
    Ignored,
    Window(chrome::Action),
    Page(Page),
    Edit(Command),
    EqEnabled(bool),
    Band(usize, f32),
    Preamp(f32),
    Apply,
    Revert,
    Mixer(u8, f32),
    Key(Key, Modifiers),
}

pub struct App {
    pub(crate) offline: bool,
    pub(crate) tx: Sender<Command>,
    pub(crate) rx: Receiver<Event>,
    pub(crate) state: Option<State>,
    pub(crate) preview: Option<State>,
    pub(crate) control_drafts: Vec<Command>,
    pub(crate) draft: Option<Equalizer>,
    pub(crate) draft_context: Option<EqContext>,
    pub(crate) dirty: bool,
    pub(crate) pending: Option<Activity>,
    pub(crate) confirmed: Option<Confirmed>,
    pub(crate) error: Option<String>,
    pub(crate) notice: Option<String>,
    pub(crate) page: Page,
    pub(crate) mixer_drafts: BTreeMap<u8, f32>,
    pub(crate) mixer_editing: BTreeSet<u8>,
}

impl App {
    pub fn new(port: Option<String>) -> Self {
        let (tx, rx) = worker::spawn(port);
        let mut app = Self::with_channels(tx, rx);
        app.send(Command::Refresh);
        app
    }

    /// An offline preview of the bundled fixture.
    pub fn demo() -> Self {
        let (tx, _) = mpsc::channel();
        let (_, rx) = mpsc::channel();
        let mut app = Self::with_channels(tx, rx);
        app.offline = true;
        let mut state: State =
            serde_json::from_str(include_str!("../fixtures/demo.json")).expect("offline fixture");
        state.firmware = "Offline preview".into();
        app.on_state(state, Completion::Refresh);
        app
    }

    pub(crate) fn with_channels(tx: Sender<Command>, rx: Receiver<Event>) -> Self {
        Self {
            offline: false,
            tx,
            rx,
            state: None,
            preview: None,
            control_drafts: Vec::new(),
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

    pub(crate) fn send(&mut self, command: Command) {
        // Also guard against several widgets firing in the same frame.
        if self.offline || self.busy() {
            return;
        }
        let activity = Activity::describe(&command, self.state.as_ref());
        if self.tx.send(command).is_ok() {
            self.pending = Some(activity);
            self.confirmed = None;
            self.error = None;
            self.notice = None;
        } else {
            self.error = Some("Device worker stopped. Restart the application.".into());
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn in_flight(&self, control: Control) -> bool {
        self.pending.as_ref().is_some_and(|a| a.control == control)
    }

    pub(crate) fn events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::State(state, completed) => {
                    self.on_state(state, completed);
                }
                Event::Error(error) => self.on_error(error),
            }
        }
    }

    pub(crate) fn on_state(&mut self, state: State, completed: Completion) {
        let applied = matches!(completed, Completion::Applied);
        if applied || matches!(completed, Completion::Refresh) {
            self.control_drafts.clear();
            self.mixer_editing.clear();
            self.mixer_drafts.clear();
        }
        let context = EqContext::from_state(&state);
        let context_changed = self.draft_context.is_some_and(|old| old != context);
        let reset = !self.dirty
            || context_changed
            || matches!(
                completed,
                Completion::Refresh | Completion::EqApplied | Completion::Applied
            );
        if context_changed && !applied && (self.dirty || !self.control_drafts.is_empty()) {
            self.control_drafts.clear();
            self.notice = Some("The output, sound mode or profile changed. Pending device and EQ edits were discarded; the current device state is shown.".into());
        }
        if reset {
            self.draft.clone_from(&state.eq);
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
        self.rebuild_preview();
        if !matches!(completed, Completion::Poll)
            && let Some(activity) = self.pending.take()
        {
            self.confirmed = Some(Confirmed {
                control: activity.control,
                message: activity.done,
                at: Instant::now(),
            });
        }
    }

    pub(crate) fn on_error(&mut self, error: String) {
        self.error = Some(error);
        self.state = None;
        self.preview = None;
        self.control_drafts.clear();
        self.pending = None;
        self.confirmed = None;
        self.draft = None;
        self.draft_context = None;
        self.dirty = false;
        self.mixer_drafts.clear();
        self.mixer_editing.clear();
    }

    #[expect(
        clippy::unused_self,
        reason = "Iced calls this with the state, even when nothing depends on it"
    )]
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(TICK).map(|_| Message::Tick),
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
        ])
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                self.events();
                if self
                    .confirmed
                    .as_ref()
                    .is_some_and(|c| c.at.elapsed() >= CONFIRMATION)
                {
                    self.confirmed = None;
                }
            }
            Message::Ignored => (),
            Message::Window(action) => return chrome::perform(action),
            Message::Key(key, modifiers) => {
                return self.on_key(&key, modifiers);
            }
            Message::Page(page) => self.page = page,
            Message::Edit(command) => self.stage(command),
            Message::EqEnabled(value) => self.edit_eq(|eq| eq.enabled = value),
            Message::Band(index, value) => {
                self.edit_eq(|eq| eq.bands_db[index] = value);
            }
            Message::Preamp(value) => {
                if value.is_finite() {
                    self.edit_eq(|eq| eq.preamp_db = value);
                }
            }
            Message::Apply => self.apply(),
            Message::Revert => self.revert(),
            Message::Mixer(index, value) => {
                if !self.busy() && !self.offline && value.is_finite() {
                    self.mixer_editing.insert(index);
                    self.mixer_drafts.insert(index, value);
                }
            }
        }
        Task::none()
    }

    fn on_key(&mut self, key: &Key, modifiers: Modifiers) -> Task<Message> {
        match key {
            Key::Named(Named::Tab) if modifiers.shift() => {
                iced::widget::operation::focus_previous()
            }
            Key::Named(Named::Tab) => iced::widget::operation::focus_next(),
            Key::Character(key) if modifiers.control() && key.eq_ignore_ascii_case("r") => {
                self.send(Command::Refresh);
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub(crate) fn edit_eq(&mut self, edit: impl FnOnce(&mut Equalizer)) {
        if self.busy() || self.context_pending() {
            return;
        }
        if let Some(eq) = &mut self.draft {
            edit(eq);
            self.dirty = true;
        }
    }
}
