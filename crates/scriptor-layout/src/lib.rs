//! Canvas layout engine.
//!
//! Shaping (cosmic-text + [`scriptor_fonts`] policy) -> line-break -> pagination -> a resumable
//! block engine that emits paint commands, compiled to WASM (browser canvas) and native
//! (server / agent). The foundation shapes + lays out text and returns per-glyph positions (the
//! data a canvas paint pass and a caret/hit-test both consume); pagination, tables, floats, and
//! the paint pass build on top.

use cosmic_text::fontdb::Database;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};

mod metafile;

// The data model and the queries over it. None of these touch `Renderer`: they describe what goes
// in (`block`, `table_data`), what comes out (`geometry`), and the pure arithmetic in between.
// Re-exported flat, so `scriptor_layout::Block` and friends resolve as before.
mod block;
mod fingerprint;
mod geometry;
mod table_data;
mod tabs;
mod text;

// The renderer pipeline, in the order a page goes through it: shape -> paginate (and lay out any
// tables) -> paint (rasterizing glyphs and compositing images). Each module carries one
// `impl Renderer` block; the struct itself stays here, so they reach its private caches directly.
mod picture;
mod paginate;
mod paint;
mod raster;
mod shape;
mod table_layout;

pub use block::*;
pub use geometry::*;
pub use table_data::*;
pub use text::*;
pub(crate) use fingerprint::*;
pub(crate) use tabs::*;

pub use scriptor_fonts::{line_height_factor, resolve_family, DEFAULT_FAMILY, FALLBACK_LINE_HEIGHT};

// Single-spacing line height is the font's own natural leading (`hhea` ascent - descent + lineGap),
// taken per font via [`scriptor_fonts::line_height_factor`] - Word lays out `lineRule="auto"` at that
// factor, which differs per family (Carlito ~1.221, Times/Arial clones ~1.15). The paragraph's
// `line_mult` (from `w:spacing w:line` / docDefaults) multiplies it. A previous single 1.15 constant
// fit the serif/sans clones but ran ~6% short for Calibri, drifting tall single-font bodies a line.

/// Colour of the margin change-bar (a neutral slate, author-independent - Word draws a single bar
/// per changed line regardless of who changed it).
const CHANGE_BAR_RGB: [u8; 3] = [0x70, 0x74, 0x80];




// Revision-balloon palette: a faint fill, a soft grey border + connector. The text inside is
// author-coloured (struck) by the wasm resolver, like inline deletions.
const BALLOON_BG: [u8; 3] = [0xFB, 0xFB, 0xFD];
const BALLOON_BORDER: [u8; 3] = [0xC8, 0xCC, 0xD4];

// Passthrough placeholder box (an unmodeled OLE object / chart / shape): a faint neutral fill, a soft
// border, and a muted caption - just enough to show the object occupies space (see `docs/passthrough.md`).
const PLACEHOLDER_BG: [u8; 3] = [0xF2, 0xF3, 0xF5];
const PLACEHOLDER_BORDER: [u8; 3] = [0xB4, 0xB8, 0xC0];
const PLACEHOLDER_TEXT: [u8; 3] = [0x6A, 0x70, 0x7A];


