//! Accepting and rejecting tracked changes.
//! 
//! Resolving a revision means removing its marks and, for a deletion, the text under
//! them; rejecting means the reverse. Table structure changes resolve through the
//! grid rather than the text, so a rejected inserted row disappears entirely.

use crate::*;

impl CollabDoc {
    /// Accept a tracked change by revision id (insertion -> drop the mark; deletion -> remove the
    /// text; paragraph marks may merge paragraphs). Returns whether it resolved anything.
    pub fn accept_revision(&self, id: u64, audit: &str) -> Result<bool> {
        let changed = self.resolve_one(id, true)?;
        if changed {
            self.doc.set_next_commit_message(audit);
            self.doc.commit();
        }
        Ok(changed)
    }

    /// Reject a tracked change by revision id (insertion -> remove the text; deletion -> drop the
    /// mark). Returns whether it resolved anything.
    pub fn reject_revision(&self, id: u64, audit: &str) -> Result<bool> {
        let changed = self.resolve_one(id, false)?;
        if changed {
            self.doc.set_next_commit_message(audit);
            self.doc.commit();
        }
        Ok(changed)
    }

    /// Resolve a single revision by id without committing. Text / formatting changes go through
    /// [`model::resolve_revision`]; paragraph-mark revisions are handled here because accepting a
    /// deletion (or rejecting an insertion) removes the ¶ - merging two paragraphs, which must also
    /// update the `body` structure. A multi-paragraph deletion shares one id across **both** run-region
    /// deletions and **several** ¶ marks, so run regions resolve first, then every matching ¶ mark is
    /// resolved highest-index-first (a merge can only shift indices above it, never an as-yet-unresolved
    /// lower mark).
    fn resolve_one(&self, id: u64, accept: bool) -> Result<bool> {
        // 1. Run-level / format / move regions (resolves every matching run across all paragraphs).
        let mut changed = model::resolve_revision(&self.doc, id, accept)?;
        // 2. Paragraph-mark revisions with this id (one for a tracked Enter/Backspace; many for a
        //    multi-paragraph deletion). Resolve descending so a merge never moves a pending mark.
        loop {
            let mark = self.paragraphs()?.iter().enumerate().rev().find_map(|(pi, p)| {
                p.mark_change.as_ref().filter(|m| m.id == id).map(|m| (pi, m.kind))
            });
            let Some((pi, kind)) = mark else { break };
            model::clear_para_mark(&self.doc, pi)?;
            // The ¶ is effectively removed (the paragraphs merge) when an *inserted* ¶ is rejected or a
            // *deleted* ¶ is accepted. A move's ¶s follow the same rule: moveTo (a ¶ the move created at
            // the destination) merges on reject, moveFrom (a ¶ inside the moved-away source) merges on
            // accept.
            let merge = matches!(
                (kind, accept),
                (TrackKind::Ins, false)
                    | (TrackKind::MoveTo, false)
                    | (TrackKind::Del, true)
                    | (TrackKind::MoveFrom, true)
            );
            if merge && pi + 1 < self.paragraphs()?.len() {
                // Carry the merged-away paragraph's own ¶ mark onto the survivor. Without this a
                // *chain* of tracked ¶s (e.g. several inserted paragraphs resolved one id at a time)
                // loses every mark but the first when its paragraph is joined away, stranding an
                // empty paragraph. The carried mark has a different id, so it is left for its own
                // later resolve pass, not re-found in this loop.
                let carried =
                    self.paragraphs()?.get(pi + 1).and_then(|p| p.mark_change.clone());
                model::join_paragraph(&self.doc, pi + 1)?;
                if let Some(m) = carried {
                    model::set_para_mark(&self.doc, pi, &m)?;
                }
            }
            changed = true;
        }
        if changed {
            return Ok(true);
        }
        // Last: a tracked table-structure revision (row / column ins-del) lives in the in-memory body.
        self.resolve_table_change(id, accept)
    }

