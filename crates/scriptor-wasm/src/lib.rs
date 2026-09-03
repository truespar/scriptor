//! WASM bindings for the Scriptor canvas editor (M0: the wasm32 compile gate).
//!
//! Thin glue between the browser TS canvas shell and the shared Rust core. Edit semantics live in
//! [`scriptor_edit`]; this layer only marshals bytes and (M1+) paints. M0 proves the whole core
//! graph - `scriptor-ooxml` (zip + quick-xml) -> `scriptor-crdt` (loro) -> `scriptor-edit` /
//! `scriptor-layout` (cosmic-text) - links for `wasm32-unknown-unknown`. That is the canvas-route
//! go / no-go: if the shaping + CRDT + OOXML stack cross-compiles, the engine has a foundation.

use scriptor_crdt::{FIELD_NUMPAGES, FIELD_PAGE, TrackKind};
use wasm_bindgen::prelude::*;

mod doc;
mod resolve;

use resolve::*;

/// How tracked changes (`w:ins` / `w:del`) are displayed - mirrors Word's Review > Display for
/// Review. Drives both appearance (insertions underlined / deletions struck, author-coloured) and
/// pagination (hiding deletions shortens the flow, matching Word's Final view).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackDisplay {
    /// Everything shown: insertions underlined + author-coloured, deletions struck + author-coloured.
    #[default]
    AllMarkup,
    /// Insertions as normal text, deletions hidden - but every changed paragraph gets a margin
    /// change-bar (the only markup Word's "Simple Markup" shows). Same text flow as `NoMarkup`.
    SimpleMarkup,
    /// Insertions as normal text, deletions hidden - the "Final" / accept-all view (shortest flow).
    NoMarkup,
    /// Insertions hidden, deletions as normal text - the pre-change "Original" view.
    Original,
}

impl TrackDisplay {
    /// Parse the API token (`all` / `simple` / `none` / `original`); aliases for Word's names too.
    fn parse(s: &str) -> Option<Self> {
        match s {
            "all" | "allMarkup" => Some(Self::AllMarkup),
            "simple" | "simpleMarkup" => Some(Self::SimpleMarkup),
            "none" | "noMarkup" | "final" => Some(Self::NoMarkup),
            "original" => Some(Self::Original),
            _ => None,
        }
    }
    /// Whether a run with this track kind is hidden (not laid out) in this display mode. A move's
    /// source half (`MoveFrom`) hides with deletions in the final views (the text shows at its new
    /// home); its destination half (`MoveTo`) hides with insertions in Original (the text shows at its
    /// old home).
    fn hides(self, kind: TrackKind) -> bool {
        matches!(
            (self, kind),
            (TrackDisplay::NoMarkup | TrackDisplay::SimpleMarkup, TrackKind::Del | TrackKind::MoveFrom)
                | (TrackDisplay::Original, TrackKind::Ins | TrackKind::MoveTo)
        )
    }
}

/// Route Rust panics to the browser console (dev ergonomics). Runs on module init.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Which document "story" a paragraph index addresses. The body, header, and footer are independent
/// flows (each its own child `CollabDoc`); to keep the JS shell's single `(para, off)` caret
/// coordinate, header/footer paragraph indices are *namespaced* into disjoint ranges above the body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    Body,
    Header,
    Footer,
}

// Header paragraphs occupy `[HEADER_BASE, FOOTER_BASE)`, footer `[FOOTER_BASE, ..)`, body `[0, ..)`.
// 2^28 is far above any realistic body paragraph count, so the ranges never collide in practice.
const HEADER_BASE: usize = 1 << 28;
const FOOTER_BASE: usize = 2 << 28;

/// How far an inactive region's ink fades toward white (Word greys the header/footer + logo while you
/// edit the body, and the body once you enter a header/footer). `0` solid, `1` invisible.
const DIM_INACTIVE: f32 = 0.62;

/// Split a namespaced paragraph index into its region + the region-local index.
fn decode_region(para: usize) -> (Region, usize) {
    if para >= FOOTER_BASE {
        (Region::Footer, para - FOOTER_BASE)
    } else if para >= HEADER_BASE {
        (Region::Header, para - HEADER_BASE)
    } else {
        (Region::Body, para)
    }
}

/// The first namespaced index of a region (so the JS shell can clamp caret movement to one story).
fn region_base(region: Region) -> usize {
    match region {
        Region::Body => 0,
        Region::Header => HEADER_BASE,
        Region::Footer => FOOTER_BASE,
    }
}

// ── editable-picture id encoding ─────────────────────────────────────────────
// A picture's wasm id encodes WHICH story owns it plus the story-local id, so a hit-test / edit
// routes back to the exact story. Body ids stay in `[0, IMG_STORY)` (small - the API is unchanged
// for body pictures). Story `1 + i` is header/footer PART `i` (the index into `ScriptorDoc::
// hf_sets`, one band per part file) - so two parts carrying the same story-local image id never
// collide, and a multi-section document routes each part's pictures to its own child doc.
const IMG_STORY: u64 = 1 << 28;
const IMG_BODY: u8 = 0;

/// Encode a story-local picture id into the global wasm id (story band + local).
fn img_enc(story: u8, local: u64) -> u64 {
    story as u64 * IMG_STORY + local
}
/// The owning story (`IMG_BODY`, or `1 + hf_sets index`) of an encoded picture id.
fn img_story(enc: u64) -> u8 {
    (enc / IMG_STORY) as u8
}
/// The story-local picture id of an encoded picture id.
fn img_local(enc: u64) -> u64 {
    enc % IMG_STORY
}

