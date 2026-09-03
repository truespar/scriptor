//! Per-run annotation tables, computed once before serialization.
//! 
//! Comments, fields, bookmarks and hyperlinks are marks over a range, but OOXML wants
//! them as elements opened before a run and closed after it. Rather than rediscover
//! that per run, the whole document is scanned up front into open/close tables a run
//! can look itself up in. `IdAlloc` hands out the export-time ids those wrappers use.

use super::*;

/// Per-paragraph, per-run lists of annotation ids (a comment can open/close several anchors at one
/// run, so each run carries a `Vec<u64>`).
pub(crate) type SpanGrid = Vec<Vec<Vec<u64>>>;
/// Per-paragraph, per-run single annotation id (a field / bookmark marker is at most one per run).
pub(crate) type OptSpanGrid = Vec<Vec<Option<u64>>>;

/// For every paragraph, the comment ids to open (`commentRangeStart`) before each run and to close
/// (`commentRangeEnd` + a reference run) after each run. Computed over the whole run sequence in
/// document order, so a comment anchor spanning several paragraphs opens once at its first run and
/// closes once at its last - exactly, even across paragraph boundaries.
pub(crate) fn comment_spans(paras: &[Paragraph]) -> (SpanGrid, SpanGrid) {
    multi_id_spans(paras, |r| &r.comments)
}

/// The open/close tables for a MULTI-id run annotation (comments, bookmarks - several can overlap on
/// one run), read by `get`. Each id opens once at its first run in document order and closes once at
/// its last, even across paragraph / cell boundaries. Mirrors [`single_id_spans`] for the
/// list-valued case.
fn multi_id_spans(paras: &[Paragraph], get: impl Fn(&Run) -> &[u64]) -> (SpanGrid, SpanGrid) {
    let mut opens: Vec<Vec<Vec<u64>>> =
        paras.iter().map(|p| vec![Vec::new(); p.runs.len()]).collect();
    let mut closes: Vec<Vec<Vec<u64>>> =
        paras.iter().map(|p| vec![Vec::new(); p.runs.len()]).collect();
    let mut flat: Vec<(usize, usize)> = Vec::new();
    for (pi, p) in paras.iter().enumerate() {
        for ri in 0..p.runs.len() {
            flat.push((pi, ri));
        }
    }
    const EMPTY: &[u64] = &[];
    for (k, &(pi, ri)) in flat.iter().enumerate() {
        let cur = get(&paras[pi].runs[ri]);
        if cur.is_empty() {
            continue;
        }
        let prev: &[u64] = if k == 0 {
            EMPTY
        } else {
            let (a, b) = flat[k - 1];
            get(&paras[a].runs[b])
        };
        let next: &[u64] = if k + 1 == flat.len() {
            EMPTY
        } else {
            let (a, b) = flat[k + 1];
            get(&paras[a].runs[b])
        };
        for &id in cur {
            if !prev.contains(&id) {
                opens[pi][ri].push(id);
            }
            if !next.contains(&id) {
                closes[pi][ri].push(id);
            }
        }
    }
    (opens, closes)
}

/// The move track (`w:moveFrom` / `w:moveTo`) a run carries, if any.
pub(crate) fn run_move(run: &Run) -> Option<&Track> {
    run.track.as_ref().filter(|t| t.kind.is_move())
}

/// For every paragraph, the field id that opens (begin/instrText/separate) before each run and the one
/// that closes (end) after each run. Computed over the whole run sequence in document order so a field
/// whose cached result spans several paragraphs opens once at its first run and closes once at its
/// last - mirroring [`comment_spans`], but a run is in at most one (outermost) field.
pub(crate) fn field_spans(paras: &[Paragraph]) -> (OptSpanGrid, OptSpanGrid) {
    let mut opens: Vec<Vec<Option<u64>>> = paras.iter().map(|p| vec![None; p.runs.len()]).collect();
    let mut closes: Vec<Vec<Option<u64>>> = opens.clone();
    single_id_spans(paras, &mut opens, &mut closes, |r| r.field);
    (opens, closes)
}

/// Per-run open/close tables for bookmark ranges (`bkm~{id}` -> `w:bookmarkStart`/`End`). A
/// bookmark is a single contiguous range: each id opens once at its FIRST run in document order and
/// closes once at its LAST, spanning any gap (a bookmark whose marks are not contiguous - e.g. one
/// wrapping a table, where the run before and after the table carry the mark but an inner boundary
/// does not - must still emit ONE start + ONE end, not a pair per run cluster; a per-cluster
/// emission duplicated the id, which the validator rejects). Several bookmarks can overlap on one
/// run (a stack of TOC bookmarks on a heading), so each id is tracked independently.
pub(crate) fn bookmark_spans(paras: &[Paragraph]) -> (SpanGrid, SpanGrid) {
    use std::collections::BTreeMap;
    /// A run address as `(paragraph index, run index)`.
    type RunAt = (usize, usize);
    // id -> (first run, last run) in document order.
    let mut extent: BTreeMap<u64, (RunAt, RunAt)> = BTreeMap::new();
    for (pi, p) in paras.iter().enumerate() {
        for (ri, run) in p.runs.iter().enumerate() {
            for &id in &run.bookmarks {
                extent
                    .entry(id)
                    .and_modify(|e| e.1 = (pi, ri))
                    .or_insert(((pi, ri), (pi, ri)));
            }
        }
    }
    let mut opens: Vec<Vec<Vec<u64>>> =
        paras.iter().map(|p| vec![Vec::new(); p.runs.len()]).collect();
    let mut closes: Vec<Vec<Vec<u64>>> =
        paras.iter().map(|p| vec![Vec::new(); p.runs.len()]).collect();
    // Emit ids in ascending order at each run so the output is deterministic.
    for (id, ((fp, fr), (lp, lr))) in extent {
        opens[fp][fr].push(id);
        closes[lp][lr].push(id);
    }
    (opens, closes)
}

