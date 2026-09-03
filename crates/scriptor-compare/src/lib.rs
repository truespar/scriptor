//! Document comparison (redline / blacklining): compute the A->B edit script between two `.docx`
//! files and replay it as authored tracked-change suggestions onto A's [`CollabDoc`], so the output
//! is an ordinary Word redline that flows through the existing accept/reject + display + collab
//! machinery.
//!
//! The engine is a *diff engine that speaks in `suggest_*` calls*:
//!
//! 1. Import A into a mutable `CollabDoc`; read B read-only as `Vec<Paragraph>`.
//! 2. [`align`](align) A's blocks to B's (exact backbone + similarity gap-fill).
//! 3. Replay the alignment as authored suggestions on A (inline word diff, format / paragraph-format
//!    changes, whole-paragraph insert / delete), then `to_docx_bytes()`.
//!
//! The correctness contract - the *oracle* - is that `compare(A, B)` then accept-all reproduces B
//! and reject-all reproduces A, text-stable. See [`check`].

pub mod align;
pub mod diff;
pub mod manifest;
pub mod overlay;
pub mod token;

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use scriptor_crdt::{model::BodyItem, CollabDoc, Paragraph, Run, RunFormat};

use align::{word_bag, Align, WordBag};
pub use manifest::{AlignEntry, AlignKind, Change, ChangeKind, Manifest};
pub use overlay::{AnnotatedManifest, Annotation, Materiality};
use token::tokenize;

/// Knobs for one comparison.
#[derive(Debug, Clone)]
pub struct CompareOptions {
    /// The single reviewer every emitted revision is attributed to (like Word's Compare).
    pub author: String,
    /// ISO-8601 date stamped on every revision (a parameter, never "now" - the engine is
    /// deterministic).
    pub date: String,
    /// The loro commit / audit message stamped on each op.
    pub audit: String,
    /// Similarity threshold in `[0, 1]`: two unmatched blocks whose word-bag Sorensen-Dice score is
    /// at least this are treated as an *edited* paragraph pair rather than delete + insert. Lower =
    /// more aggressive pairing (fewer whole-paragraph churns, risk of pairing unrelated text);
    /// higher = more conservative. 0.5 is a sensible default.
    pub similarity_threshold: f64,
    /// Detect and redline **formatting** changes - run properties (`w:rPrChange`), paragraph
    /// properties (`w:pPrChange`), and style. When `false`, only *content* (text / structure) changes
    /// are reported (Word / Litera "ignore formatting"). Default `true`.
    pub detect_formatting: bool,
    /// Detect a paragraph **moved** verbatim (`w:moveFrom`/`w:moveTo`). When `false`, a move is
    /// reported as a deletion plus an insertion. Default `true`.
    pub detect_moves: bool,
    /// Ignore differences that are only **whitespace** (e.g. single vs. double space) - such a change
    /// is not redlined. Default `false`.
    pub ignore_whitespace: bool,
    /// Ignore differences that are only **letter case** - a case-only change is not redlined. Default
    /// `false`.
    pub ignore_case: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            author: "Compare".to_string(),
            date: "2026-01-01T00:00:00Z".to_string(),
            audit: "document comparison".to_string(),
            similarity_threshold: 0.5,
            detect_formatting: true,
            detect_moves: true,
            ignore_whitespace: false,
            ignore_case: false,
        }
    }
}

/// The result of a comparison: the redlined `.docx` and the machine-readable manifest of changes.
pub struct CompareResult {
    pub redline: Vec<u8>,
    pub manifest: Manifest,
}

/// Ends a [`CollabDoc`] bulk-emission batch when it drops, so a `?` early-return (or a panic) inside
/// emission can't leave the batch's block-sequence memo active on the thread.
struct BulkGuard<'a>(&'a CollabDoc);
impl Drop for BulkGuard<'_> {
    fn drop(&mut self) {
        self.0.end_bulk();
    }
}

/// Compare two `.docx` documents and produce a redline (A with every difference as an authored
/// tracked change) plus the change manifest.
pub fn compare(original: &[u8], revised: &[u8], opts: &CompareOptions) -> Result<CompareResult> {
    let a = CollabDoc::from_docx_bytes(original)?;
    let b = CollabDoc::from_docx_bytes(revised)?;
    let a_paras = a.paragraphs()?;
    let b_paras = b.paragraphs()?;
    let a_body = a.body();
    let b_body = b.body();

    // Emission replays thousands of `suggest_*` ops onto `a`; run it as one bulk batch so each op's
    // index-resolution + id-allocation is O(1) amortized instead of an O(N) whole-document rescan.
    let manifest = {
        a.begin_bulk()?;
        let _bulk = BulkGuard(&a);
        emit(&a, &b, &a_paras, &a_body, &b_paras, &b_body, opts)?
    };
    let redline = a.to_docx_bytes()?;
    Ok(CompareResult { redline, manifest })
}

/// The oracle: run `compare`, then verify accept-all reproduces `revised` and reject-all reproduces
/// `original`, text-stable. Returns the report; `ok` is the gate.
pub fn check(original: &[u8], revised: &[u8], opts: &CompareOptions) -> Result<OracleReport> {
    let result = compare(original, revised, opts)?;

    let want_b = doc_signature(&CollabDoc::from_docx_bytes(revised)?)?;
    let want_a = doc_signature(&CollabDoc::from_docx_bytes(original)?)?;

    // Resolve *every* revision (accept_all / reject_all walk the full id set, including the ¶ splits
    // that the manifest does not surface as separate entries).
    let accepted = CollabDoc::from_docx_bytes(&result.redline)?;
    accepted.accept_all("oracle accept")?;
    let got_b = doc_signature(&accepted)?;

    let rejected = CollabDoc::from_docx_bytes(&result.redline)?;
    rejected.reject_all("oracle reject")?;
    let got_a = doc_signature(&rejected)?;

    Ok(OracleReport {
        accept_ok: got_b == want_b,
        reject_ok: got_a == want_a,
        accept_mismatch: first_mismatch(&want_b, &got_b),
        reject_mismatch: first_mismatch(&want_a, &got_a),
        changes: result.manifest.changes.len(),
    })
}

/// Outcome of the [`check`] oracle.
#[derive(Debug)]
pub struct OracleReport {
    pub accept_ok: bool,
    pub reject_ok: bool,
    /// `(paragraph, expected, got)` of the first differing paragraph after accept-all, if any.
    pub accept_mismatch: Option<(usize, String, String)>,
    pub reject_mismatch: Option<(usize, String, String)>,
    pub changes: usize,
}

