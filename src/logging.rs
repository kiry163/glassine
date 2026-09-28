//! File logging with size-based rotation, plus the panic hook that gives a
//! GUI-subsystem crash a record.
//!
//! Lines go straight to the `File` with no `BufWriter`. A release build aborts
//! on panic, and a buffer would hold exactly the lines that matter at that
//! moment.

use crate::config::LogLevel;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Rotate once the live file would pass this.
pub const MAX_LOG_BYTES: u64 = 1 << 20;
/// `glassine.log`, `glassine.log.1`, `glassine.log.2`.
pub const KEEP_FILES: usize = 3;

/// `OnceLock` rather than `LazyLock`: the sink's initializer needs runtime input
/// (the resolved path and level) and its failure has to reach `init`'s caller,
/// which `LazyLock`'s infallible initializer cannot express.
static LOGGER: OnceLock<FileLogger> = OnceLock::new();

/// `%LOCALAPPDATA%\glassine\logs\glassine.log`.
pub fn log_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("glassine").join("logs").join("glassine.log")
}

/// Installs the rotating file logger and the panic hook.
///
/// Returns an error only if the log file cannot be opened, which the caller is
/// expected to report and then ignore: a sticky note must still run on a
/// machine where `%LOCALAPPDATA%` is not writable.
pub fn init(level: LogLevel) -> io::Result<()> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let sink = FileSink::open(&path, MAX_LOG_BYTES)?;
    let filter = level_filter(level);

    let logger = LOGGER.get_or_init(|| FileLogger { sink: Mutex::new(sink), level: filter });
    // A second call is not an error: the previous logger stays installed.
    let _ = log::set_logger(logger);
    log::set_max_level(filter);

    install_panic_hook();
    Ok(())
}

fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Error => LevelFilter::Error,
        LogLevel::Warn => LevelFilter::Warn,
        LogLevel::Info => LevelFilter::Info,
        LogLevel::Debug => LevelFilter::Debug,
        LogLevel::Trace => LevelFilter::Trace,
    }
}

struct FileLogger {
    sink: Mutex<FileSink>,
    level: LevelFilter,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {} {}",
            timestamp(),
            record.level(),
            record.target(),
            record.args()
        );
        // A console-launched run should be readable without tailing a file.
        if record.level() <= Level::Warn {
            eprintln!("{line}");
        }
        // A poisoned lock means some other thread panicked mid-write; the log
        // is a diagnostic, not a correctness requirement, so drop the line
        // rather than propagating a panic of our own.
        if let Ok(mut sink) = self.sink.lock() {
            let _ = sink.write_line(&line);
        }
    }

    fn flush(&self) {}
}

/// Logs the panic at `error` before the process aborts.
///
/// The release profile sets `panic = "abort"`, so the hook is the last code to
/// run: without it a panic in a GUI-subsystem build leaves no console output
/// and no trace, and the window simply disappears.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_message(info);
        // Written through the sink directly rather than `log::error!`: the
        // panic may have happened while the sink lock was held, and blocking on
        // that lock here would hang the hook. `try_lock` gives up instead.
        if let Some(logger) = LOGGER.get() {
            if let Ok(mut sink) = logger.sink.try_lock() {
                let _ = sink.write_line(&format!(
                    "{} {:<5} panic {message}",
                    timestamp(),
                    Level::Error
                ));
            }
        }
        eprintln!("glassine: panic: {message}");
        previous(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let location = info
        .location()
        .map(|location| format!("{}:{}:{}", location.file(), location.line(), location.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string());
    format!("{location}: {payload}")
}

fn timestamp() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "-".to_string())
}

/// An append-only log file that rotates itself once it would pass `max_bytes`.
pub struct FileSink {
    path: PathBuf,
    /// `None` only while rotating: Windows refuses to rename a file that still
    /// has an open handle, so the sink has to let go of it first.
    file: Option<File>,
    written: u64,
    max_bytes: u64,
}

impl FileSink {
    /// Opens `path` for appending without creating its parent directory, so a
    /// missing directory is reported to the caller instead of being papered
    /// over.
    pub fn open(path: &Path, max_bytes: u64) -> io::Result<FileSink> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let written = file.metadata()?.len();
        Ok(FileSink {
            path: path.to_path_buf(),
            file: Some(file),
            written,
            max_bytes,
        })
    }

    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        let bytes = line.len() as u64 + 1;
        if self.written + bytes > self.max_bytes {
            self.rotate()?;
        }
        let file = self.handle()?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        self.written += bytes;
        Ok(())
    }

    fn handle(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            self.file = Some(OpenOptions::new().create(true).append(true).open(&self.path)?);
        }
        Ok(self.file.as_mut().expect("just opened"))
    }

    /// Drops the oldest archive, shifts `log.N` to `log.N+1`, and starts a fresh
    /// live file, so the set stays at `log`, `log.1` .. `log.KEEP_FILES-1`.
    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let _ = fs::remove_file(archive_path(&self.path, KEEP_FILES - 1));
        for index in (1..KEEP_FILES).rev() {
            let from = archive_path(&self.path, index - 1);
            if from.exists() {
                fs::rename(&from, archive_path(&self.path, index))?;
            }
        }
        self.written = 0;
        Ok(())
    }
}

fn archive_path(path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return path.to_path_buf();
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_three_files_and_drops_the_oldest() {
        let dir = std::env::temp_dir().join("glassine-plan-rotation");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("glassine.log");

        // Four writes of MAX_BYTES each must leave log, log.1, log.2 -- and no log.3.
        let mut sink = FileSink::open(&path, MAX_LOG_BYTES).unwrap();
        for _ in 0..4 {
            sink.write_line(&"x".repeat(MAX_LOG_BYTES as usize)).unwrap();
        }

        assert!(path.exists());
        assert!(dir.join("glassine.log.1").exists());
        assert!(dir.join("glassine.log.2").exists());
        assert!(!dir.join("glassine.log.3").exists());
    }

    #[test]
    fn writing_never_panics_when_the_directory_is_missing() {
        let path = std::env::temp_dir().join("glassine-plan-nonexistent/nested/x.log");
        assert!(FileSink::open(&path, MAX_LOG_BYTES).is_err());
    }
}
