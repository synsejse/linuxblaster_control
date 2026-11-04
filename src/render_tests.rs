//! The real views, laid out, rendered and clicked without a window. Set
//! `X4_UI_CAPTURE_DIR` to save a PNG of every page and state.
#![expect(
    clippy::cast_precision_loss,
    reason = "Window sizes and click positions are small whole numbers"
)]

use crate::{
    activity::{Activity, Confirmed, Control},
    app::{App, Message, Page, tests},
    worker::{Command, Completion, Event},
};
use iced::{
    Point, Rectangle, Size,
    advanced::{
        Layout, Shell, clipboard, layout,
        renderer::{Headless, Style},
        widget::Tree,
    },
    mouse,
};
use iced_t3::{font, widget as t3};
use std::time::Instant;

fn load_fonts() {
    let mut fonts = iced::advanced::graphics::text::font_system()
        .write()
        .unwrap();
    fonts.load_font(iced_aw::ICED_AW_FONT_BYTES.into());
    for bytes in font::ALL {
        fonts.load_font(bytes.into());
    }
}

fn renderer() -> iced::Renderer {
    pollster::block_on(<iced::Renderer as Headless>::new(
        font::REGULAR,
        14.0.into(),
        Some("tiny-skia"),
    ))
    .expect("software renderer")
}

