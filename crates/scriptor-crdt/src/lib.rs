//! OOXML <-> CRDT binding (loro).
//!
//! [`CollabDoc`] wraps a `loro::LoroDoc` holding the document as a block tree (see [`model`]):
//! one node per paragraph, run text as `LoroText`, run formatting and tracked changes as Peritext
//! marks. Concurrent edits from any number of peers (humans and the agent) merge deterministically
//! and re-serialize to valid, Word-openable OOXML: import a `.docx` into the tree, edit, and
//! export valid `document.xml` back, with tracked changes preserved as marks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use loro::{
    ContainerID, ContainerTrait, ExpandType, ExportMode, LoroDoc, StyleConfig, StyleConfigMap, TreeID,
    UndoManager, VersionVector,
};

mod doc;
mod package;

use package::*;
pub mod extract;
pub mod model;
pub mod table_crdt;
pub use model::{
    Align, BodyItem, Border, CellMargins, Comment, EdgeBorders, FormatChange, ImagePlacement, LineRule,
    ImportStats, ListFormat, NumLevel, Numbering, PageGeometry, ParaPropChange, ParaProps, Paragraph,
    Run, RunFormat, RunProps, SectionCols, SelectionFormat, StyleProps, StyleTable, Table, TableCell,
    TablePropChange, TablePropLevel, TablePropSnapshot, TableRow, Track, TrackKind, TrackedRegion,
    VMerge, FIELD_NUMPAGES, FIELD_PAGE,
};

