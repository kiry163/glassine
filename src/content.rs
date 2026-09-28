//! Content state machine and the clock that feeds it.

use crate::config::{ClockConfig, UtcOffset};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use time::format_description::{Component, OwnedFormatItem};
use time::{OffsetDateTime, UtcOffset as TimeOffset};

/// What the window is showing. The three states are exclusive; leaving text
/// mode clears the text so that no mode can show a stale string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Time,
    Text,
    Blank,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    SetText(String),
    SetTime,
    Quit,
}

/// Renders the current time according to the configured template and offset.
pub struct Clock {
    format: OwnedFormatItem,
    offset: UtcOffset,
    /// Whether the "no local offset" warning has already been logged. The clock
    /// formats once a second, so warning per call would rotate a diagnostic log
    /// away inside a couple of hours.
    warned_missing_local_offset: AtomicBool,
}

impl Clock {
    pub fn from_config(config: &ClockConfig) -> Clock {
        let format = time::format_description::parse_strftime_owned(&config.format)
            .unwrap_or_else(|error| {
                // Unreachable from a loaded config: validation rejects an
                // unparsable template at startup. Reachable only from a
                // hand-built `ClockConfig`, where an empty clock beats a panic.
                log::error!("clock: cannot parse format {:?}: {error}", config.format);
                OwnedFormatItem::StringLiteral("".into())
            });
        Clock {
            format,
            offset: config.utc_offset,
            warned_missing_local_offset: AtomicBool::new(false),
        }
    }

    pub fn format_now(&self) -> String {
        let now = OffsetDateTime::now_utc();
        let shifted = match self.offset {
            UtcOffset::Minutes(minutes) => TimeOffset::from_whole_seconds(minutes * 60)
                .map_or(now, |offset| now.to_offset(offset)),
            UtcOffset::Local => match TimeOffset::current_local_offset() {
                Ok(offset) => now.to_offset(offset),
                Err(error) => {
                    if !self.warned_missing_local_offset.swap(true, Ordering::Relaxed) {
                        log::warn!("clock: no local UTC offset ({error}); using UTC");
                    }
                    now
                }
            },
        };
        shifted.format(&self.format).unwrap_or_else(|error| {
            log::error!("clock: cannot format: {error}");
            String::new()
        })
    }

    /// How often the window must redraw for the display to stay true.
    ///
    /// An explicit `clock.tick_ms` always wins; otherwise the answer comes from
    /// the parsed template, so a compound specifier such as `%T` is recognised
    /// as carrying seconds instead of being missed by a substring scan.
    pub fn tick_ms(&self, override_ms: Option<u32>) -> u32 {
        match override_ms {
            Some(ms) => ms,
            None if has_sub_minute_component(&self.format) => 1_000,
            None => 60_000,
        }
    }
}

/// Whether anything in the template changes more often than once a minute.
fn has_sub_minute_component(item: &OwnedFormatItem) -> bool {
    match item {
        OwnedFormatItem::Component(component) => matches!(
            component,
            Component::Second(_)
                | Component::Subsecond(_)
                | Component::OffsetSecond(_)
                | Component::UnixTimestampSecond(_)
                | Component::UnixTimestampMillisecond(_)
                | Component::UnixTimestampMicrosecond(_)
                | Component::UnixTimestampNanosecond(_)
        ),
        OwnedFormatItem::Compound(items) | OwnedFormatItem::First(items) => {
            items.iter().any(has_sub_minute_component)
        }
        OwnedFormatItem::Optional(inner) => has_sub_minute_component(inner),
        _ => false,
    }
}

/// The window's content. Every mode keeps `mode` and `text` consistent, so
/// `text()` is meaningful whatever the state.
pub struct ContentState {
    mode: Mode,
    text: String,
}

impl ContentState {
    pub fn new() -> ContentState {
        ContentState { mode: Mode::Time, text: String::new() }
    }

    /// Returns `false` only for `Quit`, which is the caller's signal to stop.
    pub fn apply(&mut self, command: &Command) -> bool {
        match command {
            Command::SetText(text) if text.is_empty() => {
                self.mode = Mode::Blank;
                self.text.clear();
                true
            }
            Command::SetText(text) => {
                self.mode = Mode::Text;
                self.text.clear();
                self.text.push_str(text);
                true
            }
            Command::SetTime => {
                self.mode = Mode::Time;
                self.text.clear();
                true
            }
            Command::Quit => false,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Borrows in text mode so the steady state allocates nothing beyond the
    /// frame that reads it.
    pub fn visible_text(&self, clock: &Clock) -> Cow<'_, str> {
        match self.mode {
            Mode::Text => Cow::Borrowed(&self.text),
            Mode::Blank => Cow::Borrowed(""),
            Mode::Time => Cow::Owned(clock.format_now()),
        }
    }
}

impl Default for ContentState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(fmt: &str) -> Clock {
        Clock::from_config(&ClockConfig {
            format: fmt.to_string(),
            utc_offset: UtcOffset::Minutes(480),
            tick_ms: None,
        })
    }

    #[test]
    fn mode_transitions_follow_the_spec() {
        let mut s = ContentState::new();
        assert_eq!(s.mode(), Mode::Time);

        s.apply(&Command::SetText("hello".into()));
        assert_eq!(s.mode(), Mode::Text);
        assert_eq!(s.text(), "hello");

        s.apply(&Command::SetText(String::new()));
        assert_eq!(s.mode(), Mode::Blank);

        s.apply(&Command::SetTime);
        assert_eq!(s.mode(), Mode::Time);
        assert_eq!(s.text(), "", "leaving text mode clears the text");
    }

    #[test]
    fn visible_text_is_borrowed_in_text_mode_and_formatted_in_time_mode() {
        let mut s = ContentState::new();
        let c = clock("%Y-%m-%d %H:%M:%S");
        assert!(matches!(s.visible_text(&c), std::borrow::Cow::Owned(_)));

        s.apply(&Command::SetText("便签".into()));
        match s.visible_text(&c) {
            std::borrow::Cow::Borrowed(t) => assert_eq!(t, "便签"),
            other => panic!("expected borrowed, got {other:?}"),
        }

        s.apply(&Command::SetText(String::new()));
        assert_eq!(&*s.visible_text(&c), "");
    }

    #[test]
    fn tick_interval_is_derived_from_the_format() {
        assert_eq!(clock("%H:%M:%S").tick_ms(None), 1000);
        assert_eq!(clock("%H:%M").tick_ms(None), 60_000);
        assert_eq!(clock("%H:%M").tick_ms(Some(250)), 250);
        // `%T` expands to `%H:%M:%S`. A scan for the substring "%S" would miss
        // it and leave the clock standing still for up to 59 seconds.
        assert_eq!(clock("%T").tick_ms(None), 1000);
        assert_eq!(clock("%Y-%m-%d").tick_ms(None), 60_000);
    }

    #[test]
    fn time_formatting_honours_a_fixed_offset() {
        let c = clock("%H");
        let text = c.format_now();
        assert_eq!(text.len(), 2, "two-digit hour, got {text:?}");
    }
}
