//! Tracked changes on the live model.
//! 
//! Recording a suggestion (insert, delete, format, move) as Peritext marks, walking
//! the document for the regions they cover, and resolving one by accepting or
//! rejecting it.

use super::*;

// ── tracked edits (the agent peer + the document.* MCP path) ─────────────────

/// The text container of the `idx`-th paragraph (document order).
pub(crate) fn nth_block_text(doc: &LoroDoc, idx: usize) -> Result<LoroText> {
    let r = block_ref_at(doc, idx).ok_or_else(|| anyhow!("no block at index {idx}"))?;
    block_ref_text(doc, &r).ok_or_else(|| anyhow!("block {idx} has no text container"))
}

/// Create a loro [`Cursor`] anchored at codepoint `off` in body block `para`, biased to `side`.
/// The cursor binds to the block's text container + the op-id at `off` (not the integer index), so
/// it survives concurrent edits. Caller wraps it in a [`crate::Anchor`].
pub(crate) fn cursor_at(doc: &LoroDoc, para: usize, off: usize, side: Side) -> Result<Cursor> {
    let text = nth_block_text(doc, para)?;
    text.get_cursor(off, side)
        .ok_or_else(|| anyhow!("cannot anchor at block {para} codepoint {off}"))
}

/// Resolve a [`Cursor`] to `(para, off, repinned)` in this document, or `None` when its content is
/// gone (the whole block was deleted -> loro errors, or the container is no longer a live body block).
/// `repinned` is true when the exact anchored character was deleted and loro re-pinned `off` to a
/// neighbour (loro returns a `PosQueryResult.update`): the position is a best-effort neighbour, not the
/// original spot, so the caller should treat it as a *moved* reference, not a live one.
pub(crate) fn resolve_cursor(doc: &LoroDoc, cursor: &Cursor) -> Option<(usize, usize, bool)> {
    let q = doc.get_cursor_pos(cursor).ok()?; // Err => container deleted / id not found
    let para = block_index_of_container(doc, &cursor.container)?;
    Some((para, q.current.pos, q.update.is_some()))
}

/// The flat-order index of the block whose text container is `cid`, or `None` if no live body block
/// owns it (e.g. the block was deleted). Resolves a **cell paragraph**'s durable node id (the cell
/// paragraph has no tree node of its own; its `text` container id is its replicated identity).
pub(crate) fn block_index_of_container(doc: &LoroDoc, cid: &ContainerID) -> Option<usize> {
    block_seq(doc)
        .iter()
        .position(|r| block_ref_text(doc, r).is_some_and(|t| t.id() == *cid))
}

/// The flat-order index of the **top-level** block node `id`, or `None` if it's not a live top-level
/// paragraph (deleted / unknown / a table node). A cell paragraph isn't a tree node, so it is addressed
/// by its text container id instead (see [`block_index_of_container`]).
pub(crate) fn block_index_of(doc: &LoroDoc, id: TreeID) -> Option<usize> {
    block_seq(doc).iter().position(|r| matches!(r, BlockRef::Top(t) if *t == id))
}

/// The largest tracked-change id in the document (0 if none); new revisions take `max + 1`.
///
/// NOTE: in OOXML, revision `w:id` shares an id space with bookmarks and comments. Once those are
/// modeled they must feed this same allocator - keep a single pool.
pub fn max_revision_id(doc: &LoroDoc) -> Result<u64> {
    let mut max = 0;
    for para in read_paragraphs(doc)? {
        for run in &para.runs {
            if let Some(t) = &run.track {
                max = max.max(t.id);
            }
            if let Some(fc) = &run.fmt_change {
                max = max.max(fc.id);
            }
        }
        if let Some(c) = &para.prop_change {
            max = max.max(c.id);
        }
        if let Some(m) = &para.mark_change {
            max = max.max(m.id);
        }
    }
    // Comments share OOXML's `w:id` pool with revisions/bookmarks - feed them into the allocator.
    for c in read_comments(doc) {
        max = max.max(c.id);
    }
    Ok(max)
}

