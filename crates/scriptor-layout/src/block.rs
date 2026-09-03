//! The block model: what the renderer is asked to lay out.
//! 
//! A [`Block`] is one paragraph resolved down to presentation - styled spans, indents,
//! spacing, borders, list marker, tracked-change ranges - with everything the caller
//! knows about styles already folded in. The renderer only paints what it finds here.

use crate::*;

/// One styled text span within a [`Block`] - resolved presentation (size in px, weight, italic,
/// color). The caller (the wasm layer) resolves paragraph style + inline run formatting into these;
/// the renderer just paints them.
#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub size_px: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: [u8; 3],
    /// Highlight fill (RGB) painted behind this span's glyphs (`w:highlight`), or `None`.
    pub highlight: Option<[u8; 3]>,
    /// Baseline shift in px: positive raises the glyphs (superscript), negative lowers them
    /// (subscript), 0 for normal text. The caller also shrinks `size_px` for super/subscript.
    pub baseline_shift: f32,
    /// The shaping family name (already resolved to a bundled clone, e.g. "Gelasio" for Georgia).
    pub family: String,
}

/// Paragraph alignment, mapped to a cosmic-text `Align` at shape time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// One painted edge of a paragraph border box (`w:pBdr`): the line thickness, the gap from the text
/// to the line, and the colour - all in device px / RGB, resolved by the wasm layer at the render
/// scale so the painter is scale-agnostic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderLine {
    pub width_px: f32,
    pub space_px: f32,
    pub rgb: [u8; 3],
}

/// The four edges of a paragraph's border box. A `None` edge draws no line. Adjacent paragraphs that
/// share an identical box merge into one (the painter suppresses the seam between them).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockBorders {
    pub top: Option<BorderLine>,
    pub left: Option<BorderLine>,
    pub bottom: Option<BorderLine>,
    pub right: Option<BorderLine>,
}

/// A rectangular exclusion inside a paragraph's content box - a text frame that the body wraps around
/// on BOTH sides (Word's two-sided "around" wrap). `x0`/`x1` are absolute page px (clearance already
/// folded in); `top`/`bot` are px RELATIVE to the paragraph's top. The shaper flows the text
/// above the hole at full width, then the left column, then the right column, then below it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WrapHole {
    pub x0: f32,
    pub x1: f32,
    pub top: f32,
    pub bot: f32,
}