/// A collaborative Word document backed by a loro CRDT.
pub struct CollabDoc {
    doc: LoroDoc,
    /// The `word/styles.xml` definitions exactly as imported (document defaults + named styles +
    /// merged-in Word built-ins). Immutable after construction - the base over which runtime
    /// style-definition edits ([`model::STYLE_OVERRIDES`]) are reconciled to form `styles`.
    styles_base: StyleTable,
    /// The **effective** style table: `styles_base` with the runtime style edits folded in. Rebuilt
    /// from base + the loro [`model::STYLE_OVERRIDES`] map on read (see [`Self::styles`]), gated by
    /// `styles_dirty` so an unedited document never re-clones it. Interior-mutable because the
    /// `&self` read path reconciles it (mirrors the `numbering` field). Not itself in the op log;
    /// the *edits* are (in `STYLE_OVERRIDES`), so they persist / sync / undo.
    styles: std::cell::RefCell<StyleTable>,
    /// Set whenever the [`model::STYLE_OVERRIDES`] map may have changed (a `set_style_props`, a
    /// `merge` from a peer / snapshot, an undo/redo) so the next `styles()` read rebuilds the
    /// effective table; cleared once rebuilt. Avoids a full table clone on every keystroke's relayout.
    styles_dirty: std::cell::Cell<bool>,
    /// Page size + margins from `w:sectPr` (static doc metadata; drives the rendered page size).
    page: PageGeometry,
    /// Legacy paragraph-spacing mode from `word/settings.xml`
    /// ([`model::settings_legacy_spacing`]): adjacent space-after + space-before SUM instead of
    /// consolidating to the max, selected by `w:doNotUseHTMLParagraphAutoSpacing` or a
    /// `compatibilityMode` of Word 2003 or older. Static doc metadata, read by the layout.
    legacy_spacing: bool,
    /// The document page-background fill (`<w:background w:color>`, hex) - always kept for the
    /// save round-trip; painted only when [`Self::background_shown`] (Word's
    /// `w:displayBackgroundShape` settings gate).
    background: Option<String>,
    background_shown: bool,
    /// Per-section newspaper-column geometry (`w:sectPr/w:cols`), in document order - entry N is
    /// section N (aligning with the `section_terminator` paragraphs layout walks). Drives multi-column
    /// page flow. Empty / all-single-column for the overwhelming majority of documents.
    sections: Vec<SectionCols>,
    /// Header / footer **parts** as child documents (each its own loro tree + undo), keyed by part
    /// name (`word/header2.xml`), parsed from `headerN.xml` / `footerN.xml`. Making them full
    /// `CollabDoc`s means they edit through the exact same path as the body - typing, tracked
    /// changes, accept/reject, formatting all reuse the body machinery. Part-keyed (not role-keyed):
    /// a multi-section document has one part per distinct `headerReference`/`footerReference` target,
    /// several sections may share one part (Word's cross-section inheritance IS reference sharing),
    /// and each part must save back to its own file. Boxed to break the otherwise-infinite recursive
    /// type; a child's own map is always empty.
    hf_docs: std::collections::BTreeMap<String, HfPartDoc>,
    /// Per-section header/footer bindings, one entry per `<w:sectPr>` in document order (entry N =
    /// section N, aligned with the section-terminator paragraphs; the body-final sectPr is last).
    /// Slots hold **effective** part names: Word's inheritance (a section without its own reference
    /// uses the previous section's part) is resolved at import, so a `None` here means no section up
    /// to this one defines that slot. Always at least one entry.
    sections_hf: Vec<SectionHf>,
    /// `<w:titlePg/>` anywhere in the document - kept for the synthesized single-section export
    /// (multi-section documents re-emit their sectPrs verbatim). Per-section rendering reads
    /// [`SectionHf::title_pg`] instead.
    title_pg: bool,
    /// Anchored text boxes (`wps:wsp` with `w:txbxContent`) found in header/footer parts - the
    /// rotated margin stamps legal templates put in a footer. Render-only metadata (v1): painted onto
    /// the page; the XML itself round-trips inside the part's paragraph passthrough.
    textboxes: Vec<PlacedTextBox>,
    /// The original OPC parts of the opened `.docx` (empty for a fresh document). Retained so a full
    /// save can re-zip with every other part preserved verbatim.
    source_parts: Vec<scriptor_ooxml::Part>,
    /// The comments exactly as imported, so a save can tell whether any of them actually changed.
    ///
    /// A comment body is modeled as plain text (see [`Comment`]), so re-emitting `comments.xml` from
    /// the model discards run formatting, paragraph properties and any table inside a comment. That
    /// is an accepted modelling limit for a comment somebody edited; it is not acceptable for a
    /// document that was merely opened and saved. Comparing against this snapshot keeps the original
    /// part whenever the comment set is untouched.
    ///
    /// A snapshot rather than a dirty flag because every comment mutator takes `&self` through loro's
    /// interior mutability, so there is no single `&mut` choke point to hang a flag on the way
    /// [`HfPartDoc::dirty`] hangs on `hf_part_doc_mut` - and an equality check cannot be bypassed by
    /// a mutator added later that forgets to set the flag.
    imported_comments: Vec<Comment>,
    /// The section's header/footer references (default + first) + their part names, for save round-trip.
    hf: Vec<(model::HfRef, String)>,
    /// Resolved list definitions - drives the computed list markers. Holds two populations: definitions
    /// parsed from an imported `word/numbering.xml` (set once at construction), and runtime-synthesized
    /// definitions whose identity lives in the loro [`model::NUM_SYNTH`] map. The latter are reconciled
    /// in from loro lazily on read (see [`Self::numbering`]), so a runtime list survives a reopen,
    /// arrives from a peer over `merge`, and renders live - not just on export. Interior-mutable because
    /// the reconcile + the `&self` `ensure_list` write paths run on a shared `&self`.
    numbering: std::cell::RefCell<Numbering>,
    /// Pictures found in the body + header/footer parts, with their blip resolved to a media part
    /// name. Render-only metadata (v1): composited onto the page, not yet in the CRDT / save path.
    images: Vec<PlacedImage>,
    /// Bytes for pictures **inserted** this session, keyed by their media part name (e.g.
    /// `word/media/image3.png`), injected into the package on save (`to_docx_bytes`). Imported pictures
    /// keep their bytes in `source_parts`; this holds only newly-inserted media. Interior-mutable so the
    /// `&self` insert path can stash bytes. Not synced (media does not yet transfer to a joined peer).
    pending_media: std::cell::RefCell<HashMap<String, Vec<u8>>>,
    /// Undo/redo over this peer's local edits (loro's built-in manager). Text/format/structural CRDT
    /// edits are undoable; page-geometry + header/footer changes (not in the op log) are not yet.
    undo: UndoManager,
    /// Batch revision-id allocator, active only inside [`Self::begin_bulk`]/[`Self::end_bulk`]. Normally
    /// `None`: [`Self::next_revision_id`] scans the whole document for the max id (fine for one
    /// interactive edit). During a bulk emission (document comparison replays thousands of `suggest_*`
    /// ops) that per-op O(N) rescan is quadratic, so a batch seeds this once and hands out monotonically
    /// increasing ids without rescanning. Interior-mutable: the allocator runs on the `&self` edit path.
    rev_counter: std::cell::Cell<Option<u64>>,
}

/// Where a [`PlacedImage`] lives - drives which page(s) it paints on and how its anchor resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageContext {
    /// In the document body, anchored to the given 0-based paragraph index.
    Body(usize),
    /// In a header/footer part (by part name, e.g. `word/footer2.xml`). Painted on exactly the pages
    /// whose section shows that part; `header` picks the top vs bottom band.
    Hf { part: String, header: bool },
}