/// Every distinct revision id carried by table row / cell structure changes in `body`, in document
/// order (deduped). Table-structure changes live in the in-memory body, not the loro op log, so the id
/// allocator + accept-all / navigation must fold these in alongside the run/paragraph/comment ids.
pub fn table_change_ids(body: &[BodyItem]) -> Vec<u64> {
    let mut ids = Vec::new();
    let push = |id: u64, ids: &mut Vec<u64>| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };
    for item in body {
        if let BodyItem::Table(t) = item {
            if let Some(pc) = &t.prop_change {
                push(pc.id, &mut ids);
            }
            for row in &t.rows {
                for id in row.change.iter().chain(row.cells.iter().filter_map(|c| c.change.as_ref())).map(|c| c.id) {
                    push(id, &mut ids);
                }
                if let Some(pc) = &row.prop_change {
                    push(pc.id, &mut ids);
                }
                for pc in row.cells.iter().filter_map(|c| c.prop_change.as_ref()) {
                    push(pc.id, &mut ids);
                }
            }
        }
    }
    ids
}

/// Insert `text` at codepoint `pos` in paragraph `idx` and mark it a tracked insertion. Caller
/// allocates the id (see [`max_revision_id`]) and commits.
pub fn suggest_insertion(doc: &LoroDoc, idx: usize, pos: usize, text: &str, track: &Track)
    -> Result<()>
{
    let body = nth_block_text(doc, idx)?;
    body.insert(pos, text)?;
    let n = text.chars().count();
    mark_track(&body, pos..pos + n, track)
}

/// Mark codepoint `range` in paragraph `idx` as a tracked deletion (text is retained, mirroring
/// `w:delText`, so the suggestion can be rejected). Caller commits.
pub fn suggest_deletion(doc: &LoroDoc, idx: usize, range: Range<usize>, track: &Track)
    -> Result<()>
{
    let body = nth_block_text(doc, idx)?;
    mark_track(&body, range, track)
}

/// Apply a [`RunFormat`] over `range` in paragraph `idx` as a tracked run-property change
/// (`w:rPrChange`): each homogeneous sub-range that is *plain* (not already an insertion/deletion or
/// a prior format change) gets the new formatting marks + an `rfmt` mark recording its old props +
/// `author`/`date`/`id`. Insertions get the formatting directly (it's part of the pending insertion,
/// not a separate rPrChange); runs already carrying an `rfmt` keep their original old props. Caller
/// allocates `id` (see [`max_revision_id`]) and commits.
pub fn suggest_format(
    doc: &LoroDoc,
    idx: usize,
    range: Range<usize>,
    fmt: &RunFormat,
    author: &str,
    date: &str,
    id: u64,
) -> Result<()> {
    // Capture per-run old props + eligibility BEFORE mutating.
    let paras = read_paragraphs(doc)?;
    let para = paras.get(idx).ok_or_else(|| anyhow!("no block at index {idx}"))?;
    let mut targets: Vec<(usize, usize, RunProps)> = Vec::new();
    let mut pos = 0usize;
    for run in &para.runs {
        let n = run.text.chars().count();
        let (rs, re) = (pos, pos + n);
        let (s, e) = (rs.max(range.start), re.min(range.end));
        if s < e && run.track.is_none() && run.fmt_change.is_none() {
            targets.push((s, e, RunProps::of(run)));
        }
        pos = re;
    }
    // Apply the new formatting over the whole range (insertions get it directly; that's correct).
    apply_run_format(doc, idx, range, fmt)?;
    // Record each eligible sub-range's old props as an `rfmt` mark (the CRDT form of w:rPrChange).
    let text = nth_block_text(doc, idx)?;
    for (s, e, old) in targets {
        let fc = FormatChange { author: author.into(), date: date.into(), id, old };
        mark_fmt_change(&text, s..e, &fc)?;
    }
    Ok(())
}

