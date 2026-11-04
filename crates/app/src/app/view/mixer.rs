//! The mixer page: one section per group, one row per control.
use super::gated;
use crate::app::{Message, X4App, activity::Control, worker::Command};
use iced::{
    Element,
    widget::{Space, column, container, row, slider},
};
use iced_t3::{color, font, icon, style, widget as t3};
use soundblaster_x4::{MixerControl, MixerGroup, MixerRange};

impl X4App {
    pub(super) fn mixer_view<'a>(
        &'a self,
        state: &'a soundblaster_x4::State,
    ) -> Element<'a, Message> {
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
        let trailing = self.mixer_trailing(control);
        if let (Some(range), Some(actual)) = (&control.range, control.level_db)
        {
            self.level_row(control, range, actual, trailing)
        } else {
            self.flag_row(control, trailing)
        }
    }
    /// The boost switch or the mute icon at the end of a row.
    fn mixer_trailing<'a>(
        &'a self,
        control: &'a MixerControl,
    ) -> Element<'a, Message> {
        if control.is_microphone_boost() {
            return match control.microphone_boost {
                Some(value) => t3::switch(
                    "microphone-boost",
                    Some("Enabled"),
                    value,
                    self.on_toggle(!self.offline, |v| {
                        Message::Edit(Command::MicrophoneBoost(v))
                    }),
                ),
                None => Space::new().into(),
            };
        }
        let Some(muted) = control.muted else {
            return Space::new().into();
        };
        let index = control.index;
        let available = control.mute_available();
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
                    available,
                    Message::Edit(Command::MixerMute(index, !muted)),
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
    }
    /// A control with no level to adjust.
    fn flag_row<'a>(
        &'a self,
        control: &'a MixerControl,
        trailing: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let boost = control.is_microphone_boost();
        let trailing = self.with_progress(
            if boost {
                Control::MicrophoneBoost
            } else {
                Control::MixerMute(control.index)
            },
            trailing,
        );
        if !boost {
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
        }
    }
    fn level_row<'a>(
        &'a self,
        control: &'a MixerControl,
        range: &MixerRange,
        actual: f32,
        trailing: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let index = control.index;
        let enabled = !self.offline;
        let value = self.mixer_drafts.get(&index).copied().unwrap_or(actual);
        let changed = (value - actual).abs() > 0.001;
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
                slider(
                    range.min_db..=range.max_db,
                    value,
                    gated(enabled && !self.busy(), move |v| {
                        Message::Mixer(index, v)
                    }),
                )
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
                container(trailing).align_right(
                    if control.is_microphone_boost() {
                        96
                    } else {
                        28
                    }
                ),
            ]
            .spacing(12)
            .align_y(iced::Center),
        )
        .padding([12, 16])
        .width(iced::Fill)
        .into()
    }
}
