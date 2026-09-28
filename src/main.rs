//! `glassine`: argument handling, diagnostics, and the host for the window and
//! the HTTP server.

#![windows_subsystem = "windows"]

use glassine::app::App;
use glassine::config::{Config, Overrides};
use glassine::http;
use glassine::logging;
use glassine::platform::win::{self, LayeredWindow};
use glassine::window_state;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Configuration was rejected (spec 13).
const EXIT_CONFIG: u8 = 2;
/// The port was taken. The spec forbids silently choosing another one.
const EXIT_PORT: u8 = 3;
/// A second instance, or any other reason the process cannot do its job. The
/// spec fixes 1 for the instance case and leaves the rest to this bucket.
const EXIT_FAILURE: u8 = 1;

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match run(&raw) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("glassine: {}", failure.message);
            ExitCode::from(failure.exit_code)
        }
    }
}

/// A failure with the exit code the spec assigns its category.
struct Failure {
    exit_code: u8,
    message: String,
}

impl Failure {
    fn config(message: impl Into<String>) -> Failure {
        Failure { exit_code: EXIT_CONFIG, message: message.into() }
    }

    /// The port number belongs in the message: this exit path exists so the
    /// user can find the process that holds it.
    fn port(port: u16, error: http::BindError) -> Failure {
        Failure {
            exit_code: EXIT_PORT,
            message: format!("cannot listen on 127.0.0.1:{port}: {error}"),
        }
    }

    fn instance() -> Failure {
        Failure { exit_code: EXIT_FAILURE, message: "已有实例在运行".to_string() }
    }

    fn other(message: impl Into<String>) -> Failure {
        Failure { exit_code: EXIT_FAILURE, message: message.into() }
    }
}

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    port: Option<String>,
    log_level: Option<String>,
    check_config: bool,
    reset_window_position: bool,
    install_autostart: bool,
    uninstall_autostart: bool,
    /// Runs a real window for this many milliseconds, then exits. Exists so a
    /// test can drive the window without a user.
    smoke_ms: Option<u64>,
    /// Moves the window as a drag would, then checks after this many
    /// milliseconds that `present` still agrees about where it is.
    smoke_move_ms: Option<u64>,
}

fn run(raw: &[String]) -> Result<(), Failure> {
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
    // Maintenance flags act and exit without claiming the single-instance
    // mutex, the port, or a window, so they work while glassine is running.
    if args.reset_window_position {
        return clear_position(&state_path);
    }
    if args.install_autostart || args.uninstall_autostart {
        return set_autostart(args.install_autostart, &path);
    }

    run_window(config, state_path, args.smoke_ms, args.smoke_move_ms)
}

/// Deletes the position override (spec 10.1): the only way back to a window
/// placed by the configuration rather than by a drag.
fn clear_position(state_path: &Path) -> Result<(), Failure> {
    window_state::clear(state_path).map_err(|error| {
        Failure::other(format!("cannot remove {}: {error}", state_path.display()))
    })?;
    log::info!("window_state: removed {}", state_path.display());
    Ok(())
}

/// Creates or deletes the Startup shortcut (spec 10.1). Both directions succeed
/// when the file is already in the requested state, so the flags are safe to
/// run twice.
fn set_autostart(install: bool, config: &Path) -> Result<(), Failure> {
    let dir = win::autostart::startup_dir()
        .map_err(|error| Failure::other(format!("cannot locate the startup folder: {error}")))?;
    let link = dir.join(win::autostart::LINK_NAME);

    if install {
        win::autostart::install_into(&dir, config)
            .map_err(|error| Failure::other(format!("cannot create {}: {error}", link.display())))?;
        log::info!("autostart: installed {}", link.display());
    } else {
        win::autostart::uninstall_from(&dir)
            .map_err(|error| Failure::other(format!("cannot remove {}: {error}", link.display())))?;
        log::info!("autostart: removed {}", link.display());
    }
    Ok(())
}