/// Slice the runs of `runs` over codepoint `[start, end)` into fresh [`Run`]s that carry only the
/// formatting - a moved copy starts clean (no tracked-change marks, no comment anchors of its own).
fn slice_runs(runs: &[Run], start: usize, end: usize) -> Vec<Run> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for run in runs {
        let n = run.text.chars().count();
        let (rs, re) = (pos, pos + n);
        let (s, e) = (rs.max(start), re.min(end));
        if s < e {
            let text: String = run.text.chars().skip(s - rs).take(e - s).collect();
            out.push(Run {
                text,
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
                char_style: run.char_style.clone(),
                shading: run.shading.clone(),
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
            });
        }
        pos = re;
    }
    out
}

/// Suggest a move: mark codepoint `from_range` in paragraph `from_idx` as the source half of a move
/// (`w:moveFrom`, text retained like a deletion) and insert a formatting-preserving copy at codepoint
/// `to_pos` in paragraph `to_idx` as the destination half (`w:moveTo`). Both halves share `id`, so
/// accepting / rejecting either resolves the whole move. The destination must lie outside the source
/// range when both are in the same paragraph. Caller allocates `id` (see [`max_revision_id`]) and
/// commits.
#[allow(clippy::too_many_arguments)]
pub fn suggest_move(
    doc: &LoroDoc,
    from_idx: usize,
    from_range: Range<usize>,
    to_idx: usize,
    to_pos: usize,
    author: &str,
    date: &str,
    id: u64,
) -> Result<()> {
    if from_idx == to_idx && to_pos > from_range.start && to_pos < from_range.end {
        return Err(anyhow!("cannot move a range into itself"));
    }
    // Capture the moved runs (formatting preserved) before mutating anything.
    let paras = read_paragraphs(doc)?;
    let src = paras.get(from_idx).ok_or_else(|| anyhow!("no block at index {from_idx}"))?;
    let mut moved = slice_runs(&src.runs, from_range.start, from_range.end);
    let mv_to = Track { kind: TrackKind::MoveTo, author: author.into(), date: date.into(), id };
    for r in &mut moved {
        r.track = Some(mv_to.clone());
    }
    // Mark the source as moveFrom first - marks don't change text length, so the destination offset
    // (computed in the pre-move coordinates) stays valid even when moving within one paragraph.
    let mv_from = Track { kind: TrackKind::MoveFrom, author: author.into(), date: date.into(), id };
    let from_text = nth_block_text(doc, from_idx)?;
    mark_track(&from_text, from_range, &mv_from)?;
    // Insert the moved copy at the destination (append_runs applies formatting + the moveTo mark).
    let to_text = nth_block_text(doc, to_idx)?;
    append_runs(&to_text, to_pos, &moved)?;
    Ok(())
}

