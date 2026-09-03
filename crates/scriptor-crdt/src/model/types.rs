//! The document value types.
//! 
//! [`Paragraph`], [`Run`] and their properties, plus the table structure and the
//! tracked-change records attached to them. This is the shape the CRDT is read into
//! and written out of; every other module in the model either produces these or
//! consumes them.

/// Which kind of tracked change a run carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// A suggested insertion (`w:ins`).
    Ins,
    /// A suggested deletion (`w:del`); the text is retained and marked, not removed.
    Del,
    /// A suggested run-property change (`w:rPrChange`). Unlike Ins/Del this never lives in
    /// [`Run::track`] - the change is carried by [`Run::fmt_change`] (which also holds the old props);
    /// this variant only labels a *region* synthesized for navigation / resolution / the tooltip.
    Fmt,
    /// The **source** half of a move (`w:moveFrom`): the text being moved away. Resolves like a
    /// deletion (accept removes it, reject restores it), but is paired with its [`MoveTo`](Self::MoveTo)
    /// half by a shared revision id so accepting / rejecting either resolves both.
    MoveFrom,
    /// The **destination** half of a move (`w:moveTo`): the relocated text. Resolves like an insertion
    /// (accept keeps it, reject removes it). Shares its revision id with the [`MoveFrom`](Self::MoveFrom).
    MoveTo,
}

impl TrackKind {
    /// The Peritext mark key for this kind.
    pub(crate) fn mark_key(self) -> &'static str {
        match self {
            TrackKind::Ins => "ins",
            TrackKind::Del => "del",
            TrackKind::Fmt => "rfmt",
            TrackKind::MoveFrom => "mvf",
            TrackKind::MoveTo => "mvt",
        }
    }

    /// The OOXML revision wrapper element (Fmt is serialized inside `w:rPr`, so `wrapper` is never
    /// called for it - it returns a harmless placeholder).
    pub(crate) fn wrapper(self) -> &'static str {
        match self {
            TrackKind::Ins | TrackKind::Fmt => "w:ins",
            TrackKind::Del => "w:del",
            TrackKind::MoveFrom => "w:moveFrom",
            TrackKind::MoveTo => "w:moveTo",
        }
    }

    /// Whether this kind's run text serializes as `w:delText` (deletions) vs `w:t`. Only `Del` uses
    /// `w:delText`; `w:moveFrom` content is regular `w:t` (the text exists, just elsewhere).
    pub(crate) fn is_del_text(self) -> bool {
        matches!(self, TrackKind::Del)
    }

    /// Whether this kind is a move half (`w:moveFrom` / `w:moveTo`).
    pub fn is_move(self) -> bool {
        matches!(self, TrackKind::MoveFrom | TrackKind::MoveTo)
    }
}

/// A run's formatting attributes (the `w:rPr`-relevant subset), as an owned snapshot. Used to record
/// the *before* state of a tracked run-property change so a reject can restore it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunProps {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub size: Option<u16>,
    pub color: Option<String>,
    pub font: Option<String>,
    pub highlight: Option<String>,
    /// Vertical alignment (`w:vertAlign w:val`): "superscript" / "subscript", or `None` for baseline.
    pub vert_align: Option<String>,
    /// Proofing language (`w:lang w:val`), preserved for round-trip.
    pub lang: Option<String>,
}

impl RunProps {
    /// The current formatting of a run.
    pub(crate) fn of(run: &Run) -> Self {
        Self {
            bold: run.bold,
            italic: run.italic,
            underline: run.underline,
            strike: run.strike,
            size: run.size,
            color: run.color.clone(),
            font: run.font.clone(),
            highlight: run.highlight.clone(),
            vert_align: run.vert_align.clone(),
            lang: run.lang.clone(),
        }
    }
}

/// A tracked run-property change (`w:rPrChange`): who / when / the revision id, plus the run's
/// formatting *before* the change. The run itself carries the new formatting; this records what to
/// restore on reject (and what Word writes inside `w:rPrChange/w:rPr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatChange {
    pub author: String,
    pub date: String,
    pub id: u64,
    pub old: RunProps,
}

/// A tracked paragraph-property change (`w:pPrChange`): who / when / the revision id, plus the
/// paragraph's style + properties *before* the change. The paragraph carries the new props; this
/// records what to restore on reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParaPropChange {
    pub author: String,
    pub date: String,
    pub id: u64,
    pub old_style: Option<String>,
    pub old: ParaProps,
}