/// One headless-rendered page: dimensions + non-premultiplied RGBA8.
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Render every page of a `.docx` to RGBA8 (native test bench / server use). Reuses the exact same
/// pipeline as the browser - `relayout` + `paintPage` - so headless output matches the canvas. The
/// visual-diff harness compares these against a reference renderer (Word / LibreOffice). `scale` 1.0
/// = 96 px/in. `track` is the tracked-change display mode (`all` / `simple` / `none` / `original`);
/// unknown values fall back to All-Markup. Pass the matching mode to the reference renderer so a
/// Final-view comparison lines up.
pub fn render_all_pages(bytes: &[u8], scale: f32, track: &str) -> anyhow::Result<Vec<RenderedPage>> {
    let mut d = build_scriptor_doc(bytes)?;
    d.set_track_display(track);
    d.relayout(scale).map_err(|_| anyhow::anyhow!("layout failed"))?;
    let (w, h, n) = (d.layout.page_width, d.layout.page_height, d.layout.pages.len() as u32);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        out.push(RenderedPage { width: w, height: h, rgba: d.paint_page(i) });
    }
    Ok(out)
}

/// Page count of a `.docx` as Scriptor's layout engine paginates it - the SAME
/// pagination the canvas editor + headless renderer use, so the number matches
/// what the user sees in the editor (not Word/LibreOffice, which may break pages
/// slightly differently). Native/server use: the host server returns this so an agent
/// asked for "N pages" can hit the target by editing in place instead of
/// exporting a PDF to measure. Lays out the clean Final view (no revision
/// balloons) - the count a reader sees, not the markup view. `scale` 1.0 = 96
/// px/in; page count is scale-invariant.
pub fn docx_page_count(bytes: &[u8], scale: f32) -> anyhow::Result<usize> {
    let mut d = build_scriptor_doc(bytes)?;
    d.set_track_display("none");
    d.relayout(scale).map_err(|_| anyhow::anyhow!("layout failed"))?;
    Ok(d.layout.pages.len())
}

/// Dump the engine's per-paragraph layout geometry as JSON - the Scriptor half of the geometry oracle
/// (diff against the Word-COM reference, `scripts/word-geometry.ps1`). Reuses the EXACT caret geometry
/// we render, so the dump is "where we actually put each paragraph," not a parallel estimate. Emitted
/// in **points** (1pt = 96/72 px at scale 1.0) - resolution-independent and the same unit Word's
/// `Information()` reports, so the two line up without a conversion fudge. One record per body
/// paragraph in document order (top-level AND table-cell - the shared build_flow cursor flattens both
/// into `blocks`), each `{i, page (1-based), xPt, yPt, list, text}`. `xPt`/`yPt` are the paragraph's
/// start caret (offset 0) - its first glyph after any list marker - page-relative, matching Word's
/// collapse-to-start range position. `list` is the marker the engine computed ("1.", "1.1", a bullet).
pub fn dump_geometry(bytes: &[u8], scale: f32, track: &str) -> anyhow::Result<String> {
    let mut d = build_scriptor_doc(bytes)?;
    d.set_track_display(track);
    d.relayout(scale).map_err(|_| anyhow::anyhow!("layout failed"))?;
    Ok(d.geometry_json())
}

/// Escape a string for a JSON value: quote/backslash escaped, control chars (incl. newlines) flattened
/// to spaces. Non-ASCII passes through (the file is written UTF-8).
fn geo_json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// First ~60 chars of a paragraph's text, control chars folded to spaces + trimmed - a human-readable
/// anchor that also drives the diff's text alignment.
fn geo_preview(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    cleaned.trim().chars().take(60).collect()
}

impl ScriptorDoc {
    /// Build the geometry JSON for [`dump_geometry`] from the current layout. Per body paragraph: its
    /// page + start-caret position (points, page-relative) + computed list marker + a text preview.
    fn geometry_json(&self) -> String {
        let s = if self.scale_last > 0.0 { self.scale_last } else { 1.0 };
        let px_to_pt = (72.0 / 96.0) / s; // device px at the render scale -> points
        let stride = (self.layout.page_height + self.layout.gap) as f32; // page sheet + gutter
        let page_h_pt = self.layout.page_height as f32 * px_to_pt; // for absolute-position diffing
        let mut out = String::from("{\"source\":\"scriptor\",\"units\":\"pt\",\"pages\":");
        out.push_str(&self.layout.pages.len().max(1).to_string());
        out.push_str(&format!(",\"pageHeightPt\":{:.1},\"paragraphs\":[", page_h_pt));
        for (p, block) in self.blocks.iter().enumerate() {
            let text = self.para_texts.get(p).map(|t| t.as_str()).unwrap_or("");
            let page0 = self.paragraph_page(p as u32); // 0-based; resolves cells too
            let hint = self.page_hint_for(p);
            let (x, y, _h) = self.layout.caret_rect(p, 0, hint); // absolute device px
            let y_local = y - page0 as f32 * stride; // page-relative
            // Visual line count for this paragraph: the wrap-fidelity signal. One per laid-out
            // visual line; >=1 even for an empty paragraph.
            let lines = self.layout.lines.iter().filter(|l| l.para == p).count().max(1);
            if p > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"i\":{},\"page\":{},\"xPt\":{:.1},\"yPt\":{:.1},\"lines\":{},\"list\":\"{}\",\"keep\":{},\"text\":\"{}\"}}",
                p,
                page0 + 1,
                x * px_to_pt,
                y_local * px_to_pt,
                lines,
                geo_json_escape(block.marker.trim()),
                block.keep_next,
                geo_json_escape(&geo_preview(text)),
            ));
        }
        out.push_str("]}");
        out
    }
}