impl OracleReport {
    pub fn ok(&self) -> bool {
        self.accept_ok && self.reject_ok
    }
}

// ── the emission driver ───────────────────────────────────────────────────────

/// Replay the A->B alignment as authored suggestions on `a`, returning the manifest.
fn emit(
    a: &CollabDoc,
    b: &CollabDoc,
    a_paras: &[Paragraph],
    a_body: &[BodyItem],
    b_paras: &[Paragraph],
    b_body: &[BodyItem],
    opts: &CompareOptions,
) -> Result<Manifest> {
    let has_table = |body: &[BodyItem]| body.iter().any(|i| matches!(i, BodyItem::Table(_)));
    if has_table(a_body) || has_table(b_body) {
        return emit_structured(a, b, a_paras, a_body, b_paras, b_body, opts);
    }
    emit_flat(a, b, a_paras, b_paras, opts)
}

/// The flat, table-free emission path: align the paragraph list and replay the edit
/// script. Used when neither document contains a table.
fn emit_flat(
    a: &CollabDoc,
    b: &CollabDoc,
    a_paras: &[Paragraph],
    b_paras: &[Paragraph],
    opts: &CompareOptions,
) -> Result<Manifest> {
    let (a_styles, b_styles) = (a.styles(), b.styles());
    let sigs_a: Vec<String> = a_paras.iter().map(|p| signature(p, &a_styles.names)).collect();
    let sigs_b: Vec<String> = b_paras.iter().map(|p| signature(p, &b_styles.names)).collect();
    let bags_a: Vec<WordBag> = a_paras.iter().map(|p| word_bag(&para_text(p))).collect();
    let bags_b: Vec<WordBag> = b_paras.iter().map(|p| word_bag(&para_text(p))).collect();

    let alignment = align::align(&sigs_a, &sigs_b, &bags_a, &bags_b, opts.similarity_threshold);
    drop((a_styles, b_styles));

    let mut em = Emit { a, b, opts, changes: Vec::new() };

    // Pass A: edited pairs (inline text + run-format + paragraph-format). Paragraph count is
    // unchanged, so indices stay identity through this pass and pass B.
    for entry in &alignment {
        if let Align::Equal { a: ai, b: bi } = *entry {
            em.edit_pair(ai, &a_paras[ai], &b_paras[bi])?;
        }
    }

    // Group the alignment into gaps (maximal runs of Delete/Insert) bracketed by kept paragraphs.
    let gaps = group_gaps(&alignment, a_paras.len());

    // Detect verbatim single-paragraph moves: an isolated deleted paragraph and an isolated inserted
    // paragraph with identical text (a clause relocated unchanged). Each becomes a `w:moveFrom`
    // source + a `w:moveTo` destination sharing one revision id, instead of a delete + an insert.
    // Disabled (a move falls back to delete + insert) when `detect_moves` is off.
    let moves = if opts.detect_moves { detect_moves(&gaps, a_paras, b_paras) } else { Vec::new() };
    let moved_a: HashSet<usize> = moves.iter().map(|m| m.0).collect();
    let mut move_id_of: HashMap<usize, u64> = HashMap::new();

    // Move sources (`w:moveFrom`) are non-destructive, like deletions - emit at original indices.
    // Merge the moved-away paragraph forward into the next paragraph, or backward into the previous
    // one when it is the document's last paragraph (using its live length - the previous paragraph
    // may already carry pass-A edits).
    for &(a_del, b_ins) in &moves {
        let id = if a_del + 1 < a_paras.len() {
            em.a.suggest_move_span(a_del, 0, a_del + 1, 0, em.author(), em.date(), em.audit())?
        } else {
            let prev_len = em.live_len(a_del - 1)?;
            let self_len = em.live_len(a_del)?;
            em.a.suggest_move_span(a_del - 1, prev_len, a_del, self_len, em.author(), em.date(), em.audit())?
        };
        move_id_of.insert(b_ins, id);
        em.record(
            a_del as i64,
            -4,
            Change::new(id, ChangeKind::Move, a_del).before(para_text(&a_paras[a_del])),
        );
    }

    // Pass B: whole-paragraph deletions (non-destructive, no index shift); skip moved sources.
    for g in &gaps {
        em.delete_block(g, a_paras, &moved_a)?;
    }

    // Pass C: whole-paragraph insertions (these DO grow the live paragraph list, tracked via
    // `live_of`); a moved destination is emitted as `w:moveTo` under its source's id.
    let mut live_of: Vec<usize> = (0..a_paras.len()).collect();
    for g in &gaps {
        em.insert_block(g, b_paras, &mut live_of, &move_id_of)?;
    }

    let mut changes = em.changes;
    changes.sort_by_key(|(p, s, _)| (*p, *s));
    Ok(Manifest {
        changes: changes.into_iter().map(|(_, _, c)| c).collect(),
        alignment: align_entries(&alignment, a_paras, b_paras),
    })
}

/// Project the block alignment into the manifest's paragraph correspondence (the side-by-side view's
/// scroll-lock + highlight data). An `Equal` pair whose text differs is reported as `Edited`.
fn align_entries(alignment: &[Align], a_paras: &[Paragraph], b_paras: &[Paragraph]) -> Vec<AlignEntry> {
    alignment
        .iter()
        .map(|e| match *e {
            Align::Equal { a, b } => {
                let kind = if para_text(&a_paras[a]) == para_text(&b_paras[b]) {
                    AlignKind::Equal
                } else {
                    AlignKind::Edited
                };
                AlignEntry { a: Some(a), b: Some(b), kind }
            }
            Align::Delete { a } => AlignEntry { a: Some(a), b: None, kind: AlignKind::Delete },
            Align::Insert { b } => AlignEntry { a: None, b: Some(b), kind: AlignKind::Insert },
        })
        .collect()
}

/// A top-level body item (a paragraph or a table) with its flat-paragraph span and, for tables, the
/// per-row / per-cell flat layout - the bridge between the structural body and the flat paragraph
/// list the `suggest_*` primitives address.
struct Item {
    is_table: bool,
    flat_start: usize,
    /// For a table: rows -> cells -> (cell's first flat paragraph index, paragraph count).
    rows: Vec<Vec<(usize, usize)>>,
    /// All text of the item, for the alignment signature + similarity.
    text: String,
}

