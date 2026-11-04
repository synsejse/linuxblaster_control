use serde_json::{Map, Value, json};
use soundblaster_x4::{Output, open};
use std::error::Error;

const USAGE: &str = "Linuxblaster Control — Sound Blaster X4
Usage: linuxblaster_control [--port /dev/serial/by-id/...] [--status | --profiles]

Without a query option, opens the GUI. --demo opens an offline preview (no device access). --status and --profiles print JSON.
Queries may unlock the control channel but do not change audio settings.
Only one controller can own the X4 serial connection at a time.";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Query {
    Status,
    Profiles,
}

#[derive(Debug, Default, PartialEq)]
struct Options {
    port: Option<String>,
    query: Option<Query>,
    help: bool,
    demo: bool,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => options.demo = true,
            "--help" | "-h" => options.help = true,
            "--port" => {
                options.port = Some(
                    args.next()
                        .filter(|s| !s.starts_with('-'))
                        .ok_or("--port requires a path")?,
                );
            }
            "--status" if options.query.is_none() => {
                options.query = Some(Query::Status);
            }
            "--profiles" if options.query.is_none() => {
                options.query = Some(Query::Profiles);
            }
            _ => return Err(format!("Unknown or conflicting argument: {arg}")),
        }
    }
    if options.demo && (options.query.is_some() || options.port.is_some()) {
        return Err("--demo cannot be combined with device arguments".into());
    }
    Ok(options)
}

/// Read the device and print the answer to `query` as JSON.
fn print_query(query: Query, port: Option<&str>) -> Result<(), Box<dyn Error>> {
    let mut device = open(port)?;
    let value = match query {
        Query::Status => serde_json::to_value(device.state()?)?,
        Query::Profiles => {
            let mut outputs = Map::new();
            for (name, output) in [
                ("speaker", Output::Speaker),
                ("headphone", Output::Headphone),
            ] {
                let mut profiles = Vec::new();
                for info in device.eq_profiles(output)? {
                    let eq = device.stored_eq_profile(output, info.index)?;
                    profiles.push(json!({"index":info.index,"name":info.name,"eq":eq}));
                }
                outputs.insert(name.into(), Value::Array(profiles));
            }
            Value::Object(outputs)
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let options = parse_args(std::env::args().skip(1))?;
    if options.help {
        println!("{USAGE}");
        return Ok(());
    }
    if let Some(query) = options.query {
        return print_query(query, options.port.as_deref());
    }
    if options.demo {
        linuxblaster_control::run_demo()?;
    } else {
        linuxblaster_control::run(options.port)?;
    }
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
        let parse = |args: &[&str]| parse_args(args.iter().map(ToString::to_string));
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