/// EMU (English Metric Units, 914400/inch - the unit the image model + `.docx` speak) -> canvas px at
/// zoom `scale` (1.0 = 96 px/in). The view sizes selection handles + draws crop overlays in px, so it
/// converts at this boundary rather than hard-coding the magic numbers. Natural-image-size conversion
/// (DPI-independent, 96 px/in) passes `scale = 1.0`; on-screen handle math passes the current zoom.
#[wasm_bindgen(js_name = emuToPx)]
pub fn emu_to_px(emu: f64, scale: f64) -> f64 {
    emu / 914_400.0 * 96.0 * scale
}

/// Canvas px at zoom `scale` -> EMU (the inverse of [`emu_to_px`]). The view turns a resize-handle
/// drag (px at the current zoom) or a decoded natural size (px at `scale = 1.0`) into the EMU the
/// edit ops want. Returns 0 for a non-positive scale (no sensible inverse).
#[wasm_bindgen(js_name = pxToEmu)]
pub fn px_to_emu(px: f64, scale: f64) -> f64 {
    if scale > 0.0 {
        px / (96.0 * scale) * 914_400.0
    } else {
        0.0
    }
}

/// Every bundled substitute face, so the DOM chrome can register `@font-face` rules and preview a
/// font / style menu in the SAME clone the canvas renders (true WYSIWYG - the OS has none of these MS
/// fonts installed). One entry per face: `family` is the MS name it substitutes for (so a CSS
/// `font-family:'Cambria'` label draws in Caladea, matching what the shaper paints), `bold`/`italic`
/// are the style flags, and `bytes` is the raw font data (the exact bytes embedded in this module -
/// no second copy shipped as a web asset). The DejaVu broad-Unicode fallback is skipped: it stands in
/// for no MS family (`substitute_family` never returns it), so it is never a menu entry.
#[wasm_bindgen(js_name = fontFaces)]
pub fn font_faces() -> js_sys::Array {
    let arr = js_sys::Array::new();
    for f in scriptor_fonts::bundled_fonts() {
        if f.substitutes == "DejaVu Sans" {
            continue;
        }
        let o = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&o, &JsValue::from_str("family"), &JsValue::from_str(f.substitutes));
        let _ = js_sys::Reflect::set(&o, &JsValue::from_str("bold"), &JsValue::from_bool(f.bold));
        let _ = js_sys::Reflect::set(&o, &JsValue::from_str("italic"), &JsValue::from_bool(f.italic));
        let _ = js_sys::Reflect::set(&o, &JsValue::from_str("bytes"), &js_sys::Uint8Array::from(f.data));
        arr.push(&o);
    }
    arr
}

/// Compare two `.docx` documents (blacklining): produce a **redline** - `original` with every
/// difference as an author-attributed tracked change - plus the change manifest. Returns a
/// `{ redline: Uint8Array, manifest: string }` object: `redline` is a Word-openable tracked-changes
/// `.docx` the view can open like any document (its changes then appear in the reviewing pane);
/// `manifest` is the deterministic change set as JSON (`{"changes":[…]}`) the UI parses for a
/// summary / change-list. The redline is attributed to `author` and dated `date` (a parameter, so the
/// result is deterministic).
#[wasm_bindgen(js_name = compareDocx)]
#[allow(clippy::too_many_arguments)]
pub fn compare_docx(
    original: &[u8],
    revised: &[u8],
    author: &str,
    date: &str,
    detect_formatting: bool,
    detect_moves: bool,
    ignore_whitespace: bool,
    ignore_case: bool,
) -> Result<JsValue, JsError> {
    let opts = scriptor_compare::CompareOptions {
        author: author.to_string(),
        date: date.to_string(),
        detect_formatting,
        detect_moves,
        ignore_whitespace,
        ignore_case,
        ..Default::default()
    };
    let result = scriptor_compare::compare(original, revised, &opts).map_err(to_js)?;
    let obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &obj,
        &JsValue::from_str("redline"),
        &js_sys::Uint8Array::from(&result.redline[..]),
    );
    let _ = js_sys::Reflect::set(
        &obj,
        &JsValue::from_str("manifest"),
        &JsValue::from_str(&result.manifest.to_json()),
    );
    Ok(obj.into())
}