/// A header/footer part's child document plus its role (a `w:hdr` vs a `w:ftr` - the role decides
/// the wrapper element on save and which band it paints in).
struct HfPartDoc {
    is_header: bool,
    doc: Box<CollabDoc>,
    /// Whether this part has been handed out for mutation since it was imported.
    ///
    /// Save re-renders a header/footer from the child story, and that story is a flat paragraph
    /// list - a table in a header comes back as loose paragraphs, losing rows, cells and borders.
    /// So an untouched part is left exactly as it arrived instead, and only an edited one is
    /// rewritten. Set by [`CollabDoc::hf_part_doc_mut`], the single `&mut` accessor.
    ///
    /// Deliberately over-approximating: taking the `&mut` marks the part edited whether or not the
    /// caller writes anything. Erring this way costs a needless re-render, which is what every save
    /// used to do; erring the other way would silently discard a real edit.
    dirty: bool,
}

/// One section's header/footer bindings (see `CollabDoc::sections_hf`): **effective** part names
/// per slot, with Word's carry-forward inheritance already applied. `even` slots are carried for
/// completeness but not rendered (`w:evenAndOddHeaders` is not modeled; their refs + parts
/// round-trip verbatim).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionHf {
    pub header_default: Option<String>,
    pub header_first: Option<String>,
    pub footer_default: Option<String>,
    pub footer_first: Option<String>,
    /// This section's `<w:titlePg/>`: its FIRST page uses the `first` slots (blank when unset - Word
    /// shows an empty band, not the default) and every other page the `default` slots.
    pub title_pg: bool,
}

/// An anchored text box (`wps:wsp` + `w:txbxContent`) from a header/footer part, resolved for
/// painting: the legal-template margin stamp. Positions are EMU offsets from the anchor origin
/// named by `h_from` / `v_from` (`page` / `margin` / `column` / `paragraph`), like a floating
/// picture's.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedTextBox {
    /// The part the box lives in - painted on the pages that show this part.
    pub context: ImageContext,
    /// The box's plain text (first paragraph of `w:txbxContent`, cached field results included).
    pub text: String,
    /// Run font family (`w:rFonts w:ascii`), `None` = inherit default.
    pub font: Option<String>,
    /// Run font size in half-points (`w:sz`), 0 = unset (inherit default).
    pub size_half_points: u32,
    /// Run color as `RRGGBB` hex, `None` = automatic.
    pub color: Option<String>,
    /// Text flow from `wps:bodyPr w:vert`: `0` horizontal, `1` = `vert` (top-to-bottom, 90 CW),
    /// `2` = `vert270` (bottom-to-top, 90 CCW - the rotated margin stamp).
    pub vert: u8,
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub h_from: String,
    pub v_from: String,
    /// 0-based paragraph index of the box's anchor within its part's story - the caret target a
    /// click on the box resolves to.
    pub para: usize,
}

/// A picture with its blip resolved to a media part name + the context that positions it.
#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub part: String,
    pub w_emu: i64,
    pub h_emu: i64,
    pub anchored: bool,
    pub behind: bool,
    /// Text-wrap mode for a floating image (`square` / `tight` / `through` / `topAndBottom` / `none`).
    /// Only `topAndBottom` displaces vertical flow; the others sit in their own layer.
    pub wrap: String,
    pub x_emu: i64,
    pub y_emu: i64,
    pub h_from: String,
    pub v_from: String,
    pub h_align: String,
    pub v_align: String,
    /// `<a:srcRect>` crop (thousandths of a percent, l/t/r/b) - **signed**: a negative value pads the
    /// display box instead of cropping (Word keeps a logo's aspect this way). Mirrors `DrawImage`.
    pub crop_l: i64,
    pub crop_t: i64,
    pub crop_r: i64,
    pub crop_b: i64,
    pub context: ImageContext,
}

/// A tracked table-structure change (a row or column insertion / deletion), surfaced for navigation +
/// the reviewing pane. Table structure lives in the in-memory body (not the loro op log), so these are
/// enumerated separately from run / paragraph revisions; one entry per distinct revision id (a column's
/// cells share one).
#[derive(Debug, Clone)]
pub struct TableChange {
    pub id: u64,
    pub kind: TrackKind,
    /// `true` for a row revision (`w:trPr/ins|del`), `false` for a column revision (`w:tcPr` cells).
    /// Ignored for a property change (see `prop_level`).
    pub is_row: bool,
    /// `Some` for a tracked table-**property** change (`w:tblPrChange` / `w:trPrChange` /
    /// `w:tcPrChange`) - which level it sits on; `None` for a structural row / column ins-del.
    pub prop_level: Option<TablePropLevel>,
    pub author: String,
    pub date: String,
    /// The flat paragraph index to navigate to (the change's first cell paragraph).
    pub para: usize,
}