    /// Resolve a tracked table-structure revision by id (a row's `w:trPr/ins|del`, or a column's
    /// per-cell `w:cellIns|cellDel` sharing one id). Accepting an insertion or rejecting a deletion
    /// keeps the row/column and drops the mark; the opposite removes it (and its paragraphs). Does not
    /// commit. Returns whether anything resolved.
    fn resolve_table_change(&self, id: u64, accept: bool) -> Result<bool> {
        for node in model::body_nodes(&self.doc) {
            let model::BodyNode::Table(tnode) = node else { continue };
            let grid = model::open_table_grid(&self.doc, tnode)?;
            let row_ids = grid.row_ids()?;
            let col_ids = grid.col_ids()?;

            // A tracked ROW revision (`w:trPr/ins|del`): keep (drop the mark) on Ins-accept / Del-reject,
            // else remove the row (and the table if it empties).
            if let Some(row_pos) =
                row_ids.iter().position(|r| grid.row_change(r).ok().flatten().is_some_and(|c| c.id == id))
            {
                let kind = grid.row_change(&row_ids[row_pos])?.expect("row change present").kind;
                if matches!((kind, accept), (TrackKind::Ins, false) | (TrackKind::Del, true)) {
                    self.remove_grid_row(tnode, &grid, row_pos)?;
                } else {
                    grid.set_row_change(&row_ids[row_pos], None)?;
                }
                return Ok(true);
            }

            // A tracked COLUMN revision (per-cell `w:cellIns|cellDel`, one shared id across the column).
            let col_hit = col_ids.iter().enumerate().find_map(|(ci, c)| {
                row_ids
                    .iter()
                    .find_map(|r| grid.cell_change(r, c).ok().flatten().filter(|ch| ch.id == id).map(|ch| ch.kind))
                    .map(|kind| (ci, kind))
            });
            if let Some((col_pos, kind)) = col_hit {
                if matches!((kind, accept), (TrackKind::Ins, false) | (TrackKind::Del, true)) {
                    self.remove_grid_column(tnode, &grid, col_pos)?;
                } else {
                    let cid = &col_ids[col_pos];
                    for r in &row_ids {
                        if grid.cell_change(r, cid)?.is_some_and(|ch| ch.id == id) {
                            grid.set_cell_change(r, cid, None)?;
                        }
                    }
                }
                return Ok(true);
            }

            // A tracked table-PROPERTY revision (`tblPrChange` / `trPrChange` / `tcPrChange`): accept
            // keeps the new props (drop the record); reject restores the old snapshot first.
            if let Some(pc) = grid.table_prop_change()?.filter(|pc| pc.id == id) {
                if !accept {
                    grid.restore_table_props(&pc.old)?;
                }
                grid.set_table_prop_change(None)?;
                return Ok(true);
            }
            for r in &row_ids {
                if let Some(pc) = grid.row_prop_change(r)?.filter(|pc| pc.id == id) {
                    if !accept {
                        grid.restore_row_props(r, &pc.old)?;
                    }
                    grid.set_row_prop_change(r, None)?;
                    return Ok(true);
                }
                for c in &col_ids {
                    if let Some(pc) = grid.cell_prop_change(r, c)?.filter(|pc| pc.id == id) {
                        if !accept {
                            grid.restore_cell_props(r, c, &pc.old)?;
                        }
                        grid.set_cell_prop_change(r, c, None)?;
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Accept every tracked change in the document. Returns the count resolved (one commit).
    pub fn accept_all(&self, audit: &str) -> Result<usize> {
        self.resolve_all(true, audit)
    }

    /// Reject every tracked change in the document. Returns the count resolved (one commit).
    pub fn reject_all(&self, audit: &str) -> Result<usize> {
        self.resolve_all(false, audit)
    }

    fn resolve_all(&self, accept: bool, audit: &str) -> Result<usize> {
        let mut ids = model::all_revision_ids(&self.doc)?;
        // Table-structure changes ride in the grid containers, surfaced via the derived body - fold them in.
        for id in model::table_change_ids(&self.body()) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let mut n = 0usize;
        // Each `resolve_one` re-materializes the tree, so resolving id-by-id without an intermediate
        // commit stays correct (a delete/merge in one paragraph can't shift another's offsets).
        for id in ids {
            if self.resolve_one(id, accept)? {
                n += 1;
            }
        }
        if n > 0 {
            self.doc.set_next_commit_message(audit);
            self.doc.commit();
        }
        Ok(n)
    }

    /// The caret `(para, off)` of the next tracked change after `(para, off)` (wraps), or `None` when
    /// there are no tracked changes. For "move to next change".
    pub fn next_change(&self, para: usize, off: usize) -> Result<Option<(usize, usize)>> {
        model::adjacent_change(&self.doc, para, off, true)
    }

    /// The caret of the previous tracked change before `(para, off)` (wraps), or `None`.
    pub fn prev_change(&self, para: usize, off: usize) -> Result<Option<(usize, usize)>> {
        model::adjacent_change(&self.doc, para, off, false)
    }

    /// Every tracked-change caret position, sorted - lets the caller run Next / Previous across
    /// multiple stories (body + header + footer) by merging each story's list. Includes table-structure
    /// changes (each navigates to its row / column's first cell paragraph).
    pub fn change_carets(&self) -> Result<Vec<(usize, usize)>> {
        let mut carets = model::change_carets(&self.doc)?;
        for tc in self.table_changes() {
            carets.push((tc.para, 0));
        }
        carets.sort_unstable();
        carets.dedup();
        Ok(carets)
    }

    /// Every tracked table change - structural (row / column ins-del) and property
    /// (`w:tblPrChange` / `w:trPrChange` / `w:tcPrChange`) - one entry per distinct revision id (a
    /// column's cells share one), in document order, for navigation + the reviewing pane.
    pub fn table_changes(&self) -> Vec<TableChange> {
        let body = self.body();
        let mut out: Vec<TableChange> = Vec::new();
        let mut seen: Vec<u64> = Vec::new();
        let push = |out: &mut Vec<TableChange>,
                        seen: &mut Vec<u64>,
                        id: u64,
                        kind: TrackKind,
                        is_row: bool,
                        prop_level: Option<TablePropLevel>,
                        author: &str,
                        date: &str,
                        para: usize| {
            if !seen.contains(&id) {
                seen.push(id);
                out.push(TableChange {
                    id,
                    kind,
                    is_row,
                    prop_level,
                    author: author.into(),
                    date: date.into(),
                    para,
                });
            }
        };
        for (item, it) in body.iter().enumerate() {
            let model::BodyItem::Table(t) = it else { continue };
            if let Some(pc) = &t.prop_change {
                let para = cell_flat_start(&body, item, 0, 0);
                push(&mut out, &mut seen, pc.id, TrackKind::Fmt, false,
                     Some(TablePropLevel::Table), &pc.author, &pc.date, para);
            }
            for (ri, row) in t.rows.iter().enumerate() {
                if let Some(c) = &row.change {
                    let para = cell_flat_start(&body, item, ri, 0);
                    push(&mut out, &mut seen, c.id, c.kind, true, None, &c.author, &c.date, para);
                }
                if let Some(pc) = &row.prop_change {
                    let para = cell_flat_start(&body, item, ri, 0);
                    push(&mut out, &mut seen, pc.id, TrackKind::Fmt, true,
                         Some(TablePropLevel::Row), &pc.author, &pc.date, para);
                }
                for (ci, cell) in row.cells.iter().enumerate() {
                    if let Some(c) = &cell.change {
                        let para = cell_flat_start(&body, item, ri, ci);
                        push(&mut out, &mut seen, c.id, c.kind, false, None, &c.author, &c.date, para);
                    }
                    if let Some(pc) = &cell.prop_change {
                        let para = cell_flat_start(&body, item, ri, ci);
                        push(&mut out, &mut seen, pc.id, TrackKind::Fmt, false,
                             Some(TablePropLevel::Cell), &pc.author, &pc.date, para);
                    }
                }
            }
        }
        out
    }

    /// The tracked region under codepoint `off` in paragraph `para` (for the hover tooltip + the
    /// click accept/reject popup), or `None` when the point isn't over a tracked change.
    pub fn track_at(&self, para: usize, off: usize) -> Result<Option<TrackedRegion>> {
        model::track_at(&self.doc, para, off)
    }

    /// Whether codepoint range `[start, end)` in paragraph `para` lies entirely within tracked
    /// insertions authored by `author` (so deleting it removes the text outright, like Word).
    pub fn range_is_own_insertion(
        &self,
        para: usize,
        start: usize,
        end: usize,
        author: &str,
    ) -> Result<bool> {
        model::range_is_own_insertion(&self.doc, para, start, end, author)
    }
}