/// A tracked change attached to a run: who, when, and the OOXML revision id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub kind: TrackKind,
    pub author: String,
    pub date: String,
    pub id: u64,
}

/// One run of text with uniform formatting and an optional tracked-change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Inline font size in **half-points** (OOXML `w:sz`), or `None` to inherit from the paragraph
    /// style. Kept as the raw OOXML integer (half-points) so the model stays `Eq`.
    pub size: Option<u16>,
    /// Inline run color: an RGB hex string ("RRGGBB"), the literal `"auto"` (automatic - renders
    /// near-black and overrides any inherited colour), or `None` to inherit.
    pub color: Option<String>,
    /// Inline font family (OOXML `w:rFonts` ascii), or `None` to inherit.
    pub font: Option<String>,
    /// Highlight color name (OOXML `w:highlight w:val`, e.g. "yellow"), or `None`.
    pub highlight: Option<String>,
    /// Vertical alignment (OOXML `w:vertAlign w:val`): "superscript" / "subscript", or `None` for
    /// the normal baseline. Rendered as smaller, raised / lowered text.
    pub vert_align: Option<String>,
    /// Proofing language (OOXML `w:lang w:val`, e.g. "en-US"), or `None`. Not rendered; preserved so
    /// it survives the `document.xml` round-trip (Word tags most runs with a language).
    pub lang: Option<String>,
    /// Character style id (OOXML `w:rStyle w:val`, e.g. "Strong"), or `None`. Preserved for the
    /// round-trip (used by ~12% of real docs - Hyperlink / FootnoteReference / Strong / ...) and
    /// resolved at render for the style's highlight (a run with no direct highlight inherits its
    /// character style's, above the paragraph style). Other char-style rPr (colour/size) is not yet
    /// resolved.
    pub char_style: Option<String>,
    /// Run-level shading fill (OOXML `w:rPr/w:shd w:fill`), RGB hex, or `None`. Painted as a fill
    /// behind the run's glyphs (like a highlight, but any hex colour); a highlight on the same run
    /// wins over it. Distinct from paragraph (`pPr/w:shd`) and cell (`tcPr/w:shd`) shading.
    pub shading: Option<String>,
    pub track: Option<Track>,
    /// A tracked run-property change (`w:rPrChange`): present when this run's formatting was changed
    /// under Track-Changes. The run carries the *new* formatting; this holds the old, for reject.
    pub fmt_change: Option<FormatChange>,
    /// The ids of every comment whose anchored range covers this run (OOXML
    /// `w:commentRangeStart`/`End`). Each is a per-comment Peritext mark (`cmt~{id}`) on the run text;
    /// sorted ascending. Empty for an un-commented run. The comment *bodies* live document-level (see
    /// [`Comment`] / the `comments` loro map), keyed by these ids.
    pub comments: Vec<u64>,
    /// The id of the OOXML field (`w:fldChar`/`w:instrText`/`w:fldSimple`) whose cached *result* this
    /// run is part of, or `None`. The field *instruction* (`TOC \o ...`) lives document-level in the
    /// `fields` loro map ([`FIELDS`]); this run-level Peritext mark (`fld~{id}`) tracks the result range
    /// so export can re-wrap it in the field markup (begin / instrText / separate … end). Only the
    /// outermost field is modeled - nested fields (e.g. PAGEREF inside a TOC) flatten to their text.
    pub field: Option<u64>,
    /// The ids of the bookmarks (`w:bookmarkStart`/`End`) whose ranges cover this run, ascending.
    /// Each is a `bkm~{id}` Peritext mark; the bookmark *name* lives in the `bookmarks` map
    /// ([`BOOKMARKS`]). Several bookmarks can overlap on one run (Word starts multiple bookmarks at
    /// the same point - e.g. a stack of TOC bookmarks on a heading), so this is a list, not one id
    /// (a single id collapsed them and duplicated one id across their disjoint spans).
    pub bookmarks: Vec<u64>,
    /// The ids of **collapsed** bookmarks - `<w:bookmarkStart/><w:bookmarkEnd/>` with nothing
    /// between - sitting immediately *before* this run, ascending.
    ///
    /// They cover no codepoints, so they cannot be a range mark; each is a `bkp~{id}` mark on this
    /// run's first codepoint, and export re-emits the start/end pair ahead of the run. Their names
    /// live in the same [`BOOKMARKS`] map as ranged ones.
    ///
    /// This is the normal shape for a cross-reference target: Word writes `_Ref…` as a bare
    /// insertion point. Dropping them, as v1 did, silently broke every cross-reference pointing at
    /// one - 79 bookmarks across 47 documents of the LibreOffice corpus.
    pub point_bookmarks: Vec<u64>,
    /// The ids of collapsed bookmarks sitting immediately *after* this run, ascending.
    ///
    /// Same mechanism as [`Self::point_bookmarks`] (a `bkpe~{id}` mark) for the case where the
    /// bookmark fell past the paragraph's last codepoint and had nothing left to sit before -
    /// typically because what follows it is not modeled as text, such as an OMML formula.
    pub end_point_bookmarks: Vec<u64>,
    /// The id of the hyperlink (`w:hyperlink`) whose range covers this run, or `None`. A `lnk~{id}`
    /// mark; the *target* (an internal `#anchor` or an external URL) lives in the `hyperlinks` map
    /// ([`HYPERLINKS`]). Hyperlinked runs render blue + underlined.
    pub link: Option<u64>,
    /// The id of the picture (`w:drawing`) this run carries, or `None`. An image is a single
    /// placeholder run bearing an `img~{id}` mark; the picture's media / size / crop / placement live
    /// in the `images` map ([`IMAGES`]), keyed by this id (mirrors [`Run::link`]). Both inline and
    /// floating pictures use this - the [`ImagePlacement`] in the map distinguishes them.
    pub image: Option<u64>,
    /// The id of **verbatim passthrough** content this run carries, or `None` - an unmodeled embedded
    /// object (`w:object` OLE, `w:control` ActiveX, ...) preserved byte-for-byte for the round-trip. A
    /// single placeholder run bearing a `raw~{id}` mark; the captured `<w:r>...</w:r>` XML lives in the
    /// `rawxml` map ([`RAWXML`]), keyed by this id (mirrors [`Run::image`]). See `docs/passthrough.md`.
    pub raw: Option<u64>,
}

