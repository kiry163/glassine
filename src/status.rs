//! The snapshot `/status` reports, and the wire spelling of the enums it
//! carries.
//!
//! The enums' `Serialize` impls live here rather than beside their types: the
//! JSON vocabulary is this module's subject, and keeping it here leaves
//! `content` and `window_state` free of any dependency on serde.

use crate::content::Mode;
use crate::geometry::Rect;
use crate::window_state::PositionSource;
use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

/// What the window thread publishes for `/status` to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub mode: Mode,
    /// The window's current physical rectangle, not the configured one: a drag
    /// or a DPI change moves it, and the answer has to be where it really is.
    pub window: Rect,
    pub monitor_name: String,
    pub dpi: u32,
    pub truncated: bool,
    pub text_bytes: usize,
    pub position_source: PositionSource,
}

/// The lowercase spelling `mode` is reported as.
pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Time => "time",
        Mode::Text => "text",
        Mode::Blank => "blank",
    }
}

impl Serialize for StatusSnapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// Serialised from the four numbers so that `geometry::Rect` stays free
        /// of serde.
        #[derive(Serialize)]
        struct Window {
            x: i32,
            y: i32,
            width: u32,
            height: u32,
        }

        /// `monitor` is nested in the JSON while the snapshot keeps the two
        /// facts flat, because that is the shape the spec documents.
        #[derive(Serialize)]
        struct Monitor<'a> {
            name: &'a str,
            dpi: u32,
        }

        let mut state = serializer.serialize_struct("StatusSnapshot", 6)?;
        state.serialize_field("mode", mode_name(self.mode))?;
        state.serialize_field(
            "window",
            &Window {
                x: self.window.x,
                y: self.window.y,
                width: self.window.width,
                height: self.window.height,
            },
        )?;
        state.serialize_field(
            "monitor",
            &Monitor { name: &self.monitor_name, dpi: self.dpi },
        )?;
        state.serialize_field("truncated", &self.truncated)?;
        state.serialize_field("text_bytes", &self.text_bytes)?;
        state.serialize_field("position_source", &self.position_source)?;
        state.end()
    }
}

impl Serialize for PositionSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            PositionSource::Config => "config",
            PositionSource::Override => "override",
        })
    }
}