/// One paragraph to lay out: its styled spans, vertical spacing (px) above/below, alignment, a
/// line-height multiplier (1.0 = single), and left/right indents (px).
#[derive(Debug, Clone, Default)]
pub struct Block {
    pub spans: Vec<Span>,
    /// Byte offset of this block's text within its source paragraph - non-zero only for the
    /// CONTINUATION fragment of a table row split across a page boundary (its spans are a slice of
    /// the paragraph's tail). Caret-stop emission adds it so `(para, byte)` stays paragraph-global.
    pub byte_offset: usize,
    pub space_before_px: f32,
    pub space_after_px: f32,
    pub align: BlockAlign,
    /// `w:spacing w:line` with `lineRule="auto"`: a multiplier on the font-natural line height
    /// (1.0 = single). Ignored when `line_exact_px > 0` (exact rule).
    pub line_mult: f32,
    /// `lineRule="exact"`: every line is exactly this many px tall, regardless of font size. `0` = unset.
    pub line_exact_px: f32,
    /// `lineRule="atLeast"`: a floor (px) under the natural line height. `0` = unset.
    pub line_min_px: f32,
    pub indent_left_px: f32,
    pub indent_right_px: f32,
    /// A list marker (`1.`, `a.`, `•`) rendered before the text. Painted but excluded from the caret
    /// geometry (it isn't part of the editable paragraph text), so editing stays aligned.
    pub marker: String,
    /// List hanging-indent distance (px): how far the marker out-dents to the LEFT of the text. When
    /// `> 0` (and a `marker` is set), the marker hangs at the block's left edge (`indent_left_px`) and
    /// the text + every wrapped continuation line align at `indent_left_px + hang_px` - Word's hanging
    /// indent, so the text starts at a fixed edge regardless of marker width. `0` = marker inline.
    pub hang_px: f32,
    /// `w:keepNext` - keep this paragraph on the same page as the one after it. When the paragraph
    /// would land at a page foot with no room for the start of the next, pagination breaks before it
    /// instead (Word's "keep with next", carried by heading styles so a heading is never orphaned).
    pub keep_next: bool,
    /// Force this paragraph to the top of a new page before laying it out (`w:pageBreakBefore`, or a
    /// manual `<w:br w:type="page"/>` in the preceding paragraph, which the caller maps onto the next
    /// block's flag). No effect when the paragraph is already at the top of a page.
    pub page_break_before: bool,
    /// This paragraph is a section terminator (a bare `w:sectPr` carrier). When it is also EMPTY, it
    /// doesn't spill to a new page if its line won't fit at the foot - Word lets that trailing mark sit
    /// at the bottom and starts the next section on the new page anyway. (A manual `<w:br>` does NOT set
    /// this; an empty manual-break paragraph paginates normally.)
    pub section_terminator: bool,
    /// This paragraph carries a **continuous** (or `nextColumn`) section break - the break after it
    /// does NOT create a page. When it is also EMPTY, Word consolidates the carrier away: it occupies
    /// no line and contributes no space-after, and the surrounding paragraphs ride up over it
    /// (tdf169986 + the `*bottomSpacing` continuous-break fixtures). Set independently of
    /// [`Self::section_terminator`] (which marks page-*creating* section ends).
    pub continuous_break: bool,
    /// `w:contextualSpacing` - "Don't add space between paragraphs of the same style". When two
    /// ADJACENT paragraphs share a `style_group` and both opt in, the space between them (this one's
    /// space-before + the previous one's space-after) is suppressed in the flow.
    pub contextual_spacing: bool,
    /// Legacy paragraph-spacing mode (a doc-level setting, stamped on every body block): adjacent
    /// space-after + space-before SUM instead of Word's modern max-consolidation. Selected by
    /// `w:doNotUseHTMLParagraphAutoSpacing` or a `compatibilityMode` of Word 2003 or older
    /// (tdf145716, tdf153964); `false` (consolidate) is the modern default.
    pub legacy_spacing: bool,
    /// Identity of the paragraph's style, for the contextual-spacing same-style adjacency test. 0 =
    /// the default/Normal style (so consecutive unstyled body paragraphs count as the same style).
    pub style_group: u64,
    /// Paragraph shading fill (RGB), painted behind the text across the indent box. `None` = none.
    pub shading: Option<[u8; 3]>,
    /// Custom tab-stop positions (px from the content-box left). Empty = use `default_tab_px`.
    pub tab_stops_px: Vec<f32>,
    /// Per-stop alignment, parallel to `tab_stops_px`: 0=left, 1=center, 2=right, 3=decimal.
    /// Shorter than (or empty relative to) `tab_stops_px` = the missing entries are left tabs.
    pub tab_kinds: Vec<u8>,
    /// Default tab interval (px) for positions past the last custom stop (Word default 0.5in).
    pub default_tab_px: f32,
    /// A trailing glyph painted after the text but excluded from the caret geometry - used for a
    /// tracked paragraph-mark revision (a coloured "¶"). Empty = none.
    pub trailing: String,
    /// Colour of the trailing glyph (RGB).
    pub trailing_color: [u8; 3],
    /// Whether the trailing glyph is struck through (a deleted ¶).
    pub trailing_strike: bool,
    /// Whether this paragraph carries any tracked change (insertion / deletion / run- or paragraph-
    /// formatting / paragraph-mark). Drives the margin change-bar; the wasm resolver sets it only in
    /// the display modes that show markup (All / Simple). Layout-neutral - the bar paints in the
    /// left-margin gutter and never shifts the text.
    pub has_change: bool,
    /// Byte ranges (into this block's text) that carry a tracked change, so the margin change-bar can
    /// be drawn beside only the **visual lines** that changed (Word bars lines, not whole paragraphs).
    /// A zero-width range marks a position-only change (a deletion hidden by the display mode, or the
    /// paragraph-mark ¶ at the end); `(0, len)` bars the whole paragraph (a paragraph-property change).
    /// Empty when `has_change` is false. The wasm resolver fills it only in the markup display modes.
    pub change_ranges: Vec<(usize, usize)>,
    /// Inline pictures anchored in this paragraph; each reserves a line of its own height in the flow
    /// (see [`InlineImage`]). Empty for most paragraphs.
    pub inline_images: Vec<InlineImage>,
    /// Verbatim-passthrough objects anchored in this paragraph (OLE / chart / shape); each reserves a
    /// line and paints a labelled placeholder box (see [`Placeholder`]). Empty for most paragraphs.
    pub placeholders: Vec<Placeholder>,
    /// Paragraph border box (`w:pBdr`): the lines drawn around this paragraph (Word's paragraph
    /// borders). This is what draws a text frame's visible rectangle. `None` = no box.
    pub borders: Option<BlockBorders>,
    /// Two-sided wrap exclusions (a text frame straddling the content centre): the paragraph's text
    /// flows around each hole, left column then right column. Empty for the overwhelming majority of
    /// paragraphs; set by the wasm wrap pass only for a centre-straddling frame.
    pub wrap_holes: Vec<WrapHole>,
}

