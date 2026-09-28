//! Text shaping, line breaking, alignment, and glyph rasterisation.

use crate::config::{Align, Rgb, TextStyle};
use crate::render::blit_coverage;
use cosmic_text::{
    Attrs, AttrsOwned, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache,
    SwashContent, Weight,
};

/// A glyph positioned in viewport coordinates, with the cache key needed to
/// rasterise it.
#[derive(Clone, Copy, Debug)]
pub struct PlacedGlyph {
    pub x: i32,
    pub y: i32,
    pub key: CacheKey,
}

/// Shapes text and rasterises its glyphs.
///
/// The glyphs are rasterised through `SwashCache::get_image` and composited with
/// [`blit_coverage`] rather than through `SwashCache::with_pixels` or
/// `Buffer::draw`. Both of those fold the coverage into a colour that ignores
/// the base colour's alpha (cosmic-text 0.19, `swash.rs` — `//TODO: blend base
/// alpha?`), which would silently discard `text.alpha` and window opacity.
pub struct TextEngine {
    font_system: FontSystem,
    buffer: Buffer,
    attrs: AttrsOwned,
    cache: SwashCache,
    line_height: f32,
    align: Align,
    /// Whether the skipped-glyph warning has been emitted. Rasterising happens
    /// once per redraw, so warning per frame would fill the log with one
    /// unchanging complaint.
    warned_skipped_glyphs: bool,
}

impl TextEngine {
    pub fn new(style: &TextStyle) -> TextEngine {
        let mut font_system = FontSystem::new();
        let family = resolve_family(&font_system, &style.family);
        let attrs = Attrs::new().family(family).weight(Weight(style.weight));

        TextEngine {
            buffer: Buffer::new(&mut font_system, Metrics::new(style.size, style.line_height)),
            attrs: AttrsOwned::new(&attrs),
            cache: SwashCache::new(),
            line_height: style.line_height,
            align: style.align,
            warned_skipped_glyphs: false,
            font_system,
        }
    }