/// A live document held across the FFI boundary. Owns the CRDT replica and the canvas renderer;
/// the TS shell holds an opaque handle to it.
#[wasm_bindgen]
pub struct ScriptorDoc {
    pub(crate) doc: scriptor_crdt::CollabDoc,
    pub(crate) renderer: scriptor_layout::Renderer,
    /// Caret geometry from the most recent paint - drives hit-testing + caret/selection rects.
    pub(crate) layout: scriptor_layout::DocLayout,
    /// Each paragraph's plain text as of the last paint. The caret geometry indexes glyphs by
    /// **byte** offset, but the model + the JS shell speak **codepoints**; these strings convert
    /// between the two at the API boundary so the rest of the system stays in char space. `para_texts`
    /// is the body; the header / footer stories keep their own (byte<->char conversion is per region).
    pub(crate) para_texts: Vec<String>,
    /// The resolved render blocks from the last `relayout` (style + inline formatting baked in).
    /// Retained so `paintPage` can rasterize a single page without re-resolving the whole document.
    pub(crate) blocks: Vec<scriptor_layout::Block>,
    /// Each page's last raster (RGBA), so `paintPageBand` can pixel-diff a fresh raster against
    /// what the caller's canvas shows and ship only the changed rows. Byte-capped LRU
    /// (`paint_order` is oldest-first); a missing / size-mismatched entry degrades to a full page.
    pub(crate) paint_cache: std::collections::HashMap<u32, Vec<u8>>,
    pub(crate) paint_order: Vec<u32>,
    /// Text frames (`w:framePr`) positioned per page in the last `relayout`; painted over the body by
    /// `paintPage` (the body already wrapped around them).
    pub(crate) frames: Vec<scriptor_layout::PageFrame>,
    /// Resolved render + caret data per header/footer PART, in [`scriptor_crdt::CollabDoc::hf_parts`]
    /// order (stable across relayouts of an unedited document). Which entry paints on a page comes
    /// from [`Self::page_hf`]; picture ids band by the index (`img_enc(1 + i, ..)`).
    pub(crate) hf_sets: Vec<HfSet>,
    /// Per page: the `hf_sets` index shown in the header band and the footer band (`None` = a blank
    /// band). Derived per relayout from the page's section (counting section-terminator blocks before
    /// the page's first block) + that section's effective refs + its `titlePg` - this is where Word's
    /// "which header does page N get" rule lives. Pages past the map (frame-extended tail) carry the
    /// last entry forward.
    pub(crate) page_hf: Vec<[Option<usize>; 2]>,
    pub(crate) header_y: f32,
    pub(crate) footer_dist_px: f32,
    /// Page geometry from the last relayout (device px) - drives image placement in `paintPage`.
    pub(crate) ml_px: f32,
    pub(crate) mr_px: f32,
    pub(crate) mt_px: f32,
    pub(crate) scale_last: f32,
    /// Whether the body contains a computed-field placeholder (PAGE/NUMPAGES) - so `paintPage` only
    /// clones + substitutes the body blocks when there is actually something to substitute.
    pub(crate) has_body_fields: bool,
    /// How tracked changes are displayed (Word's Display-for-Review). Read by `relayout`.
    pub(crate) track_display: TrackDisplay,
    /// Track-Changes (suggesting) mode: when on, typing / deleting author tracked changes instead of
    /// editing directly. The 1:1 analogue of Word's Review > Track Changes toggle.
    pub(crate) track_changes: bool,
    /// The current author stamped on tracked changes (`w:author`) + a stable id for the audit trail.
    pub(crate) author_id: String,
    pub(crate) author_name: String,
    /// The most recent ISO-8601 timestamp handed in by the JS shell (the engine never invents time);
    /// stamped on the next tracked change. Refreshed via [`set_now`] right before each tracked edit.
    pub(crate) now: String,
    /// Reviewers whose markup is filtered out of the display ("Show Markup by reviewer", keyed by
    /// `w:author`): their tracked changes + comments are suppressed in `relayout` (display-only).
    pub(crate) hidden_reviewers: std::collections::HashSet<String>,
    /// Review > Lock Tracking: when set, Track-Changes can't be turned off (it's forced on). Session
    /// state in v1 (not yet persisted to `settings.xml`'s `w:documentProtection`).
    pub(crate) track_locked: bool,
    /// Body paragraphs (by index) the editor expanded to inline All-Markup while the document is in
    /// Simple Markup - "click a change-bar to reveal that paragraph's redline". Only consulted in
    /// Simple Markup; cleared when the display mode changes or the document is replaced.
    pub(crate) expanded: std::collections::HashSet<usize>,
    /// Per-paragraph visible-run maps from the last `relayout`: each paragraph's surviving runs as
    /// full-text byte ranges, in order (`body_segments[i]` aligns with body paragraph `i`, table cells
    /// included). The caret geometry indexes the *visible* text while the model + JS shell index the
    /// *full* text; these bridge the two so editing lands correctly when runs are hidden (No-Markup /
    /// Original / Simple). Empty map = identity (nothing hidden in that paragraph).
    pub(crate) body_segments: Vec<Vec<(usize, usize)>>,
    /// Which story the caret is in (body / header / footer), set by the JS shell on selection change.
    /// Routes undo/redo to that story's child `CollabDoc` (each has its own loro `UndoManager`), so
    /// Ctrl+Z in a header undoes the header edit instead of always hitting the body.
    pub(crate) active_region: Region,
    /// The page whose header/footer instance the caret is on. A header/footer is one logical story
    /// painted on every page; this picks which painted instance the caret + selection resolve to, so a
    /// click into a header on page 3 of a multi-page document doesn't snap the caret to page 1.
    pub(crate) active_hf_page: u32,
    /// Revision balloons (Word's "Show Revisions in Balloons"): when on, tracked deletions move from
    /// the line into right-margin bubbles. Only takes effect in the markup display modes (All / Simple).
    pub(crate) balloons: bool,
    /// Balloon placements from the last `relayout` (one per paragraph with ballooned deletions),
    /// retained so `paint_page` can draw them per page.
    pub(crate) balloon_placements: Vec<scriptor_layout::BalloonPlacement>,
    /// Editable picture hit-rects (absolute canvas px) from the last `relayout` - inline + floating
    /// body pictures, id-tagged - for click-to-select + drawing the selection handles in the view.
    pub(crate) image_rects: Vec<ImageHit>,
    /// Picture ids hidden under the current tracked-change display mode (a deleted picture in Final /
    /// an inserted one in Original, or a filtered reviewer's), from the last `relayout`. Floating
    /// pictures + hit-rects consult this; inline pictures are filtered inside `resolve_blocks`.
    pub(crate) hidden_images: std::collections::HashSet<u64>,
}

