//! Page navigation and the device footer.
use crate::app::{Message, Page, X4App, icon as app_icon};
use iced::{
    Element,
    widget::{Space, column, container, image, row},
};
use iced_t3::{chrome, color, font, icon, widget as t3};
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
        let body = column![
            nav(Page::Equalizer, icon::AUDIO_LINES, "Equalizer"),
            nav(Page::Mixer, icon::SLIDERS_HORIZONTAL, "Mixer"),
            nav(Page::Device, icon::SETTINGS, "Device"),
        ]
        .spacing(2);
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
}