/// Suggest a **multi-paragraph** move: source `(sp,so)..(ep,eo)` to destination `(tp, to_pos)`, all
/// under one revision `id`. The source runs are marked `w:moveFrom` (retained) with the ¶s between the
/// spanned paragraphs marked moveFrom; the moved content is re-inserted at the destination as `w:moveTo`
/// runs, recreating its internal paragraph breaks as moveTo ¶s. So accepting removes the source (and
/// merges it) while keeping the destination; rejecting removes the destination (merging it) and
/// restores the source. Returns the number of NEW paragraphs created at the destination
/// (= source paragraph count - 1), so the caller can sync its body structure. The destination must lie
/// outside the source span; both must be top-level (the caller enforces). Caller allocates `id` + commits.
#[allow(clippy::too_many_arguments)]
pub fn suggest_move_multi(
    doc: &LoroDoc,
    sp: usize,
    so: usize,
    ep: usize,
    eo: usize,
    tp: usize,
    to_pos: usize,
    author: &str,
    date: &str,
    id: u64,
) -> Result<usize> {
    let mv_from = Track { kind: TrackKind::MoveFrom, author: author.into(), date: date.into(), id };
    let mv_to = Track { kind: TrackKind::MoveTo, author: author.into(), date: date.into(), id };

    // Capture the moved runs per source paragraph BEFORE mutating (slice_runs returns clean copies).
    let paras = read_paragraphs(doc)?;
    let len = |p: usize| -> usize {
        paras.get(p).map(|x| x.runs.iter().map(|r| r.text.chars().count()).sum()).unwrap_or(0)
    };
    let k = ep - sp + 1;
    let mut moved: Vec<Vec<Run>> = Vec::with_capacity(k);
    moved.push(slice_runs(&paras[sp].runs, so, len(sp))); // start tail
    #[allow(clippy::needless_range_loop)]
    for p in (sp + 1)..ep {
        moved.push(slice_runs(&paras[p].runs, 0, len(p))); // whole middle paragraphs
    }
    moved.push(slice_runs(&paras[ep].runs, 0, eo)); // end head

    // Mark the SOURCE moveFrom: each spanned slice + the ¶ of every paragraph except the last (marks
    // change no lengths/counts, so the destination indices below stay valid).
    let sl = len(sp);
    if so < sl {
        mark_track(&nth_block_text(doc, sp)?, so..sl, &mv_from)?;
    }
    for p in (sp + 1)..ep {
        let l = len(p);
        if l > 0 {
            mark_track(&nth_block_text(doc, p)?, 0..l, &mv_from)?;
        }
    }
    if eo > 0 {
        mark_track(&nth_block_text(doc, ep)?, 0..eo, &mv_from)?;
    }
    for p in sp..ep {
        set_para_mark(doc, p, &mv_from)?;
    }

    // DEST: split at to_pos (the new break is a moveTo ¶), then splice the moved paragraphs in - the
    // first merges onto the head, strictly-internal ones become their own moveTo paragraphs, the last
    // merges onto the tail. `append_runs` applies each run's `track`, so setting it to moveTo suffices.
    let tagged = |src: &[Run]| -> Vec<Run> {
        let mut v = src.to_vec();
        for r in &mut v {
            r.track = Some(mv_to.clone());
        }
        v
    };
    split_paragraph(doc, tp, to_pos)?;
    set_para_mark(doc, tp, &mv_to)?;
    append_runs(&nth_block_text(doc, tp)?, to_pos, &tagged(&moved[0]))?;
    #[allow(clippy::needless_range_loop)]
    for i in 1..(k - 1) {
        let idx = tp + i;
        insert_empty_paragraph(doc, idx)?;
        append_runs(&nth_block_text(doc, idx)?, 0, &tagged(&moved[i]))?;
        set_para_mark(doc, idx, &mv_to)?;
    }
    append_runs(&nth_block_text(doc, tp + (k - 1))?, 0, &tagged(&moved[k - 1]))?;
    Ok(k - 1)
}

/// Set every formatting mark over `range` to match `p` (mark when set, unmark when not) - used to
/// restore a run's before-state when a tracked formatting change is rejected.
fn apply_props(text: &LoroText, range: Range<usize>, p: &RunProps) -> Result<()> {
    if p.bold { text.mark(range.clone(), "b", true)?; } else { text.unmark(range.clone(), "b")?; }
    if p.italic { text.mark(range.clone(), "i", true)?; } else { text.unmark(range.clone(), "i")?; }
    if p.underline { text.mark(range.clone(), "u", true)?; } else { text.unmark(range.clone(), "u")?; }
    if p.strike { text.mark(range.clone(), "strike", true)?; } else { text.unmark(range.clone(), "strike")?; }
    match p.size {
        Some(s) => text.mark(range.clone(), "sz", s as i64)?,
        None => text.unmark(range.clone(), "sz")?,
    }
    match &p.color {
        Some(c) => text.mark(range.clone(), "color", c.as_str())?,
        None => text.unmark(range.clone(), "color")?,
    }
    match &p.font {
        Some(f) => text.mark(range.clone(), "font", f.as_str())?,
        None => text.unmark(range.clone(), "font")?,
    }
    match &p.highlight {
        Some(h) => text.mark(range.clone(), "hl", h.as_str())?,
        None => text.unmark(range.clone(), "hl")?,
    }
    match &p.vert_align {
        Some(v) => text.mark(range.clone(), "va", v.as_str())?,
        None => text.unmark(range.clone(), "va")?,
    }
    match &p.lang {
        Some(l) => text.mark(range.clone(), "lang", l.as_str())?,
        None => text.unmark(range.clone(), "lang")?,
    }
    Ok(())
}