impl Run {
    /// A plain run with no formatting and no tracked change.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            size: None,
            color: None,
            font: None,
            highlight: None,
            vert_align: None,
            lang: None,
            char_style: None,
            shading: None,
            track: None,
            fmt_change: None,
            comments: Vec::new(),
            field: None,
            bookmarks: Vec::new(),
            point_bookmarks: Vec::new(),
            end_point_bookmarks: Vec::new(),
            link: None,
            image: None,
            raw: None,
        }
    }
}

/// A comment (`word/comments.xml`): the threaded annotation behind a `w:commentReference`. The
/// anchored range is carried separately as Peritext `cmt~{id}` marks on the body text (see
/// [`Run::comments`]); this is the comment's identity + body + thread state. Bodies are modeled as
/// plain text (one `\n`-separated line per `w:p`); intra-comment run formatting is not modeled (it is
/// a rare annotation concern - the comment text, author, threading, and resolved state round-trip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// The OOXML `w:id` (shares the revision/bookmark id pool - see [`max_revision_id`]).
    pub id: u64,
    pub author: String,
    /// `w:initials` (the balloon avatar text); derived from the author when absent.
    pub initials: String,
    pub date: String,
    /// The id of the comment this one replies to (`commentsExtended` `w15:paraIdParent`), or `None`
    /// for a top-level comment.
    pub parent: Option<u64>,
    /// Whether the thread is marked resolved (`commentsExtended` `w15:done="1"`).
    pub resolved: bool,
    /// The comment body as plain text (paragraph breaks as `\n`).
    pub text: String,
}

/// Line-spacing rule (OOXML `w:spacing/@w:lineRule`). `None` (the default) is `auto`: the `w:line`
/// value is 240ths of a line (a multiplier). `AtLeast`/`Exact` reinterpret `w:line` as an absolute
/// height in twips - at-least a floor under the natural line, exact a fixed height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRule {
    AtLeast,
    Exact,
}

