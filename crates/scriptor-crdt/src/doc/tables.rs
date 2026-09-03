//! Table structure and cell properties.
//! 
//! Rows, columns, merges and splits operate on the loro grid, which is the source of
//! truth for table structure. Each structural edit has a direct form and a tracked
//! form; the tracked form records a row or cell revision the review pass can resolve.

use crate::*;

impl CollabDoc {
    /// Append a plain-text table at the end of the document. `rows[0]` is treated as the header row -
    /// its cells render bold. Every cell is a single plain-text paragraph; the column count is the widest
    /// row (short rows leave the trailing cells empty). The table gets the `TableGrid` style so its
    /// borders show. Operates on the loro tree (`&self`) and persists in the op-log under `audit`.
    pub fn append_table(&self, rows: &[Vec<String>], audit: &str) -> Result<()> {
        let node = model::create_table_node(&self.doc)?;
        let grid = model::open_table_grid(&self.doc, node)?;
        let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        for j in 0..ncols {
            grid.push_col(&format!("c{j}"))?;
        }
        for (i, row) in rows.iter().enumerate() {
            let row_id = format!("r{i}");
            grid.push_row(&row_id)?;
            let header = i == 0;
            for (j, text) in row.iter().enumerate() {
                let col_id = format!("c{j}");
                if header {
                    // Header cells carry a single bold run (full run fidelity via set_cell_paragraphs).
                    let mut run = Run::plain(text.clone());
                    run.bold = true;
                    grid.set_cell_paragraphs(
                        &row_id,
                        &col_id,
                        &[Paragraph {
                            style: None,
                            props: ParaProps::default(),
                            runs: vec![run],
                            prop_change: None,
                            mark_change: None,
                        }],
                    )?;
                } else {
                    grid.set_cell(&row_id, &col_id, text)?;
                }
            }
        }
        grid.set_style(Some("TableGrid"))?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Resolve a flat paragraph index inside a table to its grid coordinates (the table node, the grid
    /// handle, and the row / visible-cell the index sits in), via [`model::block_seq`]. `None` when
    /// `para` isn't inside a table cell. The structural edit ops + property setters address the grid
    /// through this - table structure is a loro citizen now (tables-crdt T2.7), so there is no in-memory
    /// `body` to mutate; the grid is the source of truth.
    fn cell_addr(&self, para: usize) -> Option<CellAddr> {
        let model::BlockRef::Cell { node, row, col, .. } =
            model::block_seq(&self.doc).into_iter().nth(para)?
        else {
            return None;
        };
        let grid = model::open_table_grid(&self.doc, node).ok()?;
        let row_ids = grid.row_ids().ok()?;
        let row_pos = row_ids.iter().position(|r| *r == row)?;
        let vis_cols = grid.row_visible_cols(&row).ok()?;
        let cell_pos = vis_cols.iter().position(|c| *c == col)?;
        Some(CellAddr { node, grid, row_pos, row_id: row, col_id: col, cell_pos, n_rows: row_ids.len(), vis_cols })
    }

    /// Remove grid row at `row_pos` (purge its cells + drop its row id), and the table node if that
    /// empties it. Does **not** commit (the caller does) - shared by the direct delete + accept-resolution.
    pub(crate) fn remove_grid_row(&self, node: TreeID, grid: &crate::table_crdt::TableGrid, row_pos: usize) -> Result<()> {
        let row_ids = grid.row_ids()?;
        let Some(rid) = row_ids.get(row_pos) else { return Ok(()) };
        grid.purge_row_cells(rid)?;
        grid.delete_row(row_pos)?;
        if grid.row_ids()?.is_empty() {
            model::delete_block_node(&self.doc, node)?;
        }
        Ok(())
    }

    /// Remove grid column at `col_pos` (purge its cells + drop its col id), and the table node if that
    /// empties it. Does **not** commit (the caller does).
    pub(crate) fn remove_grid_column(&self, node: TreeID, grid: &crate::table_crdt::TableGrid, col_pos: usize) -> Result<()> {
        let col_ids = grid.col_ids()?;
        let Some(cid) = col_ids.get(col_pos) else { return Ok(()) };
        grid.purge_col_cells(cid)?;
        grid.delete_col(col_pos)?;
        if grid.col_ids()?.is_empty() {
            model::delete_block_node(&self.doc, node)?;
        }
        Ok(())
    }

    /// The caret's table context: `(row, col, n_rows, n_cols)` when paragraph `para` is inside a
    /// table cell (`col`/`n_cols` are cell indices within the row), else `None`. Drives the table
    /// context menu's enabled state.
    pub fn table_context(&self, para: usize) -> Option<(usize, usize, usize, usize)> {
        let body = self.body();
        let BodyLoc::Cell { item, row, cell } = body_locate(&body, para)? else {
            return None;
        };
        let model::BodyItem::Table(t) = &body[item] else { return None };
        let n_cols = t.rows.get(row).map(|r| r.cells.len()).unwrap_or(0);
        Some((row, cell, t.rows.len(), n_cols))
    }

    /// The flat paragraph index of the first paragraph of the cell one step forward (`forward=true`) /
    /// backward from the caret's cell, in reading order (across a row, then to the next/previous row's
    /// edge cell). `None` when `para` isn't in a cell or there's no next/previous cell (the caret is at
    /// the table's last / first cell). Drives Tab / Shift+Tab cell navigation.
    pub fn cell_step(&self, para: usize, forward: bool) -> Option<usize> {
        let body = self.body();
        let BodyLoc::Cell { item, row, cell } = body_locate(&body, para)? else { return None };
        let model::BodyItem::Table(t) = &body[item] else { return None };
        let (tr, tc) = if forward {
            if cell + 1 < t.rows[row].cells.len() {
                (row, cell + 1)
            } else if row + 1 < t.rows.len() {
                (row + 1, 0)
            } else {
                return None; // last cell of the table
            }
        } else if cell > 0 {
            (row, cell - 1)
        } else if row > 0 {
            (row - 1, t.rows[row - 1].cells.len().saturating_sub(1))
        } else {
            return None; // first cell of the table
        };
        // The cell paragraphs are contiguous in document order from the table's first flat index,
        // row-major over cells (each cell contributes `para_count`).
        let mut flat = flat_before_item(&body, item);
        for (ri, r) in t.rows.iter().enumerate() {
            for (ci, c) in r.cells.iter().enumerate() {
                if ri == tr && ci == tc {
                    return Some(flat);
                }
                flat += c.para_count;
            }
        }
        None
    }

    /// Insert a row above (`below=false`) or below (`below=true`) the caret's row, mirroring its
    /// column layout (each new cell holds one empty paragraph). When `change` is `Some`, the new row
    /// is marked a tracked revision. Returns the caret position (first cell of the new row), or `None`
    /// if `para` isn't in a table.
    fn do_insert_table_row(
        &self,
        para: usize,
        below: bool,
        change: Option<Track>,
        audit: &str,
    ) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let target = if below { a.row_pos + 1 } else { a.row_pos };
        let new_rid = fresh_id(&a.grid.row_ids()?, 'r', self.doc.peer_id());
        a.grid.insert_row(target, &new_rid)?;
        if let Some(c) = &change {
            a.grid.set_row_change(&new_rid, Some(c))?;
        }
        // Mirror the reference row's visible columns: one empty paragraph per new cell, same gridSpan.
        let empty = model::empty_paragraph();
        for col in &a.vis_cols {
            a.grid.insert_cell_block(&new_rid, col, 0, &empty)?;
            let span = a.grid.cell_grid_span(&a.row_id, col)?;
            if span > 1 {
                a.grid.set_cell_grid_span(&new_rid, col, span)?;
            }
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        let first_col = a.vis_cols.first().map(String::as_str).unwrap_or("");
        Ok(Some(model::cell_first_flat(&self.doc, a.node, &new_rid, first_col).unwrap_or(0)))
    }

    /// Insert a row (direct edit, no revision). See [`do_insert_table_row`](Self::do_insert_table_row).
    pub fn insert_table_row(&self, para: usize, below: bool, audit: &str) -> Result<Option<usize>> {
        self.do_insert_table_row(para, below, None, audit)
    }

    /// Insert a row as a tracked change (`w:trPr/w:ins`): the row is added *and* marked, attributed to
    /// `author`/`date`. Returns the caret (first cell of the new row), or `None` if not in a table.
    pub fn suggest_insert_table_row(
        &self,
        para: usize,
        below: bool,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<Option<usize>> {
        let id = self.next_revision_id()?;
        let track = Track { kind: TrackKind::Ins, author: author.into(), date: date.into(), id };
        self.do_insert_table_row(para, below, Some(track), audit)
    }

    /// Delete the caret's row (direct edit; and the table if it empties). Returns the caret position
    /// after deletion (clamped), or `None` if `para` isn't in a table.
    pub fn delete_table_row(&self, para: usize, audit: &str) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let first_col = a.vis_cols.first().map(String::as_str).unwrap_or("");
        let caret = model::cell_first_flat(&self.doc, a.node, &a.row_id, first_col)
            .unwrap_or_else(|| model::table_first_flat(&self.doc, a.node));
        self.remove_grid_row(a.node, &a.grid, a.row_pos)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        let total = self.paragraphs()?.len();
        Ok(Some(caret.min(total.saturating_sub(1))))
    }

    /// Delete the caret's row as a tracked change (`w:trPr/w:del`): the row is *marked*, not removed -
    /// it survives until the revision is accepted. Returns the caret (the row's first cell), or `None`
    /// if not in a table. Idempotent when the row is already a pending revision.
    pub fn suggest_delete_table_row(
        &self,
        para: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let first_col = a.vis_cols.first().map(String::as_str).unwrap_or("");
        let caret = model::cell_first_flat(&self.doc, a.node, &a.row_id, first_col).unwrap_or(0);
        if a.grid.row_change(&a.row_id)?.is_none() {
            let id = self.next_revision_id()?;
            a.grid.set_row_change(
                &a.row_id,
                Some(&Track { kind: TrackKind::Del, author: author.into(), date: date.into(), id }),
            )?;
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(Some(caret))
    }

    /// Insert a column left (`right=false`) or right (`right=true`) of the caret's cell - a new cell
    /// with one empty paragraph in every row, plus a grid column. When `change` is `Some`, every new
    /// cell is marked a tracked revision (one shared id). Returns the caret position (the new cell in
    /// the caret's row), or `None` if `para` isn't in a table.
    fn do_insert_table_column(
        &self,
        para: usize,
        right: bool,
        change: Option<Track>,
        audit: &str,
    ) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let col_ids = a.grid.col_ids()?;
        let cur = col_ids.iter().position(|c| *c == a.col_id).unwrap_or(0);
        // Insert before the caret's column, or after the grid columns its horizontal span covers.
        let target = if right {
            (cur + a.grid.cell_grid_span(&a.row_id, &a.col_id)? as usize).min(col_ids.len())
        } else {
            cur
        };
        let new_cid = fresh_id(&col_ids, 'c', self.doc.peer_id());
        a.grid.insert_col(target, &new_cid)?;
        // Mirror a neighbour's width (the column to the left, else the first).
        let w = col_ids
            .get(target.saturating_sub(1))
            .or_else(|| col_ids.first())
            .and_then(|c| a.grid.col_width(c).ok().flatten())
            .unwrap_or(1440);
        a.grid.set_col_width(&new_cid, w)?;
        // One empty paragraph in the new cell of every row (the conflict-free column insert: one op on
        // `col_order`, then a per-row cell materialization).
        let empty = model::empty_paragraph();
        for rid in a.grid.row_ids()? {
            a.grid.insert_cell_block(&rid, &new_cid, 0, &empty)?;
            if let Some(c) = &change {
                a.grid.set_cell_change(&rid, &new_cid, Some(c))?;
            }
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(Some(model::cell_first_flat(&self.doc, a.node, &a.row_id, &new_cid).unwrap_or(0)))
    }

    /// Insert a column (direct edit). See [`do_insert_table_column`](Self::do_insert_table_column).
    pub fn insert_table_column(&self, para: usize, right: bool, audit: &str) -> Result<Option<usize>> {
        self.do_insert_table_column(para, right, None, audit)
    }

    /// Insert a column as a tracked change (`w:tcPr/w:cellIns` on every new cell, one shared id):
    /// added *and* marked, attributed to `author`/`date`. Returns the caret, or `None` if not in a table.
    pub fn suggest_insert_table_column(
        &self,
        para: usize,
        right: bool,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<Option<usize>> {
        let id = self.next_revision_id()?;
        let track = Track { kind: TrackKind::Ins, author: author.into(), date: date.into(), id };
        self.do_insert_table_column(para, right, Some(track), audit)
    }

    /// Delete the caret's column (the cell at that index in every row + its grid column), and the
    /// table if it empties. Returns the caret position after deletion, or `None` if not in a table.
    pub fn delete_table_column(&self, para: usize, audit: &str) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let Some(target) = a.grid.col_ids()?.iter().position(|c| *c == a.col_id) else {
            return Ok(None);
        };
        let table_start = model::table_first_flat(&self.doc, a.node);
        self.remove_grid_column(a.node, &a.grid, target)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        // Caret: the same-or-last visible cell of the caret's row, or the table start if it emptied
        // (the table node was removed).
        let caret = match model::open_table_grid(&self.doc, a.node).and_then(|g| g.row_visible_cols(&a.row_id)) {
            Ok(vis) if !vis.is_empty() => {
                let ci = a.cell_pos.min(vis.len() - 1);
                model::cell_first_flat(&self.doc, a.node, &a.row_id, &vis[ci]).unwrap_or(table_start)
            }
            _ => table_start,
        };
        let total = self.paragraphs()?.len();
        Ok(Some(caret.min(total.saturating_sub(1))))
    }

    /// Delete the caret's column as a tracked change (`w:tcPr/w:cellDel` on the cell in every row, one
    /// shared id): the cells are *marked*, not removed - they survive until the revision is accepted.
    /// Returns the caret (the marked cell in the caret's row), or `None` if not in a table.
    pub fn suggest_delete_table_column(
        &self,
        para: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let caret = model::cell_first_flat(&self.doc, a.node, &a.row_id, &a.col_id).unwrap_or(0);
        let id = self.next_revision_id()?;
        // Mark the caret's grid column (same col id) deleted in every row - one shared revision id.
        for rid in a.grid.row_ids()? {
            if a.grid.cell_change(&rid, &a.col_id)?.is_none() {
                a.grid.set_cell_change(
                    &rid,
                    &a.col_id,
                    Some(&Track { kind: TrackKind::Del, author: author.into(), date: date.into(), id }),
                )?;
            }
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(Some(caret))
    }

    /// Move the caret's table row up (`up=true`) or down one position - a first-class `MovableList`
    /// reorder on `row_order` (no duplicate-on-concurrent-move). Returns the caret (the same cell, now
    /// at the row's new position), or `None` if `para` isn't in a table or the move runs off the edge.
    /// A direct structural edit - a row reorder isn't a tracked revision in OOXML.
    pub fn move_table_row(&self, para: usize, up: bool, audit: &str) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let target = if up {
            if a.row_pos == 0 {
                return Ok(None);
            }
            a.row_pos - 1
        } else {
            if a.row_pos + 1 >= a.n_rows {
                return Ok(None);
            }
            a.row_pos + 1
        };
        a.grid.move_row(a.row_pos, target)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(model::cell_first_flat(&self.doc, a.node, &a.row_id, &a.col_id))
    }

    /// Move the caret's table column left (`left=true`) or right one position - a `MovableList` reorder
    /// on `col_order`. Returns the caret (the same cell, now at the column's new position), or `None` if
    /// not in a table or the move runs off the edge. Direct edit.
    pub fn move_table_column(&self, para: usize, left: bool, audit: &str) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let col_ids = a.grid.col_ids()?;
        let Some(pos) = col_ids.iter().position(|c| *c == a.col_id) else { return Ok(None) };
        let target = if left {
            if pos == 0 {
                return Ok(None);
            }
            pos - 1
        } else {
            if pos + 1 >= col_ids.len() {
                return Ok(None);
            }
            pos + 1
        };
        a.grid.move_col(pos, target)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(model::cell_first_flat(&self.doc, a.node, &a.row_id, &a.col_id))
    }

    /// Merge the caret's cell with the `count - 1` visible cells to its right in the same row (a
    /// horizontal `w:gridSpan` merge): the absorbed cells' content is appended to the caret cell, their
    /// own content is cleared (they become the columns the span covers), and the caret cell's gridSpan
    /// becomes the sum of the merged cells' spans. Returns the caret (the surviving cell), or `None` if
    /// not in a table / there aren't `count` cells from the caret rightward. `count < 2` is a no-op.
    pub fn merge_cells_right(&self, para: usize, count: usize, audit: &str) -> Result<Option<usize>> {
        if count < 2 {
            return Ok(None);
        }
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        if a.cell_pos + count > a.vis_cols.len() {
            return Ok(None); // not enough cells to the right
        }
        let absorbed: Vec<String> = a.vis_cols[a.cell_pos + 1..a.cell_pos + count].to_vec();
        let mut total_span = a.grid.cell_grid_span(&a.row_id, &a.col_id)?;
        let mut merged = a.grid.cell_paragraphs(&a.row_id, &a.col_id)?;
        for col in &absorbed {
            total_span += a.grid.cell_grid_span(&a.row_id, col)?;
            merged.extend(a.grid.cell_paragraphs(&a.row_id, col)?);
            a.grid.set_cell_paragraphs(&a.row_id, col, &[])?; // cleared -> a column the span covers
            a.grid.set_cell_grid_span(&a.row_id, col, 1)?;
        }
        a.grid.set_cell_paragraphs(&a.row_id, &a.col_id, &merged)?;
        a.grid.set_cell_grid_span(&a.row_id, &a.col_id, total_span)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(model::cell_first_flat(&self.doc, a.node, &a.row_id, &a.col_id))
    }

    /// Split (unmerge) a horizontally-merged cell: its `w:gridSpan` returns to 1 and each grid column the
    /// span covered re-materializes an empty cell. Returns the caret, or `None` if not in a table / the
    /// cell isn't horizontally merged (span <= 1).
    pub fn split_cell_horizontal(&self, para: usize, audit: &str) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let span = a.grid.cell_grid_span(&a.row_id, &a.col_id)?;
        if span <= 1 {
            return Ok(None);
        }
        let col_ids = a.grid.col_ids()?;
        let Some(apos) = col_ids.iter().position(|c| *c == a.col_id) else { return Ok(None) };
        let empty = model::empty_paragraph();
        for off in 1..span as usize {
            if let Some(covered) = col_ids.get(apos + off) {
                a.grid.set_cell_paragraphs(&a.row_id, covered, std::slice::from_ref(&empty))?;
            }
        }
        a.grid.set_cell_grid_span(&a.row_id, &a.col_id, 1)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(model::cell_first_flat(&self.doc, a.node, &a.row_id, &a.col_id))
    }

    /// Merge the caret's cell with the `count - 1` cells below it in the same column (a vertical
    /// `w:vMerge` merge): the caret cell becomes the `restart` anchor and each cell below becomes an
    /// empty `continue` placeholder (a vMerge group occupies the same grid column across rows - Word
    /// keeps only the anchor's content). Returns the caret, or `None` if not in a table / there aren't
    /// `count` rows from the caret downward. `count < 2` is a no-op.
    pub fn merge_cells_down(&self, para: usize, count: usize, audit: &str) -> Result<Option<usize>> {
        if count < 2 {
            return Ok(None);
        }
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        let row_ids = a.grid.row_ids()?;
        if a.row_pos + count > row_ids.len() {
            return Ok(None);
        }
        a.grid.set_cell_vmerge(&a.row_id, &a.col_id, VMerge::Restart)?;
        let empty = model::empty_paragraph();
        for rid in &row_ids[a.row_pos + 1..a.row_pos + count] {
            a.grid.set_cell_vmerge(rid, &a.col_id, VMerge::Continue)?;
            a.grid.set_cell_paragraphs(rid, &a.col_id, std::slice::from_ref(&empty))?;
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(model::cell_first_flat(&self.doc, a.node, &a.row_id, &a.col_id))
    }

    /// Split (unmerge) a vertically-merged cell: the `restart` anchor and the `continue` cells below it
    /// in the same column return to independent (`w:vMerge` cleared). Returns the caret, or `None` if not
    /// in a table / the caret cell isn't a vertical-merge anchor.
    pub fn split_cell_vertical(&self, para: usize, audit: &str) -> Result<Option<usize>> {
        let Some(a) = self.cell_addr(para) else { return Ok(None) };
        if a.grid.cell_vmerge(&a.row_id, &a.col_id)? != VMerge::Restart {
            return Ok(None);
        }
        a.grid.set_cell_vmerge(&a.row_id, &a.col_id, VMerge::None)?;
        let row_ids = a.grid.row_ids()?;
        for off in 1.. {
            let Some(rid) = row_ids.get(a.row_pos + off) else { break };
            if a.grid.cell_vmerge(rid, &a.col_id)? == VMerge::Continue {
                a.grid.set_cell_vmerge(rid, &a.col_id, VMerge::None)?;
            } else {
                break;
            }
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(model::cell_first_flat(&self.doc, a.node, &a.row_id, &a.col_id))
    }

    /// The shading fill (`w:shd`) of the caret's table cell, or `None` (not in a cell / no shading).
    pub fn cell_shading(&self, para: usize) -> Option<String> {
        let body = self.body();
        let BodyLoc::Cell { item, row, cell } = body_locate(&body, para)? else { return None };
        let model::BodyItem::Table(t) = body.get(item)? else { return None };
        t.rows.get(row)?.cells.get(cell)?.shading.clone()
    }

    /// Set the caret cell's shading fill (`w:shd` in `w:tcPr`) directly (no revision); `fill = None`
    /// clears it. Returns whether the caret was in a table cell. Caller re-layouts + re-paints.
    pub fn set_cell_shading(&self, para: usize, fill: Option<String>, audit: &str) -> Result<bool> {
        self.edit_cell(para, None, audit, |g, r, c| g.set_cell_shading(r, c, fill.as_deref()))
    }

    /// Set the caret cell's shading fill as a tracked change (`w:tcPrChange`), attributed to
    /// `author`/`date` (the old cell props are recorded for reject). Returns whether in a table cell.
    pub fn suggest_cell_shading(
        &self,
        para: usize,
        fill: Option<String>,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<bool> {
        let id = self.next_revision_id()?;
        self.edit_cell(para, Some((author, date, id)), audit, |g, r, c| {
            g.set_cell_shading(r, c, fill.as_deref())
        })
    }

    /// Set the caret row's height (`w:trHeight`, twips) directly (no revision); `height = None` clears
    /// it. Returns whether the caret was in a table row. Caller re-layouts + re-paints.
    pub fn set_row_height(
        &self,
        para: usize,
        height: Option<u32>,
        exact: bool,
        audit: &str,
    ) -> Result<bool> {
        self.edit_row(para, None, audit, |g, rid| match height {
            Some(h) => g.set_row_height(rid, h, exact),
            None => g.clear_row_height(rid),
        })
    }

    /// Set the caret row's height as a tracked change (`w:trPrChange`). Returns whether in a table row.
    pub fn suggest_row_height(
        &self,
        para: usize,
        height: Option<u32>,
        exact: bool,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<bool> {
        let id = self.next_revision_id()?;
        self.edit_row(para, Some((author, date, id)), audit, |g, rid| match height {
            Some(h) => g.set_row_height(rid, h, exact),
            None => g.clear_row_height(rid),
        })
    }

    /// Set a uniform single-line border on every edge of the caret's table (`w:tblBorders`) directly
    /// (no revision); `border = None` removes all borders. Returns whether the caret was in a table.
    pub fn set_table_borders(
        &self,
        para: usize,
        border: Option<model::Border>,
        audit: &str,
    ) -> Result<bool> {
        self.edit_table(para, None, audit, |g| g.set_table_borders(&uniform_borders(border.clone())))
    }

    /// Set a uniform border on the caret's table as a tracked change (`w:tblPrChange`). Returns whether
    /// the caret was in a table.
    pub fn suggest_table_borders(
        &self,
        para: usize,
        border: Option<model::Border>,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<bool> {
        let id = self.next_revision_id()?;
        self.edit_table(para, Some((author, date, id)), audit, |g| {
            g.set_table_borders(&uniform_borders(border.clone()))
        })
    }

    /// Locate the caret's cell, apply `f` to the grid, and (when `track` is set + the cell has no
    /// pending property change yet) bank the before-state into the cell's `w:tcPrChange`. Commits.
    fn edit_cell(
        &self,
        para: usize,
        track: Option<(&str, &str, u64)>,
        audit: &str,
        f: impl Fn(&crate::table_crdt::TableGrid, &str, &str) -> Result<()>,
    ) -> Result<bool> {
        let Some(a) = self.cell_addr(para) else { return Ok(false) };
        if let Some((author, date, id)) = track
            && a.grid.cell_prop_change(&a.row_id, &a.col_id)?.is_none() {
                let old = model::TablePropSnapshot::Cell {
                    width: a.grid.cell_width(&a.row_id, &a.col_id)?,
                    grid_span: a.grid.cell_grid_span(&a.row_id, &a.col_id)? as usize,
                    vmerge: a.grid.cell_vmerge(&a.row_id, &a.col_id)?,
                    borders: a.grid.cell_borders(&a.row_id, &a.col_id)?,
                    margins: a.grid.cell_margins(&a.row_id, &a.col_id)?,
                    shading: a.grid.cell_shading(&a.row_id, &a.col_id)?,
                };
                a.grid.set_cell_prop_change(
                    &a.row_id,
                    &a.col_id,
                    Some(&TablePropChange { author: author.into(), date: date.into(), id, old }),
                )?;
            }
        f(&a.grid, &a.row_id, &a.col_id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Locate the caret's row, apply `f` to the grid, banking the before-state when `track` is set. Commits.
    fn edit_row(
        &self,
        para: usize,
        track: Option<(&str, &str, u64)>,
        audit: &str,
        f: impl Fn(&crate::table_crdt::TableGrid, &str) -> Result<()>,
    ) -> Result<bool> {
        let Some(a) = self.cell_addr(para) else { return Ok(false) };
        if let Some((author, date, id)) = track
            && a.grid.row_prop_change(&a.row_id)?.is_none() {
                let (height, height_exact) = match a.grid.row_height(&a.row_id)? {
                    Some((h, e)) => (Some(h), e),
                    None => (None, false),
                };
                let old = model::TablePropSnapshot::Row { height, height_exact };
                a.grid.set_row_prop_change(
                    &a.row_id,
                    Some(&TablePropChange { author: author.into(), date: date.into(), id, old }),
                )?;
            }
        f(&a.grid, &a.row_id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Locate the caret's table, apply `f` to the grid, banking the before-state when `track` is set. Commits.
    fn edit_table(
        &self,
        para: usize,
        track: Option<(&str, &str, u64)>,
        audit: &str,
        f: impl Fn(&crate::table_crdt::TableGrid) -> Result<()>,
    ) -> Result<bool> {
        let Some(a) = self.cell_addr(para) else { return Ok(false) };
        if let Some((author, date, id)) = track
            && a.grid.table_prop_change()?.is_none() {
                let old = model::TablePropSnapshot::Table {
                    style: a.grid.style()?,
                    borders: a.grid.table_borders()?,
                    cell_margins: a.grid.table_cell_margins()?,
                };
                a.grid.set_table_prop_change(Some(&TablePropChange {
                    author: author.into(),
                    date: date.into(),
                    id,
                    old,
                }))?;
            }
        f(&a.grid)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }
}
