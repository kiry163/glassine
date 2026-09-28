//! Window geometry: work areas, anchors, and rect resolution.
//!
//! Platform-free by design — the spec forbids `windows` outside `platform`, and
//! this is what lets the placement rules be tested without a desktop.

/// A rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Right edge, exclusive.
    pub fn right(&self) -> i32 {
        self.x.saturating_add(to_i32(self.width))
    }

    /// Bottom edge, exclusive.
    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(to_i32(self.height))
    }

    /// Whether a screen point falls inside the rectangle.
    ///
    /// Used by the move-mode cursor check (plan Task 13).
    #[allow(dead_code)]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// Which edge or centre of a monitor's work area the window is placed against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

/// The vertical half of an anchor.
#[derive(Clone, Copy)]
enum Vertical {
    Top,
    Middle,
    Bottom,
}

/// The horizontal half of an anchor.
#[derive(Clone, Copy)]
enum Horizontal {
    Left,
    Center,
    Right,
}

impl Anchor {
    /// Every anchor, in the order `--check-config` lists them.
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::TopCenter,
        Anchor::TopRight,
        Anchor::MiddleLeft,
        Anchor::MiddleCenter,
        Anchor::MiddleRight,
        Anchor::BottomLeft,
        Anchor::BottomCenter,
        Anchor::BottomRight,
    ];

    /// Parses `"<vertical>-<horizontal>"`, e.g. `"bottom-right"`.
    pub fn parse(value: &str) -> Option<Anchor> {
        use Anchor::*;
        Some(match value {
            "top-left" => TopLeft,
            "top-center" => TopCenter,
            "top-right" => TopRight,
            "middle-left" => MiddleLeft,
            "middle-center" => MiddleCenter,
            "middle-right" => MiddleRight,
            "bottom-left" => BottomLeft,
            "bottom-center" => BottomCenter,
            "bottom-right" => BottomRight,
            _ => return None,
        })
    }

    /// The spelling `parse` accepts.
    pub fn as_str(&self) -> &'static str {
        match self {
            Anchor::TopLeft => "top-left",
            Anchor::TopCenter => "top-center",
            Anchor::TopRight => "top-right",
            Anchor::MiddleLeft => "middle-left",
            Anchor::MiddleCenter => "middle-center",
            Anchor::MiddleRight => "middle-right",
            Anchor::BottomLeft => "bottom-left",
            Anchor::BottomCenter => "bottom-center",
            Anchor::BottomRight => "bottom-right",
        }
    }

    fn parts(&self) -> (Vertical, Horizontal) {
        use Anchor::*;
        match self {
            TopLeft => (Vertical::Top, Horizontal::Left),
            TopCenter => (Vertical::Top, Horizontal::Center),
            TopRight => (Vertical::Top, Horizontal::Right),
            MiddleLeft => (Vertical::Middle, Horizontal::Left),
            MiddleCenter => (Vertical::Middle, Horizontal::Center),
            MiddleRight => (Vertical::Middle, Horizontal::Right),
            BottomLeft => (Vertical::Bottom, Horizontal::Left),
            BottomCenter => (Vertical::Bottom, Horizontal::Center),
            BottomRight => (Vertical::Bottom, Horizontal::Right),
        }
    }
}

/// Resolves an anchor plus an offset into the window's screen rectangle.
///
/// The offset is in screen coordinates for every anchor: `+x` right, `+y` down.
/// Negative results are permitted (a window may sit off the work area), and an
/// oversized window is not clamped — the spec allows both and relies on
/// `--check-config` and `GET /status` to make them visible.
pub fn resolve_rect(
    anchor: Anchor,
    offset: (i32, i32),
    size: (u32, u32),
    work_area: Rect,
) -> Rect {
    let (vertical, horizontal) = anchor.parts();

    let width = to_i32(size.0);
    let height = to_i32(size.1);
    let area_width = to_i32(work_area.width);
    let area_height = to_i32(work_area.height);

    let right_edge = work_area.right();
    let bottom_edge = work_area.bottom();

    let x = match horizontal {
        Horizontal::Left => work_area.x,
        Horizontal::Center => work_area.x.saturating_add((area_width - width) / 2),
        Horizontal::Right => right_edge.saturating_sub(width),
    };
    let y = match vertical {
        Vertical::Top => work_area.y,
        Vertical::Middle => work_area.y.saturating_add((area_height - height) / 2),
        Vertical::Bottom => bottom_edge.saturating_sub(height),
    };

    Rect {
        x: x.saturating_add(offset.0),
        y: y.saturating_add(offset.1),
        width: size.0,
        height: size.1,
    }
}

/// Narrows a `u32` dimension to `i32` without wrapping. The config validator
/// rejects absurd sizes in Task 2; this keeps the arithmetic honest regardless.
fn to_i32(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect { Rect { x: 100, y: 50, width: 1920, height: 1040 } }

    #[test]
    fn anchor_parse_accepts_all_nine_and_rejects_junk() {
        for anchor in Anchor::ALL {
            assert_eq!(Anchor::parse(anchor.as_str()), Some(anchor));
        }
        assert_eq!(Anchor::parse("top-middle"), None);
        assert_eq!(Anchor::parse(""), None);
    }

    #[test]
    fn resolve_rect_applies_offset_in_screen_coordinates() {
        let r = resolve_rect(Anchor::TopLeft, (0, 0), (640, 220), area());
        assert_eq!((r.x, r.y, r.width, r.height), (100, 50, 640, 220));

        // +x right, +y down, for every anchor: bottom-right with a negative
        // offset moves inward.
        let r = resolve_rect(Anchor::BottomRight, (-20, -20), (640, 220), area());
        assert_eq!((r.x, r.y), (100 + 1920 - 640 - 20, 50 + 1040 - 220 - 20));

        let r = resolve_rect(Anchor::MiddleCenter, (5, -5), (100, 40), area());
        assert_eq!((r.x, r.y), (100 + 960 - 50 + 5, 50 + 520 - 20 - 5));
    }

    #[test]
    fn resolve_rect_allows_negative_positions_and_oversized_windows() {
        let r = resolve_rect(Anchor::TopLeft, (-500, -500), (640, 220), area());
        assert_eq!((r.x, r.y), (-400, -450));

        // A window larger than the work area must not panic or wrap.
        let r = resolve_rect(Anchor::BottomRight, (0, 0), (4000, 3000), area());
        assert_eq!((r.x, r.y), (100 + 1920 - 4000, 50 + 1040 - 3000));
    }
}
