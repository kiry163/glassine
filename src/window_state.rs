//! The position override file: where the window was last dragged to.

use crate::geometry::{resolve_rect, Anchor, Rect};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Where the window was last dragged to, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPosition {
    pub x: i32,
    pub y: i32,
}

/// Which of the two authorities placed the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionSource {
    Config,
    Override,
}

/// `%LOCALAPPDATA%\glassine\window_state.json`.
pub fn state_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("glassine").join("window_state.json")
}

/// Reads the override, or `None` when it is missing, unreadable, malformed, or
/// missing a field.
///
/// This is a cache, not configuration, so a bad file must never fail a start
/// (spec 10.4). The malformed cases warn so that "my drag did not stick" is
/// answerable from the log.
pub fn load(path: &Path) -> Option<SavedPosition> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // Absence is the normal state, not a problem worth a log line.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::warn!("window_state: cannot read {}: {error}", path.display());
            return None;
        }
    };
    match serde_json::from_str::<SavedPosition>(&text) {
        Ok(saved) => Some(saved),
        Err(error) => {
            log::warn!("window_state: ignoring malformed {}: {error}", path.display());
            None
        }
    }
}

/// Writes the override, creating its parent directory.
///
/// The write goes to a sibling temp file that is then renamed over the target:
/// a crash mid-write would otherwise leave a truncated file, and the next start
/// would treat it as the authoritative position.
pub fn save(path: &Path, position: SavedPosition) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(&position).map_err(io::Error::other)?;

    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(".tmp");
    let temp = PathBuf::from(temp_name);

    std::fs::write(&temp, json)?;
    std::fs::rename(&temp, path)
}

/// Removes the override. A missing file is success, since the caller's intent
/// is simply "no override afterwards".
pub fn clear(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Combines the two authorities: the override wins for the origin, and the
/// anchor decides whenever there is none.
///
/// Size is deliberately not part of the override. `anchor` and `offset` exist
/// to answer "where does the window go when nothing has moved it?", and a saved
/// origin is the record that something did.
pub fn resolve(
    anchor: Anchor,
    offset: (i32, i32),
    size: (u32, u32),
    work_area: Rect,
    saved: Option<SavedPosition>,
) -> (Rect, PositionSource) {
    let anchored = resolve_rect(anchor, offset, size, work_area);
    match saved {
        Some(saved) => (
            Rect { x: saved.x, y: saved.y, ..anchored },
            PositionSource::Override,
        ),
        None => (anchored, PositionSource::Config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect { x: 0, y: 0, width: 1920, height: 1080 }
    }
    const SIZE: (u32, u32) = (640, 220);
    const OFFSET: (i32, i32) = (0, 64);

    #[test]
    fn without_a_saved_position_the_anchor_wins() {
        let (rect, source) = resolve(Anchor::TopCenter, OFFSET, SIZE, area(), None);
        assert_eq!(source, PositionSource::Config);
        assert_eq!((rect.x, rect.y), (640, 64));
    }

    #[test]
    fn a_saved_position_overrides_the_anchor() {
        let (rect, source) = resolve(
            Anchor::TopCenter,
            OFFSET,
            SIZE,
            area(),
            Some(SavedPosition { x: -300, y: 900 }),
        );
        assert_eq!(source, PositionSource::Override);
        // Only the origin is overridden; size always comes from the config.
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (-300, 900, 640, 220));
    }

    #[test]
    fn a_corrupt_file_falls_back_to_the_anchor_without_erroring() {
        let dir = std::env::temp_dir().join("glassine-plan-state-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("window_state.json");

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), None);
        std::fs::write(&path, "{\"x\": 5}").unwrap();
        assert_eq!(load(&path), None);

        let (rect, source) = resolve(Anchor::TopLeft, (0, 0), SIZE, area(), load(&path));
        assert_eq!(source, PositionSource::Config);
        assert_eq!((rect.x, rect.y), (0, 0));
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join("glassine-plan-state-roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("window_state.json");
        let _ = clear(&path);

        save(&path, SavedPosition { x: -12, y: 34 }).unwrap();
        assert_eq!(load(&path), Some(SavedPosition { x: -12, y: 34 }));

        clear(&path).unwrap();
        assert_eq!(load(&path), None);
        assert!(clear(&path).is_ok(), "clearing a missing file is not an error");
    }
}
