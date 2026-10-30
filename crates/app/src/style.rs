//! Shared surface, typography and control styles inspired by razerr-rs.
use iced::{
    Border, Color, Font, Theme, font,
    widget::{button, container, slider, text_input},
};
pub const REGULAR: Font = Font::with_name("Inter");
pub const SEMIBOLD: Font = Font {
    weight: font::Weight::Semibold,
    ..REGULAR
};
pub const ICONS: Font = Font::with_name("lucide");
pub const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/inter/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/inter/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/lucide/Lucide.ttf"),
];
pub const ACCENT: Color = Color::from_rgb8(92, 218, 111);
pub const MUTED: Color = Color::from_rgb8(143, 151, 163);
pub const TEXT: Color = Color::from_rgb8(232, 236, 241);
pub const LINE: Color = Color::from_rgb8(43, 48, 55);
pub const INSET: Color = Color::from_rgb8(17, 20, 24);
pub const RAISED: Color = Color::from_rgb8(35, 40, 46);
pub const WARNING: Color = Color::from_rgb8(239, 188, 102);
pub const DANGER: Color = Color::from_rgb8(240, 112, 112);
pub fn theme(_: &crate::app::X4App) -> Theme {
    Theme::custom(
        "X4",
        iced::theme::Palette {
            background: Color::from_rgb8(20, 23, 27),
            text: TEXT,
            primary: ACCENT,
            success: ACCENT,
            warning: WARNING,
            danger: DANGER,
        },
    )
}
fn surface(color: Color, radius: f32) -> container::Style {
    container::Style {
        background: Some(color.into()),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: radius.into(),
        },
        ..Default::default()
    }
}
pub fn card(_: &Theme) -> container::Style {
    surface(Color::from_rgb8(26, 30, 35), 12.0)
}
pub fn well(_: &Theme) -> container::Style {
    surface(INSET, 8.0)
}
pub fn sidebar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb8(13, 16, 19).into()),
        ..Default::default()
    }
}
pub fn divider(_: &Theme) -> container::Style {
    container::Style {
        background: Some(LINE.into()),
        ..Default::default()
    }
}
pub fn chip(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(ACCENT),
        ..surface(ACCENT.scale_alpha(0.09), 6.0)
    }
}
pub fn neutral_chip(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(MUTED),
        ..surface(RAISED, 6.0)
    }
}
pub fn pending(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(WARNING),
        ..surface(WARNING.scale_alpha(0.06), 8.0)
    }
}
pub fn error(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(DANGER),
        ..surface(DANGER.scale_alpha(0.06), 8.0)
    }
}
pub fn choice(
    selected: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered =
            matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(
                if selected {
                    ACCENT.scale_alpha(0.10)
                } else if hovered {
                    RAISED
                } else {
                    INSET
                }
                .into(),
            ),
            text_color: if selected {
                ACCENT
            } else if status == button::Status::Disabled {
                MUTED.scale_alpha(0.55)
            } else {
                TEXT
            },
            border: Border {
                color: if selected {
                    ACCENT.scale_alpha(0.5)
                } else {
                    LINE
                },
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    }
}
pub fn secondary(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(
            if matches!(
                status,
                button::Status::Hovered | button::Status::Pressed
            ) {
                LINE
            } else {
                RAISED
            }
            .into(),
        ),
        text_color: if status == button::Status::Disabled {
            MUTED.scale_alpha(0.45)
        } else {
            TEXT
        },
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 7.0.into(),
        },
        ..Default::default()
    }
}
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    let disabled = status == button::Status::Disabled;
    button::Style {
        background: Some(
            if disabled {
                RAISED
            } else if status == button::Status::Hovered {
                Color::from_rgb8(124, 235, 141)
            } else {
                ACCENT
            }
            .into(),
        ),
        text_color: if disabled {
            MUTED.scale_alpha(0.6)
        } else {
            INSET
        },
        border: Border {
            radius: 7.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut s = text_input::default(theme, status);
    s.background = INSET.into();
    s.border = Border {
        color: if matches!(status, text_input::Status::Focused { .. }) {
            ACCENT
        } else {
            LINE
        },
        width: 1.0,
        radius: 6.0.into(),
    };
    s
}
pub fn fader(
    enabled: bool,
) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_, status| slider::Style {
        rail: slider::Rail {
            backgrounds: (LINE.into(), LINE.into()),
            width: 3.0,
            border: Border::default(),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Rectangle {
                width: 8,
                border_radius: 3.0.into(),
            },
            background: if !enabled {
                MUTED.scale_alpha(0.4)
            } else if status == slider::Status::Dragged {
                ACCENT
            } else {
                TEXT
            }
            .into(),
            border_width: 1.0,
            border_color: if enabled { ACCENT } else { LINE },
        },
    }
}
pub fn level(theme: &Theme, status: slider::Status) -> slider::Style {
    let mut s = slider::default(theme, status);
    s.rail.backgrounds = (ACCENT.scale_alpha(0.65).into(), LINE.into());
    s.rail.width = 4.0;
    s.handle.background = TEXT.into();
    s.with_circular_handle(5.0)
}
pub struct AudioSlider;
impl iced_audio::h_slider::StyleSheet for AudioSlider {
    type Style = Theme;
    fn idle(&self, _: &Theme) -> iced_audio::h_slider::Appearance {
        iced_audio::h_slider::Appearance::Rect(
            iced_audio::h_slider::RectAppearance {
                back_color: LINE,
                back_border_width: 0.0,
                back_border_radius: 3.0,
                back_border_color: Color::TRANSPARENT,
                filled_color: ACCENT.scale_alpha(0.65),
                handle_color: TEXT,
                handle_width: 5,
                handle_filled_gap: 0.0,
            },
        )
    }
}
pub fn number_input(
    _: &Theme,
    status: iced_aw::style::Status,
) -> iced_aw::style::number_input::Style {
    iced_aw::style::number_input::Style {
        button_background: Some(INSET.into()),
        icon_color: if matches!(
            status,
            iced_aw::style::Status::Hovered | iced_aw::style::Status::Pressed
        ) {
            ACCENT
        } else {
            MUTED
        },
    }
}
