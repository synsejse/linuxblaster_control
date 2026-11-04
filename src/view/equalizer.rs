//! The device settings, the EQ section and the docked apply bar.
use super::{PAGE_WIDTH, gated};
use crate::{
    activity::Control,
    app::{App, Message},
    curve::Curve,
    worker::{Command, EqContext},
};
use iced::{
    Element,
    widget::{Space, canvas, column, container, row, slider, vertical_slider},
};
use iced_aw::NumberInput;
use iced_t3::{color, font, icon, style, widget as t3};
use soundblaster_x4::{EQ_LIMIT_DB, EqProfileInfo, Equalizer, Feature, State};

const EQ_RANGE: std::ops::RangeInclusive<f32> = -EQ_LIMIT_DB..=EQ_LIMIT_DB;
const BANDS: [&str; 10] = [
    "31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz",
];

fn number_input(_: &iced::Theme, _: iced_aw::style::Status) -> iced_aw::style::number_input::Style {
    iced_aw::style::number_input::Style {
        button_background: None,
        icon_color: color::MUTED,
    }
}

/// A typed gain, shown without step buttons.
fn gain_input<'a>(
    value: f32,
    change: impl Fn(f32) -> Message + Copy + 'static,
    enabled: bool,
) -> NumberInput<'a, f32, Message> {
    NumberInput::new(&value, EQ_RANGE, change)
        .on_input_maybe(enabled.then_some(change))
        .ignore_buttons(true)
        .ignore_scroll(true)
        .step(0.1)
        .style(number_input)
}

impl App {
    pub(super) fn eq_view<'a>(&'a self, state: &'a State) -> Element<'a, Message> {
        let enabled = !state.feature(Feature::DirectMode) && !self.context_pending();
        let mut rows = match &self.draft {
            Some(eq) => {
                vec![self.bands_view(state, eq), self.preamp_view(eq, enabled)]
            }
            None => vec![t3::setting(
                "Bands",
                t3::muted("EQ is unavailable in the current device state."),
                Space::new(),
            )],
        };
        if !state.eq_profiles.is_empty() {
            rows.insert(
                0,
                t3::setting_titled(
                    "Profile",
                    t3::segmented(
                        state
                            .eq_profiles
                            .iter()
                            .map(|profile| self.profile_choice(state, profile)),
                    ),
                ),
            );
        }
        if self.context_pending() {
            rows.insert(
                0,
                t3::setting_titled(
                    "Pending device selection",
                    t3::warning("Apply the selection before editing its EQ."),
                ),
            );
        }
        t3::section(
            match state.active_profile() {
                Some(profile) => t3::section_heading("Equalizer", &profile.name),
                None => t3::section_title("Equalizer").into(),
            },
            Some(t3::switch(
                "eq-enabled",
                Some("Enabled"),
                self.draft.as_ref().is_some_and(|e| e.enabled),
                self.on_toggle(enabled && self.draft.is_some(), Message::EqEnabled),
            )),
            rows,
        )
    }