/// A reusable canvas renderer: owns the font system (loaded with our bundled clone fonts) and the
/// swash glyph cache, and rasterizes document text into an RGBA pixel buffer the browser blits to a
/// `<canvas>`. [`Renderer::render_blocks`] is the M1.5 path (per-run sizes / bold / italic / color +
/// paragraph spacing); tracked-change rendering and pagination build on top.
pub struct Renderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Decoded images by key (the OOXML media part name), decoded once + reused across paints.
    image_cache: std::collections::HashMap<String, image::RgbaImage>,
    /// How far the current paint dims its ink toward white (0 = solid, 1 = invisible). Word greys an
    /// inactive header/footer (or the body, while editing a header/footer); `paint_page` sets this
    /// around each region's text so glyph coverage scales by `1 - dim`. Transient paint state.
    dim: f32,
    /// The page-sheet fill (`w:background w:color`, RGB) painted under everything instead of white.
    /// `None` = the plain white sheet. Set once per document ([`Self::set_page_background`]).
    page_background: Option<[u8; 3]>,
    /// Shaped-lines memo: `(fold_block content hash, width, x)` -> the [`Self::shape_block_lines`]
    /// result. Shaping through rustybuzz is the dominant per-keystroke cost of a relayout and the
    /// input is content-addressed, so unchanged paragraphs hit here and only edited ones re-shape -
    /// the heart of incremental relayout. Entries are generation-tagged and swept in
    /// [`Self::begin_shape_pass`].
    shape_cache: std::collections::HashMap<u64, ShapeEntry>,
    /// The current layout generation (bumped per [`Renderer::layout_doc`]), stamping cache entries
    /// so eviction keeps only recently-used blocks.
    shape_gen: u32,
}

/// One [`Renderer::shape_cache`] entry: the shaped block height + per-line geometry, tagged with
/// the last generation that used it.
struct ShapeEntry {
    last_gen: u32,
    bh: f32,
    geom: Vec<(f32, f32, Vec<CaretStop>)>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

/// An inline picture (`<w:drawing>`/`wp:inline`) anchored in a paragraph. cosmic-text has no
/// inline-object box, so each reserves a line of its own height in the flow - exact for the figure
/// case (a picture-only paragraph), and a picture amid text degrades to its own line below that text.
/// `byte` is the visible-text offset it anchors at; `key`/`crop` drive the composite.
#[derive(Debug, Clone, Default)]
pub struct InlineImage {
    /// The editable picture id this box renders (for hit-test + selection).
    pub id: u64,
    pub byte: usize,
    pub w: f32,
    pub h: f32,
    pub key: String,
    pub crop: [i64; 4],
}

/// A verbatim-passthrough object anchored in a paragraph (an OLE object, chart, or shape the layout
/// engine does not model - see `docs/passthrough.md`). The renderer can't draw the object itself, so
/// like [`InlineImage`] it reserves a line of its own height and paints a neutral labelled placeholder
/// box there, so the view shows *something is there* instead of a blank gap. `byte` is the visible-text
/// offset it anchors at; `label` is the sniffed kind ("OLE Object" / "Chart" / "Shape").
#[derive(Debug, Clone, Default)]
pub struct Placeholder {
    pub id: u64,
    pub byte: usize,
    pub w: f32,
    pub h: f32,
    pub label: String,
}

/// An image placement on a page (device px), referencing a decoded image by key.
#[derive(Debug, Clone)]
pub struct PageImage {
    pub key: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// `behindDoc` images paint before the text (behind it); others paint over the text.
    pub behind: bool,
    /// `<a:srcRect>` crop in thousandths of a percent (l, t, r, b) cut from each edge of the source
    /// before it is scaled into the `w`x`h` box. `[0; 4]` = no crop (the whole source).
    pub crop: [i64; 4],
    /// The page this placement lands on (inline images are placed by `layout_doc`, which knows the
    /// page; floating placements are built per-page so this matches the page being painted).
    pub page: u32,
    /// The editable picture id (`Some`) for a body picture that hit-tests + selects; `None` for a
    /// read-only header/footer picture.
    pub id: Option<u64>,
    /// How far to dim this picture toward white (0 = solid, 1 = invisible) - matches the dimming of
    /// the region it belongs to (a header logo greys while the body is active, etc.).
    pub dim: f32,
}

/// A passthrough placeholder box placed on a page (device px): a neutral rectangle + caption painted
/// where an unmodeled object (OLE / chart / shape) sits, so the view isn't a blank gap. Reserved in the
/// flow by [`Renderer::layout_doc`] like a [`PageImage`]; painted by [`Renderer::paint_page`].
#[derive(Debug, Clone)]
pub struct PagePlaceholder {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
    pub page: u32,
}

/// A text frame (`w:framePr`) placed on a page: a positioned box of its own paragraphs that body
/// text wraps around (like a floating picture, but holding text). Built per page by the wasm layer
/// (which resolves the framePr anchor/align to `x`/`y`), painted by [`Renderer::paint_page`] via the
/// same block-flow as a table cell.
#[derive(Debug, Clone)]
pub struct PageFrame {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    /// The frame's box height (px): its content height, or a larger `w:h` floor (`atLeast`/`exact`).
    /// The border box is drawn at this height - so an explicit-height frame's rectangle matches Word
    /// even when its text is shorter.
    pub h: f32,
    pub blocks: Vec<Block>,
    /// The frame's border box (`w:pBdr` of its framed paragraph), painted at the frame's full `[w, h]`
    /// rather than per-paragraph. `None` = a borderless frame. Lifted off the blocks so it isn't also
    /// drawn around just the text.
    pub border: Option<BlockBorders>,
}

impl Renderer {
    /// Build a renderer whose font database holds only our bundled clone fonts - no system source
    /// (the wasm reality, and deterministic on native too). See [`scriptor_fonts::bundled_fonts`].
    pub fn new() -> Self {
        let mut db = Database::new();
        for font in scriptor_fonts::bundled_fonts() {
            db.load_font_data(font.data.to_vec());
        }
        let font_system = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            image_cache: Default::default(),
            dim: 0.0,
            page_background: None,
            shape_cache: Default::default(),
            shape_gen: 0,
        }
    }

