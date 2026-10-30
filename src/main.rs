mod app;
use linuxblaster_control::{Output, open};

const USAGE: &str = "Linuxblaster Control — Sound Blaster X4
Usage: linuxblaster_control [--port /dev/serial/by-id/...] [--status | --profiles]

Without a query option, opens the GUI. --status and --profiles print JSON.
Queries may unlock the control channel but do not change audio settings.
Only one controller can own the X4 serial connection at a time.";

#[derive(Debug, PartialEq)]
struct Options {
    port: Option<String>,
    query: Option<String>,
    help: bool,
}
fn parse_args(
    mut args: impl Iterator<Item = String>,
) -> Result<Options, String> {
    let mut options = Options {
        port: None,
        query: None,
        help: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            serde_json::json!({
                "speaker": device.stored_profile(Output::Speaker).map_err(|e| e.to_string())?,
                "headphone": device.stored_profile(Output::Headphone).map_err(|e| e.to_string())?,
            })
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let icon = eframe::icon_data::from_png_bytes(include_bytes!(
        "../packaging/linuxblaster-control.png"
    ))?;
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Linuxblaster Control — X4")
            .with_app_id("linuxblaster-control")
            .with_icon(icon)
            .with_inner_size([900.0, 680.0])
            .with_min_inner_size([800.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Linuxblaster Control — X4",
        native,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(eframe::egui::Visuals::dark());
            Ok(Box::new(app::X4App::new(options.port, cc.egui_ctx.clone())))
        }),
    )?;
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