/// A whole row to insert into a matched table: anchored on an existing row (below it, or above it
/// for a row inserted before the first row), with the new row's per-column cell texts.
struct RowInsert {
    /// A paragraph in the anchor row.
    anchor_flat: usize,
    /// Insert below the anchor row (`true`) or above it (`false`, only for a top-of-table insert).
    below: bool,
    /// The row's index in B, for stable ordering of a multi-row insertion block.
    b_order: usize,
    /// The new row's cell texts, one per column.
    cells: Vec<String>,
}

/// The number of columns if every row has the same cell count (a uniform grid), else `None` (a table
/// with merged / spanning cells - column alignment is skipped for it).
fn uniform_cols(rows: &[Vec<(usize, usize)>]) -> Option<usize> {
    let n = rows.first()?.len();
    rows.iter().all(|r| r.len() == n).then_some(n)
}

/// The text of column `c` across all rows (its alignment signature).
fn col_text(rows: &[Vec<(usize, usize)>], c: usize, paras: &[Paragraph]) -> String {
    rows.iter().filter_map(|r| r.get(c)).map(|cell| cell_text(*cell, paras)).collect::<Vec<_>>().join("\n")
}

/// The text of one cell (its paragraphs joined) - `cell` is `(first flat paragraph, paragraph count)`.
fn cell_text(cell: (usize, usize), paras: &[Paragraph]) -> String {
    let (start, count) = cell;
    (start..start + count).map(|i| paras.get(i).map(para_text).unwrap_or_default()).collect::<Vec<_>>().join("\n")
}

/// The text of one row (its cells tab-joined) - the row-alignment signature.
fn row_text(row: &[(usize, usize)], paras: &[Paragraph]) -> String {
    row.iter().map(|c| cell_text(*c, paras)).collect::<Vec<_>>().join("\t")
}

/// Walk the structural body against the flat paragraph list, assigning each item its flat span.
fn build_items(body: &[BodyItem], paras: &[Paragraph]) -> Vec<Item> {
    let text_at = |i: usize| paras.get(i).map(para_text).unwrap_or_default();
    let mut items = Vec::new();
    let mut flat = 0usize;
    for bi in body {
        match bi {
            BodyItem::Paragraph => {
                items.push(Item { is_table: false, flat_start: flat, rows: Vec::new(), text: text_at(flat) });
                flat += 1;
            }
            BodyItem::Table(t) => {
                let start = flat;
                let mut rows = Vec::new();
                let mut text = String::new();
                for row in &t.rows {
                    let mut cells = Vec::new();
                    for cell in &row.cells {
                        let cstart = flat;
                        for _ in 0..cell.para_count {
                            text.push_str(&text_at(flat));
                            text.push('\n');
                            flat += 1;
                        }
                        cells.push((cstart, cell.para_count));
                    }
                    rows.push(cells);
                }
                items.push(Item { is_table: true, flat_start: start, rows, text });
            }
        }
    }
    items
}

/// The structured (table-aware) emission path. Aligns top-level body items (paragraphs + tables),
/// then emits: matched paragraphs through the inline pipeline; matched tables by recursing into their
/// cells (content edits, whole-row insert/delete, whole-column delete); and a whole removed table as
/// per-row deletions. **Staged** (detected but not emitted, so the grid is never corrupted and
/// reject-all still reproduces the original): whole-table *insertion*, column *insertion* (a 2-D flat
/// shift), a body paragraph added/removed around a table, and cell merges / nested tables.
fn emit_structured(
    a: &CollabDoc,
    b: &CollabDoc,
    a_paras: &[Paragraph],
    a_body: &[BodyItem],
    b_paras: &[Paragraph],
    b_body: &[BodyItem],
    opts: &CompareOptions,
) -> Result<Manifest> {
    let a_items = build_items(a_body, a_paras);
    let b_items = build_items(b_body, b_paras);
    let item_sig = |it: &Item| format!("{}\u{0}{}", if it.is_table { 'T' } else { 'P' }, it.text);
    let sigs_a: Vec<String> = a_items.iter().map(&item_sig).collect();
    let sigs_b: Vec<String> = b_items.iter().map(&item_sig).collect();
    let bags_a: Vec<WordBag> = a_items.iter().map(|it| word_bag(&it.text)).collect();
    let bags_b: Vec<WordBag> = b_items.iter().map(|it| word_bag(&it.text)).collect();

    let alignment = align::align(&sigs_a, &sigs_b, &bags_a, &bags_b, opts.similarity_threshold);

    let mut em = Emit { a, b, opts, changes: Vec::new() };
    // Content edits (matched paragraphs + matched-table cells); collect table structural ops.
    let mut row_deletes: Vec<(usize, String)> = Vec::new();
    let mut row_inserts: Vec<RowInsert> = Vec::new();
    let mut col_deletes: Vec<(usize, String)> = Vec::new();
    for entry in &alignment {
        match *entry {
            Align::Equal { a: ai, b: bi } => {
                let (ia, ib) = (&a_items[ai], &b_items[bi]);
                match (ia.is_table, ib.is_table) {
                    (false, false) => em.edit_pair(ia.flat_start, &a_paras[ia.flat_start], &b_paras[ib.flat_start])?,
                    (true, true) => em.diff_table(ia, ib, a_paras, b_paras, &mut row_deletes, &mut row_inserts, &mut col_deletes)?,
                    _ => {} // a paragraph replaced by a table (or vice versa) - structural, staged
                }
            }
            Align::Delete { a: ai } => {
                // A whole table removed: mark every row deleted (accept removes the table). A deleted
                // body paragraph in a table document is staged (it needs the flat block machinery).
                let ia = &a_items[ai];
                if ia.is_table {
                    for row in &ia.rows {
                        if let Some(cell) = row.first() {
                            row_deletes.push((cell.0, row_text(row, a_paras)));
                        }
                    }
                }
            }
            Align::Insert { .. } => {} // whole-table / body-paragraph insertion - staged
        }
    }

    // Row + column deletions are tracked (non-destructive), so no flat-index shift - apply at
    // original indices, in any order.
    for (flat, text) in &row_deletes {
        em.a.suggest_delete_table_row(*flat, em.author(), em.date(), em.audit())?;
        em.record(*flat as i64, -3, Change::new(0, ChangeKind::TableRowDelete, *flat).before(text.clone()));
    }
    for (flat, text) in &col_deletes {
        em.a.suggest_delete_table_column(*flat, em.author(), em.date(), em.audit())?;
        em.record(*flat as i64, -3, Change::new(0, ChangeKind::TableColumnDelete, *flat).before(text.clone()));
    }

    // Row insertions grow the flat list, so apply back-to-front (highest anchor first): a later
    // insert sits above an earlier one and never shifts its still-original anchor index. Within one
    // anchor, higher b-order first so the block ends up in document order.
    row_inserts.sort_by_key(|ri| (std::cmp::Reverse(ri.anchor_flat), std::cmp::Reverse(ri.b_order)));
    for ri in &row_inserts {
        let Some(caret) = em.a.suggest_insert_table_row(ri.anchor_flat, ri.below, em.author(), em.date(), em.audit())? else {
            continue;
        };
        let count = em.a.paragraphs()?.len();
        for (j, cell) in ri.cells.iter().enumerate() {
            // The new row's j-th cell is one empty paragraph at caret + j (uniform single-paragraph
            // cells). Content rides the tracked-inserted row, so a plain fill is correct.
            if !cell.is_empty() && caret + j < count {
                em.a.insert_text(caret + j, 0, cell, em.audit())?;
            }
        }
        em.record(
            ri.anchor_flat as i64,
            2_000_000 + ri.b_order as i64,
            Change::new(0, ChangeKind::TableRowInsert, ri.anchor_flat).after(ri.cells.join("\t")),
        );
    }

    // Item-level correspondence (paragraphs interleaved with tables) mapped to each item's first flat
    // paragraph - coarser than the flat path (one anchor per body item, not per paragraph) but enough
    // to scroll-lock the side-by-side view across a document with tables.
    let align_out: Vec<AlignEntry> = alignment
        .iter()
        .map(|e| match *e {
            Align::Equal { a, b } => {
                let kind = if a_items[a].text == b_items[b].text {
                    AlignKind::Equal
                } else {
                    AlignKind::Edited
                };
                AlignEntry { a: Some(a_items[a].flat_start), b: Some(b_items[b].flat_start), kind }
            }
            Align::Delete { a } => {
                AlignEntry { a: Some(a_items[a].flat_start), b: None, kind: AlignKind::Delete }
            }
            Align::Insert { b } => {
                AlignEntry { a: None, b: Some(b_items[b].flat_start), kind: AlignKind::Insert }
            }
        })
        .collect();

    let mut changes = em.changes;
    changes.sort_by_key(|(p, s, _)| (*p, *s));
    Ok(Manifest { changes: changes.into_iter().map(|(_, _, c)| c).collect(), alignment: align_out })
}