impl LineRule {
    pub(crate) fn from_ooxml(v: &str) -> Option<LineRule> {
        match v {
            "atLeast" => Some(LineRule::AtLeast),
            "exact" | "exactly" => Some(LineRule::Exact),
            _ => None, // "auto" (or anything else) = the multiplier interpretation
        }
    }
    /// The OOXML token, also used for the meta / JSON boundary.
    pub fn as_str(self) -> &'static str {
        match self {
            LineRule::AtLeast => "atLeast",
            LineRule::Exact => "exact",
        }
    }
}

/// Paragraph alignment (OOXML `w:jc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

impl Align {
    /// Parse an OOXML `w:jc` value.
    pub(crate) fn from_ooxml(v: &str) -> Option<Align> {
        match v {
            "left" | "start" => Some(Align::Left),
            "center" => Some(Align::Center),
            "right" | "end" => Some(Align::Right),
            "both" | "distribute" | "justify" => Some(Align::Justify),
            _ => None,
        }
    }
    /// The OOXML `w:jc` value.
    pub(crate) fn to_ooxml(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "both",
        }
    }
    /// A stable lowercase token for the JS / API boundary.
    pub fn as_str(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "justify",
        }
    }
    /// Parse the API token (`left` / `center` / `right` / `justify`).
    pub fn parse(s: &str) -> Option<Align> {
        match s {
            "left" => Some(Align::Left),
            "center" => Some(Align::Center),
            "right" => Some(Align::Right),
            "justify" => Some(Align::Justify),
            _ => None,
        }
    }
}