// ── resolve tracked changes on the live model (accept / reject) ──────────────
//
// Mirrors `scriptor_ooxml::resolve`'s `op_for`, but on the loro CRDT (so the app resolves changes
// live, not only as a file transform): accept ins = drop the mark (keep text); accept del = remove
// the text; reject ins = remove the text; reject del = drop the mark (keep text).

/// A contiguous tracked region within a paragraph: the change, its text, and its codepoint range.
/// Adjacent runs that share one revision id (e.g. an insertion split by inner bold) merge into one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedRegion {
    pub track: Track,
    pub text: String,
    /// Codepoint range within the paragraph.
    pub start: usize,
    pub end: usize,
}

/// The contiguous tracked regions of a paragraph, in order (adjacent same-id runs coalesced). A run
/// contributes its insertion/deletion (`track`) or, failing that, its formatting change
/// (`fmt_change`, surfaced as a synthesized `Fmt` region for navigation / tooltip / resolution).
fn paragraph_regions(para: &Paragraph) -> Vec<TrackedRegion> {
    let mut out: Vec<TrackedRegion> = Vec::new();
    let mut pos = 0usize;
    for run in &para.runs {
        let n = run.text.chars().count();
        let aspect: Option<Track> = match (&run.track, &run.fmt_change) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(fc)) => Some(Track {
                kind: TrackKind::Fmt,
                author: fc.author.clone(),
                date: fc.date.clone(),
                id: fc.id,
            }),
            (None, None) => None,
        };
        if let Some(t) = aspect {
            if let Some(last) = out.last_mut()
                && last.track.id == t.id && last.track.kind == t.kind && last.end == pos {
                    last.end = pos + n;
                    last.text.push_str(&run.text);
                    pos += n;
                    continue;
                }
            out.push(TrackedRegion { track: t, text: run.text.clone(), start: pos, end: pos + n });
        }
        pos += n;
    }
    out
}

/// Every distinct revision id in the document, in first-appearance (document) order.
pub fn all_revision_ids(doc: &LoroDoc) -> Result<Vec<u64>> {
    let mut ids: Vec<u64> = Vec::new();
    for para in read_paragraphs(doc)? {
        for r in paragraph_regions(&para) {
            if !ids.contains(&r.track.id) {
                ids.push(r.track.id);
            }
        }
        if let Some(c) = &para.prop_change
            && !ids.contains(&c.id) {
                ids.push(c.id);
            }
        if let Some(m) = &para.mark_change
            && !ids.contains(&m.id) {
                ids.push(m.id);
            }
    }
    Ok(ids)
}