/// Render `app` at `width`×`height` and return its RGBA pixels.
fn screenshot(app: &App, width: u32, height: u32) -> Vec<u8> {
    let mut renderer = renderer();
    let mut view = app.view();
    let mut tree = Tree::new(&view);
    let size = Size::new(width as f32, height as f32);
    let node = view
        .as_widget_mut()
        .layout(&mut tree, &renderer, &layout::Limits::new(size, size));
    let theme = iced_t3::theme();
    let mut messages = Vec::new();
    view.as_widget_mut().update(
        &mut tree,
        &iced::Event::Window(iced::window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &renderer,
        &mut clipboard::Null,
        &mut Shell::new(&mut messages),
        &Rectangle::with_size(size),
    );
    view.as_widget().draw(
        &tree,
        &mut renderer,
        &theme,
        &Style {
            text_color: theme.palette().text,
        },
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &Rectangle::with_size(size),
    );
    renderer.screenshot(Size::new(width, height), 1.0, theme.palette().background)
}

fn activity(control: Control, progress: &str) -> Activity {
    Activity {
        control,
        progress: progress.into(),
        done: String::new(),
    }
}

/// Put `app` into the state the capture `name` shows.
fn set_scene(app: &mut App, name: &str) {
    app.control_drafts.clear();
    app.mixer_drafts.clear();
    app.offline = matches!(name, "equalizer" | "mixer" | "device");
    app.dirty = name == "pending";
    app.notice = (name == "pending").then(|| "The output, sound mode or profile changed.".into());
    app.error = None;
    app.pending = match name {
        "loading" => Some(activity(Control::Profile(1), "Loading Movie…")),
        "saving" => Some(activity(Control::Eq, "Writing EQ to Music…")),
        "applying" => {
            app.mixer_drafts.insert(0, -20.0);
            let mixer = &mut app.state.as_mut().unwrap().mixer;
            mixer[1].muted = Some(true);
            Some(activity(
                Control::MixerLevel(0),
                "Setting Headphones to -20.0 dB…",
            ))
        }
        _ => None,
    };
    app.confirmed = (name == "confirmed").then(|| Confirmed {
        control: Control::Profile(0),
        message: "Music loaded".into(),
        at: Instant::now(),
    });
    if name == "disconnected" {
        app.state = None;
        app.error = Some("No Sound Blaster X4 found.".into());
    }
    if name == "direct" {
        app.state.as_mut().unwrap().features |= 1 << 5;
    }
    if name == "device-draft" {
        app.stage(Command::Output(soundblaster_x4::Output::Speaker));
    }
    if name == "mixer-draft" {
        app.stage(Command::MixerMute(0, true));
        let _ = app.update(Message::Mixer(0, -20.0));
    }
    app.rebuild_preview();
}

#[test]
fn offline_render_and_state_flows() {
    load_fonts();
    let mut app = App::demo();
    assert!(app.offline);
    let confirmed = app.state.as_ref().unwrap().eq.clone();
    let _ = app.update(Message::Band(2, 3.0));
    assert!(app.dirty);
    assert_eq!(app.state.as_ref().unwrap().eq, confirmed);
    let _ = app.update(Message::Revert);
    assert!(!app.dirty);
    assert_eq!(app.draft, confirmed);
    let fixture = app.state.clone();
    for (page, name) in [
        (Page::Equalizer, "equalizer"),
        (Page::Mixer, "mixer"),
        (Page::Device, "device"),
        (Page::Equalizer, "pending"),
        (Page::Equalizer, "disconnected"),
        (Page::Equalizer, "direct"),
        (Page::Equalizer, "loading"),
        (Page::Equalizer, "confirmed"),
        (Page::Equalizer, "saving"),
        (Page::Mixer, "applying"),
        (Page::Device, "device-draft"),
        (Page::Mixer, "mixer-draft"),
    ] {
        app.page = page;
        app.state.clone_from(&fixture);
        set_scene(&mut app, name);
        for (width, height) in [(1120, 780), (820, 560), (820, 1240)] {
            let pixels = screenshot(&app, width, height);
            assert_eq!(pixels.len(), (width * height * 4) as usize);
            if let Ok(directory) = std::env::var("X4_UI_CAPTURE_DIR") {
                std::fs::create_dir_all(&directory).unwrap();
                image::save_buffer(
                    format!("{directory}/{name}-{width}x{height}.png"),
                    &pixels,
                    width,
                    height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}

/// Heights of the step buttons `NumberInput` lays out even with
/// `ignore_buttons`, which skips drawing them.
const HIDDEN_STEPPERS: [f32; 3] = [8.9375, 9.75, 20.5];
fn off_grid(layout: Layout<'_>, found: &mut Vec<Rectangle>) {
    let bounds = layout.bounds();
    if (bounds.y.fract() != 0.0 || bounds.height.fract() != 0.0)
        && !HIDDEN_STEPPERS.contains(&bounds.height)
    {
        found.push(bounds);
    }
    for child in layout.children() {
        off_grid(child, found);
    }
}

/// Iced truncates fractional positions, so one off-grid box puts everything
/// inside it out of line.
#[test]
fn every_box_sits_on_the_pixel_grid_vertically() {
    load_fonts();
    let renderer = renderer();
    let mut app = App::demo();
    let fixture = app.state.clone();
    for page in [Page::Equalizer, Page::Mixer, Page::Device] {
        for connected in [true, false] {
            app.page = page;
            app.state = if connected { fixture.clone() } else { None };
            app.rebuild_preview();
            for (width, height) in [(1120.0, 780.0), (820.0, 560.0)] {
                let size = Size::new(width, height);
                let mut view = app.view();
                let mut tree = Tree::new(&view);
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(size, size),
                );
                let mut found = Vec::new();
                off_grid(Layout::new(&node), &mut found);
                assert!(found.is_empty(), "{page:?} at {width}x{height}: {found:#?}");
            }
        }
    }
}

/// Clicks down the column of mute icons and returns the first mute sent.
fn click_mute(app: &App) -> Option<(u8, bool)> {
    let renderer = renderer();
    let size = Size::new(1120.0, 780.0);
    let mut view = app.view();
    let mut tree = Tree::new(&view);
    let node = view
        .as_widget_mut()
        .layout(&mut tree, &renderer, &layout::Limits::new(size, size));
    let x = 1120.0 - t3::PAGE_GUTTER - 16.0 - 13.0;
    let mut messages = Vec::new();
    for y in (60..400).step_by(6) {
        let position = Point::new(x, y as f32);
        for event in [
            mouse::Event::CursorMoved { position },
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            view.as_widget_mut().update(
                &mut tree,
                &iced::Event::Mouse(event),
                Layout::new(&node),
                mouse::Cursor::Available(position),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
        }
    }
    messages.into_iter().find_map(|message| match message {
        Message::Edit(Command::MixerMute(index, muted)) => Some((index, muted)),
        _ => None,
    })
}

#[test]
fn mute_icon_toggles_the_draft_without_writing() {
    let (mut app, events, commands) = tests::app();
    app.page = Page::Mixer;
    for muted in [false, true] {
        let mut state = tests::state();
        state.mixer[0].muted = Some(muted);
        events
            .send(Event::State(state, Completion::Refresh))
            .unwrap();
        app.events();
        assert_eq!(click_mute(&app), Some((1, !muted)));
        let _ = app.update(Message::Edit(Command::MixerMute(1, !muted)));
        assert_eq!(click_mute(&app), Some((1, muted)));
        assert!(commands.try_recv().is_err());
    }
}

#[test]
fn sidebar_only_navigates_and_never_edits_device_settings() {
    load_fonts();
    let mut app = App::demo();
    app.offline = false;
    let renderer = renderer();
    let size = Size::new(1120.0, 780.0);
    let mut view = app.view();
    let mut tree = Tree::new(&view);
    let node = view
        .as_widget_mut()
        .layout(&mut tree, &renderer, &layout::Limits::new(size, size));
    let mut messages = Vec::new();
    for y in (48..760).step_by(6) {
        let cursor = mouse::Cursor::Available(Point::new(100.0, y as f32));
        for event in [
            mouse::Event::CursorMoved {
                position: Point::new(100.0, y as f32),
            },
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            view.as_widget_mut().update(
                &mut tree,
                &iced::Event::Mouse(event),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
        }
    }
    assert!(
        messages
            .iter()
            .any(|message| matches!(message, Message::Page(Page::Device)))
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, Message::Edit(_) | Message::Apply))
    );
}