/// Paragraph-level formatting (the Home tab's Paragraph group). All-optional: a `Some` field is a
/// value to set (or, in a query result, the paragraph's current value); `None` = unset / inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParaProps {
    pub align: Option<Align>,
    /// Line spacing value (`w:spacing/@w:line`). With [`Self::line_rule`] `None` (auto) it is 240ths
    /// of a line: 240 = single, 360 = 1.5, 480 = double. With `AtLeast`/`Exact` it is twips.
    pub line_spacing: Option<u16>,
    /// The line-spacing rule. `None` = `auto` (multiplier); `Some` = `atLeast`/`exact` (absolute twips).
    pub line_rule: Option<LineRule>,
    /// Direct paragraph spacing before / after, in twips. Overrides the style's spacing - a paragraph
    /// that sets `w:spacing w:after="0"` must render tight even when the style/docDefaults add space.
    pub space_before: Option<u32>,
    pub space_after: Option<u32>,
    /// Indents in twips (1/20 pt). `first` is positive for a first-line indent, negative for hanging.
    pub indent_left: Option<i32>,
    pub indent_right: Option<i32>,
    pub indent_first: Option<i32>,
    /// List membership (OOXML `w:numPr`): the numbering id + level. The rendered marker (`1.`, `a.`,
    /// `•`) is computed from `numbering.xml` against these.
    pub num_id: Option<i32>,
    pub num_ilvl: Option<i32>,
    /// Paragraph shading fill color (`w:shd w:fill`), RGB hex, or `None`/`auto`.
    pub shading: Option<String>,
    /// Custom tab-stop positions (twips, `w:tabs`/`w:tab w:pos`). A literal `w:tab` in a run
    /// advances to the next stop past the pen (or the default interval); the stop's alignment
    /// (`tab_kinds`) decides whether the following text is left/centre/right/decimal-aligned on it.
    pub tab_stops: Vec<u32>,
    /// Per-stop alignment kind, parallel to `tab_stops`: 0=left, 1=center, 2=right, 3=decimal.
    /// Shorter than (or empty relative to) `tab_stops` means the missing entries are left tabs - so
    /// an all-left paragraph stores nothing extra and round-trips byte-identically to the format
    /// that predates alignment support.
    pub tab_kinds: Vec<u8>,
    /// `w:keepNext` - keep this paragraph on the same page as the one after it (Word's "keep with
    /// next", carried by heading styles so a heading is never orphaned at a page foot). `None` =
    /// inherit the style; `Some(false)` explicitly turns an inherited keep off.
    pub keep_next: Option<bool>,
    /// `w:contextualSpacing` - "Don't add space between paragraphs of the same style". When set, the
    /// space-after / space-before between two ADJACENT same-style paragraphs is suppressed (Word does
    /// this for list + body styles). `None` = inherit the style.
    pub contextual_spacing: Option<bool>,
    /// `w:pageBreakBefore` - force this paragraph to the top of a new page.
    pub page_break_before: bool,
    /// This paragraph contains a manual page break (`<w:br w:type="page"/>`), so the content after it
    /// continues on a new page. v1 approximates a mid-paragraph break as "break after this paragraph"
    /// (correct when the break is at the paragraph end / in its own paragraph, the common case).
    pub page_break_after: bool,
    /// This paragraph is a **section terminator** - its `w:pPr` carried a `w:sectPr` that ends a
    /// section with a page-creating break (`nextPage`). Distinct from `page_break_after` (which a
    /// manual `<w:br>` also sets): a section terminator that is empty must NOT spill to its own page
    /// (Word lets the mark sit at the foot), and a table immediately after one starts a fresh page.
    pub section_end: bool,
    /// This paragraph carries a `w:sectPr` whose following section starts **continuous** (or
    /// `nextColumn`) - i.e. the break after it does NOT create a page. When such a carrier is *empty*
    /// (a bare section-break mark, no text), Word consolidates it away: it occupies no line and
    /// contributes no spacing, and the surrounding paragraphs' spacing collapses around it. The layout
    /// uses this to drop the empty carrier's line height + space-after (tdf169986 + the `*bottomSpacing`
    /// continuous-break fixtures). Distinct from [`Self::section_end`], which marks page-*creating* ends.
    pub continuous_break: bool,
    /// This paragraph contains a manual **column break** (`<w:br w:type="column"/>`). We don't lay out
    /// newspaper columns, so in a single-column document a column break is equivalent to a page break
    /// (there is no next column) - the importer maps it to [`Self::page_break_after`] when the document
    /// has no multi-column section. Kept as its own flag for round-trip fidelity.
    pub column_break_after: bool,
    /// A text-frame definition (`w:pPr/w:framePr`): this paragraph (with any consecutive same-frame
    /// paragraphs) is a positioned floating box that body text wraps around, NOT inline flow. Stored
    /// as the raw `framePr` attribute string (e.g. `w:w="2880" w:hAnchor="margin" w:wrap="around"
    /// w:xAlign="right" w:y="720"`) - re-emitted verbatim for the round-trip, parsed on demand by the
    /// layout for position / size / wrap. `None` = a normal in-flow paragraph.
    pub frame: Option<String>,
    /// Paragraph borders (`w:pPr/w:pBdr`): the box drawn around the paragraph (Word's "Borders and
    /// Shading"). Stored as a compact `edge=val,sz,space,color` list joined by `|`, where `edge` is
    /// `t|l|b|r`, `sz` is eighths-of-a-point line weight, `space` is the text-to-line gap in points,
    /// and `color` is an RGB hex or `auto`. Edges with `w:val="none"/"nil"` are dropped. `None` = no
    /// box. Re-emitted as `<w:pBdr>` on export; parsed on demand by the layout to paint the lines.
    /// This is what draws a text frame's visible rectangle (frames carry their box via pBdr).
    pub border: Option<String>,
    /// The paragraph MARK's font size in half-points (`w:pPr/w:rPr/w:sz`). Word sizes an EMPTY
    /// paragraph's line by its mark - legal templates use tiny-mark empty paragraphs as spacers
    /// (a sz=10 spacer is a 5pt line, not a full text line); ignoring it inflated every such
    /// spacer to the style's line height.
    pub mark_size: Option<u32>,
    /// The **verbatim inner XML** of a `<w:sectPr>` this paragraph carries in its `w:pPr` (an
    /// in-paragraph section break: this paragraph is the LAST of the section that sectPr defines).
    /// Stored whole so per-section page size / orientation / margins / columns / header-footer refs
    /// / page borders / line numbering all round-trip - the model still derives the layout's
    /// page-break flags ([`Self::section_end`] / [`Self::continuous_break`]) from the type, but the
    /// full properties are preserved here rather than collapsed into the single synthesized final
    /// `sectPr` (which merged every section's header/footer refs into one, overflowing
    /// `EG_HdrFtrReferences` - the multi-section corpus docs). `None` = not a section boundary. The
    /// body-final section's `sectPr` lives document-level (see [`SECTPR`]), not on a paragraph.
    pub sect_pr: Option<String>,
}