/// Which side of the anchored position a [`Anchor`] sticks to when content is inserted exactly there
/// - the left/right "stickiness" knob (ProseMirror `assoc`, CKEditor stickiness). Use [`Side::Left`]
///   for a range head ("stay before an insertion at my position") and [`Side::Right`] for a tail.
///   Re-exported from loro so callers needn't depend on loro directly.
pub use loro::cursor::Side;

/// An opaque, edit-stable reference to a point in the body story. Backed by a loro `Cursor` (bound to
/// an op-id, not an integer index), so a concurrent insertion or deletion elsewhere cannot silently
/// move it to the wrong place - the agent's #1 failure mode with raw offsets. Serialize for the wire
/// with [`Anchor::to_bytes`] / [`Anchor::from_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor(loro::cursor::Cursor);

impl Anchor {
    /// Encode to bytes for transport (postcard, via loro). The decoded form resolves identically.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.encode()
    }
    /// Decode an anchor produced by [`Anchor::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        loro::cursor::Cursor::decode(bytes)
            .map(Anchor)
            .map_err(|e| anyhow::anyhow!("invalid anchor bytes: {e}"))
    }
}

/// A range over the body = a head + a tail anchor (head biased [`Side::Left`], tail [`Side::Right`],
/// so the range neither grows nor shrinks spuriously when text is inserted at either edge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorRange {
    pub start: Anchor,
    pub end: Anchor,
}

impl AnchorRange {
    /// Encode both endpoints for transport: `len(start) as u32-le ++ start ++ end`.
    /// Pairs with [`Self::from_bytes`]; resolve later with
    /// [`CollabDoc::resolve_range`] / [`CollabDoc::resolve_range_multi`]. Used to
    /// carry a selected span (the editor's inline select->ask) to the agent's
    /// document edit so it lands on the exact content even if the doc shifted.
    pub fn to_bytes(&self) -> Vec<u8> {
        let s = self.start.to_bytes();
        let e = self.end.to_bytes();
        let mut out = Vec::with_capacity(4 + s.len() + e.len());
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(&s);
        out.extend_from_slice(&e);
        out
    }

    /// Decode an [`AnchorRange`] produced by [`Self::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            anyhow::bail!("invalid anchor-range bytes: too short");
        }
        let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let rest = &bytes[4..];
        if rest.len() < n {
            anyhow::bail!("invalid anchor-range bytes: truncated start anchor");
        }
        let start = Anchor::from_bytes(&rest[..n])?;
        let end = Anchor::from_bytes(&rest[n..])?;
        Ok(AnchorRange { start, end })
    }
}

/// An opaque capture of this document's version (the loro oplog version vector).
///
/// Capture one with [`CollabDoc::version`] *before* a mutation, then
/// [`CollabDoc::export_updates_since`] *after* it to get just the ops added in
/// between - the incremental delta to ship over a relay. This is the efficient
/// wire unit for collaborative editing (vs. a full [`CollabDoc::snapshot`]); it
/// keeps the loro `VersionVector` opaque so callers needn't depend on loro.
#[derive(Debug, Clone, PartialEq)]
pub struct DocVersion(VersionVector);

impl DocVersion {
    /// Encode to bytes so a non-Rust peer (the browser) can hold a version
    /// across calls and pass it back to [`CollabDoc::export_updates_since`].
    pub fn encode(&self) -> Vec<u8> {
        self.0.encode()
    }
    /// Decode a version produced by [`Self::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        VersionVector::decode(bytes)
            .map(DocVersion)
            .map_err(|e| anyhow::anyhow!("decode version: {e}"))
    }
}

/// One hit from [`CollabDoc::find_text`]: the body paragraph + full-text codepoint span that matched,
/// an edit-stable [`AnchorRange`] over it, and a short surrounding snippet so the agent can pick the
/// right occurrence (the quote-based addressing the SOTA editor tools converge on).
#[derive(Debug, Clone)]
pub struct TextMatch {
    pub para: usize,
    pub start: usize,
    pub end: usize,
    pub anchor: AnchorRange,
    pub snippet: String,
    /// Whether the match begins inside text that is already a tracked deletion (`w:del` / `w:moveFrom`)
    /// - so the agent doesn't redline text a human has already marked for removal (it would be editing a
    ///   phantom). The full run text is searched (deleted text included) so quote-addressing still finds
    ///   it; this flag is how the agent tells live text from pending-deleted text.
    pub in_deletion: bool,
}

/// A durable, serializable handle to a body paragraph (block) - stable across split / merge / undo and
/// not shifted when other paragraphs are added/removed (unlike a flat index). A **top-level** paragraph
/// is its block node's `TreeID`; a **table-cell** paragraph (which has no tree node of its own) is its
/// `text` container's `ContainerID` - both are loro replicated identities. Round-trips through its string
/// form (`Display` / `FromStr`): a top-level node is the bare `TreeID` (`{counter}@{peer}`), a cell
/// paragraph a `cid:...` string - disjoint prefixes, so `FromStr` picks the right variant. The agent can
/// read the outline, hold a node id, and read/edit that node later even after the document moved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(NodeRef);