    /// Set the page-sheet fill colour (`w:background w:color`) painted under every page instead of
    /// white. The caller decides Word's `displayBackgroundShape` gate; `None` restores plain white.
    pub fn set_page_background(&mut self, rgb: Option<[u8; 3]>) {
        self.page_background = rgb;
    }

    /// Open a new layout generation: bump the stamp cache entries carry, and once the cache has
    /// grown past its cap, sweep entries no recent pass touched. Keeps memory bounded without ever
    /// evicting the live document's blocks mid-session.
    fn begin_shape_pass(&mut self) {
        self.shape_gen = self.shape_gen.wrapping_add(1);
        if self.shape_cache.len() > 16_384 {
            let g = self.shape_gen;
            self.shape_cache.retain(|_, e| g.wrapping_sub(e.last_gen) <= 2);
        }
    }

}

/// Draw a continuous decoration line (underline / strike) under the glyphs whose metadata has `bit`
/// set, at canvas y `y` and thickness `thick`. Consecutive decorated glyphs are merged into one
/// span (from the first glyph's left to the last glyph's right) so the line is solid, not dashed.
/// Whether a block contains a literal tab character (needs the tab-stop layout path).
fn block_has_tab(block: &Block) -> bool {
    block.spans.iter().any(|s| s.text.contains('\t'))
}

/// The vertical gap between two consecutive blocks in a stacked run (a table cell / text frame),
/// mirroring the body flow's rules: contextualSpacing between same-style neighbours drops the gap
/// entirely; legacy documents SUM the previous space-after and the next space-before; everyone else
/// takes Word's MAX-consolidation of the two. `None` = the run's first block (its own space-before).
fn stack_gap(prev: Option<&Block>, b: &Block) -> f32 {
    let Some(p) = prev else { return b.space_before_px };
    if b.contextual_spacing && p.contextual_spacing && p.style_group == b.style_group {
        return 0.0;
    }
    if b.legacy_spacing {
        return p.space_after_px + b.space_before_px;
    }
    p.space_after_px.max(b.space_before_px)
}