/// One maximal run of unmatched blocks between two kept paragraphs.
struct Gap {
    /// Original A index of the kept paragraph before the gap (`None` = document start).
    prev: Option<usize>,
    /// Original A index of the kept paragraph after the gap (`None` = document end).
    next: Option<usize>,
    /// Deleted A block, original indices, ascending and contiguous.
    dels: Vec<usize>,
    /// Inserted B block, original B indices, ascending.
    ins: Vec<usize>,
}

fn group_gaps(alignment: &[Align], _a_len: usize) -> Vec<Gap> {
    let mut gaps = Vec::new();
    let mut prev: Option<usize> = None;
    let mut dels: Vec<usize> = Vec::new();
    let mut ins: Vec<usize> = Vec::new();
    for entry in alignment {
        match *entry {
            Align::Equal { a, .. } => {
                if !dels.is_empty() || !ins.is_empty() {
                    gaps.push(Gap {
                        prev,
                        next: Some(a),
                        dels: std::mem::take(&mut dels),
                        ins: std::mem::take(&mut ins),
                    });
                }
                prev = Some(a);
            }
            Align::Delete { a } => dels.push(a),
            Align::Insert { b } => ins.push(b),
        }
    }
    if !dels.is_empty() || !ins.is_empty() {
        gaps.push(Gap { prev, next: None, dels, ins });
    }
    gaps
}

/// Detect verbatim single-paragraph moves: a paragraph that is the sole deletion of its gap and,
/// elsewhere, the sole insertion of another gap, with identical text (a clause relocated unchanged).
/// Returns `(source A index, destination B index)` pairs. Blank paragraphs and a last-paragraph
/// source (no forward-merge target) are excluded; a move + edit stays a delete + insert (conservative,
/// so an edit is never hidden inside a "move").
fn detect_moves(gaps: &[Gap], a_paras: &[Paragraph], b_paras: &[Paragraph]) -> Vec<(usize, usize)> {
    let mut del_by_text: HashMap<String, Vec<usize>> = HashMap::new();
    let mut ins_by_text: HashMap<String, Vec<usize>> = HashMap::new();
    for g in gaps {
        if g.dels.len() == 1 && g.ins.is_empty() {
            let a = g.dels[0];
            // Need a neighbour to merge the moved-away source into (forward, or backward if it is the
            // document's last paragraph).
            if a_paras.len() >= 2 {
                del_by_text.entry(para_text(&a_paras[a])).or_default().push(a);
            }
        }
        if g.ins.len() == 1 && g.dels.is_empty() {
            ins_by_text.entry(para_text(&b_paras[g.ins[0]])).or_default().push(g.ins[0]);
        }
    }
    let mut moves = Vec::new();
    for (text, dels) in &del_by_text {
        if text.trim().is_empty() {
            continue;
        }
        if let Some(inss) = ins_by_text.get(text) {
            for k in 0..dels.len().min(inss.len()) {
                moves.push((dels[k], inss[k]));
            }
        }
    }
    moves.sort_unstable(); // HashMap iteration is unordered; keep the result deterministic
    moves
}

struct Emit<'d> {
    a: &'d CollabDoc,
    /// The revised document - kept for cross-document style resolution (see [`same_para_style`]); the
    /// redline is authored onto `a`.
    b: &'d CollabDoc,
    opts: &'d CompareOptions,
    /// (primary sort key, secondary sort key, change) - flattened to document order at the end.
    changes: Vec<(i64, i64, Change)>,
}

/// Whether two paragraph style ids denote the **same** style: equal ids, or ids that resolve to the
/// same display name (`w:name`) across the two documents. Comparison inputs are routinely the same
/// content re-saved with localized vs. English built-in style ids (`Brdtext`<->`BodyText`,
/// `Rubrik1`<->`Heading1`, `Liststycke`<->`ListParagraph`); those denote one style, are not a real
/// formatting change, and must not flood the redline with `w:pPrChange` noise. Matching on `w:name`
/// (Word's stable, language-independent style identity) collapses them; a genuine restyle (e.g.
/// Normal -> Heading 1, whose names differ) is still reported.
fn same_para_style(a: &CollabDoc, a_id: Option<&str>, b: &CollabDoc, b_id: Option<&str>) -> bool {
    if a_id == b_id {
        return true;
    }
    let name = |doc: &CollabDoc, id: Option<&str>| -> Option<String> {
        id.and_then(|i| doc.styles().names.get(i).map(|n| n.trim().to_ascii_lowercase()))
    };
    match (name(a, a_id), name(b, b_id)) {
        (Some(na), Some(nb)) => na == nb,
        _ => false,
    }
}