/// One header/footer part's resolved render + caret data (see [`ScriptorDoc::hf_sets`]): the
/// blocks the band paints, the plain texts + visible-run maps the caret speaks, and a content hash
/// for the per-page fingerprint fold.
struct HfSet {
    /// Part name (`word/footer2.xml`) - the key back into the crdt's part map.
    part: String,
    /// `w:hdr` vs `w:ftr` - which band the part paints in.
    header: bool,
    blocks: Vec<scriptor_layout::Block>,
    texts: Vec<String>,
    segments: Vec<Vec<(usize, usize)>>,
    hash: u64,
}

/// An editable picture's hit-rect in absolute canvas coordinates (page-stacked y), for hit-test +
/// selection in the view.
#[derive(Clone, Copy)]
struct ImageHit {
    id: u64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// The page this hit-rect is on. A header/footer picture repeats on every page (and a Different
    /// First Page doc has the first + default stories share an image id), so `image_rect` must return
    /// the instance on the page the user actually clicked - not just the first id match.
    page: u32,
}

/// Build a `ScriptorDoc` from raw `.docx` bytes via the anyhow path. Kept separate from the wasm
/// `open_docx` so the native CLI / geometry oracle gets a clean import error instead of aborting:
/// constructing a `JsError` (what `open_docx` returns) panics off-wasm, so an unopenable file
/// (encrypted, ODF-mislabelled, malformed) would crash the process rather than report. The wasm
/// `open_docx` wraps this and maps the error to `JsError` at the boundary, where that's fine.
fn build_scriptor_doc(bytes: &[u8]) -> anyhow::Result<ScriptorDoc> {
    Ok(scriptor_doc_from(scriptor_crdt::CollabDoc::from_docx_bytes(bytes)?))
}

/// Wrap a ready [`scriptor_crdt::CollabDoc`] in a fresh `ScriptorDoc` (renderer +
/// empty layout caches). Shared by the `.docx` open path and the collaboration
/// `fromSnapshot` path, which differ only in how the CRDT replica is built.
fn scriptor_doc_from(doc: scriptor_crdt::CollabDoc) -> ScriptorDoc {
    let mut renderer = scriptor_layout::Renderer::new();
    // The page-sheet fill: painted only when the document opts in (w:displayBackgroundShape).
    if doc.page_background_shown() {
        renderer.set_page_background(doc.page_background().map(parse_hex));
    }
    ScriptorDoc {
        doc,
        renderer,
        layout: scriptor_layout::DocLayout::default(),
        para_texts: Vec::new(),
        blocks: Vec::new(),
        frames: Vec::new(),
        paint_cache: Default::default(),
        paint_order: Vec::new(),
        hf_sets: Vec::new(),
        page_hf: Vec::new(),
        header_y: 0.0,
        footer_dist_px: 0.0,
        ml_px: 0.0,
        mr_px: 0.0,
        mt_px: 0.0,
        scale_last: 1.0,
        has_body_fields: false,
        track_display: TrackDisplay::default(),
        track_changes: false,
        author_id: "local".to_string(),
        author_name: "You".to_string(),
        now: String::new(),
        hidden_reviewers: std::collections::HashSet::new(),
        track_locked: false,
        expanded: std::collections::HashSet::new(),
        body_segments: Vec::new(),
        active_region: Region::Body,
        active_hf_page: 0,
        balloons: false,
        balloon_placements: Vec::new(),
        image_rects: Vec::new(),
        hidden_images: std::collections::HashSet::new(),
    }
}

#[wasm_bindgen]
impl ScriptorDoc {
    /// Open a `.docx` (the raw OPC zip bytes, e.g. from a `File`) into the CRDT model.
    #[wasm_bindgen(js_name = openDocx)]
    pub fn open_docx(bytes: &[u8]) -> Result<ScriptorDoc, JsError> {
        build_scriptor_doc(bytes).map_err(to_js)
    }

    /// Build a document from a loro snapshot - the collaboration join message
    /// from the server. Unlike the `constructor`, it seeds NO empty paragraph:
    /// the snapshot is the authoritative content, and a seed would union a stray
    /// blank paragraph into the merged document.
    #[wasm_bindgen(js_name = fromSnapshot)]
    pub fn from_snapshot(snapshot: &[u8]) -> Result<ScriptorDoc, JsError> {
        let doc = scriptor_crdt::CollabDoc::new();
        doc.merge(snapshot).map_err(to_js)?;
        doc.clear_undo(); // the joined state is the baseline, not an undoable edit
        Ok(scriptor_doc_from(doc))
    }

