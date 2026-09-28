//! `glassine`: argument handling, diagnostics, and the window host.

#![windows_subsystem = "windows"]

use glassine::app::App;
use glassine::config::{Config, Overrides};
use glassine::logging;
use glassine::platform::win::{self, LayeredWindow};
use glassine::window_state;
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
    /// Runs a real window for this many milliseconds, then exits. Exists so a
    /// test can drive the window without a user.
    smoke_ms: Option<u64>,
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

    let state_path = window_state::state_path();
    if args.check_config {
        print!("{}", report(&config, &path, &state_path));
        return Ok(());
    }

    run_window(config, state_path, args.smoke_ms)
}

fn run_window(config: Config, state_path: PathBuf, smoke_ms: Option<u64>) -> Result<(), String> {
    let monitors = win::enumerate_monitors();
    let (work_area, device_name) = win::resolve_work_area(&config.window.monitor, &monitors);

    // Spec 10.4: a saved origin outranks the configured anchor, and the log
    // records which of the two won.
    let saved = window_state::load(&state_path);
    let (rect, source) = window_state::resolve(
        config.window.anchor,
        config.window.offset,
        config.window.size,
        work_area,
        saved,
    );

    let window = LayeredWindow::create(&config.window, rect)
        .map_err(|error| format!("cannot create the window: {error}"))?;
    log::info!(
        "window created hwnd={:?} dpi={} monitor={} work_area={},{},{},{} rect={},{},{},{} position_source={:?}",
        window.hwnd(),
        window.dpi(),
        device_name,
        work_area.x,
        work_area.y,
        work_area.width,
        work_area.height,
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        source
    );

    let mut app = App::new(config, window, state_path);
    app.set_position_source(source);
    let tick_ms = app.tick_interval_ms();
    if let Some(window) = app.window_mut() {
        window.set_tick_interval(tick_ms);
        if let Some(milliseconds) = smoke_ms {
            window.close_after(milliseconds as u32);
        }
    }

    app.redraw();
    app.run();
    app.shutdown();
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
            "--smoke-ms" => {
                let value = flag_value(raw, &mut index)?;
                args.smoke_ms = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--smoke-ms needs a number of milliseconds, got {value:?}"))?,
                );
            }
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

fn report(config: &Config, path: &Path, state_path: &Path) -> String {
    let monitors = win::enumerate_monitors();
    let (work_area, device_name) = win::resolve_work_area(&config.window.monitor, &monitors);
    let (rect, source) = window_state::resolve(
        config.window.anchor,
        config.window.offset,
        config.window.size,
        work_area,
        window_state::load(state_path),
    );

    let mut out = String::new();
    let _ = writeln!(out, "config={} mtime={}", path.display(), mtime(path));
    let _ = writeln!(
        out,
        "monitor={} work_area={},{},{},{}",
        device_name, work_area.x, work_area.y, work_area.width, work_area.height
    );
    let _ = writeln!(
        out,
        "rect={},{},{},{} position_source={}",
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        match source {
            window_state::PositionSource::Config => "config",
            window_state::PositionSource::Override => "override",
        }
    );
    let _ = writeln!(out, "port={}", config.server.port);
    let _ = writeln!(out, "font={}", config.text.family);
    out
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