/// A hole-free sub-`Block` carrying the byte range `[start, end)` of `block`'s editable text, its
/// spans sliced to that range. The paragraph chrome (marker / trailing ¶ / inline images / borders /
/// wrap holes / indents / spacing) is dropped - the caller positions the region itself - but the
/// alignment + line multiplier carry over so each column matches the paragraph's look. Used by
/// [`Renderer::flow_regions`] for two-sided frame wrap.
fn slice_block(block: &Block, start: usize, end: usize) -> Block {
    let mut spans = Vec::new();
    let mut pos = 0usize;
    for s in &block.spans {
        let (s0, s1) = (pos, pos + s.text.len());
        pos = s1;
        let a = start.max(s0).saturating_sub(s0);
        let b = end.min(s1).saturating_sub(s0);
        if a < b && s.text.is_char_boundary(a) && s.text.is_char_boundary(b) {
            spans.push(Span { text: s.text[a..b].to_string(), ..s.clone() });
        }
    }
    Block { spans, align: block.align, line_mult: block.line_mult, ..Default::default() }
}


/// Lerp an RGB colour `dim` of the way toward white (0 = unchanged, 1 = white) - the dimming applied
/// to an inactive header/footer (or the body, while a header/footer is active).
fn dim_rgb(c: [u8; 3], dim: f32) -> [u8; 3] {
    if dim <= 0.0 {
        return c;
    }
    let f = dim.clamp(0.0, 1.0);
    let mix = |v: u8| (v as f32 * (1.0 - f) + 255.0 * f).round().clamp(0.0, 255.0) as u8;
    [mix(c[0]), mix(c[1]), mix(c[2])]
}

/// Split a block's spans into tab-delimited segments (tab chars dropped). Each segment carries the
/// model-byte offset of its first character, so caret stops can be mapped back to the source text.
fn split_segments(spans: &[Span]) -> Vec<(Vec<Span>, usize)> {
    let mut segs: Vec<(Vec<Span>, usize)> = vec![(Vec::new(), 0)];
    let mut byte = 0usize;
    for s in spans {
        let mut first = true;
        for part in s.text.split('\t') {
            if !first {
                byte += 1; // the dropped '\t'
                segs.push((Vec::new(), byte));
            }
            first = false;
            if !part.is_empty() {
                segs.last_mut().unwrap().0.push(Span { text: part.to_string(), ..s.clone() });
                byte += part.len();
            }
        }
    }
    segs
}

/// A tab-free child block carrying one segment's spans (no marker, no tab stops) - shaped by the
/// normal path.
fn seg_block(block: &Block, spans: Vec<Span>) -> Block {
    Block {
        spans,
        marker: String::new(),
        tab_stops_px: Vec::new(),
        tab_kinds: Vec::new(),
        trailing: String::new(),
        ..block.clone()
    }
}

/// The span that paints a block's list marker, sized + fonted like the paragraph's first text run
/// and inked like it too - a white heading on a dark band keeps its number white (mirrors the
/// marker prefix [`Renderer::rich_spans`] builds). `None` when the block has no marker. Used by
/// the hanging-indent raster path to draw the marker on its own.
fn marker_span(block: &Block) -> Option<Span> {
    if block.marker.is_empty() {
        return None;
    }
    let f = block.spans.first();
    Some(Span {
        text: block.marker.clone(),
        size_px: f.map(|s| s.size_px).unwrap_or(16.0),
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: f.map(|s| s.color).unwrap_or([0x1a, 0x1a, 0x1a]),
        highlight: None,
        baseline_shift: 0.0,
        family: f.map(|s| s.family.clone()).unwrap_or_else(|| DEFAULT_FAMILY.to_string()),
    })
}