    /// Start an empty document (no file) - used to author a fresh doc in the editor. Seeds one
    /// empty paragraph so the caret has a block to type into (an editor never has zero paragraphs).
    #[wasm_bindgen(constructor)]
    pub fn new() -> ScriptorDoc {
        let doc = scriptor_crdt::CollabDoc::new();
        let _ = doc.append_paragraph(&[], None);
        doc.clear_undo(); // the seed paragraph is the baseline, not an undoable edit
        ScriptorDoc {
            doc,
            renderer: scriptor_layout::Renderer::new(),
            layout: scriptor_layout::DocLayout::default(),
            para_texts: Vec::new(),
            blocks: Vec::new(),
            frames: Vec::new(),
            paint_cache: Default::default(),
            paint_order: Vec::new(),
            hf_sets: Vec::new(),
            page_hf: Vec::new(),
            header_y: 0.0,
            footer_dist_px: 0.0,
            ml_px: 0.0,
            mr_px: 0.0,
            mt_px: 0.0,
            scale_last: 1.0,
            has_body_fields: false,
            track_display: TrackDisplay::default(),
            track_changes: false,
            author_id: "local".to_string(),
            author_name: "You".to_string(),
            now: String::new(),
            hidden_reviewers: std::collections::HashSet::new(),
            track_locked: false,
            expanded: std::collections::HashSet::new(),
            body_segments: Vec::new(),
            active_region: Region::Body,
            active_hf_page: 0,
            balloons: false,
            balloon_placements: Vec::new(),
            image_rects: Vec::new(),
            hidden_images: std::collections::HashSet::new(),
        }
    }

    // ── Live collaboration ────────────────────────────────────────
    // Expose the CRDT replica's merge / snapshot / delta + anchor primitives to
    // the TS collab provider, so the browser is a real loro peer over the collab
    // websocket. The wire is raw loro bytes (snapshots or `ExportMode::Updates`
    // deltas); anchors are edit-stable caret references for remote cursors and
    // for keeping the local caret on the same character across a remote merge.

    /// A full, self-contained snapshot of the document (history + state) - what a
    /// joining client ships, and the merge unit the server sends on join.
    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<Vec<u8>, JsError> {
        self.doc.snapshot().map_err(to_js)
    }

    /// Merge a remote loro blob (a snapshot or an update delta) into this replica.
    /// Loro merges are commutative + idempotent, so order does not matter and a
    /// re-merge is a no-op. The caller re-renders afterward (`relayout` re-reads
    /// the model); pair with `caretAnchor` + `resolveAnchor` to keep the local
    /// caret on the same character when a concurrent edit shifts offsets.
    #[wasm_bindgen(js_name = merge)]
    pub fn merge(&self, bytes: &[u8]) -> Result<(), JsError> {
        self.doc.merge(bytes).map_err(to_js)
    }

    /// The current oplog version, encoded. Hold it, then `exportUpdatesSince` to
    /// ship only the ops committed since - the efficient wire delta.
    #[wasm_bindgen(js_name = oplogVersion)]
    pub fn oplog_version(&self) -> Vec<u8> {
        self.doc.version().encode()
    }

    /// Export the ops committed since `version` (from `oplogVersion`) as a loro
    /// update delta to send to peers.
    #[wasm_bindgen(js_name = exportUpdatesSince)]
    pub fn export_updates_since(&self, version: &[u8]) -> Result<Vec<u8>, JsError> {
        let from = scriptor_crdt::DocVersion::decode(version).map_err(to_js)?;
        self.doc.export_updates_since(&from).map_err(to_js)
    }

    /// Encode an edit-stable anchor at a body caret position (codepoint offset).
    /// Send it as your cursor for presence, or capture it before a merge and
    /// resolve it after to remap the local caret.
    #[wasm_bindgen(js_name = caretAnchor)]
    pub fn caret_anchor(&self, para: u32, off: u32) -> Result<Vec<u8>, JsError> {
        let a = self
            .doc
            .anchor(para as usize, off as usize, scriptor_crdt::Side::Left)
            .map_err(to_js)?;
        Ok(a.to_bytes())
    }

    /// Resolve an anchor (from `caretAnchor`) to a current body `[para, off]`, or
    /// `undefined` if the anchored block was deleted. Both a live and a shifted
    /// anchor return a position (the caret follows the content); only a deleted
    /// block returns nothing.
    #[wasm_bindgen(js_name = resolveAnchor)]
    pub fn resolve_anchor(&self, anchor: &[u8]) -> Option<Vec<u32>> {
        let a = scriptor_crdt::Anchor::from_bytes(anchor).ok()?;
        match self.doc.resolve(&a) {
            scriptor_crdt::Resolved::Live { para, off }
            | scriptor_crdt::Resolved::Shifted { para, off } => Some(vec![para as u32, off as u32]),
            scriptor_crdt::Resolved::Deleted => None,
        }
    }

    /// Encode an edit-stable anchor for a SELECTED RANGE `[(p1,o1), (p2,o2))`
    /// (body codepoint offsets). Send it with an inline select->ask so the agent
    /// edits exactly that span via the anchored `document_propose_edit`. The head
    /// biases left, the tail right, so the range doesn't grow/shrink spuriously
    /// when text is inserted at either edge.
    #[wasm_bindgen(js_name = anchorRange)]
    pub fn anchor_range(&self, p1: u32, o1: u32, p2: u32, o2: u32) -> Result<Vec<u8>, JsError> {
        let start = self
            .doc
            .anchor(p1 as usize, o1 as usize, scriptor_crdt::Side::Left)
            .map_err(to_js)?;
        let end = self
            .doc
            .anchor(p2 as usize, o2 as usize, scriptor_crdt::Side::Right)
            .map_err(to_js)?;
        Ok(scriptor_crdt::AnchorRange { start, end }.to_bytes())
    }

