//! Configuration: defaults, TOML parsing, validation, and file location.
//!
//! Parsing is deliberately permissive and validation is entirely ours: every
//! field is read as a wide type (`i64`, `Vec<i64>`, `String`) so that a bad
//! *value* never surfaces as a serde type error with no field name. The spec
//! requires every rejection to name its field and forbids silent fallbacks, so
//! the field name has to survive the round trip.

use crate::geometry::Anchor;
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub window: WindowConfig,
    pub text: TextStyle,
    pub clock: ClockConfig,
    pub server: ServerConfig,
    pub log: LogConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowConfig {
    pub anchor: Anchor,
    pub offset: (i32, i32),
    pub size: (u32, u32),
    pub monitor: MonitorSelector,
    pub opacity: u8,
    pub reassert_topmost_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorSelector {
    Primary,
    Index(usize),
    DeviceName(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub family: String,
    pub size: f32,
    pub weight: u16,
    pub line_height: f32,
    pub color: Rgb,
    pub alpha: u8,
    pub align: Align,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClockConfig {
    pub format: String,
    pub utc_offset: UtcOffset,
    pub tick_ms: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UtcOffset {
    Local,
    /// Fixed offset from UTC, in minutes.
    Minutes(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogConfig {
    pub level: LogLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// A rejected configuration value, naming the dotted field that caused it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub field: String,
    pub message: String,
}

impl ConfigError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self { field: field.into(), message: message.into() }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Upper bound for a dimension, keeping `resolve_rect`'s arithmetic in range.
const MAX_DIMENSION: i64 = 100_000;
const MAX_REASSERT_MS: i64 = 3_600_000;
const MIN_WEIGHT: i64 = 100;
const MAX_WEIGHT: i64 = 900;

impl Config {
    /// The spec's documented defaults, used when the file is absent or a key
    /// is missing.
    pub fn defaults() -> Config {
        Config {
            window: WindowConfig {
                anchor: Anchor::TopCenter,
                offset: (0, 64),
                size: (640, 220),
                monitor: MonitorSelector::Primary,
                opacity: 100,
                reassert_topmost_ms: 5000,
            },
            text: TextStyle {
                family: "Microsoft YaHei".to_string(),
                size: 34.0,
                weight: 400,
                line_height: 44.0,
                color: Rgb { r: 255, g: 255, b: 255 },
                alpha: 230,
                align: Align::Center,
            },
            clock: ClockConfig {
                format: "%H:%M:%S".to_string(),
                utc_offset: UtcOffset::Local,
                tick_ms: None,
            },
            server: ServerConfig { port: 17321 },
            log: LogConfig { level: LogLevel::Info },
        }
    }

    /// Parses and validates a configuration document.
    pub fn from_toml(source: &str) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(source)
            .map_err(|error| ConfigError::new("<config>", error.to_string()))?;
        validate(raw)
    }

    /// Loads the configuration from `path`. A missing file is not an error and
    /// yields the defaults; a file that exists but cannot be read or parsed is.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(source) => Config::from_toml(&source),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::defaults()),
            Err(error) => Err(ConfigError::new(
                "<config>",
                format!("cannot read {}: {error}", path.display()),
            )),
        }
    }

    /// `%APPDATA%\glassine\config.toml`.
    pub fn config_path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("glassine").join("config.toml")
    }
}

/// Environment and command-line overrides, applied on top of the parsed file.
///
/// Values arrive as strings so they pass through exactly the same validators as
/// the file's: `GLASSINE_PORT=loud` fails with field `server.port` rather than
/// falling back to a default.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub port: Option<String>,
    pub log_level: Option<String>,
}

impl Config {
    /// Applies overrides in place, in the spec's order: environment first, then
    /// the command line.
    pub fn apply(&mut self, overrides: &Overrides) -> Result<(), ConfigError> {
        if let Some(raw) = overrides.port.as_deref() {
            let value: i64 = raw.trim().parse().map_err(|_| invalid_port(raw))?;
            self.server.port = parse_port(value)?;
        }
        if let Some(raw) = overrides.log_level.as_deref() {
            self.log.level = parse_log_level(raw)?;
        }
        Ok(())
    }
}

fn parse_port(value: i64) -> Result<u16, ConfigError> {
    if (1..=65535).contains(&value) {
        Ok(value as u16)
    } else {
        Err(ConfigError::new(
            "server.port",
            format!("must be 1-65535, got {value}"),
        ))
    }
}

fn invalid_port(raw: &str) -> ConfigError {
    ConfigError::new("server.port", format!("must be 1-65535, got {raw:?}"))
}

fn parse_log_level(value: &str) -> Result<LogLevel, ConfigError> {
    match value {
        "error" => Ok(LogLevel::Error),
        "warn" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        other => Err(ConfigError::new(
            "log.level",
            format!(
                "expected \"error\", \"warn\", \"info\", \"debug\" or \"trace\"; got {other:?}"
            ),
        )),
    }
}

/// Parses the three accepted monitor spellings.
pub fn parse_monitor(value: &str) -> Result<MonitorSelector, ConfigError> {
    if value == "primary" {
        return Ok(MonitorSelector::Primary);
    }
    if let Ok(index) = value.parse::<usize>() {
        return Ok(MonitorSelector::Index(index));
    }
    if value.starts_with("\\\\.\\") {
        return Ok(MonitorSelector::DeviceName(value.to_string()));
    }
    Err(ConfigError::new(
        "window.monitor",
        format!("expected \"primary\", an index, or a device name; got {value:?}"),
    ))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawConfig {
    window: Option<RawWindow>,
    text: Option<RawText>,
    clock: Option<RawClock>,
    server: Option<RawServer>,
    log: Option<RawLog>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawWindow {
    anchor: Option<String>,
    offset: Option<Vec<i64>>,
    size: Option<Vec<i64>>,
    monitor: Option<String>,
    opacity: Option<i64>,
    reassert_topmost_ms: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawText {
    family: Option<String>,
    size: Option<f64>,
    weight: Option<i64>,
    line_height: Option<f64>,
    color: Option<String>,
    alpha: Option<i64>,
    align: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawClock {
    format: Option<String>,
    utc_offset: Option<String>,
    tick_ms: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawServer {
    port: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawLog {
    level: Option<String>,
}

fn validate(raw: RawConfig) -> Result<Config, ConfigError> {
    let defaults = Config::defaults();
    let window = raw.window.unwrap_or_default();
    let text = raw.text.unwrap_or_default();
    let clock = raw.clock.unwrap_or_default();
    let server = raw.server.unwrap_or_default();
    let log = raw.log.unwrap_or_default();

    Ok(Config {
        window: WindowConfig {
            anchor: match window.anchor.as_deref() {
                None => defaults.window.anchor,
                Some(value) => Anchor::parse(value).ok_or_else(|| {
                    ConfigError::new(
                        "window.anchor",
                        format!("unknown anchor {value:?}"),
                    )
                })?,
            },
            offset: match window.offset {
                None => defaults.window.offset,
                Some(values) => {
                    let pair = pair_i64("window.offset", values)?;
                    if pair[0] < i32::MIN as i64
                        || pair[0] > i32::MAX as i64
                        || pair[1] < i32::MIN as i64
                        || pair[1] > i32::MAX as i64
                    {
                        return Err(ConfigError::new(
                            "window.offset",
                            "offsets must fit in 32 bits",
                        ));
                    }
                    (pair[0] as i32, pair[1] as i32)
                }
            },
            size: match window.size {
                None => defaults.window.size,
                Some(values) => {
                    let pair = pair_i64("window.size", values)?;
                    if pair[0] < 1 || pair[1] < 1 {
                        return Err(ConfigError::new(
                            "window.size",
                            "both dimensions must be at least 1 pixel",
                        ));
                    }
                    if pair[0] > MAX_DIMENSION || pair[1] > MAX_DIMENSION {
                        return Err(ConfigError::new(
                            "window.size",
                            format!("dimensions must not exceed {MAX_DIMENSION} pixels"),
                        ));
                    }
                    (pair[0] as u32, pair[1] as u32)
                }
            },
            monitor: match window.monitor.as_deref() {
                None => defaults.window.monitor,
                Some(value) => parse_monitor(value)?,
            },
            opacity: match window.opacity {
                None => defaults.window.opacity,
                Some(value) if (0..=100).contains(&value) => value as u8,
                Some(value) => {
                    return Err(ConfigError::new(
                        "window.opacity",
                        format!("must be 0-100, got {value}"),
                    ))
                }
            },
            reassert_topmost_ms: match window.reassert_topmost_ms {
                None => defaults.window.reassert_topmost_ms,
                Some(value) if (0..=MAX_REASSERT_MS).contains(&value) => value as u32,
                Some(value) => {
                    return Err(ConfigError::new(
                        "window.reassert_topmost_ms",
                        format!("must be 0-{MAX_REASSERT_MS} (0 disables it), got {value}"),
                    ))
                }
            },
        },
        text: TextStyle {
            family: match text.family {
                None => defaults.text.family,
                Some(value) if value.trim().is_empty() => {
                    return Err(ConfigError::new("text.family", "must not be empty"))
                }
                Some(value) => value,
            },
            size: positive_f32("text.size", text.size, defaults.text.size)?,
            weight: match text.weight {
                None => defaults.text.weight,
                Some(value) if (MIN_WEIGHT..=MAX_WEIGHT).contains(&value) => value as u16,
                Some(value) => {
                    return Err(ConfigError::new(
                        "text.weight",
                        format!("must be {MIN_WEIGHT}-{MAX_WEIGHT}, got {value}"),
                    ))
                }
            },
            line_height: positive_f32("text.line_height", text.line_height, defaults.text.line_height)?,
            color: match text.color.as_deref() {
                None => defaults.text.color,
                Some(value) => parse_color(value)?,
            },
            alpha: match text.alpha {
                None => defaults.text.alpha,
                Some(value) if (0..=255).contains(&value) => value as u8,
                Some(value) => {
                    return Err(ConfigError::new(
                        "text.alpha",
                        format!("must be 0-255, got {value}"),
                    ))
                }
            },
            align: match text.align.as_deref() {
                None => defaults.text.align,
                Some("left") => Align::Left,
                Some("center") => Align::Center,
                Some("right") => Align::Right,
                Some(value) => {
                    return Err(ConfigError::new(
                        "text.align",
                        format!("expected \"left\", \"center\" or \"right\"; got {value:?}"),
                    ))
                }
            },
        },
        clock: ClockConfig {
            format: match clock.format {
                None => defaults.clock.format,
                Some(value) => {
                    validate_clock_format(&value)?;
                    value
                }
            },
            utc_offset: match clock.utc_offset.as_deref() {
                None => defaults.clock.utc_offset,
                Some(value) => parse_utc_offset(value)?,
            },
            tick_ms: match clock.tick_ms {
                None => None,
                Some(value) if (1..=MAX_REASSERT_MS).contains(&value) => Some(value as u32),
                Some(value) => {
                    return Err(ConfigError::new(
                        "clock.tick_ms",
                        format!("must be 1-{MAX_REASSERT_MS}, got {value}"),
                    ))
                }
            },
        },
        server: ServerConfig {
            port: match server.port {
                None => defaults.server.port,
                Some(value) => parse_port(value)?,
            },
        },
        log: LogConfig {
            level: match log.level.as_deref() {
                None => defaults.log.level,
                Some(value) => parse_log_level(value)?,
            },
        },
    })
}

fn pair_i64(field: &str, values: Vec<i64>) -> Result<[i64; 2], ConfigError> {
    if values.len() != 2 {
        return Err(ConfigError::new(
            field,
            format!("expected exactly two numbers, got {}", values.len()),
        ));
    }
    Ok([values[0], values[1]])
}

fn positive_f32(field: &str, value: Option<f64>, default: f32) -> Result<f32, ConfigError> {
    match value {
        None => Ok(default),
        Some(value) if value.is_finite() && value > 0.0 && value <= MAX_DIMENSION as f64 => {
            Ok(value as f32)
        }
        Some(value) => Err(ConfigError::new(
            field,
            format!("must be a positive number up to {MAX_DIMENSION}, got {value}"),
        )),
    }
}

/// Accepts `#RRGGBB` only. Alpha has its own key, so `#RRGGBBAA` is rejected
/// rather than silently ignored.
fn parse_color(value: &str) -> Result<Rgb, ConfigError> {
    let hex = value.strip_prefix('#').ok_or_else(|| {
        ConfigError::new("text.color", format!("expected #RRGGBB, got {value:?}"))
    })?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ConfigError::new(
            "text.color",
            format!("expected #RRGGBB (alpha has its own key), got {value:?}"),
        ));
    }
    let byte = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).unwrap_or(0);
    Ok(Rgb { r: byte(0), g: byte(2), b: byte(4) })
}

/// A strftime template must be non-empty and must parse.
///
/// Parsing up front is what turns an unsupported specifier into a startup
/// failure with a field name, instead of a failure the first time the clock
/// tries to format itself.
fn validate_clock_format(value: &str) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Err(ConfigError::new("clock.format", "must not be empty"));
    }
    time::format_description::parse_strftime_owned(value)
        .map(|_| ())
        .map_err(|error| ConfigError::new("clock.format", error.to_string()))
}

/// Accepts `"local"` or `±HH:MM`.
fn parse_utc_offset(value: &str) -> Result<UtcOffset, ConfigError> {
    if value == "local" {
        return Ok(UtcOffset::Local);
    }
    let invalid = || {
        ConfigError::new(
            "clock.utc_offset",
            format!("expected \"local\" or ±HH:MM, got {value:?}"),
        )
    };
    let bytes = value.as_bytes();
    if bytes.len() != 6 || (bytes[0] != b'+' && bytes[0] != b'-') || bytes[3] != b':' {
        return Err(invalid());
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i32> {
        let text = value.get(range)?;
        text.bytes().all(|b| b.is_ascii_digit()).then(|| text.parse().ok())?
    };
    let hours = digits(1..3).ok_or_else(invalid)?;
    let minutes = digits(4..6).ok_or_else(invalid)?;
    if hours > 14 || minutes > 59 || (hours == 14 && minutes != 0) {
        return Err(invalid());
    }
    let total = hours * 60 + minutes;
    Ok(UtcOffset::Minutes(if bytes[0] == b'-' { -total } else { total }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::defaults();
        assert_eq!(c.window.anchor, Anchor::TopCenter);
        assert_eq!(c.window.offset, (0, 64));
        assert_eq!(c.window.size, (640, 220));
        assert_eq!(c.window.opacity, 100);
        assert_eq!(c.window.reassert_topmost_ms, 5000);
        assert_eq!(c.text.family, "Microsoft YaHei");
        assert_eq!(c.text.size, 34.0);
        assert_eq!(c.text.line_height, 44.0);
        assert_eq!(c.text.alpha, 230);
        assert_eq!(c.text.align, Align::Center);
        assert_eq!(c.clock.format, "%H:%M:%S");
        assert_eq!(c.clock.utc_offset, UtcOffset::Local);
        assert_eq!(c.server.port, 17321);
    }

    #[test]
    fn empty_toml_yields_defaults() {
        assert_eq!(Config::from_toml("").unwrap().server.port, 17321);
    }

    #[test]
    fn every_invalid_field_fails_and_names_itself() {
        let cases: &[(&str, &str)] = &[
            ("[window]\nanchor = \"top-middle\"\n", "window.anchor"),
            ("[window]\nsize = [0, 220]\n", "window.size"),
            ("[window]\noffset = [1]\n", "window.offset"),
            ("[window]\nmonitor = \"display\"\n", "window.monitor"),
            ("[window]\nopacity = 101\n", "window.opacity"),
            ("[text]\ncolor = \"#GGGGGG\"\n", "text.color"),
            ("[text]\ncolor = \"#FFFFFF00\"\n", "text.color"),
            ("[text]\nalpha = 256\n", "text.alpha"),
            ("[text]\nalign = \"justify\"\n", "text.align"),
            ("[text]\nweight = 50\n", "text.weight"),
            ("[text]\nsize = 0\n", "text.size"),
            ("[clock]\nformat = \"\"\n", "clock.format"),
            ("[clock]\nformat = \"%\"\n", "clock.format"),
            ("[clock]\nformat = \"%Q\"\n", "clock.format"),
            ("[clock]\nutc_offset = \"+8\"\n", "clock.utc_offset"),
            ("[server]\nport = 0\n", "server.port"),
            ("[server]\nport = 70000\n", "server.port"),
            ("[log]\nlevel = \"loud\"\n", "log.level"),
        ];
        for (toml, field) in cases {
            let err = Config::from_toml(toml).unwrap_err();
            assert_eq!(&err.field, field, "input: {toml:?}");
            assert!(!err.message.is_empty(), "input: {toml:?}");
        }
    }

    #[test]
    fn valid_edge_values_are_accepted() {
        let c = Config::from_toml(
            "[window]\noffset = [-300, -300]\nopacity = 0\nreassert_topmost_ms = 0\n\
             [text]\nalpha = 0\nalign = \"left\"\n\
             [clock]\nutc_offset = \"+08:00\"\n\n[server]\nport = 65535\n",
        )
        .unwrap();
        assert_eq!(c.window.opacity, 0);
        assert_eq!(c.text.alpha, 0);
        assert_eq!(c.text.align, Align::Left);
        assert_eq!(c.clock.utc_offset, UtcOffset::Minutes(480));
        assert_eq!(c.server.port, 65535);
    }

    #[test]
    fn monitor_selector_parses_its_three_forms() {
        assert_eq!(parse_monitor("primary").unwrap(), MonitorSelector::Primary);
        assert_eq!(parse_monitor("1").unwrap(), MonitorSelector::Index(1));
        assert_eq!(
            parse_monitor("\\\\.\\DISPLAY2").unwrap(),
            MonitorSelector::DeviceName("\\\\.\\DISPLAY2".to_string())
        );
        assert!(parse_monitor("display").is_err());
    }
}