/// The two durable identities a [`NodeId`] can carry (see [`NodeId`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum NodeRef {
    /// A top-level paragraph: its block node's tree id.
    Block(TreeID),
    /// A table-cell paragraph: its `text` container id (the cell paragraph has no tree node).
    Cell(ContainerID),
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            NodeRef::Block(id) => id.fmt(f),
            NodeRef::Cell(cid) => cid.fmt(f), // "cid:..." - disjoint from a TreeID's "{counter}@{peer}"
        }
    }
}

impl std::str::FromStr for NodeId {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        if s.starts_with("cid:") {
            ContainerID::try_from(s)
                .map(|c| NodeId(NodeRef::Cell(c)))
                .map_err(|_| anyhow::anyhow!("invalid node id (container) {s:?}"))
        } else {
            TreeID::try_from(s)
                .map(|id| NodeId(NodeRef::Block(id)))
                .map_err(|e| anyhow::anyhow!("invalid node id {s:?}: {e}"))
        }
    }
}

/// What a body paragraph is, for the agent's structural map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Paragraph,
    Heading,
    ListItem,
    TableCell,
}

/// One paragraph in the agent's outline: a stable handle + its current index + enough metadata to
/// decide whether to drill in. `preview` is the leading text (token-budgeted); `read_node` fetches the
/// full content.
#[derive(Debug, Clone)]
pub struct OutlineNode {
    pub node_id: NodeId,
    /// Current flat paragraph index (valid at this `revision`; convert to an anchor before holding it).
    pub para: usize,
    pub kind: NodeKind,
    /// Heading level 1-9 when `kind == Heading`.
    pub heading_level: Option<u8>,
    /// Named paragraph style id (`w:pStyle`), if any.
    pub style: Option<String>,
    pub char_count: usize,
    pub preview: String,
    /// Whether this paragraph carries any tracked change.
    pub has_changes: bool,
    /// For a `TableCell` node, its `(row, col, n_rows, n_cols)` within the table - so the agent perceives
    /// table structure from the outline (and knows a row/column edit is meaningful here). `None` for a
    /// non-cell paragraph.
    pub table: Option<(usize, usize, usize, usize)>,
}

/// A token-budgeted structural snapshot of the body: a freshness/version token + the outline. The
/// agent reads this first (cheap), then drills into specific nodes with `read_node`.
#[derive(Debug, Clone)]
pub struct DocSnapshot {
    /// Monotonic op count - changes on *any* edit. The freshness token: if it differs from when the
    /// agent read, the document moved underneath (the optimistic-concurrency token).
    pub revision: u64,
    /// Total body paragraphs in the document. When `nodes.len() < total` the outline was capped
    /// (`max_nodes`) and the agent should page with `offset` to see the rest.
    pub total: usize,
    /// The paragraph at which this window starts (0 unless paged).
    pub offset: usize,
    pub nodes: Vec<OutlineNode>,
}

/// The full content of one body paragraph, fetched by `read_node`.
#[derive(Debug, Clone)]
pub struct NodeContent {
    pub node_id: NodeId,
    pub para: usize,
    pub kind: NodeKind,
    pub heading_level: Option<u8>,
    pub style: Option<String>,
    pub text: String,
    pub runs: Vec<Run>,
}

/// One tracked change in the document, in the agent's shape - so it can triage (accept/reject by id)
/// or report what's pending.
#[derive(Debug, Clone)]
pub struct ChangeSummary {
    pub id: u64,
    /// `ins` / `del` / `fmt` / `movefrom` / `moveto` / `rowins` / `rowdel` / `colins` / `coldel` /
    /// `tableprop`.
    pub kind: String,
    pub author: String,
    pub date: String,
    /// The changed text (insertion / deletion text); empty for formatting / table changes.
    pub text: String,
    pub para: usize,
    pub node_id: NodeId,
}

/// Where a comment is anchored in the body: the codepoint span its `cmt~{id}` marks cover, possibly
/// spanning paragraphs. Pairs with the comment *body* (text / author / thread) from
/// [`CollabDoc::comments`] by `id` - so the agent can see both what a comment says and what it points
/// at. Only comments anchored in *this* story appear (a body comment whose anchor lives in a
/// header/footer child shows up when that child is queried).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentLocation {
    pub id: u64,
    pub start_para: usize,
    pub start_off: usize,
    pub end_para: usize,
    pub end_off: usize,
}

