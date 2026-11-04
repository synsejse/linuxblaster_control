mod app;
mod curve;
use soundblaster_x4::{Output, open};

const USAGE: &str = "Linuxblaster Control — Sound Blaster X4
Usage: linuxblaster_control [--port /dev/serial/by-id/...] [--status | --profiles]

Without a query option, opens the GUI. --demo opens an offline preview (no device access). --status and --profiles print JSON.
Queries may unlock the control channel but do not change audio settings.
Only one controller can own the X4 serial connection at a time.";

#[derive(Debug, PartialEq)]
struct Options {
    port: Option<String>,
    query: Option<String>,
    help: bool,
    demo: bool,
}
fn parse_args(
    mut args: impl Iterator<Item = String>,
) -> Result<Options, String> {
    let mut options = Options {
        port: None,
        query: None,
        help: false,
        demo: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => options.demo = true,
            "--help" | "-h" => options.help = true,
            "--port" => {
                options.port = Some(
                    args.next()
                        .filter(|s| !s.starts_with('-'))
                        .ok_or("--port requires a path")?,
                )
            }
            "--status" | "--profiles" if options.query.is_none() => {
                options.query = Some(arg)
            }
            _ => return Err(format!("Unknown or conflicting argument: {arg}")),
        }
    }
    if options.demo && (options.query.is_some() || options.port.is_some()) {
        return Err("--demo cannot be combined with device arguments".into());
    }
    Ok(options)
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let options = parse_args(std::env::args().skip(1))?;
    if options.help {
        println!("{USAGE}");
        return Ok(());
    }
    if let Some(query) = options.query {
        let mut device =
            open(options.port.as_deref()).map_err(|e| e.to_string())?;
        let value = if query == "--status" {
            serde_json::to_value(device.state().map_err(|e| e.to_string())?)?
        } else {
            let mut outputs = serde_json::Map::new();
            for (name, output) in [
                ("speaker", Output::Speaker),
                ("headphone", Output::Headphone),
            ] {
                let mut profiles = Vec::new();
                for info in
                    device.eq_profiles(output).map_err(|e| e.to_string())?
                {
                    let eq = device
                        .stored_eq_profile(output, info.index)
                        .map_err(|e| e.to_string())?;
                    profiles.push(serde_json::json!({"index":info.index,"name":info.name,"eq":eq}));
                }
                outputs.insert(name.into(), serde_json::Value::Array(profiles));
            }
            serde_json::Value::Object(outputs)
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let icon = image::load_from_memory(include_bytes!(
        "../../../packaging/linuxblaster-control.png"
    ))?
    .into_rgba8();
    let (width, height) = icon.dimensions();
    let icon = iced::window::icon::from_rgba(icon.into_raw(), width, height)?;
    iced::application(
        move || {
            if options.demo {
                app::X4App::demo()
            } else {
                app::X4App::new(options.port.clone())
            }
        },
        app::X4App::update,
        app::X4App::view,
    )
    .title("Linuxblaster Control — X4")
    .theme(|_: &app::X4App| iced_t3::theme())
    .subscription(app::X4App::subscription)
    .settings(iced_t3::settings())
    .font(iced_aw::ICED_AW_FONT_BYTES)
    .window(iced::window::Settings {
        size: iced::Size::new(1120.0, 900.0),
        min_size: Some(iced::Size::new(860.0, 640.0)),
        icon: Some(icon),
        // Matches the desktop entry, so the taskbar shows the app's icon.
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: "linuxblaster-control".into(),
            ..Default::default()
        },
        ..iced_t3::chrome::window_settings()
    })
    .run()?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_rejects_conflicts_and_missing_port() {
        let parse =
            |args: &[&str]| parse_args(args.iter().map(|s| s.to_string()));
        assert!(parse(&["--demo", "--status"]).is_err());
        assert!(parse(&["--demo", "--port", "/dev/ttyACM0"]).is_err());
        assert!(parse(&["--demo"]).unwrap().demo);
        assert!(parse(&["--port"]).is_err());
        assert!(parse(&["--port", "--status"]).is_err());
        assert!(parse(&["--status", "--profiles"]).is_err());
        assert_eq!(
            parse(&["--port", "/dev/ttyACM0", "--status"])
                .unwrap()
                .port
                .as_deref(),
            Some("/dev/ttyACM0")
        );
    }
}
