//! Output and processing settings, separate from the EQ editor.
use crate::app::{
    Message, X4App,
    activity::{Control, output_name, sxfi_name},
    worker::{Command, EqContext},
};
use iced::Element;
use iced_t3::widget as t3;
use soundblaster_x4::{Feature, Output, State, SxfiMode};

impl X4App {
    pub(super) fn device_view<'a>(
        &'a self,
        state: &'a State,
    ) -> Element<'a, Message> {
        let context = EqContext::from_state(state);
        let sxfi_allowed = state.sxfi.is_some()
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
                        Message::Edit(Command::Sxfi(context, mode)),
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
                        Message::Edit(Command::Feature(feature, value))
                    }),
                ),
            )
        };
        let outputs = [Output::Headphone, Output::Speaker].map(|output| {
            t3::segment(
                format!("output-{output:?}"),
                output_name(output),
                state.output == output,
                self.press(true, Message::Edit(Command::Output(output))),
            )
        });
        t3::section(
            t3::section_title("Sound"),
            None,
            [
                t3::setting_titled("Output", t3::segmented(outputs)),
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
}