/// Accept (`accept=true`) or reject a tracked change by revision id, across the whole document.
/// Returns whether anything was resolved. Caller commits. See the module comment for the semantics.
pub fn resolve_revision(doc: &LoroDoc, id: u64, accept: bool) -> Result<bool> {
    let paras = read_paragraphs(doc)?;
    let mut changed = false;
    for (pi, para) in paras.iter().enumerate() {
        // Paragraph-property change (pPrChange): paragraph-level, resolved on the block meta.
        if let Some(c) = &para.prop_change
            && c.id == id {
                resolve_para_format(doc, pi, c, accept)?;
                changed = true;
                continue;
            }
        // Format changes (rPrChange) resolve per-run, since the before-props vary run to run.
        if para.runs.iter().any(|r| r.fmt_change.as_ref().is_some_and(|f| f.id == id)) {
            resolve_format(doc, pi, para, id, accept)?;
            changed = true;
            continue;
        }
        // Ins/del + move halves: a move's two halves share one id, so a single paragraph can hold
        // more than one matching region (a move within one paragraph). Resolve every match, highest
        // offset first, so an earlier removal can't invalidate a later region's offsets.
        let mut regions: Vec<TrackedRegion> =
            paragraph_regions(para).into_iter().filter(|r| r.track.id == id).collect();
        if regions.is_empty() {
            continue;
        }
        regions.sort_by_key(|r| std::cmp::Reverse(r.start));
        let text = nth_block_text(doc, pi)?;
        for region in regions {
            // Source halves (Del / MoveFrom) drop their text on accept; destination halves (Ins /
            // MoveTo) drop their text on reject. The other resolution just removes the mark.
            let remove = matches!(
                (region.track.kind, accept),
                (TrackKind::Ins, false)
                    | (TrackKind::MoveTo, false)
                    | (TrackKind::Del, true)
                    | (TrackKind::MoveFrom, true)
            );
            if remove {
                text.delete(region.start, region.end - region.start)?;
            } else {
                text.unmark(region.start..region.end, region.track.kind.mark_key())?;
            }
        }
        changed = true;
    }
    Ok(changed)
}

/// Resolve a tracked run-property change by id within one paragraph: accept drops the `rfmt` mark
/// (keeping the new formatting); reject restores each run's before-props, then drops `rfmt`. Marks
/// don't change text length, so per-run order is irrelevant.
fn resolve_format(doc: &LoroDoc, pi: usize, para: &Paragraph, id: u64, accept: bool) -> Result<()> {
    let text = nth_block_text(doc, pi)?;
    let mut pos = 0usize;
    for run in &para.runs {
        let n = run.text.chars().count();
        let (s, e) = (pos, pos + n);
        if let Some(fc) = &run.fmt_change
            && fc.id == id {
                if !accept {
                    apply_props(&text, s..e, &fc.old)?;
                }
                text.unmark(s..e, "rfmt")?;
            }
        pos = e;
    }
    Ok(())
}

/// Resolve a tracked paragraph-property change: accept drops the `ppc` record (keeping the new
/// props); reject restores the old style + props, then drops `ppc`.
fn resolve_para_format(doc: &LoroDoc, pi: usize, c: &ParaPropChange, accept: bool) -> Result<()> {
    let meta = block_meta_at(doc, pi)?;
    if !accept {
        match &c.old_style {
            Some(s) => meta.insert("style", s.as_str())?,
            None => meta.delete("style")?,
        }
        set_para_props_exact(&meta, &c.old)?;
    }
    meta.delete("ppc")?;
    Ok(())
}

/// The caret `(para, start)` of the next (`forward`) / previous tracked region relative to
/// `(from_para, from_off)`. Wraps around the document when there is none in that direction (so
/// "next change" cycles, like Word). `None` only when the document has no tracked changes.
pub fn adjacent_change(
    doc: &LoroDoc,
    from_para: usize,
    from_off: usize,
    forward: bool,
) -> Result<Option<(usize, usize)>> {
    let regions = change_carets(doc)?;
    if regions.is_empty() {
        return Ok(None);
    }
    let cur = (from_para, from_off);
    let pick = if forward {
        regions.iter().find(|&&p| p > cur).or_else(|| regions.first())
    } else {
        regions.iter().rev().find(|&&p| p < cur).or_else(|| regions.last())
    };
    Ok(pick.copied())
}