/// The result of resolving an [`Anchor`] against the current document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// The anchored content is live and unmoved - here is its current `(para, off)` (the integer
    /// offset may have shifted under concurrent edits, but the anchored character is the same one).
    Live { para: usize, off: usize },
    /// The exact anchored character was deleted; `(para, off)` is loro's best-effort *neighbour*, not
    /// the original spot. The reference moved - re-verify (e.g. via `find_text`) before editing here,
    /// rather than trusting the neighbour. (Happens on a direct, non-tracked deletion of the content;
    /// tracked deletions retain the text, so they keep an anchor `Live`.)
    Shifted { para: usize, off: usize },
    /// The anchored block was deleted - the reference is stale and must be re-discovered (e.g. via
    /// `find_text`). The explicit "your anchor's content is gone" signal.
    Deleted,
}

impl CollabDoc {
    // ── image editing (insert / resize / crop / remove) ──────────────────────────────────────────

    // ── perception (the agent's read surface) ────────────────────────────────────

    // ── table structure editing (rows + columns) ─────────────────────────────────

    // ── cell merge / split (w:gridSpan horizontal, w:vMerge vertical) ─────────────────────────────
    //
    // Direct structural edits on the grid. Per the design (§4 / §2.7) merge/split has no clean CRDT
    // convergence theorem - these are the single-peer editor ops; under a concurrent merge + edit the
    // metadata converges but the intended visual result is best-effort.

    // ── table / row / cell property edits (Table Tools; tracked as w:tcPrChange / w:trPrChange /
    //    w:tblPrChange) ─────────────────────────────────────────────────────────────────────────
    //
    // Table structure lives in the in-memory body (not the loro op log), so these mutate it directly
    // and commit only to set the audit message (mirrors the structural row/column suggest ops). A
    // tracked edit captures the before-state into the parent's `prop_change` (idempotently - a second
    // edit keeps the original old snapshot), so reject can restore it.

    // ── resolve tracked changes on the live model (accept / reject) ───────────────

    // ── comments (annotations: anchor + threaded body + resolved state) ───────────

}

impl Default for CollabDoc {
    fn default() -> Self {
        Self::new()
    }
}

/// The leading `max_chars` codepoints of `text`, with an ellipsis appended when truncated - the
/// outline's per-node text preview.
fn preview_of(text: &str, max_chars: usize) -> String {
    let mut s: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        s.push('…');
    }
    s
}

/// The agent-facing label for a run-level tracked-change kind.
fn track_kind_label(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Ins => "ins",
        TrackKind::Del => "del",
        TrackKind::Fmt => "fmt",
        TrackKind::MoveFrom => "movefrom",
        TrackKind::MoveTo => "moveto",
    }
}

/// The agent-facing label for a tracked table change (structural row/column ins-del, or a property
/// change).
fn table_change_label(tc: &TableChange) -> &'static str {
    if tc.prop_level.is_some() {
        return "tableprop";
    }
    match (tc.is_row, tc.kind) {
        (true, TrackKind::Ins) => "rowins",
        (true, _) => "rowdel",
        (false, TrackKind::Ins) => "colins",
        (false, _) => "coldel",
    }
}

/// Where a flat paragraph index sits in the **derived** body (`CollabDoc::body()`) - used by the
/// read-side table queries (`table_context` / `cell_step` / `table_changes`) and to refuse
/// cross-container joins. The *mutating* table ops use [`CollabDoc::cell_addr`] (grid-native) instead.
enum BodyLoc {
    /// A top-level paragraph.
    TopLevel,
    /// A table-cell paragraph: the derived body item index + the visible row/cell that owns it.
    Cell { item: usize, row: usize, cell: usize },
}

/// A table cell located in **grid** terms (resolved from a flat paragraph index via
/// [`model::block_seq`]): the hosting table node, its grid handle, the row, and the visible cell. The
/// structural edit ops + property setters mutate the grid through this - table structure is a loro
/// citizen now (tables-crdt T2.7), so there is no in-memory `body` to mutate.
struct CellAddr {
    node: TreeID,
    grid: crate::table_crdt::TableGrid,
    /// Index of the row in `row_order`.
    row_pos: usize,
    row_id: String,
    /// Anchor column id of the caret's visible cell.
    col_id: String,
    /// Index of the caret's cell among the row's *visible* cells.
    cell_pos: usize,
    /// Number of rows in the table.
    n_rows: usize,
    /// The row's visible (anchor) column ids, left to right.
    vis_cols: Vec<String>,
}

/// A stable row / column id not already present in `existing` (prefix `'r'` / `'c'`), **namespaced by
/// the editing peer** (`{prefix}{peer}-{n}`). The peer component makes concurrent inserts on different
/// peers collision-free - two peers inserting a row at once mint distinct ids, so both rows survive a
/// merge (tables-crdt T5, live multi-party); the membership scan keeps a single peer's sequential
/// inserts unique within the grid.
fn fresh_id(existing: &[String], prefix: char, peer: u64) -> String {
    let mut n = existing.len();
    loop {
        let id = format!("{prefix}{peer}-{n}");
        if !existing.contains(&id) {
            return id;
        }
        n += 1;
    }
}