/// A materialized paragraph - the read-only view used for inspection and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    pub style: Option<String>,
    pub props: ParaProps,
    pub runs: Vec<Run>,
    /// A tracked paragraph-property change (`w:pPrChange`), if this paragraph's style / properties
    /// were changed under Track-Changes. The paragraph carries the *new* props; this holds the old.
    pub prop_change: Option<ParaPropChange>,
    /// A tracked **paragraph-mark** revision (`w:pPr/w:rPr/w:ins|w:del`): `Ins` = this paragraph's
    /// ending mark was inserted (a tracked Enter split here), `Del` = it was deleted (a tracked join
    /// merging the next paragraph into this one - non-destructive until accepted). `None` otherwise.
    pub mark_change: Option<Track>,
}

// ── tables ───────────────────────────────────────────────────────────────────

/// One border edge: line weight in eighths of a point (`w:sz`) + RGB hex color. Absence (`None`) or
/// `w:val="none"/"nil"` means no line on that edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Border {
    pub size_eighths: u16,
    pub color: String,
}

/// The border edges of a table or cell. `inside_h`/`inside_v` apply only at table level (the lines
/// between cells); cells use `top`/`left`/`bottom`/`right`.
#[derive(Debug, Clone, Default)]
pub struct EdgeBorders {
    pub top: Option<Border>,
    pub left: Option<Border>,
    pub bottom: Option<Border>,
    pub right: Option<Border>,
    pub inside_h: Option<Border>,
    pub inside_v: Option<Border>,
}

/// Cell content margins (twips), from `w:tblCellMar` (table) / `w:tcMar` (cell). Per-side
/// `Option` because OOXML sets each side independently - a table that sets only top/bottom must
/// fall back to its table style (usually TableNormal's 108-twip left/right) for the rest, not to
/// zero, and export must reproduce only the sides the source set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellMargins {
    pub top: Option<u32>,
    pub left: Option<u32>,
    pub bottom: Option<u32>,
    pub right: Option<u32>,
}

/// Vertical-merge state of a cell (`w:vMerge`): `Restart` begins a merged span, `Continue` is
/// absorbed into the cell above it in the same grid column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VMerge {
    #[default]
    None,
    Restart,
    Continue,
}

/// A table cell: how many of the document's flow paragraphs it owns (`para_count`, contiguous in
/// document order in the loro tree - see [`import_document_xml`]), grid span (`w:gridSpan`),
/// vertical merge, any per-cell border overrides (`w:tcBorders`) + margins (`w:tcMar`), and an
/// explicit width (`w:tcW`, twips). Cell text lives in the editable loro flow like body paragraphs,
/// not statically here; this struct is the cell's *structure* + its slice of the flat paragraph list.
#[derive(Debug, Clone, Default)]
pub struct TableCell {
    /// Number of flow paragraphs in this cell (>= 1 for a well-formed cell). The cell owns a
    /// contiguous run of the flat paragraph list; walking the body in order assigns the indices.
    pub para_count: usize,
    pub grid_span: usize,
    pub vmerge: VMerge,
    pub borders: EdgeBorders,
    pub margins: Option<CellMargins>,
    pub width: Option<u32>,
    /// Cell shading fill color (`w:shd w:fill` in `w:tcPr`), RGB hex, or `None`.
    pub shading: Option<String>,
    /// A tracked **cell** structure revision (`w:tcPr/w:cellIns` = `Ins`, `w:tcPr/w:cellDel` = `Del`):
    /// set on every cell of a tracked-inserted / -deleted **column** (one shared revision id across the
    /// column, so accept / reject resolves the whole column). `None` for an unchanged cell. Cells of a
    /// tracked-inserted / -deleted *row* carry the change on the row, not here.
    pub change: Option<Track>,
    /// A tracked cell-**property** change (`w:tcPrChange`): width / span / vMerge / borders / margins
    /// / shading were changed under Track-Changes. The cell carries the *new* props; this holds the old
    /// (a [`TablePropSnapshot::Cell`]). `None` for a cell whose properties weren't changed.
    pub prop_change: Option<TablePropChange>,
    /// Tables nested inside this cell, captured verbatim.
    ///
    /// A table inside a table cell is not modeled: [`TableCell`] owns a contiguous slice of the flat
    /// paragraph list, which cannot express a table. The importer used to skip a nested table's
    /// paragraphs silently, so every word inside one was lost on save - 22 corpus documents, and for
    /// several of them that was the entire document.
    ///
    /// Preserving comes before modelling, so the nested `<w:tbl>...</w:tbl>` is kept as raw XML and
    /// re-emitted where it sat. It is opaque: it renders as nothing and cannot be edited, exactly
    /// like an OLE object. Modelling it properly means giving a cell block items rather than a
    /// paragraph count, which is a much larger change.
    pub nested: Vec<NestedBlock>,
}