impl Emit<'_> {
    fn author(&self) -> &str {
        &self.opts.author
    }
    fn date(&self) -> &str {
        &self.opts.date
    }
    fn audit(&self) -> &str {
        &self.opts.audit
    }

    fn record(&mut self, primary: i64, secondary: i64, change: Change) {
        self.changes.push((primary, secondary, change));
    }

    /// Diff one matched paragraph pair and emit inline text ops, run-format changes, and
    /// paragraph-format / style changes. `idx` is the (identity) paragraph index in A.
    fn edit_pair(&mut self, idx: usize, a_para: &Paragraph, b_para: &Paragraph) -> Result<()> {
        // Fast path: identical paragraphs (same text, formatting, runs) have no changes - skip all the
        // per-character format-key work. Comparing two versions of a document, most paragraphs are
        // untouched, so this is the difference between an instant compare and a browser-freezing one.
        if a_para == b_para {
            return Ok(());
        }
        let a_text = para_text(a_para);
        let b_text = para_text(b_para);

        let mut prims = Vec::new();
        if a_text != b_text {
            inline_prims(&a_text, &b_text, self.opts, &mut prims);
        }
        // Run-format changes (w:rPrChange) - only when detecting formatting, and only when the runs
        // differ (identical run lists can't produce one).
        if self.opts.detect_formatting && a_para.runs != b_para.runs {
            format_prims(a_para, b_para, &mut prims);
        }

        // Apply within-paragraph ops in descending anchor order: no op shifts a position to its
        // left (deletions retain their text; insertions grow only to their right), so every op's
        // anchor stays valid in the original A coordinate system.
        prims.sort_by_key(|p| std::cmp::Reverse(p.anchor()));
        for prim in prims {
            self.apply_prim(idx, prim)?;
        }

        // Paragraph-level property + style changes (length-neutral) - only when detecting formatting.
        if !self.opts.detect_formatting {
            return Ok(());
        }
        if a_para.props != b_para.props {
            let id = self.a.suggest_paragraph_format(
                idx,
                &b_para.props,
                self.author(),
                self.date(),
                self.audit(),
            )?;
            self.record(idx as i64, -2, Change::new(id, ChangeKind::ParaFormat, idx));
        }
        if a_para.style != b_para.style
            && !same_para_style(self.a, a_para.style.as_deref(), self.b, b_para.style.as_deref())
        {
            let id = self.a.suggest_paragraph_style(
                idx,
                b_para.style.as_deref(),
                self.author(),
                self.date(),
                self.audit(),
            )?;
            self.record(idx as i64, -1, Change::new(id, ChangeKind::ParaFormat, idx));
        }
        Ok(())
    }

    /// Diff two matched tables. Aligns rows (by cell-text signature + similarity); edits the cells of
    /// matched rows inline; and collects whole-row insert / delete + whole-column delete ops for the
    /// caller to apply after all content edits. When the rows line up 1:1 but the *column* count
    /// changed, it instead aligns columns - so cells map through the column correspondence (not
    /// positionally) and a removed column is marked deleted. Column *insertion*, whole-table
    /// insertion, and cell merges are not modeled (staged); the grid is never corrupted.
    #[allow(clippy::too_many_arguments)]
    fn diff_table(
        &mut self,
        a: &Item,
        b: &Item,
        a_paras: &[Paragraph],
        b_paras: &[Paragraph],
        row_deletes: &mut Vec<(usize, String)>,
        row_inserts: &mut Vec<RowInsert>,
        col_deletes: &mut Vec<(usize, String)>,
    ) -> Result<()> {
        let sigs_a: Vec<String> = a.rows.iter().map(|r| row_text(r, a_paras)).collect();
        let sigs_b: Vec<String> = b.rows.iter().map(|r| row_text(r, b_paras)).collect();
        let bags_a: Vec<WordBag> = sigs_a.iter().map(|s| word_bag(s)).collect();
        let bags_b: Vec<WordBag> = sigs_b.iter().map(|s| word_bag(s)).collect();
        let ra = align::align(&sigs_a, &sigs_b, &bags_a, &bags_b, self.opts.similarity_threshold);

        // Column-change path: rows line up 1:1 but the (uniform) column count differs. Align columns
        // and route cell edits through that correspondence; mark a removed column deleted.
        let rows_stable = a.rows.len() == b.rows.len() && ra.iter().all(|e| matches!(e, Align::Equal { .. }));
        if rows_stable
            && let (Some(nca), Some(ncb)) = (uniform_cols(&a.rows), uniform_cols(&b.rows))
            && nca != ncb
        {
            let csa: Vec<String> = (0..nca).map(|c| col_text(&a.rows, c, a_paras)).collect();
            let csb: Vec<String> = (0..ncb).map(|c| col_text(&b.rows, c, b_paras)).collect();
            let cba: Vec<WordBag> = csa.iter().map(|s| word_bag(s)).collect();
            let cbb: Vec<WordBag> = csb.iter().map(|s| word_bag(s)).collect();
            let cal = align::align(&csa, &csb, &cba, &cbb, self.opts.similarity_threshold);
            for r in 0..a.rows.len() {
                for entry in &cal {
                    if let Align::Equal { a: aci, b: bci } = *entry {
                        let (a_start, a_pc) = a.rows[r][aci];
                        let (b_start, b_pc) = b.rows[r][bci];
                        for p in 0..a_pc.min(b_pc) {
                            self.edit_pair(a_start + p, &a_paras[a_start + p], &b_paras[b_start + p])?;
                        }
                    }
                }
            }
            for entry in &cal {
                if let Align::Delete { a: aci } = *entry
                    && let Some(cell) = a.rows.first().map(|r| r[aci])
                {
                    col_deletes.push((cell.0, col_text(&a.rows, aci, a_paras)));
                }
                // Column insertion is staged (it shifts every row's flat indices - a 2-D shift).
            }
            return Ok(());
        }

        // Row-change path (the default): positional cell edits + whole-row insert / delete.
        let mut prev_a: Option<usize> = None;
        for entry in &ra {
            match *entry {
                Align::Equal { a: ari, b: bri } => {
                    let (ca, cb) = (&a.rows[ari], &b.rows[bri]);
                    for c in 0..ca.len().min(cb.len()) {
                        let (a_start, a_pc) = ca[c];
                        let (b_start, b_pc) = cb[c];
                        for p in 0..a_pc.min(b_pc) {
                            self.edit_pair(a_start + p, &a_paras[a_start + p], &b_paras[b_start + p])?;
                        }
                    }
                    prev_a = Some(ari);
                }
                Align::Delete { a: ari } => {
                    if let Some(cell) = a.rows[ari].first() {
                        row_deletes.push((cell.0, row_text(&a.rows[ari], a_paras)));
                    }
                    prev_a = Some(ari);
                }
                Align::Insert { b: bri } => {
                    let cells: Vec<String> = b.rows[bri].iter().map(|c| cell_text(*c, b_paras)).collect();
                    match prev_a {
                        // Insert below the preceding existing row.
                        Some(ari) => {
                            if let Some(anchor) = a.rows[ari].first() {
                                row_inserts.push(RowInsert { anchor_flat: anchor.0, below: true, b_order: bri, cells });
                            }
                        }
                        // A row before the first row: anchor above the first row instead.
                        None => {
                            if let Some(anchor) = a.rows.first().and_then(|r| r.first()) {
                                row_inserts.push(RowInsert { anchor_flat: anchor.0, below: false, b_order: bri, cells });
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_prim(&mut self, idx: usize, prim: Prim) -> Result<()> {
        match prim {
            Prim::Del { s, e, before } => {
                let id = self.a.suggest_deletion_multi(
                    idx, s, idx, e, self.author(), self.date(), self.audit(),
                )?;
                self.record(idx as i64, s as i64, Change::new(id, ChangeKind::Delete, idx).before(before));
            }
            Prim::Ins { at, text } => {
                let after = text.clone();
                let id = self.a.suggest_insertion(idx, at, &text, self.author(), self.date(), self.audit())?;
                self.record(idx as i64, at as i64, Change::new(id, ChangeKind::Insert, idx).after(after));
            }
            Prim::Replace { s, e, before, text } => {
                self.a.suggest_deletion_multi(idx, s, idx, e, self.author(), self.date(), self.audit())?;
                let id = self.a.suggest_insertion(idx, e, &text, self.author(), self.date(), self.audit())?;
                self.record(
                    idx as i64,
                    s as i64,
                    Change::new(id, ChangeKind::Replace, idx).before(before).after(text),
                );
            }
            Prim::Fmt { s, e, fmt } => {
                let id = self.a.suggest_format(idx, s..e, &fmt, self.author(), self.date(), self.audit())?;
                self.record(idx as i64, s as i64, Change::new(id, ChangeKind::Format, idx));
            }
        }
        Ok(())
    }

    /// Delete a gap's A block by merging it forward into the following kept paragraph, so accepting
    /// removes the paragraphs entirely and rejecting restores them. Falls back to a text-only
    /// deletion (leaving empty paragraphs) where the structural merge is refused (a cross-container
    /// span) or impossible (a deletion at the very end of the document).
    fn delete_block(&mut self, g: &Gap, a_paras: &[Paragraph], moved: &HashSet<usize>) -> Result<()> {
        // A gap whose (sole) deletion is a move source is handled by the move path - nothing to
        // delete here. Only singleton-deletion gaps are ever moves, so a multi-deletion block is
        // never partially moved (which would break the contiguous forward-merge below).
        if g.dels.is_empty() || g.dels.iter().all(|d| moved.contains(d)) {
            return Ok(());
        }
        let first = g.dels[0];
        let last = *g.dels.last().unwrap();

        let structural = if let Some(next) = g.next {
            self.a.suggest_deletion_multi(first, 0, next, 0, self.author(), self.date(), self.audit())
        } else if let Some(prev) = g.prev.filter(|_| g.ins.is_empty()) {
            // Trailing deletion, nothing inserted here: merge backward into the previous kept
            // paragraph so the block still vanishes on accept. Use the *live* length of `prev` - it
            // may already carry pass-A inline edits, so its snapshot length is stale.
            let prev_len = self.live_len(prev)?;
            let last_len = self.live_len(last)?;
            self.a.suggest_deletion_multi(prev, prev_len, last, last_len, self.author(), self.date(), self.audit())
        } else {
            Err(anyhow::anyhow!("no structural merge target"))
        };

        if structural.is_err() {
            // Text-only fallback: mark each paragraph's runs deleted (the ¶s stay, so accept leaves
            // empty paragraphs - a known limitation for cross-container / doc-end blocks).
            for &d in &g.dels {
                let len = para_text(&a_paras[d]).chars().count();
                if len > 0 {
                    let id = self.a.suggest_deletion_multi(
                        d, 0, d, len, self.author(), self.date(), self.audit(),
                    )?;
                    self.record(d as i64, -1, Change::new(id, ChangeKind::ParaDelete, d).before(para_text(&a_paras[d])));
                }
            }
            return Ok(());
        }

        let id = structural.unwrap();
        for &d in &g.dels {
            self.record(d as i64, -1, Change::new(id, ChangeKind::ParaDelete, d).before(para_text(&a_paras[d])));
        }
        Ok(())
    }

    /// Insert a gap's B block. When a paragraph *follows* the insertion point (a top / interior /
    /// leading gap), each new paragraph is inserted by prepending its text to that following
    /// paragraph and splitting it off - so the text revision id is allocated *before* the ¶ revision
    /// id. That ordering is load-bearing: on reject-all (ascending id) the inserted text is removed
    /// before the ¶ is joined away, so the insertion's peritext mark never bleeds onto the following
    /// text. When nothing follows (a trailing / bottom append), split the preceding paragraph at its
    /// end and fill the new ¶ instead.
    fn insert_block(
        &mut self,
        g: &Gap,
        b_paras: &[Paragraph],
        live_of: &mut [usize],
        move_id_of: &HashMap<usize, u64>,
    ) -> Result<()> {
        if g.ins.is_empty() {
            return Ok(());
        }
        let prev_primary = g.prev.map(|p| p as i64).unwrap_or(-1);

        // Insert *before the following kept paragraph* (after any deleted block), so the split never
        // touches - and never overwrites - a deleted paragraph's own ¶ mark. When there is no kept
        // paragraph after the gap (a trailing / bottom insert), append after the predecessor instead.
        let following: Option<usize> = g.next.map(|nx| live_of[nx]);

        match following {
            Some(mut host) => {
                for (n, &bi) in g.ins.iter().enumerate() {
                    let text = para_text(&b_paras[bi]);
                    if let Some(&mid) = move_id_of.get(&bi) {
                        // Move destination: `w:moveTo` under the source's id (recorded at the source).
                        if text.is_empty() {
                            self.a.suggest_move_split(host, 0, mid, self.author(), self.date(), self.audit())?;
                        } else {
                            self.a.suggest_move_dest(host, 0, &text, mid, self.author(), self.date(), self.audit())?;
                            self.a.suggest_move_split(host, text.chars().count(), mid, self.author(), self.date(), self.audit())?;
                        }
                        shift(live_of, host);
                        host += 1;
                        continue;
                    }
                    let id = if text.is_empty() {
                        self.a.suggest_split(host, 0, self.author(), self.date(), self.audit())?
                    } else {
                        // Prepend the text (id T), then split it off as its own ¶ (id S > T).
                        let tid = self.a.suggest_insertion(host, 0, &text, self.author(), self.date(), self.audit())?;
                        self.a.suggest_split(host, text.chars().count(), self.author(), self.date(), self.audit())?;
                        tid
                    };
                    shift(live_of, host);
                    host += 1;
                    self.record(
                        prev_primary,
                        1_000_000 + (bi as i64) + n as i64,
                        Change::new(id, ChangeKind::ParaInsert, g.prev.unwrap_or(0)).after(text),
                    );
                }
            }
            None => {
                // Nothing follows: append after the preceding paragraph (a true bottom insert). Here
                // the inserted ¶ is on the surviving predecessor and sits at the tail, so split-then-
                // fill has no mark-bleed hazard.
                let mut anchor = live_of[g.prev.expect("no following paragraph implies a preceding one")];
                for (n, &bi) in g.ins.iter().enumerate() {
                    let text = para_text(&b_paras[bi]);
                    let len = self.live_len(anchor)?;
                    if let Some(&mid) = move_id_of.get(&bi) {
                        self.a.suggest_move_split(anchor, len, mid, self.author(), self.date(), self.audit())?;
                        shift(live_of, anchor + 1);
                        anchor += 1;
                        if !text.is_empty() {
                            self.a.suggest_move_dest(anchor, 0, &text, mid, self.author(), self.date(), self.audit())?;
                        }
                        continue;
                    }
                    let sid = self.a.suggest_split(anchor, len, self.author(), self.date(), self.audit())?;
                    shift(live_of, anchor + 1);
                    anchor += 1;
                    let id = if text.is_empty() {
                        sid
                    } else {
                        self.a.suggest_insertion(anchor, 0, &text, self.author(), self.date(), self.audit())?
                    };
                    self.record(
                        prev_primary,
                        1_000_000 + (bi as i64) + n as i64,
                        Change::new(id, ChangeKind::ParaInsert, g.prev.unwrap_or(0)).after(text),
                    );
                }
            }
        }
        Ok(())
    }

    /// Current codepoint length of the live paragraph at `idx`.
    fn live_len(&self, idx: usize) -> Result<usize> {
        let paras = self.a.paragraphs()?;
        Ok(paras.get(idx).map(|p| para_text(p).chars().count()).unwrap_or(0))
    }
}

/// Shift a live-index map: every original paragraph currently at or after `q` moves down by one.
fn shift(live_of: &mut [usize], q: usize) {
    for x in live_of.iter_mut() {
        if *x >= q {
            *x += 1;
        }
    }
}

// ── per-paragraph diff primitives ─────────────────────────────────────────────

/// A within-paragraph operation, in original-A codepoint coordinates.
enum Prim {
    Del { s: usize, e: usize, before: String },
    Ins { at: usize, text: String },
    Replace { s: usize, e: usize, before: String, text: String },
    Fmt { s: usize, e: usize, fmt: RunFormat },
}

impl Prim {
    fn anchor(&self) -> usize {
        match self {
            Prim::Del { s, .. } | Prim::Replace { s, .. } | Prim::Fmt { s, .. } => *s,
            Prim::Ins { at, .. } => *at,
        }
    }
}

/// Word-level inline diff of two paragraph texts into Del / Ins / Replace prims.
fn inline_prims(a_text: &str, b_text: &str, opts: &CompareOptions, out: &mut Vec<Prim>) {
    let a_toks = tokenize(a_text);
    let b_toks = tokenize(b_text);
    // Compare tokens by a *normalized* key so ignore-whitespace / ignore-case diffs don't redline: a
    // whitespace run collapses to one key, and letters lower-case. The emitted text is still the
    // original (an ignored diff simply produces no op, so A's text is kept verbatim).
    let key = |t: &token::Token| -> String {
        if opts.ignore_whitespace && !t.text.is_empty() && t.text.chars().all(char::is_whitespace) {
            return " ".to_string();
        }
        if opts.ignore_case {
            return t.text.to_lowercase();
        }
        t.text.clone()
    };
    let a_keys: Vec<String> = a_toks.iter().map(key).collect();
    let b_keys: Vec<String> = b_toks.iter().map(key).collect();
    let ops = diff::diff_by(a_toks.len(), b_toks.len(), |i, j| a_keys[i] == b_keys[j]);

    let a_chars: Vec<char> = a_text.chars().collect();
    let mut a_cursor = 0usize; // codepoint position in A
    let mut in_gap = false;
    let mut del_start = 0usize;
    let mut ins_text = String::new();

    // Flush a gap into a Del / Ins / Replace prim. Deletion retains its text (so the insertion,
    // anchored at the gap's end, reads after the struck text - Word's deletion-then-insertion order).
    let do_flush = |out: &mut Vec<Prim>, del_start: usize, del_end: usize, ins_text: &str| {
        let del_len = del_end - del_start;
        let before: String = a_chars[del_start..del_end].iter().collect();
        match (del_len > 0, !ins_text.is_empty()) {
            (true, true) => out.push(Prim::Replace { s: del_start, e: del_end, before, text: ins_text.to_string() }),
            (true, false) => out.push(Prim::Del { s: del_start, e: del_end, before }),
            (false, true) => out.push(Prim::Ins { at: del_start, text: ins_text.to_string() }),
            (false, false) => {}
        }
    };

    for op in ops {
        match op {
            diff::Op::Equal(i, _) => {
                if in_gap {
                    do_flush(out, del_start, a_cursor, &ins_text);
                    in_gap = false;
                    ins_text.clear();
                }
                a_cursor = a_toks[i].end;
            }
            diff::Op::Delete(i) => {
                if !in_gap {
                    del_start = a_cursor;
                    ins_text.clear();
                    in_gap = true;
                }
                a_cursor = a_toks[i].end;
            }
            diff::Op::Insert(j) => {
                if !in_gap {
                    del_start = a_cursor;
                    ins_text.clear();
                    in_gap = true;
                }
                ins_text.push_str(&b_toks[j].text);
            }
        }
    }
    if in_gap {
        do_flush(out, del_start, a_cursor, &ins_text);
    }
}

/// Run-format changes (`w:rPrChange`) over retained (textually equal) spans. Compares per-codepoint
/// format keys and coalesces maximal runs where A differs from a *uniform* B target.
fn format_prims(a_para: &Paragraph, b_para: &Paragraph, out: &mut Vec<Prim>) {
    let a_text = para_text(a_para);
    let b_text = para_text(b_para);
    let a_keys = char_fmt_keys(a_para);
    let b_keys = char_fmt_keys(b_para);

    // Re-diff at token level to find retained spans with their B counterparts.
    let a_toks = tokenize(&a_text);
    let b_toks = tokenize(&b_text);
    let ops = diff::diff_by(a_toks.len(), b_toks.len(), |i, j| a_toks[i].text == b_toks[j].text);

    for op in ops {
        if let diff::Op::Equal(i, j) = op {
            let a_range = a_toks[i].start..a_toks[i].end;
            let b_start = b_toks[j].start;
            // Walk the equal token codepoint-by-codepoint, coalescing runs that need the same B key.
            let mut k = 0usize;
            let len = a_range.end - a_range.start;
            while k < len {
                let ak = a_range.start + k;
                let bk = b_start + k;
                if a_keys[ak] != b_keys[bk] {
                    let target = &b_keys[bk];
                    let run_start = ak;
                    let mut kk = k;
                    while kk < len {
                        let a2 = a_range.start + kk;
                        let b2 = b_start + kk;
                        if a_keys[a2] != b_keys[b2] && &b_keys[b2] == target {
                            kk += 1;
                        } else {
                            break;
                        }
                    }
                    let run_end = a_range.start + kk;
                    out.push(Prim::Fmt { s: run_start, e: run_end, fmt: run_format_from(target) });
                    k = kk;
                } else {
                    k += 1;
                }
            }
        }
    }
}

// ── model helpers ─────────────────────────────────────────────────────────────

/// The concatenated text of a paragraph (all runs, tracked or not - consistent on both sides).
fn para_text(p: &Paragraph) -> String {
    p.runs.iter().map(|r| r.text.as_str()).collect()
}

/// Exact-match signature for the block backbone: resolved style **name** + text (run formatting is
/// compared per-pair, not here, so a bold-only change is still caught as an edited pair). The style is
/// keyed by its `w:name` (via `names`), not its raw `w:styleId`, so the same style re-saved under a
/// localized vs. English id (`Brdtext`<->`BodyText`) still yields the same signature - the paragraphs
/// exact-match on the backbone instead of relying on the fuzzy gap-fill to re-pair them. Falls back to
/// the raw id when a style has no name entry.
fn signature(p: &Paragraph, names: &std::collections::HashMap<String, String>) -> String {
    let mut s = p
        .style
        .as_deref()
        .map(|id| names.get(id).map(|n| n.trim().to_ascii_lowercase()).unwrap_or_else(|| id.to_string()))
        .unwrap_or_default();
    s.push('\u{0}');
    for r in &p.runs {
        s.push_str(&r.text);
    }
    s
}

/// The document as a **structural** signature: body items in order, and for a table its grid shape
/// plus every cell's text. This is the oracle's comparison unit - stronger than a flat paragraph-text
/// list, so a wrong grid (a missing / extra row or column, a mis-shaped cell) is caught, not just
/// wrong text. For a table-free document it reduces to one `P|<text>` entry per paragraph.
fn doc_signature(doc: &CollabDoc) -> Result<Vec<String>> {
    let paras = doc.paragraphs()?;
    let text = |i: usize| paras.get(i).map(para_text).unwrap_or_default();
    let mut flat = 0usize;
    let mut sig = Vec::new();
    for item in doc.body() {
        match item {
            BodyItem::Paragraph => {
                sig.push(format!("P|{}", text(flat)));
                flat += 1;
            }
            BodyItem::Table(t) => {
                sig.push(format!("T|{}x{}", t.rows.len(), t.col_widths.len()));
                for (ri, row) in t.rows.iter().enumerate() {
                    for (ci, cell) in row.cells.iter().enumerate() {
                        let mut ct = String::new();
                        for _ in 0..cell.para_count {
                            ct.push_str(&text(flat));
                            ct.push('\u{0}');
                            flat += 1;
                        }
                        sig.push(format!("C|{ri},{ci}|{ct}"));
                    }
                }
            }
        }
    }
    Ok(sig)
}

fn first_mismatch(want: &[String], got: &[String]) -> Option<(usize, String, String)> {
    let n = want.len().max(got.len());
    for i in 0..n {
        let w = want.get(i).cloned().unwrap_or_default();
        let g = got.get(i).cloned().unwrap_or_default();
        if w != g {
            return Some((i, w, g));
        }
    }
    None
}

/// The diffable run-format signature (the fields a `w:rPrChange` records).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FmtKey {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: Option<u16>,
    color: Option<String>,
    font: Option<String>,
    highlight: Option<String>,
    vert_align: Option<String>,
}

impl FmtKey {
    fn of(r: &Run) -> Self {
        Self {
            bold: r.bold,
            italic: r.italic,
            underline: r.underline,
            strike: r.strike,
            size: r.size,
            color: r.color.clone(),
            font: r.font.clone(),
            highlight: r.highlight.clone(),
            vert_align: r.vert_align.clone(),
        }
    }
}

/// Per-codepoint format keys for a paragraph (each run contributes its key for its char length).
fn char_fmt_keys(p: &Paragraph) -> Vec<FmtKey> {
    let mut v = Vec::new();
    for r in &p.runs {
        let key = FmtKey::of(r);
        for _ in 0..r.text.chars().count() {
            v.push(key.clone());
        }
    }
    v
}

/// Build a [`RunFormat`] that sets A's run to the target B formatting. Boolean toggles are always
/// set; optional values are set when present (clearing an inherited value back to "none" is not
/// expressible via this API in v1 - harmless, since a format-only change never alters text and so
/// never affects the oracle).
fn run_format_from(k: &FmtKey) -> RunFormat {
    RunFormat {
        bold: Some(k.bold),
        italic: Some(k.italic),
        underline: Some(k.underline),
        strike: Some(k.strike),
        size: k.size,
        color: k.color.clone(),
        font: k.font.clone(),
        highlight: k.highlight.clone(),
        vert_align: k.vert_align.clone(),
    }
}