/// Fill the highlight color behind a run's glyphs, coalescing consecutive glyphs of the same color.
/// The glyph metadata carries `(span index + 1) << 2`; 0 = marker / no span. The fill hugs the
/// highlighted span's *font cell* (`size_px * line_height_factor`), baseline-aligned - NOT the line
/// box, which on a large or line-spaced line is far taller than the glyphs (Word highlights the text,
/// not the leading). `base_y` is the run's baseline.
#[allow(clippy::too_many_arguments)]
fn highlight_run(
    glyphs: &[cosmic_text::LayoutGlyph],
    spans: &[Span],
    x_off: f32,
    base_y: i32,
    pixels: &mut [u8],
    page_w: u32,
    page_h: u32,
) {
    // (highlight colour, font-cell height) for a glyph's span, or None when the span isn't highlighted.
    let info_of = |g: &cosmic_text::LayoutGlyph| -> Option<([u8; 3], f32)> {
        let si = g.metadata >> 2;
        if si == 0 {
            return None;
        }
        let s = spans.get(si - 1)?;
        s.highlight.map(|c| (c, s.size_px * line_height_factor(&s.family)))
    };
    // (colour, x_start, x_end, max cell height across the segment's glyphs).
    let mut seg: Option<([u8; 3], f32, f32, f32)> = None;
    let flush = |c: [u8; 3], s: f32, e: f32, cell: f32, pixels: &mut [u8]| {
        // Sit the cell on the baseline: ~0.8 above (ascent), ~0.2 below (descent) - close to a Latin
        // font's metrics, so the fill tracks the glyphs instead of the line's spacing.
        let h = cell.round().max(1.0) as i32;
        let top = base_y - (cell * 0.8).round() as i32;
        fill_solid(pixels, page_w, page_h, s as i32, top, (e - s).max(1.0) as i32, h, c);
    };
    for g in glyphs {
        let (gx0, gx1) = (x_off + g.x, x_off + g.x + g.w.max(0.0));
        match info_of(g) {
            Some((col, cell)) => {
                seg = match seg {
                    Some((sc, s, _, h)) if sc == col => Some((sc, s, gx1, h.max(cell))),
                    Some((sc, s, e, h)) => {
                        flush(sc, s, e, h, pixels);
                        Some((col, gx0, gx1, cell))
                    }
                    None => Some((col, gx0, gx1, cell)),
                };
            }
            None => {
                if let Some((sc, s, e, h)) = seg.take() {
                    flush(sc, s, e, h, pixels);
                }
            }
        }
    }
    if let Some((sc, s, e, h)) = seg {
        flush(sc, s, e, h, pixels);
    }
}

#[allow(clippy::too_many_arguments)]
fn decoration_line(
    glyphs: &[cosmic_text::LayoutGlyph],
    bit: usize,
    x_off: f32,
    y: i32,
    thick: i32,
    pixels: &mut [u8],
    page_w: u32,
    page_h: u32,
    color: [u8; 3],
) {
    let mut seg: Option<(f32, f32)> = None; // (start_x, end_x) on the canvas
    let flush = |s: f32, e: f32, pixels: &mut [u8]| {
        fill_solid(pixels, page_w, page_h, s as i32, y, (e - s).max(1.0) as i32, thick, color);
    };
    for g in glyphs {
        if g.metadata & bit != 0 {
            let gx0 = x_off + g.x;
            let gx1 = gx0 + g.w.max(0.0);
            seg = Some(match seg {
                Some((s, _)) => (s, gx1),
                None => (gx0, gx1),
            });
        } else if let Some((s, e)) = seg.take() {
            flush(s, e, pixels);
        }
    }
    if let Some((s, e)) = seg {
        flush(s, e, pixels);
    }
}

