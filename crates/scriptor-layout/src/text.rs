//! Standalone text shaping.
//! 
//! The simple entry point: shape a string at a width and report glyph positions. The
//! paginating path in the renderer does not go through here; this is for callers that
//! want metrics for one run of text.

use crate::*;

/// A laid-out glyph: its x offset (px from line start) and the byte index it maps back to in the
/// source text (for caret placement + hit-testing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphBox {
    pub x: f32,
    pub source_byte: usize,
}

/// The result of laying out one string at a font size: the wrapped lines, each a run of glyphs.
#[derive(Debug, Clone, Default)]
pub struct LineLayout {
    pub lines: Vec<Vec<GlyphBox>>,
}

impl LineLayout {
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn glyph_count(&self) -> usize {
        self.lines.iter().map(|l| l.len()).sum()
    }
}

/// Shape + lay out `text` at `font_size` (px) within `width` (px; `None` = unbounded, single line).
/// Uses cosmic-text for shaping/line-breaking over the system font set; the family-substitution
/// policy is in [`scriptor_fonts`] and folds in as the model gains real run/font attributes.
pub fn layout_text(font_system: &mut FontSystem, text: &str, font_size: f32, width: Option<f32>)
    -> LineLayout
{
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, font_size * 1.2));
    // cosmic-text 0.19: shaping setters live on the font-system-borrowed view (fs-free, `&Attrs`,
    // plus a per-block `alignment` arg). Borrow once, set up, shape; `layout_runs` derefs through.
    let mut buffer = buffer.borrow_with(font_system);
    buffer.set_size(width, None);
    buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
    buffer.shape_until_scroll(false);

    let mut out = LineLayout::default();
    for run in buffer.layout_runs() {
        let glyphs = run
            .glyphs
            .iter()
            .map(|g| GlyphBox { x: g.x, source_byte: g.start })
            .collect();
        out.lines.push(glyphs);
    }
    out
}