/// An [`EdgeBorders`] with the same border on every edge (outer + inside), or all edges cleared when
/// `border` is `None` - the shape the "set table borders" edit applies.
fn uniform_borders(border: Option<model::Border>) -> model::EdgeBorders {
    model::EdgeBorders {
        top: border.clone(),
        left: border.clone(),
        bottom: border.clone(),
        right: border.clone(),
        inside_h: border.clone(),
        inside_v: border,
    }
}

/// The flat paragraph index just before `body[item]` (the sum of every earlier item's paragraphs).
fn flat_before_item(body: &[model::BodyItem], item: usize) -> usize {
    let mut flat = 0usize;
    for it in body.iter().take(item) {
        match it {
            model::BodyItem::Paragraph => flat += 1,
            model::BodyItem::Table(t) => {
                flat += t.rows.iter().flat_map(|r| r.cells.iter()).map(|c| c.para_count).sum::<usize>()
            }
        }
    }
    flat
}

/// The flat paragraph index where cell `(row, cell)` of the table at `body[item]` begins.
fn cell_flat_start(body: &[model::BodyItem], item: usize, row: usize, cell: usize) -> usize {
    let mut flat = flat_before_item(body, item);
    if let Some(model::BodyItem::Table(t)) = body.get(item) {
        for r in t.rows.iter().take(row) {
            for c in &r.cells {
                flat += c.para_count;
            }
        }
        if let Some(r) = t.rows.get(row) {
            for c in r.cells.iter().take(cell) {
                flat += c.para_count;
            }
        }
    }
    flat
}

/// Locate flat paragraph index `idx` within `body` (document order: top-level markers consume one
/// index each; a table consumes the sum of its cells' `para_count`). `None` if out of range.
fn body_locate(body: &[model::BodyItem], idx: usize) -> Option<BodyLoc> {
    let mut flat = 0usize;
    for (i, item) in body.iter().enumerate() {
        match item {
            model::BodyItem::Paragraph => {
                if flat == idx {
                    return Some(BodyLoc::TopLevel);
                }
                flat += 1;
            }
            model::BodyItem::Table(t) => {
                for (ri, row) in t.rows.iter().enumerate() {
                    for (ci, cell) in row.cells.iter().enumerate() {
                        if idx >= flat && idx < flat + cell.para_count {
                            return Some(BodyLoc::Cell { item: i, row: ri, cell: ci });
                        }
                        flat += cell.para_count;
                    }
                }
            }
        }
    }
    None
}