/// Whether a visual line whose caret stops are `stops` intersects any tracked-change byte `ranges`
/// (so it gets a margin change-bar). A zero-width range bars the line it sits on.
pub(crate) fn line_has_change(ranges: &[(usize, usize)], stops: &[CaretStop]) -> bool {
    if ranges.is_empty() {
        return false;
    }
    let lo = stops.iter().map(|s| s.byte).min().unwrap_or(0);
    let hi = stops.iter().map(|s| s.byte).max().unwrap_or(0);
    ranges.iter().any(|&(cs, ce)| cs <= hi && ce >= lo)
}

impl BlockAlign {
    pub(crate) fn to_cosmic(self) -> Option<cosmic_text::Align> {
        match self {
            BlockAlign::Left => None, // default LTR; avoids forcing alignment metadata
            BlockAlign::Center => Some(cosmic_text::Align::Center),
            BlockAlign::Right => Some(cosmic_text::Align::Right),
            BlockAlign::Justify => Some(cosmic_text::Align::Justified),
        }
    }
}

impl Block {
    /// The line-height multiplier, guarded so a zero/garbage value can't collapse the line.
    pub(crate) fn line_factor(&self) -> f32 {
        if self.line_mult > 0.1 { self.line_mult } else { 1.0 }
    }

    /// The resolved line height for a single span of `size` px in `family`, honoring the line-spacing
    /// rule: `exact` = a fixed height; `atLeast` = the natural line floored at the minimum; otherwise
    /// (auto) the natural height times the multiplier.
    pub(crate) fn span_line_height(&self, size: f32, family: &str) -> f32 {
        if self.line_exact_px > 0.0 {
            return self.line_exact_px;
        }
        let natural = size * line_height_factor(family);
        if self.line_min_px > 0.0 { natural.max(self.line_min_px) } else { natural * self.line_factor() }
    }

    /// The resolved line height at the block level (largest span / empty line): same rules as
    /// [`Self::span_line_height`] but using the block's max font factor across families.
    pub(crate) fn block_line_height(&self, max_size: f32) -> f32 {
        if self.line_exact_px > 0.0 {
            return self.line_exact_px;
        }
        let natural = max_size * self.font_factor();
        if self.line_min_px > 0.0 { natural.max(self.line_min_px) } else { natural * self.line_factor() }
    }

    /// The block's font-natural line-height factor: the MAX over its spans' families (Word sizes a
    /// line to the tallest font on it). Used for the buffer-base + empty-line metric; individual
    /// spans are metric'd per-family in `rich_spans`. Falls back to [`FALLBACK_LINE_HEIGHT`] when the
    /// block has no spans (a blank paragraph with no resolved font).
    pub(crate) fn font_factor(&self) -> f32 {
        self.spans
            .iter()
            .map(|s| line_height_factor(&s.family))
            .fold(0.0_f32, f32::max)
            .max(if self.spans.is_empty() { FALLBACK_LINE_HEIGHT } else { 0.0 })
    }
}

/// Height of one blank line for an empty paragraph: its font size (carried on a sized empty span)
/// x 1.3 x the line multiplier. Falls back to 16px when no size is present. Used wherever an empty
/// block needs a height (`layout_doc` + `block_height`) so body + header/footer stack consistently.
pub(crate) fn empty_line_height(block: &Block) -> f32 {
    let size = block.spans.iter().map(|s| s.size_px).fold(0.0_f32, f32::max);
    let size = if size > 0.0 { size } else { 16.0 };
    block.block_line_height(size)
}
