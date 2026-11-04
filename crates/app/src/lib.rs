//! The iced window over the X4 device worker, on the `iced_t3` design.

mod activity;
mod app;
mod curve;
mod edit;
#[cfg(test)]
mod render_tests;
mod view;
mod worker;

use iced::{Size, Task, window};

const APPLICATION_ID: &str = "linuxblaster-control";

/// Run the desktop window with the selected device port.
pub fn run(port: Option<String>) -> iced::Result {
    launch(move || (app::App::new(port.clone()), Task::none()))
}

/// Run an offline window without opening a device.
pub fn run_demo() -> iced::Result {
    launch(|| (app::App::demo(), Task::none()))
}

fn launch(boot: impl Fn() -> (app::App, Task<app::Message>) + 'static) -> iced::Result {
    let icon = view::icon();
    let (width, height) = icon.dimensions();
    let icon = window::icon::from_rgba(icon.into_raw(), width, height).ok();
    iced::application(boot, app::App::update, app::App::view)
        .title("Linuxblaster Control — X4")
        .theme(|_: &app::App| iced_t3::theme())
        .subscription(app::App::subscription)
        .settings(iced_t3::settings_for(APPLICATION_ID))
        .font(iced_aw::ICED_AW_FONT_BYTES)
        .window(window::Settings {
            size: Size::new(1120.0, 780.0),
            min_size: Some(Size::new(820.0, 560.0)),
            icon,
            ..iced_t3::chrome::window_settings_for(APPLICATION_ID)
        })
        .run()
}
