//! `glassine`: argument handling and the configuration diagnostic.
//!
//! The window, HTTP server, and tray arrive in later tasks; this binary's job
//! today is to resolve configuration exactly the way the app will and to prove
//! it, via `--check-config`, without creating a window or binding a port.

use glassine::config::{Config, MonitorSelector, Overrides};
use glassine::geometry::resolve_rect;
use glassine::logging;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match run(&raw) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("glassine: {message}");
            ExitCode::from(2)
        }
    }
}

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    port: Option<String>,
    log_level: Option<String>,
    check_config: bool,
}

fn run(raw: &[String]) -> Result<(), String> {
    let args = parse_args(raw)?;
    let (config, path) = resolve(&args)?;

    if let Err(error) = logging::init(config.log.level) {
        // Logging is a diagnostic, not a correctness requirement: a sticky note
        // must still run where the log directory is not writable. Same posture
        // the spec takes for a tray icon that fails to register.
        eprintln!("glassine: cannot start file logging: {error}");
    }
    // The spec's configuration-effective rule depends on this line: it is how
    // "were my edits loaded?" is answered without guessing.
    log::info!(
        "glassine {} start config={} mtime={} port={}",
        env!("CARGO_PKG_VERSION"),
        path.display(),
        mtime(&path),
        config.server.port
    );

    print!("{}", report(&config, &path));
    Ok(())
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--config" => args.config = Some(PathBuf::from(flag_value(raw, &mut index)?)),
            "--port" => args.port = Some(flag_value(raw, &mut index)?.to_string()),
            "--log-level" => args.log_level = Some(flag_value(raw, &mut index)?.to_string()),
            "--check-config" => args.check_config = true,
            other => eprintln!("glassine: ignoring unknown argument {other}"),
        }
        index += 1;
    }
    Ok(args)
}

/// Reads the value that follows the flag at `index`, advancing past it.
fn flag_value<'a>(raw: &'a [String], index: &mut usize) -> Result<&'a str, String> {
    *index += 1;
    raw.get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("{} needs a value", raw[*index - 1]))
}

/// Resolves the configuration in the spec's precedence order: defaults, then
/// the file, then the environment, then the command line. Every layer goes
/// through the same validators, so a bad override fails with the same field
/// name a bad file would.
fn resolve(args: &Args) -> Result<(Config, PathBuf), String> {
    let path = args
        .config
        .clone()
        .or_else(|| std::env::var_os("GLASSINE_CONFIG").map(PathBuf::from))
        .unwrap_or_else(Config::config_path);

    let mut config = Config::load(&path).map_err(|error| error.to_string())?;
    config
        .apply(&Overrides {
            port: std::env::var("GLASSINE_PORT").ok(),
            log_level: std::env::var("GLASSINE_LOG").ok(),
        })
        .map_err(|error| error.to_string())?;
    config
        .apply(&Overrides { port: args.port.clone(), log_level: args.log_level.clone() })
        .map_err(|error| error.to_string())?;

    Ok((config, path))
}

fn report(config: &Config, path: &Path) -> String {
    let work_area = glassine::platform::primary_work_area();
    let rect = resolve_rect(
        config.window.anchor,
        config.window.offset,
        config.window.size,
        work_area,
    );

    let mut out = String::new();
    let _ = writeln!(out, "config={} mtime={}", path.display(), mtime(path));
    let _ = writeln!(
        out,
        "monitor={} work_area={},{},{},{}",
        monitor_label(&config.window.monitor),
        work_area.x,
        work_area.y,
        work_area.width,
        work_area.height
    );
    let _ = writeln!(
        out,
        "rect={},{},{},{} position_source=config",
        rect.x, rect.y, rect.width, rect.height
    );
    let _ = writeln!(out, "port={}", config.server.port);
    let _ = writeln!(out, "font={}", config.text.family);
    out
}

/// A device name only exists once the platform can enumerate monitors (Task 8);
/// until then the selector is reported as written.
fn monitor_label(selector: &MonitorSelector) -> String {
    match selector {
        MonitorSelector::Primary => "primary".to_string(),
        MonitorSelector::Index(index) => format!("index:{index}"),
        MonitorSelector::DeviceName(name) => name.clone(),
    }
}

fn mtime(path: &Path) -> String {
    let modified = std::fs::metadata(path).and_then(|metadata| metadata.modified());
    match modified {
        Ok(stamp) => time::OffsetDateTime::from(stamp)
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "<unformatted>".to_string()),
        Err(_) => "absent".to_string(),
    }
}