    /// Resolve an anchored range (from `anchorRange`) to current body coordinates
    /// `[p1, o1, p2, o2]`, or `undefined` if it no longer resolves.
    #[wasm_bindgen(js_name = resolveRange)]
    pub fn resolve_range(&self, range: &[u8]) -> Option<Vec<u32>> {
        let r = scriptor_crdt::AnchorRange::from_bytes(range).ok()?;
        self.doc
            .resolve_range_multi(&r)
            .map(|(p1, o1, p2, o2)| vec![p1 as u32, o1 as u32, p2 as u32, o2 as u32])
    }

    // ── page commands (Layout tab) - re-paint after; page geometry is not yet a CRDT op ──────

    // ── header / footer (plain-text edit, v1) ────────────────────────────────────

    // ── tracked-change display (Review > Display for Review) ──────────────────────

    // ── tracked-change authoring (Review > Track Changes) ─────────────────────────

    // ── reviewer filter (Review > Show Markup by reviewer) ────────────────────────

    // ── accept / reject (resolve on the live model) ───────────────────────────────

    // ── comments (annotations) ────────────────────────────────────────────────────

    // ── table structure editing (rows + columns) ─────────────────────────────────

    // ── table / row / cell property edits (tracked as w:tcPrChange / w:trPrChange / w:tblPrChange) ──

    // ── paragraph formatting (Home tab Paragraph group) ──────────────────────────

    // ── run formatting (Home tab Font group) - all route through scriptor_edit::apply ──

}

/// Count words roughly the way Word does: split on whitespace AND em / en dashes (so "a—b" is two
/// words and a lone "—" between spaces is none), while keeping hyphenated and slash-joined tokens
/// ("non-disclosure", "and/or") as a single word. Exact parity with Word's proprietary counter isn't
/// achievable, but this narrows the common gaps.
fn count_words(text: &str) -> usize {
    text.split(|c: char| c.is_whitespace() || c == '\u{2014}' || c == '\u{2013}')
        .filter(|w| !w.is_empty())
        .count()
}

impl Default for ScriptorDoc {
    fn default() -> Self {
        Self::new()
    }
}

// Byte<->codepoint conversion against the paragraph texts cached at the last paint. The caret
// geometry is byte-indexed (cosmic-text glyph offsets); the model + JS shell are codepoint-indexed.
// Indices are namespaced (body / header / footer), so decode the region first.
impl ScriptorDoc {
}

/// Escape a string for embedding in a JSON string literal (used by `listComments`, which hand-builds
/// JSON since the wasm crate has no serde_json dependency).
fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Map a JS line-rule token to a [`scriptor_crdt::LineRule`]: `exact` -> absolute twips, anything
/// else (`auto` / empty / `atLeast`) -> `None` = the 240ths multiplier path. `atLeast` is accepted by
/// the model but left on the multiplier path in render, so the dialog only offers auto + exact.
fn parse_line_rule(token: &str) -> Option<scriptor_crdt::LineRule> {
    match token {
        "exact" | "exactly" => Some(scriptor_crdt::LineRule::Exact),
        "atLeast" | "atleast" => Some(scriptor_crdt::LineRule::AtLeast),
        _ => None,
    }
}

/// A valid, unique `w:styleId` derived from a human style `name`: Unicode letters + digits (so
/// "Brödtext" / "Überschrift" keep their ö / Ü), spaces + punctuation stripped (Word's styleId
/// convention), disambiguated with a numeric suffix when it collides with an existing style id /
/// name. A name with no letters/digits at all falls back to `Style`.
fn unique_style_id(existing: &std::collections::HashSet<String>, name: &str) -> String {
    let base: String = name.chars().filter(|c| c.is_alphanumeric()).collect();
    let base = if base.is_empty() { "Style".to_string() } else { base };
    if !existing.contains(&base) {
        return base;
    }
    (1..).map(|n| format!("{base}{n}")).find(|c| !existing.contains(c)).unwrap_or(base)
}

/// Per-paragraph plain text of a header/footer paragraph slice (cached for byte<->char conversion).
fn hf_plain(paras: &[scriptor_crdt::Paragraph]) -> Vec<String> {
    paras.iter().map(|p| p.runs.iter().map(|r| r.text.as_str()).collect()).collect()
}