/// Fill `opens`/`closes` with the id that opens before / closes after each run, for a single-id run
/// annotation (`get` reads it). Computed over the whole run sequence so a range spanning paragraphs
/// opens once at its first run and closes once at its last. Shared by field + bookmark spans.
fn single_id_spans(
    paras: &[Paragraph],
    opens: &mut [Vec<Option<u64>>],
    closes: &mut [Vec<Option<u64>>],
    get: impl Fn(&Run) -> Option<u64>,
) {
    let mut flat: Vec<(usize, usize)> = Vec::new();
    for (pi, p) in paras.iter().enumerate() {
        for ri in 0..p.runs.len() {
            flat.push((pi, ri));
        }
    }
    for (k, &(pi, ri)) in flat.iter().enumerate() {
        let Some(id) = get(&paras[pi].runs[ri]) else { continue };
        let prev = if k == 0 { None } else { let (a, b) = flat[k - 1]; get(&paras[a].runs[b]) };
        let next = if k + 1 == flat.len() { None } else { let (a, b) = flat[k + 1]; get(&paras[a].runs[b]) };
        if prev != Some(id) {
            opens[pi][ri] = Some(id);
        }
        if next != Some(id) {
            closes[pi][ri] = Some(id);
        }
    }
}

/// The annotation span tables + document-level maps threaded through the export walk, indexed per
/// paragraph (flat document order). Bundled so the paragraph/table serializers take one ref, not a
/// dozen slices.
/// Base for synthesized ids (range-marker pairs + uniquified wrapper repeats). Far above any id
/// a real document carries (Word emits small integers) yet inside `ST_DecimalNumber`'s
/// signed-32-bit range, so a synthesized id can never duplicate an imported
/// revision/bookmark/comment id - the validator requires document-wide uniqueness per family.
const SYNTH_MARK_ID_BASE: u64 = 900_000_000;

/// Export-wide id allocator, shared across the whole export (body + tables + cells):
/// - [`IdAlloc::fresh`] - a synthesized id for a range-marker PAIR (move rangeStart/End share
///   one; from/to halves pair by `w:name`, so the value itself is free).
/// - [`IdAlloc::wrapper`] - revision-wrapper id uniquification. One source revision can emit as
///   SEVERAL wrapper elements (a `w:del` split by field markers, a move spanning table cells, a
///   PAGE placeholder splitting into fldSimple segments); the FIRST emission keeps the source id,
///   repeats draw a fresh synthesized one. Word structures split revisions the same way (each
///   element its own id), and re-import pairs moves by name, so round-trip semantics hold.
pub(crate) struct IdAlloc {
    next: std::cell::Cell<u64>,
    seen: std::cell::RefCell<std::collections::HashSet<u64>>,
}

impl IdAlloc {
    pub(crate) fn new() -> Self {
        Self {
            next: std::cell::Cell::new(SYNTH_MARK_ID_BASE),
            seen: std::cell::RefCell::new(std::collections::HashSet::new()),
        }
    }

    pub(crate) fn fresh(&self) -> u64 {
        // Skip values already used: a re-imported document can carry synthesized ids from a
        // previous export as SOURCE ids, so the counter alone does not guarantee uniqueness.
        let mut seen = self.seen.borrow_mut();
        loop {
            let n = self.next.get();
            self.next.set(n + 1);
            if seen.insert(n) {
                return n;
            }
        }
    }

    pub(crate) fn wrapper(&self, id: u64) -> u64 {
        let first_use = self.seen.borrow_mut().insert(id); // borrow released before fresh()
        if first_use { id } else { self.fresh() }
    }
}

pub(crate) struct ExportSpans<'a> {
    /// The export-wide id allocator (see [`IdAlloc`]).
    pub(crate) ids: &'a IdAlloc,
    /// Comment ids to open (`commentRangeStart`) / close (`commentRangeEnd` + reference) per run.
    pub(crate) copens: &'a [Vec<Vec<u64>>],
    pub(crate) ccloses: &'a [Vec<Vec<u64>>],
    /// Field id opening (begin/instr/separate) / closing (end) per run.
    pub(crate) fopens: &'a [Vec<Option<u64>>],
    pub(crate) fcloses: &'a [Vec<Option<u64>>],
    /// Bookmark ids opening (`bookmarkStart`) / closing (`bookmarkEnd`) per run - multi-id, since
    /// several bookmarks can overlap on one run.
    pub(crate) bopens: &'a [Vec<Vec<u64>>],
    pub(crate) bcloses: &'a [Vec<Vec<u64>>],
    pub(crate) fields: &'a std::collections::HashMap<u64, String>,
    pub(crate) bookmarks: &'a std::collections::HashMap<u64, String>,
    /// Hyperlink target by id (`#anchor` internal / URL external); the range is per-paragraph from
    /// `Run.link` (a `w:hyperlink` can't cross paragraphs).
    pub(crate) links: &'a std::collections::HashMap<u64, String>,
    /// Picture placement by id - an image run (`Run.image`) emits a `<w:drawing>` from this instead of
    /// text. The blip references `rIdImg{id}` (the rel + media part `to_docx_bytes` injects).
    pub(crate) images: &'a std::collections::HashMap<u64, ImagePlacement>,
    /// Verbatim passthrough XML by id - a `Run.raw` run re-emits this captured `<w:r>...</w:r>` string
    /// unchanged instead of text (an unmodeled embedded object). See `docs/passthrough.md`.
    pub(crate) raw: &'a std::collections::HashMap<u64, String>,
}