fn run_window(
    config: Config,
    state_path: PathBuf,
    smoke_ms: Option<u64>,
    smoke_move_ms: Option<u64>,
) -> Result<(), Failure> {
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

    // Claimed before the port is bound, so a second instance exits with the
    // code the spec assigns it instead of fighting over the socket (spec 12).
    // The guard lives to the end of this function: dropping it releases the
    // name, and so does process exit, however the process ends.
    let _instance = match win::acquire_single_instance() {
        Ok(guard) => guard,
        Err(win::InstanceError::AlreadyRunning) => return Err(Failure::instance()),
        Err(win::InstanceError::Failed(error)) => {
            return Err(Failure::other(format!("cannot create the instance mutex: {error}")))
        }
    };

    // Bound before the window exists, so a port conflict cannot flash a window
    // and then fail.
    let server = http::Server::bind(config.server.port)
        .map_err(|error| Failure::port(config.server.port, error))?;
    log::info!("http: listening on 127.0.0.1:{}", server.local_port());

    let window = LayeredWindow::create(&config.window, rect)
        .map_err(|error| Failure::config(format!("cannot create the window: {error}")))?;
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

    // The handle travels as an `isize`: no `windows` type crosses to the HTTP
    // thread, which only has to be able to post one message.
    let handle = window.hwnd().0 as isize;
    let mut app = App::new(config, window, state_path);
    app.set_position_source(source);

    match app.install_tray("glassine") {
        Ok(()) => {}
        Err(error) => {
            // Warn rather than error: the app keeps working, and the smoke test
            // treats an ERROR in the log as a failure of the frame path.
            log::warn!("tray: {error}");
        }
    }

    let tick_ms = app.tick_interval_ms();
    if let Some(window) = app.window_mut() {
        window.set_tick_interval(tick_ms);
        if let Some(milliseconds) = smoke_ms {
            window.close_after(milliseconds as u32);
        }
    }

    let commands = app.command_sender();
    let status = app.shared_status();
    let shutdown = Arc::new(AtomicBool::new(false));
    let wake: Box<dyn Fn() + Send> = Box::new(move || win::wake(handle));
    std::thread::spawn(move || server.serve(commands, status, wake, shutdown));

    app.redraw();

    if let Some(milliseconds) = smoke_move_ms {
        // The automated half of spec §15.3 D3 and D9: move the window the way a
        // drag does, redraw, and let the platform say whether `present` still
        // agrees about where the window is.
        if let Some(window) = app.window_mut() {
            window.begin_move_mode();
            let (x, y) = window.origin();
            window.set_position(x + 40, y + 25);
        }
        app.redraw();
        if let Some(window) = app.window_mut() {
            window.check_move_after(milliseconds as u32);
        }
    }

    app.run();

    // Read before `shutdown` destroys the window the verdict lives in.
    if let Some(window) = app.window_mut() {
        if window.move_check_failed() {
            return Err(Failure::other(
                "smoke: the window is not where present draws it".to_string(),
            ));
        }
    }
    app.shutdown();
    Ok(())
}

fn parse_args(raw: &[String]) -> Result<Args, Failure> {
    let mut args = Args::default();
    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--config" => {
                args.config = Some(PathBuf::from(flag_value(raw, &mut index).map_err(Failure::config)?))
            }
            "--port" => args.port = Some(flag_value(raw, &mut index).map_err(Failure::config)?.to_string()),
            "--log-level" => {
                args.log_level = Some(flag_value(raw, &mut index).map_err(Failure::config)?.to_string())
            }
            "--check-config" => args.check_config = true,
            "--reset-window-position" => args.reset_window_position = true,
            "--install-autostart" => args.install_autostart = true,
            "--uninstall-autostart" => args.uninstall_autostart = true,
            "--smoke-ms" => {
                let value = flag_value(raw, &mut index).map_err(Failure::config)?;
                args.smoke_ms = Some(value.parse().map_err(|_| {
                    Failure::config(format!("--smoke-ms needs a number of milliseconds, got {value:?}"))
                })?);
            }
            "--smoke-move-ms" => {
                let value = flag_value(raw, &mut index).map_err(Failure::config)?;
                args.smoke_move_ms = Some(value.parse().map_err(|_| {
                    Failure::config(format!(
                        "--smoke-move-ms needs a number of milliseconds, got {value:?}"
                    ))
                })?);
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
fn resolve(args: &Args) -> Result<(Config, PathBuf), Failure> {
    let path = args
        .config
        .clone()
        .or_else(|| std::env::var_os("GLASSINE_CONFIG").map(PathBuf::from))
        .unwrap_or_else(Config::config_path);

    let mut config = Config::load(&path).map_err(|error| Failure::config(error.to_string()))?;
    config
        .apply(&Overrides {
            port: std::env::var("GLASSINE_PORT").ok(),
            log_level: std::env::var("GLASSINE_LOG").ok(),
        })
        .map_err(|error| Failure::config(error.to_string()))?;
    config
        .apply(&Overrides { port: args.port.clone(), log_level: args.log_level.clone() })
        .map_err(|error| Failure::config(error.to_string()))?;

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