    fn profile_choice<'a>(
        &'a self,
        state: &'a State,
        profile: &'a EqProfileInfo,
    ) -> Element<'a, Message> {
        let available = state.eq_profile_available(profile.index);
        let choice = t3::segment(
            format!("profile-{}", profile.index),
            profile.name.clone(),
            state.active_eq_profile == Some(profile.index),
            self.press(
                available,
                Message::Edit(Command::Profile(
                    EqContext::from_state(state),
                    profile.index,
                )),
            ),
        );
        let hint = if self.offline || available {
            return choice;
        } else if state.feature(Feature::DirectMode) {
            "Turn off Direct mode to select a profile."
        } else if profile.index == 3 {
            "Enable Super X-Fi to use this profile."
        } else {
            "Turn off Super X-Fi to use this profile."
        };
        t3::tooltip(choice, hint, t3::Position::Bottom)
    }

    /// The curve above its ten faders.
    fn bands_view(&self, state: &State, eq: &Equalizer) -> Element<'_, Message> {
        let enabled = !state.feature(Feature::DirectMode) && !self.context_pending();
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
        let live = enabled && !self.busy();
        let mut bands = row![];
        for (index, label) in BANDS.into_iter().enumerate() {
            let band = gated(live, move |value| Message::Band(index, value));
            let gain = eq.bands_db[index];
            let fader = vertical_slider(EQ_RANGE, gain, band)
                .step(0.1)
                .shift_step(1.0_f32)
                .default(0.0_f32)
                .height(96)
                .width(18)
                .style(style::slider(enabled));
            let display_db = (gain * 100.0).round() / 100.0;
            let value = gain_input(display_db, band, enabled)
                .width(iced::Fill)
                .padding([5, 2])
                .set_size(11)
                .line_height(t3::line_height(11))
                .align_x(iced::Center)
                .font(font::REGULAR)
                .input_style(style::input_quiet);
            // No row spacing: the curve centres its points in equal tenths.
            bands = bands.push(
                container(
                    column![t3::muted(label).size(11), fader, value]
                        .align_x(iced::Center)
                        .spacing(8),
                )
                .padding([0, 3])
                .width(iced::Fill),
            );
        }
        container(
            column![
                canvas(Curve {
                    bands: eq.bands_db,
                    active: eq.enabled && enabled && !reloading,
                })
                .width(iced::Fill)
                .height(196),
                bands,
            ]
            .spacing(12),
        )
        .padding([14, 16])
        .into()
    }

    fn preamp_view(&self, eq: &Equalizer, enabled: bool) -> Element<'_, Message> {
        let preamp = gated(enabled && !self.busy(), Message::Preamp);
        // Hide floating-point readback noise without changing the EQ draft.
        let display_db = (eq.preamp_db * 100.0).round() / 100.0;
        t3::setting(
            "Preamp",
            t3::muted("Overall EQ gain."),
            row![
                slider(EQ_RANGE, eq.preamp_db, preamp)
                    .step(0.1_f32)
                    .shift_step(1.0_f32)
                    .default(0.0_f32)
                    .width(240)
                    .height(22)
                    .style(style::slider(enabled)),
                gain_input(display_db, preamp, enabled)
                    .width(64)
                    .set_size(12)
                    .line_height(t3::line_height(12))
                    .padding([6, 8])
                    .input_style(style::input),
                t3::muted("dB"),
            ]
            .spacing(12)
            .align_y(iced::Center),
        )
    }

    /// Only pending edits and an active Apply need a docked bar.
    pub(super) fn apply_bar(&self) -> Option<Element<'_, Message>> {
        if self.state.is_none() || (!self.has_changes() && !self.in_flight(Control::Apply)) {
            return None;
        }
        let working = self.in_flight(Control::Apply);
        let dirty = self.has_changes();
        let invalid = self.planned_changes().err();
        let status = if working {
            "Applying changes…"
        } else {
            invalid
                .as_deref()
                .filter(|_| dirty)
                .unwrap_or("Unapplied changes")
        };
        let tint = if invalid.is_some() && dirty {
            color::DANGER
        } else if dirty {
            color::WARNING
        } else {
            color::MUTED
        };
        Some(t3::composer(
            row![
                t3::text(status.to_owned(), 13)
                    .color(tint)
                    .width(iced::Fill),
                t3::ghost(
                    icon::UNDO_2,
                    "Revert",
                    (dirty && !self.busy()).then_some(Message::Revert)
                ),
                if working {
                    t3::primary_busy("Apply")
                } else {
                    t3::primary(
                        "Apply",
                        (dirty && invalid.is_none() && !self.busy() && !self.offline)
                            .then_some(Message::Apply),
                    )
                },
            ]
            .spacing(8)
            .align_y(iced::Center),
            PAGE_WIDTH,
            dirty,
        ))
    }
}
