//! Navigation, the output and EQ profile pickers, and the device footer.
use crate::app::{
    Message, Page, X4App,
    activity::{Control, output_name},
    icon as app_icon,
    worker::{Command, EqContext},
};
use iced::{
    Element,
    widget::{Space, column, container, image, row},
};
use iced_t3::{chrome, color, font, icon, widget as t3};
use soundblaster_x4::{EqProfileInfo, Feature, Output, State};
use std::sync::LazyLock;

/// Display size of the app icon by the title.
const ICON_SIZE: u32 = 22;
/// The app icon, scaled once to double size so it stays sharp on `HiDPI` screens.
static ICON: LazyLock<image::Handle> = LazyLock::new(|| {
    let size = ICON_SIZE * 2;
    let scaled = ::image::imageops::thumbnail(&app_icon(), size, size);
    image::Handle::from_rgba(size, size, scaled.into_raw())
});

impl X4App {
    pub(super) fn sidebar(&self) -> Element<'_, Message> {
        let header = chrome::drag_region(
            container(
                row![
                    image(ICON.clone()).width(ICON_SIZE).height(ICON_SIZE),
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
        let device = t3::sidebar_device(
            "device",
            t3::SidebarDevice {
                media: t3::icon_tile(
                    icon::RADIO_RECEIVER,
                    t3::TileSize::Compact,
                ),
                title: "Sound Blaster X4".into(),
                detail: detail.into(),
                selected: false,
            },
            None,
        );
        t3::sidebar(
            header,
            body,
            column![t3::divider(), container(device).padding([6, 0])],
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
            t3::SidebarChoice {
                glyph: Some(glyph),
                label: output_name(output).into(),
                marker: self
                    .progress(control)
                    .unwrap_or_else(|| t3::radio(selected)),
                selected,
            },
            self.press(true, Message::Command(Command::Output(output))),
        )
    }
    fn profile_choice<'a>(
        &'a self,
        state: &'a State,
        profile: &'a EqProfileInfo,
    ) -> Element<'a, Message> {
        let selected = state.active_eq_profile == Some(profile.index);
        let available = state.eq_profile_available(profile.index);
        let choice = t3::sidebar_choice(
            format!("profile-{}", profile.index),
            t3::SidebarChoice {
                glyph: None,
                label: profile.name.as_str().into(),
                marker: self
                    .progress(Control::Profile(profile.index))
                    .unwrap_or_else(|| t3::radio(selected)),
                selected,
            },
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
}