/// Fill an opaque `w`x`h` rectangle of `color` at (`x`,`y`) into an RGBA8 page buffer (clipped to
/// `page_w`x`page_h`). Used for underline / strike decorations.
/// Draw a cell's resolved border edges as solid lines along its rect (page-local px). Each edge's
/// line sits inside the cell rect (so adjacent cells' shared edges align).
fn draw_cell_borders(c: &CellPlacement, page_w: u32, page_h: u32, pixels: &mut [u8]) {
    // Round the FAR edges from the float sums (never x + rounded w): adjacent rows/columns share
    // the same float boundary, so both cells' lines land on the same pixel row/column. Bottom and
    // right edges draw AT the boundary (straddling into the neighbour), collapsing with the
    // neighbour's top/left into one hairline like Word's grid - independent rounding produced
    // doubled lines and 1px white seams between rows.
    let x = c.x.round() as i32;
    let y = c.y.round() as i32;
    let x1 = (c.x + c.w).round() as i32;
    let y1 = (c.y + c.h).round() as i32;
    if let Some(b) = c.borders.top {
        let t = (b.width.round() as i32).max(1);
        fill_solid(pixels, page_w, page_h, x, y, x1 - x, t, b.color);
    }
    if let Some(b) = c.borders.bottom {
        let t = (b.width.round() as i32).max(1);
        fill_solid(pixels, page_w, page_h, x, y1, x1 - x + t, t, b.color);
    }
    if let Some(b) = c.borders.left {
        let t = (b.width.round() as i32).max(1);
        fill_solid(pixels, page_w, page_h, x, y, t, y1 - y, b.color);
    }
    if let Some(b) = c.borders.right {
        let t = (b.width.round() as i32).max(1);
        fill_solid(pixels, page_w, page_h, x1, y, t, y1 - y + t, b.color);
    }
}

/// Paint a paragraph border box (`w:pBdr`) around a block whose text occupies `[x, top_y]` to
/// `[x+w, top_y+h]`. Each edge sits *outside* the text by its own `space_px` (Word's text-to-line
/// gap); the horizontal lines span the full outer width so the corners meet. This is what draws a
/// text frame's rectangle. `dim` greys the lines to match the region. Absent edges draw nothing.
#[allow(clippy::too_many_arguments)]
fn paint_para_borders(
    b: &BlockBorders,
    x: f32,
    top_y: f32,
    w: f32,
    h: f32,
    page_w: u32,
    page_h: u32,
    pixels: &mut [u8],
    dim: f32,
) {
    let ext = |e: &Option<BorderLine>| e.map(|l| l.space_px + l.width_px).unwrap_or(0.0);
    let left_outer = (x - ext(&b.left)).round() as i32;
    let right_outer = (x + w + ext(&b.right)).round() as i32;
    let top_outer = (top_y - ext(&b.top)).round() as i32;
    let bot_outer = (top_y + h + ext(&b.bottom)).round() as i32;
    let span_w = (right_outer - left_outer).max(1);
    let span_h = (bot_outer - top_outer).max(1);
    if let Some(l) = b.top {
        let t = (l.width_px.round() as i32).max(1);
        fill_solid(pixels, page_w, page_h, left_outer, top_outer, span_w, t, dim_rgb(l.rgb, dim));
    }
    if let Some(l) = b.bottom {
        let t = (l.width_px.round() as i32).max(1);
        let y = (top_y + h + l.space_px).round() as i32;
        fill_solid(pixels, page_w, page_h, left_outer, y, span_w, t, dim_rgb(l.rgb, dim));
    }
    if let Some(l) = b.left {
        let t = (l.width_px.round() as i32).max(1);
        fill_solid(pixels, page_w, page_h, left_outer, top_outer, t, span_h, dim_rgb(l.rgb, dim));
    }
    if let Some(l) = b.right {
        let t = (l.width_px.round() as i32).max(1);
        let xr = (x + w + l.space_px).round() as i32;
        fill_solid(pixels, page_w, page_h, xr, top_outer, t, span_h, dim_rgb(l.rgb, dim));
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_solid(pixels: &mut [u8], page_w: u32, page_h: u32, x: i32, y: i32, w: i32, h: i32, color: [u8; 3]) {
    let (pw, ph) = (page_w as i32, page_h as i32);
    for dy in 0..h {
        let py = y + dy;
        if py < 0 || py >= ph {
            continue;
        }
        for dx in 0..w {
            let px = x + dx;
            if px < 0 || px >= pw {
                continue;
            }
            let idx = ((py as usize) * (page_w as usize) + (px as usize)) * 4;
            pixels[idx] = color[0];
            pixels[idx + 1] = color[1];
            pixels[idx + 2] = color[2];
            pixels[idx + 3] = 255;
        }
    }
}


#[cfg(test)]
mod tests;
