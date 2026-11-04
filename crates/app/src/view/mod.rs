//! The window: chrome, page layout and the helpers every control shares.
mod device;
mod equalizer;
mod mixer;
mod sidebar;

use crate::{
    activity::Control,
    app::{App, Message, Page},
    worker::Command,
};
use iced::{
    Element,
    widget::{column, container, row},
};
use iced_t3::{Tone, chrome, color, icon, widget as t3};

/// Display size of the sidebar icon.
pub const ICON_SIZE: u32 = 22;
pub const ICON_PNG: &[u8] = include_bytes!("../../../../packaging/linuxblaster-control.png");

pub fn icon() -> image::RgbaImage {
    image::load_from_memory(ICON_PNG)
        .expect("bundled app icon")
        .into_rgba8()
}

/// Decode and scale the sidebar icon once to twice the display size.
pub static APP_ICON: std::sync::LazyLock<iced::widget::image::Handle> =
    std::sync::LazyLock::new(|| {
        let size = ICON_SIZE * 2;
        let scaled = image::imageops::thumbnail(&icon(), size, size);
        let (width, height) = scaled.dimensions();
        iced::widget::image::Handle::from_rgba(width, height, scaled.into_raw())
    });

/// Width of the page column (T3 Code's `max-w-4xl` less its gutters).
const PAGE_WIDTH: f32 = 832.0;

/// `change`, or nothing at all unless `live`. For controls that have to stay
/// wired up so they keep their look while a write is in flight.
fn gated<T>(live: bool, change: impl Fn(T) -> Message + Copy) -> impl Fn(T) -> Message + Copy {
    move |value| {
        if live {
            change(value)
        } else {
            Message::Ignored
        }
    }
}

impl App {
    pub fn view(&self) -> Element<'_, Message> {
        let mut page = column![].spacing(24).max_width(PAGE_WIDTH);
        if let Some(error) = &self.error {
            page = page.push(t3::alert(icon::CIRCLE_ALERT, error, Tone::Danger));
        }
        if let Some(notice) = &self.notice {
            page = page.push(t3::alert(icon::TRIANGLE_ALERT, notice, Tone::Warning));
        }
        page = match &self.preview {
            Some(state) => match self.page {
                Page::Equalizer => page.push(self.eq_view(state)),
                Page::Mixer => page.push(self.mixer_view(state)),
                Page::Device => page.push(self.device_view(state)),
            },
            None if self.busy() => {
                page.push(t3::empty_state(t3::dots(), "Connecting to your X4", []))
            }
            None => page.push(t3::empty_state(
                t3::icon_tile(icon::UNPLUG, t3::TileSize::Standard),
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
        if let Some(bar) = self.apply_bar() {
            main = main.push(bar);
        }
        chrome::frame(
            row![self.sidebar(), t3::sidebar_divider(), main],
            Message::Window,
        )
    }

    /// Controls keep their look during a write; only the one in flight shows
    /// progress. Everything else ignores input until the device answers.
    pub(super) fn press(&self, allowed: bool, message: Message) -> Option<Message> {
        (allowed && !self.offline).then(|| {
            if self.busy() {
                Message::Ignored
            } else {
                message
            }
        })
    }

    /// [`Self::press`] for switches.
    fn on_toggle<F: Fn(bool) -> Message + Copy>(
        &self,
        allowed: bool,
        change: F,
    ) -> Option<impl Fn(bool) -> Message + use<F>> {
        allowed.then_some(gated(!self.busy(), change))
    }

    /// Dots while `control` is in flight, then a check once the X4 confirms it.
    fn progress<'a>(&self, control: Control) -> Option<Element<'a, Message>> {
        if self.in_flight(control) {
            Some(t3::dots())
        } else if self
            .confirmed
            .as_ref()
            .is_some_and(|confirmed| confirmed.control == control)
        {
            Some(t3::check())
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

    /// Progress stays in the Apply bar while working; confirmation then
    /// appears briefly in the titlebar without leaving an empty footer.
    fn activity_line(&self) -> Option<Element<'_, Message>> {
        let on_page = |control| {
            control == Control::Apply && (self.has_changes() || self.in_flight(Control::Apply))
        };
        if let Some(activity) = &self.pending {
            return (!on_page(activity.control)).then(|| {
                row![t3::dots(), t3::muted(&activity.progress)]
                    .spacing(8)
                    .align_y(iced::Center)
                    .into()
            });
        }
        let confirmed = self.confirmed.as_ref()?;
        (!on_page(confirmed.control)).then(|| {
            row![
                t3::check(),
                t3::text(&confirmed.message, 12).color(color::SUCCESS),
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
            Page::Device => (icon::SETTINGS, "Device"),
        };
        let mut leading = row![t3::breadcrumb(glyph, ["Sound Blaster X4"], title)]
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
                    self.press(true, Message::Edit(Command::Refresh)),
                ),
                "Refresh · Ctrl+R",
                t3::Position::Bottom,
            )
        };
        chrome::bar(leading, Some(refresh), Message::Window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tests::{app, state},
        worker::{Command, Completion, Event},
    };
    use soundblaster_x4::Output;

    #[test]
    fn apply_bar_only_appears_for_pending_changes_and_disappears_after_apply_or_revert() {
        let (mut app, events, commands) = app();
        events
            .send(Event::State(state(), Completion::Refresh))
            .unwrap();
        app.events();
        for page in [Page::Equalizer, Page::Mixer, Page::Device] {
            app.page = page;
            assert!(app.apply_bar().is_none(), "{page:?}: clean");
        }
        let _ = app.update(Message::Edit(Command::MixerMute(1, true)));
        assert!(app.apply_bar().is_some());
        let _ = app.update(Message::Revert);
        assert!(app.apply_bar().is_none());
        let _ = app.update(Message::Edit(Command::Output(Output::Speaker)));
        let _ = app.update(Message::Apply);
        assert!(matches!(commands.try_recv().unwrap(), Command::Apply(..)));
        assert!(app.apply_bar().is_some(), "keep progress visible");
        let mut applied = state();
        applied.output = Output::Speaker;
        events
            .send(Event::State(applied, Completion::Applied))
            .unwrap();
        app.events();
        assert!(app.apply_bar().is_none(), "hide after completion");
        assert!(app.confirmed.is_some(), "titlebar can still confirm");
    }
}