/// A block inside a table cell that the model does not represent, captured verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedBlock {
    /// How many of the cell's own paragraphs precede it, so export can put it back in place.
    pub after_para: usize,
    /// The raw `<w:tbl>...</w:tbl>` bytes, re-emitted unchanged.
    pub xml: String,
}

/// A table row: a left-to-right run of cells, with an optional explicit height (`w:trHeight`).
#[derive(Debug, Clone, Default)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
    /// Row height in twips (`w:trHeight w:val`); `exact` is `w:hRule="exact"` (else "atLeast").
    pub height: Option<u32>,
    pub height_exact: bool,
    /// `w:trPr/w:cantSplit` - "Allow row to break across pages" unchecked: the row must paginate
    /// whole (the layout may otherwise split a tall row's content across the page boundary).
    pub cant_split: bool,
    /// Row alignment within the text column (`w:trPr/w:jc`: "center" / "right" / ...). Word aligns
    /// each row's grid independently; a whole table centred via its rows carries it here.
    pub justify: Option<String>,
    /// A tracked **row** structure revision (`w:trPr/w:ins` = `Ins`, `w:trPr/w:del` = `Del`): the row
    /// was inserted / deleted under Track-Changes (retained until resolved). `None` for an unchanged row.
    pub change: Option<Track>,
    /// A tracked row-**property** change (`w:trPrChange`): the row height was changed under
    /// Track-Changes. The row carries the *new* height; this holds the old (a [`TablePropSnapshot::Row`]).
    pub prop_change: Option<TablePropChange>,
}

/// A single-level table: grid column widths (twips, from `w:tblGrid`), rows, the table style id
/// (`w:tblStyle` - resolved for default borders/margins), and direct table-level borders + margins.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub col_widths: Vec<u32>,
    pub rows: Vec<TableRow>,
    pub style: Option<String>,
    /// Table alignment within the text column (`w:tblPr/w:jc`: "center" / "right" / ...). The grid
    /// is positioned absolutely at its authored widths - never scaled to fit - so this decides
    /// where a narrower table sits (and a wider one spills the margins like Word).
    pub justify: Option<String>,
    pub borders: EdgeBorders,
    pub cell_margins: Option<CellMargins>,
    /// The raw attributes of `<w:tblLook .../>` (e.g. ` w:val="04A0" w:firstRow="1" ...`) -
    /// preserved verbatim for round-trip and consulted for which of the table style's conditional
    /// formats (first row, banding, ...) apply.
    pub look: Option<String>,
    /// A tracked table-**property** change (`w:tblPrChange`): style / borders / cell margins were
    /// changed under Track-Changes. The table carries the *new* props; this holds the old (a
    /// [`TablePropSnapshot::Table`]). `None` for a table whose properties weren't changed.
    pub prop_change: Option<TablePropChange>,
}

/// Which level of a table a [`TablePropChange`] sits on - the OOXML element it round-trips through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TablePropLevel {
    /// Table-level properties (`w:tblPrChange`): style / borders / cell margins.
    Table,
    /// Row-level properties (`w:trPrChange`): row height.
    Row,
    /// Cell-level properties (`w:tcPrChange`): width / span / vMerge / borders / margins / shading.
    Cell,
}

/// A snapshot of a table / row / cell's properties *before* a tracked property change - what
/// `w:tblPrChange` / `w:trPrChange` / `w:tcPrChange` carry (the nested old `w:tblPr` / `w:trPr` /
/// `w:tcPr`) and what a reject restores. The live `Table` / `TableRow` / `TableCell` keeps the *new*
/// props; this holds the old. One variant per level (matches the parent the change is attached to).
#[derive(Debug, Clone)]
pub enum TablePropSnapshot {
    Table { style: Option<String>, borders: EdgeBorders, cell_margins: Option<CellMargins> },
    Row { height: Option<u32>, height_exact: bool },
    Cell {
        width: Option<u32>,
        grid_span: usize,
        vmerge: VMerge,
        borders: EdgeBorders,
        margins: Option<CellMargins>,
        shading: Option<String>,
    },
}