/// Concatenate header/footer paragraphs into plain text (one line per paragraph).
fn hf_text(paras: &[Paragraph]) -> String {
    paras
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build header/footer paragraphs from plain text (one paragraph per line; empty text -> one empty
/// paragraph so the region stays clickable).
fn hf_from_text(text: &str) -> Vec<Paragraph> {
    if text.is_empty() {
        return vec![Paragraph {
            style: None,
            props: ParaProps::default(),
            runs: Vec::new(),
            prop_change: None,
            mark_change: None,
        }];
    }
    text.split('\n')
        .map(|line| Paragraph {
            style: None,
            props: ParaProps::default(),
            runs: if line.is_empty() { Vec::new() } else { vec![Run::plain(line)] },
            prop_change: None,
            mark_change: None,
        })
        .collect()
}

/// Parse pictures from a body XML, resolve each blip through `rels` to a media part name, and push
/// them to `out`. `context_for` maps the picture's paragraph index to its [`ImageContext`].
fn collect_images(
    xml: &[u8],
    rels: &std::collections::HashMap<String, String>,
    context_for: impl Fn(usize) -> ImageContext,
    out: &mut Vec<PlacedImage>,
) {
    for di in model::parse_images(xml) {
        let Some(target) = rels.get(&di.embed) else { continue };
        let part = if let Some(stripped) = target.strip_prefix('/') {
            stripped.to_string()
        } else {
            format!("word/{target}")
        };
        out.push(PlacedImage {
            part,
            w_emu: di.w_emu,
            h_emu: di.h_emu,
            anchored: di.anchored,
            behind: di.behind,
            wrap: di.wrap,
            x_emu: di.x_emu,
            y_emu: di.y_emu,
            h_from: di.h_from,
            v_from: di.v_from,
            h_align: di.h_align,
            v_align: di.v_align,
            crop_l: di.crop_l,
            crop_t: di.crop_t,
            crop_r: di.crop_r,
            crop_b: di.crop_b,
            context: context_for(di.para_index),
        });
    }
}

/// Parse anchored text boxes from a header/footer part's XML into [`PlacedTextBox`]es (render-only:
/// the XML itself round-trips via the part's paragraph passthrough).
fn collect_textboxes(xml: &[u8], ctx: &ImageContext, out: &mut Vec<PlacedTextBox>) {
    for tb in model::parse_textboxes(xml) {
        out.push(PlacedTextBox {
            context: ctx.clone(),
            text: tb.text,
            font: tb.font,
            size_half_points: tb.size_half_points,
            color: tb.color,
            vert: tb.vert,
            x_emu: tb.x_emu,
            y_emu: tb.y_emu,
            w_emu: tb.w_emu,
            h_emu: tb.h_emu,
            h_from: tb.h_from,
            v_from: tb.v_from,
            para: tb.para_index,
        });
    }
}

/// Register the expand behavior of every mark key the model uses. loro requires a key's expand
/// type be configured before [`loro::LoroText::mark`], and it must be consistent per replica.
///
/// - `b` / `i` / `ins`: expand `After` - typing at the right boundary continues the formatting /
///   the insertion (matches Word: extend the run you're appending to).
/// - `del`: `None` - a deletion marks retained text; new text typed at its boundary must **not**
///   become part of the deletion.
fn configure_marks(doc: &LoroDoc) {
    configure_marks_with(doc, &[], &[], &[], &[], &[], &[]);
}

/// Configure every Peritext mark key: the base formatting/track keys, plus a `cmt~{id}` key (expand
/// `None`) for each comment id in `comment_ids`. loro requires a key's expand type be set before
/// [`loro::LoroText::mark`], consistently per replica; every `cmt~*` key uses `None`, so replicas
/// converge regardless of which has seen which comment.
#[allow(clippy::too_many_arguments)]
fn configure_marks_with(
    doc: &LoroDoc,
    comment_ids: &[u64],
    field_ids: &[u64],
    bookmark_ids: &[u64],
    link_ids: &[u64],
    image_ids: &[u64],
    raw_ids: &[u64],
) {
    let mut styles = StyleConfigMap::new();
    // Run formatting (bold/italic/size/color) and insertions expand `After`: typing at the right
    // boundary continues the formatting / insertion (matches Word).
    for key in ["b", "i", "u", "strike", "ins", "sz", "color", "font", "hl", "va", "lang", "rstyle", "rshd"] {
        styles.insert(key.into(), StyleConfig { expand: ExpandType::After });
    }
    styles.insert("del".into(), StyleConfig { expand: ExpandType::None });
    // Move halves: the source (`mvf`) behaves like a deletion (fixed range, no boundary growth); the
    // destination (`mvt`) behaves like an insertion (typing at its right edge continues it).
    styles.insert("mvf".into(), StyleConfig { expand: ExpandType::None });
    styles.insert("mvt".into(), StyleConfig { expand: ExpandType::After });
    // A tracked run-property change marks existing text; typing at its boundary must not extend it.
    styles.insert("rfmt".into(), StyleConfig { expand: ExpandType::None });
    // Comment anchors are fixed ranges (one key per comment so overlaps don't collide).
    for id in comment_ids {
        styles.insert(model::comment_mark_key(*id).into(), StyleConfig { expand: ExpandType::None });
    }
    // Field result ranges are fixed too (one `fld~{id}` key per field).
    for id in field_ids {
        styles.insert(model::field_mark_key(*id).into(), StyleConfig { expand: ExpandType::None });
    }
    // Bookmark + hyperlink ranges are fixed ranges (one key per id). A collapsed bookmark also gets a
    // `bkp~{id}` key: it has no range to mark, so it anchors to the codepoint it sits before.
    for id in bookmark_ids {
        styles.insert(model::bookmark_mark_key(*id).into(), StyleConfig { expand: ExpandType::None });
        styles.insert(
            model::point_bookmark_mark_key(*id).into(),
            StyleConfig { expand: ExpandType::None },
        );
        styles.insert(
            model::end_point_bookmark_mark_key(*id).into(),
            StyleConfig { expand: ExpandType::None },
        );
    }
    for id in link_ids {
        styles.insert(model::link_mark_key(*id).into(), StyleConfig { expand: ExpandType::None });
    }
    // Image anchors are fixed single-char ranges (one `img~{id}` key per picture).
    for id in image_ids {
        styles.insert(model::image_mark_key(*id).into(), StyleConfig { expand: ExpandType::None });
    }
    // Passthrough anchors: one `raw~{id}` key per captured embedded object, fixed single-char range so
    // editing beside it never extends the mark over new text.
    for id in raw_ids {
        styles.insert(model::raw_mark_key(*id).into(), StyleConfig { expand: ExpandType::None });
    }
    doc.config_text_style(styles);
}

#[cfg(test)]
mod tests;