/// FNV-1a fingerprint over a header/footer block run's visible content (text + size + decorations +
/// change-bar flag + tracked ¶), so an edit to the header/footer dirties the pages it paints on.
fn hf_fingerprint(blocks: &[scriptor_layout::Block]) -> u64 {
    fn fold(h: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *h ^= b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in blocks {
        fold(&mut h, &[b.has_change as u8]);
        fold(&mut h, b.trailing.as_bytes());
        for s in &b.spans {
            fold(&mut h, s.text.as_bytes());
            fold(&mut h, &s.size_px.to_bits().to_le_bytes());
            fold(
                &mut h,
                &[s.bold as u8, s.italic as u8, s.underline as u8, s.strike as u8,
                  s.color[0], s.color[1], s.color[2]],
            );
        }
    }
    h
}

impl ScriptorDoc {
}

/// The result of a [`ScriptorDoc::relayout`]: the page dimensions (device px) so the browser can
/// size + lay out the page stack, plus the per-page fingerprints it diffs to decide which pages to
/// re-rasterize via [`ScriptorDoc::paint_page`].
#[wasm_bindgen]
pub struct LayoutInfo {
    page_width: u32,
    page_height: u32,
    gap: u32,
    total_height: u32,
    fingerprints: Vec<u64>,
}

#[wasm_bindgen]
impl LayoutInfo {
    #[wasm_bindgen(getter, js_name = pageWidth)]
    pub fn page_width(&self) -> u32 {
        self.page_width
    }
    #[wasm_bindgen(getter, js_name = pageHeight)]
    pub fn page_height(&self) -> u32 {
        self.page_height
    }
    #[wasm_bindgen(getter)]
    pub fn gap(&self) -> u32 {
        self.gap
    }
    #[wasm_bindgen(getter, js_name = totalHeight)]
    pub fn total_height(&self) -> u32 {
        self.total_height
    }
    #[wasm_bindgen(getter, js_name = pageCount)]
    pub fn page_count(&self) -> u32 {
        self.fingerprints.len() as u32
    }
    /// The per-page content fingerprints (one per page). The caller compares against the previous
    /// set and re-rasterizes only the pages whose value changed.
    #[wasm_bindgen(getter)]
    pub fn fingerprints(&self) -> Vec<u64> {
        self.fingerprints.clone()
    }
}

/// The selection's resolved formatting, for the toolbar. Each boolean has a companion `*Mixed`
/// getter (true when the selection spans both states); `size` is 0 when mixed/unset; `color` /
/// `font` are empty strings when mixed/unset.
#[wasm_bindgen]
pub struct SelFormat {
    bold: bool,
    bold_mixed: bool,
    italic: bool,
    italic_mixed: bool,
    underline: bool,
    underline_mixed: bool,
    strike: bool,
    strike_mixed: bool,
    size: u16,
    color: String,
    font: String,
    highlight: String,
    vert_align: String,
}

#[wasm_bindgen]
impl SelFormat {
    #[wasm_bindgen(getter)]
    pub fn bold(&self) -> bool {
        self.bold
    }
    #[wasm_bindgen(getter, js_name = boldMixed)]
    pub fn bold_mixed(&self) -> bool {
        self.bold_mixed
    }
    #[wasm_bindgen(getter)]
    pub fn italic(&self) -> bool {
        self.italic
    }
    #[wasm_bindgen(getter, js_name = italicMixed)]
    pub fn italic_mixed(&self) -> bool {
        self.italic_mixed
    }
    #[wasm_bindgen(getter)]
    pub fn underline(&self) -> bool {
        self.underline
    }
    #[wasm_bindgen(getter, js_name = underlineMixed)]
    pub fn underline_mixed(&self) -> bool {
        self.underline_mixed
    }
    #[wasm_bindgen(getter)]
    pub fn strike(&self) -> bool {
        self.strike
    }
    #[wasm_bindgen(getter, js_name = strikeMixed)]
    pub fn strike_mixed(&self) -> bool {
        self.strike_mixed
    }
    /// Font size in half-points (OOXML `w:sz`); 0 when mixed or unset.
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u16 {
        self.size
    }
    /// Text color `RRGGBB`; empty when mixed or unset.
    #[wasm_bindgen(getter)]
    pub fn color(&self) -> String {
        self.color.clone()
    }
    /// Font family; empty when mixed or unset.
    #[wasm_bindgen(getter)]
    pub fn font(&self) -> String {
        self.font.clone()
    }
    /// Highlight color name; empty when none or mixed.
    #[wasm_bindgen(getter)]
    pub fn highlight(&self) -> String {
        self.highlight.clone()
    }
    /// Vertical alignment ("superscript" / "subscript"); empty when baseline or mixed.
    #[wasm_bindgen(getter, js_name = vertAlign)]
    pub fn vert_align(&self) -> String {
        self.vert_align.clone()
    }
}

/// A paragraph's formatting, for the toolbar's Paragraph group. `align` is "" when unset;
/// `lineSpacing` (240ths) + indents are 0 when unset.
#[wasm_bindgen]
pub struct ParaFmt {
    align: String,
    line_spacing: u16,
    indent_left: i32,
    indent_right: i32,
    indent_first: i32,
}

#[wasm_bindgen]
impl ParaFmt {
    #[wasm_bindgen(getter)]
    pub fn align(&self) -> String {
        self.align.clone()
    }
    #[wasm_bindgen(getter, js_name = lineSpacing)]
    pub fn line_spacing(&self) -> u16 {
        self.line_spacing
    }
    #[wasm_bindgen(getter, js_name = indentLeft)]
    pub fn indent_left(&self) -> i32 {
        self.indent_left
    }
    #[wasm_bindgen(getter, js_name = indentRight)]
    pub fn indent_right(&self) -> i32 {
        self.indent_right
    }
    #[wasm_bindgen(getter, js_name = indentFirst)]
    pub fn indent_first(&self) -> i32 {
        self.indent_first
    }
}

/// A tracked change under a point (hover tooltip + click popup): the revision id, its kind (`"ins"`
/// or `"del"`), the author, the ISO date, and the change's text.
#[wasm_bindgen]
pub struct TrackHit {
    id: u32,
    kind: String,
    author: String,
    date: String,
    text: String,
}

#[wasm_bindgen]
impl TrackHit {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }
    /// `"ins"` (insertion) or `"del"` (deletion).
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn author(&self) -> String {
        self.author.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn date(&self) -> String {
        self.date.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.text.clone()
    }
}





/// Render an `anyhow::Error` chain into a JS error for the boundary.
fn to_js(e: anyhow::Error) -> JsError {
    JsError::new(&format!("{e:#}"))
}

#[cfg(test)]
mod tests;