/// A tracked table-property change (`w:tblPrChange` / `w:trPrChange` / `w:tcPrChange`): who / when /
/// the revision id, plus the old props to restore on reject. Attached to the `Table` / `TableRow` /
/// `TableCell` whose props changed; the `old` variant must match that level. Lives in the in-memory
/// body (not the loro op log), like the structural [`Track`] on rows / cells.
#[derive(Debug, Clone)]
pub struct TablePropChange {
    pub author: String,
    pub date: String,
    pub id: u64,
    pub old: TablePropSnapshot,
}

impl TablePropChange {
    /// The level (and OOXML element) this change round-trips through.
    pub fn level(&self) -> TablePropLevel {
        match self.old {
            TablePropSnapshot::Table { .. } => TablePropLevel::Table,
            TablePropSnapshot::Row { .. } => TablePropLevel::Row,
            TablePropSnapshot::Cell { .. } => TablePropLevel::Cell,
        }
    }
}

/// Snapshot a table's tracked properties (style / borders / cell margins) into a [`TablePropSnapshot`].
pub fn table_prop_snapshot(t: &Table) -> TablePropSnapshot {
    TablePropSnapshot::Table {
        style: t.style.clone(),
        borders: t.borders.clone(),
        cell_margins: t.cell_margins,
    }
}

/// Snapshot a row's tracked properties (height) into a [`TablePropSnapshot`].
pub fn row_prop_snapshot(r: &TableRow) -> TablePropSnapshot {
    TablePropSnapshot::Row { height: r.height, height_exact: r.height_exact }
}

/// Snapshot a cell's tracked properties into a [`TablePropSnapshot`].
pub fn cell_prop_snapshot(c: &TableCell) -> TablePropSnapshot {
    TablePropSnapshot::Cell {
        width: c.width,
        grid_span: c.grid_span,
        vmerge: c.vmerge,
        borders: c.borders.clone(),
        margins: c.margins,
        shading: c.shading.clone(),
    }
}

/// Write a [`TablePropSnapshot::Table`] back onto a table (restores the before-state on reject / banks
/// the OLD state on import). A mismatched variant is a no-op (callers pair the right level).
pub fn apply_table_snapshot(t: &mut Table, s: &TablePropSnapshot) {
    if let TablePropSnapshot::Table { style, borders, cell_margins } = s {
        t.style = style.clone();
        t.borders = borders.clone();
        t.cell_margins = *cell_margins;
    }
}

/// Write a [`TablePropSnapshot::Row`] back onto a row.
pub fn apply_row_snapshot(r: &mut TableRow, s: &TablePropSnapshot) {
    if let TablePropSnapshot::Row { height, height_exact } = s {
        r.height = *height;
        r.height_exact = *height_exact;
    }
}

/// Write a [`TablePropSnapshot::Cell`] back onto a cell.
pub fn apply_cell_snapshot(c: &mut TableCell, s: &TablePropSnapshot) {
    if let TablePropSnapshot::Cell { width, grid_span, vmerge, borders, margins, shading } = s {
        c.width = *width;
        c.grid_span = *grid_span;
        c.vmerge = *vmerge;
        c.borders = borders.clone();
        c.margins = *margins;
        c.shading = shading.clone();
    }
}

/// One item in document order: a top-level paragraph (its content lives in the editable loro flow,
/// addressed positionally) or a table (static, render-only in v1). Interleaving these reproduces the
/// document layout - e.g. a contract whose clauses live in a table.
#[derive(Debug, Clone)]
pub enum BodyItem {
    Paragraph,
    Table(Box<Table>),
}

/// Counts from an [`import_document_xml`] pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImportStats {
    /// Top-level (body) paragraphs.
    pub paragraphs: usize,
    /// Paragraphs inside table cells - now modeled into the editable flow (no longer skipped).
    /// Paragraphs in *nested* tables (depth >= 2) are still skipped and not counted here.
    pub table_paragraphs: usize,
}