/// Every tracked-change caret position in the document, sorted + de-duplicated: each run region's
/// start, plus a paragraph-property change (paragraph start) and a paragraph-mark change (paragraph
/// end). Drives Next / Previous navigation (here, and across stories at the wasm layer).
pub fn change_carets(doc: &LoroDoc) -> Result<Vec<(usize, usize)>> {
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for (pi, para) in read_paragraphs(doc)?.iter().enumerate() {
        for r in paragraph_regions(para) {
            regions.push((pi, r.start));
        }
        if para.prop_change.is_some() {
            regions.push((pi, 0)); // a paragraph-property change navigates to the paragraph start
        }
        if para.mark_change.is_some() {
            // a paragraph-mark change navigates to the paragraph end (the ¶)
            let len = para.runs.iter().map(|r| r.text.chars().count()).sum();
            regions.push((pi, len));
        }
    }
    regions.sort_unstable();
    regions.dedup();
    Ok(regions)
}

/// The tracked region under codepoint `off` in paragraph `idx` (the run containing the caret, or the
/// region ending exactly at it), for the hover tooltip + click popup. `None` when not over a change.
pub fn track_at(doc: &LoroDoc, idx: usize, off: usize) -> Result<Option<TrackedRegion>> {
    let paras = read_paragraphs(doc)?;
    let Some(para) = paras.get(idx) else { return Ok(None) };
    let regions = paragraph_regions(para);
    if let Some(r) = regions.iter().find(|r| off >= r.start && off < r.end) {
        return Ok(Some(r.clone()));
    }
    // At the paragraph end (the ¶): a paragraph-mark revision wins over a region ending there.
    let para_len: usize = para.runs.iter().map(|r| r.text.chars().count()).sum();
    if off == para_len
        && let Some(m) = &para.mark_change {
            return Ok(Some(TrackedRegion {
                track: m.clone(),
                text: "¶".to_string(),
                start: para_len,
                end: para_len,
            }));
        }
    if let Some(r) = regions.iter().find(|r| off == r.end) {
        return Ok(Some(r.clone()));
    }
    // No run-level change at the caret: fall back to a paragraph-property change (whole paragraph).
    if let Some(c) = &para.prop_change {
        let text: String = para.runs.iter().map(|r| r.text.as_str()).collect();
        let end = text.chars().count();
        return Ok(Some(TrackedRegion {
            track: Track {
                kind: TrackKind::Fmt,
                author: c.author.clone(),
                date: c.date.clone(),
                id: c.id,
            },
            text,
            start: 0,
            end,
        }));
    }
    Ok(None)
}

/// Whether codepoint range `[start, end)` in paragraph `idx` lies entirely within tracked insertions
/// authored by `author`. Word removes such text outright on delete (it's an un-accepted insertion of
/// your own), rather than stacking a `w:del` on top of a `w:ins`. Empty / non-overlapping -> false.
pub fn range_is_own_insertion(
    doc: &LoroDoc,
    idx: usize,
    start: usize,
    end: usize,
    author: &str,
) -> Result<bool> {
    if end <= start {
        return Ok(false);
    }
    let paras = read_paragraphs(doc)?;
    let Some(para) = paras.get(idx) else { return Ok(false) };
    let mut pos = 0usize;
    let mut covered = false;
    for run in &para.runs {
        let n = run.text.chars().count();
        let (rs, re) = (pos, pos + n);
        if rs < end && re > start {
            // This run overlaps the delete range; it must be your own tracked insertion.
            match &run.track {
                Some(t) if t.kind == TrackKind::Ins && t.author == author => covered = true,
                _ => return Ok(false),
            }
        }
        pos = re;
    }
    Ok(covered)
}

/// Insert `text` at codepoint `pos` in paragraph `idx` **directly** (no revision mark). Caller commits.
pub fn insert_text(doc: &LoroDoc, idx: usize, pos: usize, text: &str) -> Result<()> {
    nth_block_text(doc, idx)?.insert(pos, text)?;
    Ok(())
}

/// Delete codepoint `range` in paragraph `idx` **directly** (removes the text). Caller commits.
pub fn delete_text(doc: &LoroDoc, idx: usize, range: Range<usize>) -> Result<()> {
    nth_block_text(doc, idx)?.delete(range.start, range.end - range.start)?;
    Ok(())
}