    /// Shapes `text` into `out`, returning whether anything was dropped.
    ///
    /// The buffer is sized with an unbounded height so that it shapes every
    /// line; which lines fit is this function's decision, so that truncation can
    /// be reported rather than guessed at.
    pub fn shape(&mut self, text: &str, viewport: (u32, u32), out: &mut Vec<PlacedGlyph>) -> bool {
        out.clear();
        if text.is_empty() {
            return false;
        }

        let (width, height) = (viewport.0 as f32, viewport.1 as f32);
        self.buffer.set_size(Some(width), None);
        self.buffer
            .set_text(text, &self.attrs.as_attrs(), Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let max_rows = (height / self.line_height).floor().max(0.0) as usize;
        let total_rows = self.buffer.layout_runs().count();
        // Centring needs the row count, so it is measured before anything is
        // placed.
        let visible_rows = total_rows.min(max_rows);
        let y_origin = ((height - visible_rows as f32 * self.line_height) / 2.0).max(0.0);

        for (index, run) in self.buffer.layout_runs().enumerate() {
            if index >= max_rows {
                break;
            }
            let x_origin = match self.align {
                Align::Left => 0.0,
                Align::Center => ((width - run.line_w) / 2.0).max(0.0),
                Align::Right => (width - run.line_w).max(0.0),
            };
            for glyph in run.glyphs {
                let placed = glyph.physical((x_origin, run.line_y + y_origin), 1.0);
                out.push(PlacedGlyph { x: placed.x, y: placed.y, key: placed.cache_key });
            }
        }

        total_rows > max_rows
    }

    /// Rasterises `glyphs` onto `canvas` in `color` at `alpha`.
    pub fn draw(
        &mut self,
        glyphs: &[PlacedGlyph],
        canvas: &mut [u8],
        canvas_width: u32,
        canvas_height: u32,
        color: Rgb,
        alpha: u8,
    ) {
        let mut skipped = 0usize;
        for glyph in glyphs {
            let Some(image) = self.cache.get_image(&mut self.font_system, glyph.key) else {
                skipped += 1;
                continue;
            };
            if !matches!(image.content, SwashContent::Mask) {
                // Colour bitmaps — emoji, COLR outlines — carry their own RGBA
                // and have no single coverage value to tint.
                skipped += 1;
                continue;
            }
            let placement = image.placement;
            blit_coverage(
                canvas,
                canvas_width,
                canvas_height,
                glyph.x + placement.left,
                glyph.y - placement.top,
                placement.width,
                placement.height,
                &image.data,
                (color.r, color.g, color.b),
                alpha,
            );
        }

        if skipped > 0 && !self.warned_skipped_glyphs {
            self.warned_skipped_glyphs = true;
            log::warn!("layout: skipped {skipped} glyph(s) with no monochrome mask");
        }
    }
}

/// Resolves the configured family, falling back to the system sans-serif when
/// the font database has never heard of it (spec §13).
fn resolve_family<'a>(font_system: &FontSystem, requested: &'a str) -> Family<'a> {
    let known = font_system
        .db()
        .faces()
        .any(|face| face.families.iter().any(|(name, _)| name.eq_ignore_ascii_case(requested)));
    if known {
        return Family::Name(requested);
    }
    log::warn!(
        "layout: font family {requested:?} is not installed; using the system sans-serif"
    );
    Family::SansSerif
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(size: f32, line_height: f32, align: Align) -> TextStyle {
        TextStyle {
            family: "Microsoft YaHei".to_string(),
            size,
            weight: 400,
            line_height,
            color: Rgb { r: 255, g: 255, b: 255 },
            alpha: 230,
            align,
        }
    }

    fn engine(size: f32, line_height: f32, align: Align) -> TextEngine {
        TextEngine::new(&style(size, line_height, align))
    }

    #[test]
    fn shaping_cjk_produces_glyphs_and_no_truncation_when_it_fits() {
        let mut e = engine(34.0, 44.0, Align::Center);
        let mut out = Vec::new();
        let truncated = e.shape("玻璃纸 glassine 12:34:56", (640, 220), &mut out);
        assert!(!truncated);
        assert!(!out.is_empty(), "expected glyphs");
    }

    #[test]
    fn empty_text_produces_no_glyphs() {
        let mut e = engine(34.0, 44.0, Align::Center);
        let mut out = Vec::new();
        assert!(!e.shape("", (640, 220), &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn too_many_lines_are_truncated_and_reported() {
        let mut e = engine(34.0, 44.0, Align::Left);
        let mut out = Vec::new();
        // 220px of viewport at 44px per line fits 5 rows.
        let text: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let truncated = e.shape(&text, (640, 220), &mut out);
        assert!(truncated, "20 rows cannot fit in 220px");
    }

    #[test]
    fn a_viewport_shorter_than_one_line_truncates_instead_of_panicking() {
        let mut e = engine(34.0, 44.0, Align::Left);
        let mut out = Vec::new();
        assert!(e.shape("hello", (640, 10), &mut out));
    }

    #[test]
    fn a_sixty_kibibyte_text_shapes_without_panicking() {
        // Review Focus: the largest body the HTTP layer will deliver.
        let mut e = engine(34.0, 44.0, Align::Left);
        let mut out = Vec::new();
        let text = "字".repeat(20_000);
        let truncated = e.shape(&text, (640, 220), &mut out);
        assert!(truncated);
    }

    #[test]
    fn alignment_moves_glyphs_horizontally_but_keeps_them_inside_the_viewport() {
        let mut out_left = Vec::new();
        let mut out_right = Vec::new();
        engine(34.0, 44.0, Align::Left).shape("iiii", (640, 220), &mut out_left);
        engine(34.0, 44.0, Align::Right).shape("iiii", (640, 220), &mut out_right);

        let min_left = out_left.iter().map(|g| g.x).min().unwrap();
        let max_right = out_right.iter().map(|g| g.x).max().unwrap();
        assert!(min_left < max_right, "right alignment must sit further right");
        for g in out_left.iter().chain(out_right.iter()) {
            assert!(g.x >= -8 && g.x < 640, "glyph escaped the viewport: {g:?}");
        }
    }

    #[test]
    fn drawing_marks_pixels_and_leaves_the_rest_transparent() {
        let mut e = engine(34.0, 44.0, Align::Center);
        let mut glyphs = Vec::new();
        e.shape("M", (200, 100), &mut glyphs);
        assert!(!glyphs.is_empty());

        let mut canvas = vec![0u8; 200 * 100 * 4];
        e.draw(&glyphs, &mut canvas, 200, 100, Rgb { r: 255, g: 255, b: 255 }, 230);

        let painted = canvas.chunks_exact(4).filter(|p| p[3] != 0).count();
        assert!(painted > 0, "nothing was drawn");
        assert!(painted < 200 * 100 / 4, "suspiciously much was drawn");
        for px in canvas.chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{px:?}");
        }
    }

    fn rows(glyphs: &[PlacedGlyph]) -> usize {
        // Glyphs on one line share a baseline, so distinct y values count lines.
        let mut ys: Vec<i32> = glyphs.iter().map(|g| g.y).collect();
        ys.sort_unstable();
        ys.dedup();
        ys.len()
    }

    #[test]
    fn a_long_line_wraps_without_explicit_newlines() {
        let mut e = engine(34.0, 44.0, Align::Left);
        let text = "字".repeat(200);
        let mut narrow = Vec::new();
        let mut wide = Vec::new();
        e.shape(&text, (640, 220), &mut narrow);
        e.shape(&text, (4000, 220), &mut wide);

        assert!(rows(&narrow) > 1, "200 CJK glyphs cannot fit one 640px line");
        assert!(rows(&wide) < rows(&narrow), "a wider viewport must use fewer rows");
    }

    #[test]
    fn an_unknown_font_family_falls_back_instead_of_failing() {
        // Spec §13: a missing family warns and falls back to the system sans-serif.
        let style = TextStyle {
            family: "ThisFontDoesNotExist12345".to_string(),
            size: 34.0,
            weight: 400,
            line_height: 44.0,
            color: Rgb { r: 255, g: 255, b: 255 },
            alpha: 230,
            align: Align::Left,
        };
        let mut e = TextEngine::new(&style);
        let mut out = Vec::new();
        assert!(!e.shape("hello", (640, 220), &mut out));
        assert!(!out.is_empty(), "the fallback face produced no glyphs");
    }
}
