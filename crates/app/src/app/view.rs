//! The window: chrome, page layout and the helpers every control shares.
mod equalizer;
mod mixer;
mod sidebar;

use super::{Message, Page, X4App, activity::Control};
use iced::{
    Element,
    widget::{column, container, row},
};
use iced_t3::{Tone, chrome, color, icon, widget as t3};

/// Width of the page column (T3 Code's `max-w-4xl` less its gutters).
const PAGE_WIDTH: f32 = 832.0;

/// `change`, or nothing at all unless `live`. For controls that have to stay
/// wired up so they keep their look while a write is in flight.
fn gated<T>(
    live: bool,
    change: impl Fn(T) -> Message + Copy,
) -> impl Fn(T) -> Message + Copy {
    move |value| {
        if live {
            change(value)
        } else {
            Message::Ignored
        }
    }
}

impl X4App {
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
        page = match &self.preview {
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
        if self.state.is_some() {
            main = main.push(self.apply_bar());
        }
        chrome::frame(
            row![self.sidebar(), t3::sidebar_divider(), main],
            Message::Window,
        )
    }
    /// Controls keep their look during a write; only the one in flight shows
    /// progress. Everything else ignores input until the device answers.
    pub(super) fn press(
        &self,
        allowed: bool,
        message: Message,
    ) -> Option<Message> {
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
    /// What the app is doing or just finished, in words. EQ writes report in
    /// the action bar instead while it is on screen.
    fn activity_line(&self) -> Option<Element<'_, Message>> {
        let on_page = |control| control == Control::Apply;
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
                    self.press(true, Message::Edit(super::Command::Refresh)),
                ),
                "Refresh · Ctrl+R",
                t3::Position::Bottom,
            )
        };
        chrome::bar(leading, Some(refresh), Message::Window)
    }
}
