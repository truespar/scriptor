//! Table structure as a loro CRDT citizen - the foundational grid layer.
//!
//! Today table structure lives in the in-memory `Vec<BodyItem>` and isn't in the loro op log, so a
//! joined peer doesn't see tables and concurrent structural edits can't converge. The design's core is
//! a **sparse `(rowId, colId)` cell map over two shared ordered id-lists**:
//!
//! ```text
//! table (a LoroMap):
//!   row_order : LoroMovableList<String>   ordered stable row-ids
//!   col_order : LoroMovableList<String>   ordered stable col-ids  (the ONE source of column truth)
//!   cells     : LoroMap                    sparse; key "{rowId}:{colId}" -> per-cell LoroMap
//! ```
//!
//! Lifting "column" into a single shared list is what makes a column insert **conflict-free**: it's one
//! op on `col_order`, never a per-row write, so two peers inserting columns concurrently can't produce
//! ragged rows (the failure mode of the node-tree model every CRDT editor inherits). Row/col order use
//! `LoroMovableList` so a reorder is a first-class `mov` (no duplicate-on-concurrent-move).
//!
//! This module proves the grid CRDT in isolation: cells hold placeholder **text** here; the
//! document model layers the full cell payload and import/export on top. Pure over a `LoroMap` +
//! caller commits, matching the `model` convention.

use anyhow::Result;
use loro::{Container, LoroList, LoroMap, LoroMovableList, LoroText, LoroValue, ValueOrContainer};

use crate::model::write_paragraph_into_map;
use crate::model::NestedBlock;
use crate::{
    Border, CellMargins, EdgeBorders, ParaProps, Paragraph, Run, TablePropChange, TablePropLevel,
    TablePropSnapshot, Track, TrackKind, VMerge,
};

type Json = serde_json::Value;

const ROW_ORDER: &str = "row_order";
const COL_ORDER: &str = "col_order";
const CELLS: &str = "cells";
/// Within a cell map: the ordered paragraph blocks (a `LoroList` of paragraph maps; each map is the
/// same shape as a body block's meta - see `model::read_paragraph_from_map`).
const BLOCKS: &str = "blocks";
/// Cell geometry keys (on the cell map alongside `blocks`).
const GRID_SPAN: &str = "grid_span";
const VMERGE: &str = "vmerge";
/// A width key (twips) - used both on a cell map and in the `grid` map (per column).
const W: &str = "w";
/// Table-level keys (on the table map).
const TBL_STYLE: &str = "style";
const TBL_LOOK: &str = "look";
const TBL_JC: &str = "jc";
const ROW_JC: &str = "jc";
const GRID: &str = "grid"; // colId -> width (twips)
const ROW_PROPS: &str = "row_props"; // rowId -> { h, exact }
const ROW_H: &str = "h";
const ROW_EXACT: &str = "exact";
/// `w:trPr/w:cantSplit` - the row must paginate whole (no split across pages).
const ROW_CANTSPLIT: &str = "nosplit";
/// Table-level border + default cell-margin keys (on the table map): `w:tblBorders` / `w:tblCellMar`.
const TBL_BORDERS: &str = "tbl_borders";
const TBL_CELLMAR: &str = "tbl_cellmar";
/// Cell property keys (on a cell map): `w:tcBorders` / `w:tcMar` / `w:shd w:fill`.
const CELL_BORDERS: &str = "borders";
const CELL_MARGINS: &str = "margins";
const CELL_SHADING: &str = "shd";
/// Verbatim XML of tables nested inside a cell, as a JSON array of `{after, xml}`. Opaque - nothing
/// edits it - so it is one string rather than containers. See [`TableGrid::set_cell_nested`].
const CELL_NESTED: &str = "nested";
/// Within a border edge map: line weight (`w:sz`, eighths of a point) + RGB hex colour (`w:color`).
const B_SZ: &str = "sz";
const B_COLOR: &str = "color";
/// The six edge keys of an [`EdgeBorders`] map (OOXML element local names; `insideH`/`insideV` are
/// table-level only). Order here is the CT_Tbl/CT_Tc border child order the serializer emits.
const EDGE_KEYS: [&str; 6] = ["top", "left", "bottom", "right", "insideH", "insideV"];
/// The four margin keys of a [`CellMargins`] map.
const MARGIN_KEYS: [&str; 4] = ["top", "left", "bottom", "right"];
/// Tracked structural revision (`w:trPr/w:ins|del`, `w:tcPr/w:cellIns|cellDel`) on a row / cell map.
const CHANGE: &str = "change";
/// Tracked property revision (`w:trPrChange` / `w:tcPrChange`) on a row / cell map; the table-level
/// `w:tblPrChange` lives under `TBL_PROP_CHANGE` on the table map.
const PROP_CHANGE: &str = "prop_change";
const TBL_PROP_CHANGE: &str = "tbl_prop_change";
/// Sub-keys of a stored [`Track`] / [`TablePropChange`] map.
const T_KIND: &str = "kind";
const T_ID: &str = "id";
const T_AUTHOR: &str = "author";
const T_DATE: &str = "date";
/// The nested before-properties snapshot of a [`TablePropChange`].
const OLD: &str = "old";

/// A handle to a table's grid containers, rooted at a `LoroMap` (the table container).
pub struct TableGrid {
    map: LoroMap,
}

impl TableGrid {
    /// Open (initializing if needed) the grid containers on `map`. Idempotent.
    pub fn open(map: LoroMap) -> Result<Self> {
        let g = Self { map };
        // Touch each sub-container so it exists.
        g.row_order()?;
        g.col_order()?;
        g.cells()?;
        Ok(g)
    }

    fn row_order(&self) -> Result<LoroMovableList> {
        Ok(self.map.get_or_create_container(ROW_ORDER, LoroMovableList::new())?)
    }
    fn col_order(&self) -> Result<LoroMovableList> {
        Ok(self.map.get_or_create_container(COL_ORDER, LoroMovableList::new())?)
    }
    fn cells(&self) -> Result<LoroMap> {
        Ok(self.map.get_or_create_container(CELLS, LoroMap::new())?)
    }

    /// The ordered row ids.
    pub fn row_ids(&self) -> Result<Vec<String>> {
        Ok(list_strings(&self.row_order()?))
    }
    /// The ordered column ids.
    pub fn col_ids(&self) -> Result<Vec<String>> {
        Ok(list_strings(&self.col_order()?))
    }
    /// `(rows, cols)` dimensions.
    pub fn dims(&self) -> Result<(usize, usize)> {
        Ok((self.row_order()?.len(), self.col_order()?.len()))
    }

    /// Append a row with stable id `id`.
    pub fn push_row(&self, id: &str) -> Result<()> {
        self.row_order()?.push(id.to_string())?;
        Ok(())
    }
    /// Append a column with stable id `id`.
    pub fn push_col(&self, id: &str) -> Result<()> {
        self.col_order()?.push(id.to_string())?;
        Ok(())
    }
    /// Insert a row id at position `pos`.
    pub fn insert_row(&self, pos: usize, id: &str) -> Result<()> {
        self.row_order()?.insert(pos, id.to_string())?;
        crate::model::block_cache_invalidate(); // grid rows changed -> block_seq changed
        Ok(())
    }
    /// Insert a column id at position `pos` - the conflict-free column insert (one op on `col_order`).
    pub fn insert_col(&self, pos: usize, id: &str) -> Result<()> {
        self.col_order()?.insert(pos, id.to_string())?;
        crate::model::block_cache_invalidate(); // grid columns changed -> block_seq changed
        Ok(())
    }
    /// Move a row from `from` to `to` (first-class `mov`, no duplicate).
    pub fn move_row(&self, from: usize, to: usize) -> Result<()> {
        self.row_order()?.mov(from, to)?;
        crate::model::block_cache_invalidate(); // grid row order changed -> block_seq changed
        Ok(())
    }
    /// Move a column from `from` to `to`.
    pub fn move_col(&self, from: usize, to: usize) -> Result<()> {
        self.col_order()?.mov(from, to)?;
        crate::model::block_cache_invalidate(); // grid column order changed -> block_seq changed
        Ok(())
    }
    /// Remove the row id at `pos` from the order. (Cells of the retired row are not GC'd, per the
    /// edit-wins convergence policy.)
    pub fn delete_row(&self, pos: usize) -> Result<()> {
        self.row_order()?.delete(pos, 1)?;
        crate::model::block_cache_invalidate(); // grid rows changed -> block_seq changed
        Ok(())
    }
    /// Remove the column id at `pos` from the order.
    pub fn delete_col(&self, pos: usize) -> Result<()> {
        self.col_order()?.delete(pos, 1)?;
        crate::model::block_cache_invalidate(); // grid columns changed -> block_seq changed
        Ok(())
    }

    /// Set cell `(row_id, col_id)`'s paragraph blocks, materializing the cell container if absent.
    /// The cell holds its own block content - the same `Run`/Peritext-mark model the document body uses
    /// (so a cell's intra-text edits get Fugue ordering + tracked-change marks for free). Uses
    /// `get_or_create_container` so a concurrent first-touch of the same cell converges to **one** cell
    /// container (its block list then merges normally - no clobber-to-nothing).
    pub fn set_cell_paragraphs(&self, row_id: &str, col_id: &str, paras: &[Paragraph]) -> Result<()> {
        // Mergeable child containers (loro `ensure_mergeable_*`): two peers' concurrent first-touch of
        // the same cell resolve to ONE deterministic cell map + block list, so neither write is
        // clobbered (plain `get_or_create_container` is LWW on concurrent creation - it drops a peer's
        // cell outright).
        let cell: LoroMap = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
        let blocks: LoroList = cell.ensure_mergeable_list(BLOCKS)?;
        let n = blocks.len();
        if n > 0 {
            blocks.delete(0, n)?;
        }
        for p in paras {
            let pm: LoroMap = blocks.insert_container(blocks.len(), LoroMap::new())?;
            // Full paragraph fidelity (style + props + tracked pPrChange / paragraph-mark + run text),
            // the same codec body paragraphs use - so a cell paragraph carries alignment, spacing,
            // numbering, etc., not just style + text.
            write_paragraph_into_map(&pm, p)?;
        }
        crate::model::block_cache_invalidate(); // cell block count changed -> block_seq changed
        Ok(())
    }

    /// Cell `(row_id, col_id)`'s paragraph blocks (empty if the cell was never materialized).
    pub fn cell_paragraphs(&self, row_id: &str, col_id: &str) -> Result<Vec<Paragraph>> {
        let cells = self.cells()?;
        let Some(ValueOrContainer::Container(Container::Map(cell))) = cells.get(&cell_key(row_id, col_id))
        else {
            return Ok(Vec::new());
        };
        let Some(ValueOrContainer::Container(Container::List(blocks))) = cell.get(BLOCKS) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(blocks.len());
        for i in 0..blocks.len() {
            let Some(ValueOrContainer::Container(Container::Map(pm))) = blocks.get(i) else { continue };
            out.push(crate::model::read_paragraph_from_map(&pm));
        }
        Ok(out)
    }

    /// The number of block paragraphs in cell `(row_id, col_id)` (0 if the cell was never materialized).
    /// Drives the flat paragraph enumeration (`model::block_seq`) that descends into table cells.
    pub fn cell_block_count(&self, row_id: &str, col_id: &str) -> Result<usize> {
        Ok(match self.cell_get(row_id, col_id)? {
            Some(cell) => match cell.get(BLOCKS) {
                Some(ValueOrContainer::Container(Container::List(b))) => b.len(),
                _ => 0,
            },
            None => 0,
        })
    }

    /// The paragraph map of the `idx`-th block paragraph in cell `(row_id, col_id)`, if present (the
    /// same `{style?, text, props...}` shape `model::read_paragraph_from_map` reads).
    pub fn cell_block_map(&self, row_id: &str, col_id: &str, idx: usize) -> Result<Option<LoroMap>> {
        let Some(cell) = self.cell_get(row_id, col_id)? else { return Ok(None) };
        let Some(ValueOrContainer::Container(Container::List(blocks))) = cell.get(BLOCKS) else {
            return Ok(None);
        };
        Ok(match blocks.get(idx) {
            Some(ValueOrContainer::Container(Container::Map(pm))) => Some(pm),
            _ => None,
        })
    }

    /// The `text` container of the `idx`-th block paragraph in cell `(row_id, col_id)`, if present - the
    /// editable run text the caret + edit ops address once a cell paragraph is part of the flat index.
    pub fn cell_block_text(&self, row_id: &str, col_id: &str, idx: usize) -> Result<Option<LoroText>> {
        let Some(pm) = self.cell_block_map(row_id, col_id, idx)? else { return Ok(None) };
        Ok(match pm.get("text") {
            Some(ValueOrContainer::Container(Container::Text(t))) => Some(t),
            _ => None,
        })
    }

    /// Insert a block paragraph `p` at position `at` within cell `(row_id, col_id)` (clamped to the
    /// block count). The Enter-in-a-cell / row-or-column-materialization primitive. Caller commits.
    pub fn insert_cell_block(&self, row_id: &str, col_id: &str, at: usize, p: &Paragraph) -> Result<()> {
        let cell = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
        let blocks: LoroList = cell.ensure_mergeable_list(BLOCKS)?;
        let at = at.min(blocks.len());
        let pm: LoroMap = blocks.insert_container(at, LoroMap::new())?;
        write_paragraph_into_map(&pm, p)?;
        crate::model::block_cache_invalidate(); // a cell block was added -> block_seq changed
        Ok(())
    }

    /// Remove the `idx`-th block paragraph from cell `(row_id, col_id)` (no-op if out of range). The
    /// join / delete-paragraph-in-a-cell primitive. Caller commits.
    pub fn remove_cell_block(&self, row_id: &str, col_id: &str, idx: usize) -> Result<()> {
        if let Some(cell) = self.cell_get(row_id, col_id)?
            && let Some(ValueOrContainer::Container(Container::List(blocks))) = cell.get(BLOCKS)
                && idx < blocks.len() {
                    blocks.delete(idx, 1)?;
                    crate::model::block_cache_invalidate(); // a cell block was removed -> block_seq changed
                }
        Ok(())
    }

    /// Convenience: set a cell to a single plain-text paragraph.
    pub fn set_cell(&self, row_id: &str, col_id: &str, text: &str) -> Result<()> {
        self.set_cell_paragraphs(
            row_id,
            col_id,
            &[Paragraph {
                style: None,
                props: ParaProps::default(),
                runs: vec![Run::plain(text)],
                prop_change: None,
                mark_change: None,
            }],
        )
    }

    /// Cell `(row_id, col_id)`'s text (paragraphs newline-joined), or `None` if never materialized.
    pub fn cell_text(&self, row_id: &str, col_id: &str) -> Result<Option<String>> {
        let paras = self.cell_paragraphs(row_id, col_id)?;
        if paras.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            paras
                .iter()
                .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }

    /// The whole grid as `rows x cols` text (empty string for an unmaterialized cell) - the read
    /// projection a renderer / the in-memory `body` will derive from.
    pub fn grid_text(&self) -> Result<Vec<Vec<String>>> {
        let rows = self.row_ids()?;
        let cols = self.col_ids()?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut line = Vec::with_capacity(cols.len());
            for c in &cols {
                line.push(self.cell_text(r, c)?.unwrap_or_default());
            }
            out.push(line);
        }
        Ok(out)
    }

    // ── structural geometry (cell spans / merges / widths, row height, table style + grid) ─────────

    /// The cell map at `(row_id, col_id)` if it has been materialized (read-only; no create).
    fn cell_get(&self, row_id: &str, col_id: &str) -> Result<Option<LoroMap>> {
        Ok(match self.cells()?.get(&cell_key(row_id, col_id)) {
            Some(ValueOrContainer::Container(Container::Map(m))) => Some(m),
            _ => None,
        })
    }

    /// Set a cell's horizontal grid span (`w:gridSpan`, >= 1).
    pub fn set_cell_grid_span(&self, row_id: &str, col_id: &str, span: u32) -> Result<()> {
        self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?.insert(GRID_SPAN, span.max(1) as i64)?;
        Ok(())
    }
    /// A cell's grid span (default 1).
    pub fn cell_grid_span(&self, row_id: &str, col_id: &str) -> Result<u32> {
        Ok(self
            .cell_get(row_id, col_id)?
            .and_then(|m| match m.get(GRID_SPAN) {
                Some(ValueOrContainer::Value(LoroValue::I64(n))) => Some(n.max(1) as u32),
                _ => None,
            })
            .unwrap_or(1))
    }

    /// Set a cell's vertical merge role (`w:vMerge`); `VMerge::None` clears it.
    pub fn set_cell_vmerge(&self, row_id: &str, col_id: &str, v: VMerge) -> Result<()> {
        let cell = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
        match v {
            VMerge::None => {
                cell.delete(VMERGE)?;
            }
            VMerge::Restart => {
                cell.insert(VMERGE, "restart")?;
            }
            VMerge::Continue => {
                cell.insert(VMERGE, "continue")?;
            }
        }
        Ok(())
    }
    /// A cell's vertical-merge role (default `None`).
    pub fn cell_vmerge(&self, row_id: &str, col_id: &str) -> Result<VMerge> {
        let role = self.cell_get(row_id, col_id)?.and_then(|m| match m.get(VMERGE) {
            Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
            _ => None,
        });
        Ok(match role.as_deref() {
            Some("restart") => VMerge::Restart,
            Some("continue") => VMerge::Continue,
            _ => VMerge::None,
        })
    }

    /// Set a cell's explicit width (`w:tcW`, twips).
    pub fn set_cell_width(&self, row_id: &str, col_id: &str, w: u32) -> Result<()> {
        self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?.insert(W, w as i64)?;
        Ok(())
    }
    /// A cell's explicit width, if set.
    pub fn cell_width(&self, row_id: &str, col_id: &str) -> Result<Option<u32>> {
        Ok(self.cell_get(row_id, col_id)?.and_then(|m| match m.get(W) {
            Some(ValueOrContainer::Value(LoroValue::I64(n))) => Some(n as u32),
            _ => None,
        }))
    }

    /// Set the table style id (`w:tblStyle`); `None` clears it.
    pub fn set_style(&self, style: Option<&str>) -> Result<()> {
        match style {
            Some(s) => self.map.insert(TBL_STYLE, s)?,
            None => self.map.delete(TBL_STYLE)?,
        }
        Ok(())
    }
    /// The table style id, if set.
    pub fn style(&self) -> Result<Option<String>> {
        Ok(match self.map.get(TBL_STYLE) {
            Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
            _ => None,
        })
    }

    /// Set the table alignment (`w:tblPr/w:jc` value, e.g. "center"); `None` clears it.
    pub fn set_justify(&self, justify: Option<&str>) -> Result<()> {
        match justify {
            Some(s) => self.map.insert(TBL_JC, s)?,
            None => self.map.delete(TBL_JC)?,
        }
        Ok(())
    }
    /// The table alignment, if set.
    pub fn justify(&self) -> Result<Option<String>> {
        Ok(match self.map.get(TBL_JC) {
            Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
            _ => None,
        })
    }

    /// Set a row's alignment (`w:trPr/w:jc` value), keyed by row id; `None` clears it.
    pub fn set_row_justify(&self, row_id: &str, justify: Option<&str>) -> Result<()> {
        let rp: LoroMap = self.row_props()?.get_or_create_container(row_id, LoroMap::new())?;
        match justify {
            Some(s) => rp.insert(ROW_JC, s)?,
            None => rp.delete(ROW_JC)?,
        }
        Ok(())
    }
    /// A row's alignment, if set.
    pub fn row_justify(&self, row_id: &str) -> Result<Option<String>> {
        let Some(ValueOrContainer::Container(Container::Map(rp))) = self.row_props()?.get(row_id)
        else {
            return Ok(None);
        };
        Ok(match rp.get(ROW_JC) {
            Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
            _ => None,
        })
    }

    /// Set / clear a row's `w:cantSplit` (the row must paginate whole), keyed by row id.
    pub fn set_row_cant_split(&self, row_id: &str, on: bool) -> Result<()> {
        let rp: LoroMap = self.row_props()?.get_or_create_container(row_id, LoroMap::new())?;
        if on {
            rp.insert(ROW_CANTSPLIT, true)?;
        } else {
            rp.delete(ROW_CANTSPLIT)?;
        }
        Ok(())
    }
    /// Whether the row carries `w:cantSplit`.
    pub fn row_cant_split(&self, row_id: &str) -> Result<bool> {
        let Some(ValueOrContainer::Container(Container::Map(rp))) = self.row_props()?.get(row_id)
        else {
            return Ok(false);
        };
        Ok(matches!(rp.get(ROW_CANTSPLIT), Some(ValueOrContainer::Value(LoroValue::Bool(true)))))
    }

    /// Set the raw `w:tblLook` attribute string (see [`crate::Table::look`]); `None` clears it.
    pub fn set_look(&self, look: Option<&str>) -> Result<()> {
        match look {
            Some(s) => self.map.insert(TBL_LOOK, s)?,
            None => self.map.delete(TBL_LOOK)?,
        }
        Ok(())
    }
    /// The raw `w:tblLook` attribute string, if set.
    pub fn look(&self) -> Result<Option<String>> {
        Ok(match self.map.get(TBL_LOOK) {
            Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
            _ => None,
        })
    }

    fn grid(&self) -> Result<LoroMap> {
        Ok(self.map.get_or_create_container(GRID, LoroMap::new())?)
    }
    /// Set a column's grid width (`w:gridCol`, twips), keyed by stable col id.
    pub fn set_col_width(&self, col_id: &str, w: u32) -> Result<()> {
        self.grid()?.insert(col_id, w as i64)?;
        Ok(())
    }
    /// A column's grid width, if set.
    pub fn col_width(&self, col_id: &str) -> Result<Option<u32>> {
        Ok(match self.grid()?.get(col_id) {
            Some(ValueOrContainer::Value(LoroValue::I64(n))) => Some(n as u32),
            _ => None,
        })
    }

    fn row_props(&self) -> Result<LoroMap> {
        Ok(self.map.get_or_create_container(ROW_PROPS, LoroMap::new())?)
    }
    /// Set a row's height (`w:trHeight`, twips) and exact flag (`w:hRule="exact"`), keyed by row id.
    pub fn set_row_height(&self, row_id: &str, height: u32, exact: bool) -> Result<()> {
        let rp: LoroMap = self.row_props()?.get_or_create_container(row_id, LoroMap::new())?;
        rp.insert(ROW_H, height as i64)?;
        rp.insert(ROW_EXACT, exact)?;
        Ok(())
    }
    /// Clear a row's height (`w:trHeight`), leaving the row's other props (change / prop-change) intact.
    pub fn clear_row_height(&self, row_id: &str) -> Result<()> {
        if let Some(ValueOrContainer::Container(Container::Map(rp))) = self.row_props()?.get(row_id) {
            rp.delete(ROW_H)?;
            rp.delete(ROW_EXACT)?;
        }
        Ok(())
    }
    /// A row's `(height, exact)`, if set.
    pub fn row_height(&self, row_id: &str) -> Result<Option<(u32, bool)>> {
        let Some(ValueOrContainer::Container(Container::Map(rp))) = self.row_props()?.get(row_id) else {
            return Ok(None);
        };
        let h = match rp.get(ROW_H) {
            Some(ValueOrContainer::Value(LoroValue::I64(n))) => n as u32,
            _ => return Ok(None),
        };
        let exact = matches!(rp.get(ROW_EXACT), Some(ValueOrContainer::Value(LoroValue::Bool(true))));
        Ok(Some((h, exact)))
    }

    // ── borders / margins / shading (table-level + per-cell) ───────────────────────────────────────

    /// Set the table-level border edges (`w:tblBorders`); all-empty clears them.
    pub fn set_table_borders(&self, e: &EdgeBorders) -> Result<()> {
        write_edge_borders(&self.map, TBL_BORDERS, e)
    }
    /// The table-level border edges (empty if unset).
    pub fn table_borders(&self) -> Result<EdgeBorders> {
        Ok(read_edge_borders(&self.map, TBL_BORDERS))
    }
    /// Set the table default cell margins (`w:tblCellMar`); `None` clears them.
    pub fn set_table_cell_margins(&self, m: Option<CellMargins>) -> Result<()> {
        write_margins(&self.map, TBL_CELLMAR, m)
    }
    /// The table default cell margins, if set.
    pub fn table_cell_margins(&self) -> Result<Option<CellMargins>> {
        Ok(read_margins(&self.map, TBL_CELLMAR))
    }

    /// Set a cell's border-edge overrides (`w:tcBorders`); all-empty clears them.
    pub fn set_cell_borders(&self, row_id: &str, col_id: &str, e: &EdgeBorders) -> Result<()> {
        write_edge_borders(&self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?, CELL_BORDERS, e)
    }
    /// A cell's border-edge overrides (empty if unset).
    pub fn cell_borders(&self, row_id: &str, col_id: &str) -> Result<EdgeBorders> {
        Ok(match self.cell_get(row_id, col_id)? {
            Some(cell) => read_edge_borders(&cell, CELL_BORDERS),
            None => EdgeBorders::default(),
        })
    }
    /// Set a cell's content margins (`w:tcMar`); `None` clears them.
    pub fn set_cell_margins(&self, row_id: &str, col_id: &str, m: Option<CellMargins>) -> Result<()> {
        write_margins(&self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?, CELL_MARGINS, m)
    }
    /// A cell's content margins, if set.
    pub fn cell_margins(&self, row_id: &str, col_id: &str) -> Result<Option<CellMargins>> {
        Ok(self.cell_get(row_id, col_id)?.and_then(|cell| read_margins(&cell, CELL_MARGINS)))
    }
    /// Set a cell's shading fill colour (`w:shd w:fill`, RGB hex); `None` clears it.
    pub fn set_cell_shading(&self, row_id: &str, col_id: &str, shading: Option<&str>) -> Result<()> {
        let cell = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
        match shading {
            Some(s) => cell.insert(CELL_SHADING, s)?,
            None => cell.delete(CELL_SHADING)?,
        }
        Ok(())
    }
    /// A cell's shading fill colour, if set.
    pub fn cell_shading(&self, row_id: &str, col_id: &str) -> Result<Option<String>> {
        Ok(self.cell_get(row_id, col_id)?.and_then(|cell| match cell.get(CELL_SHADING) {
            Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
            _ => None,
        }))
    }

    /// Store the tables nested inside a cell, captured verbatim.
    ///
    /// A table inside a cell is not modeled - a cell owns a slice of the flat paragraph list, which
    /// cannot express one - so its XML is kept as-is and re-emitted where it sat. Held as JSON in a
    /// single key rather than as containers, because nothing edits it: it is opaque bytes, like an
    /// OLE object's captured run. See [`NestedBlock`](crate::model::NestedBlock).
    pub fn set_cell_nested(&self, row_id: &str, col_id: &str, nested: &[NestedBlock]) -> Result<()> {
        let cell = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
        if nested.is_empty() {
            cell.delete(CELL_NESTED)?;
            return Ok(());
        }
        let json: Vec<Json> = nested
            .iter()
            .map(|n| serde_json::json!({ "after": n.after_para, "xml": n.xml }))
            .collect();
        cell.insert(CELL_NESTED, Json::Array(json).to_string().as_str())?;
        Ok(())
    }

    /// The tables nested inside a cell, in the order they appeared.
    pub fn cell_nested(&self, row_id: &str, col_id: &str) -> Result<Vec<NestedBlock>> {
        let Some(cell) = self.cell_get(row_id, col_id)? else { return Ok(Vec::new()) };
        let Some(ValueOrContainer::Value(LoroValue::String(s))) = cell.get(CELL_NESTED) else {
            return Ok(Vec::new());
        };
        let Ok(Json::Array(items)) = serde_json::from_str::<Json>(&s) else { return Ok(Vec::new()) };
        Ok(items
            .into_iter()
            .filter_map(|v| {
                Some(NestedBlock {
                    after_para: v.get("after")?.as_u64()? as usize,
                    xml: v.get("xml")?.as_str()?.to_string(),
                })
            })
            .collect())
    }

    /// The **visible** cells of a row, as the anchor column id of each `<w:tc>` in left-to-right order -
    /// i.e. `col_order` with the columns a horizontal `gridSpan` absorbs skipped (a span-N cell occupies
    /// N grid columns but is one visible cell). This is exactly the column walk [`export_table_grid`]
    /// uses, so the visible-cell sequence here matches both the serializer and the flat paragraph index
    /// ([`crate::model::block_seq`] descends every `(row, col)`, but a covered column has no blocks).
    /// The structural edit ops address a row's cells by this visible index.
    ///
    /// [`export_table_grid`]: crate::model::export_table_grid
    pub fn row_visible_cols(&self, row_id: &str) -> Result<Vec<String>> {
        let cols = self.col_ids()?;
        let mut out = Vec::new();
        let mut ci = 0usize;
        while ci < cols.len() {
            let c = &cols[ci];
            out.push(c.clone());
            ci += self.cell_grid_span(row_id, c)?.max(1) as usize;
        }
        Ok(out)
    }

    /// Delete every materialized cell entry for a row (its sparse `(row, *)` map slots) - the content GC
    /// that pairs with [`delete_row`](Self::delete_row) when a row is removed outright (not a tracked
    /// deletion). Removing the row id from `row_order` already drops the row from the flat index + export;
    /// this reclaims the orphaned cell containers. Caller commits.
    pub fn purge_row_cells(&self, row_id: &str) -> Result<()> {
        let cells = self.cells()?;
        for c in self.col_ids()? {
            let _ = cells.delete(&cell_key(row_id, &c));
        }
        Ok(())
    }

    /// Delete every materialized cell entry for a column (its sparse `(*, col)` map slots) - the GC that
    /// pairs with [`delete_col`](Self::delete_col). Caller commits.
    pub fn purge_col_cells(&self, col_id: &str) -> Result<()> {
        let cells = self.cells()?;
        for r in self.row_ids()? {
            let _ = cells.delete(&cell_key(&r, col_id));
        }
        Ok(())
    }

    // ── tracked structural + property revisions (T2.5) ─────────────────────────────────────────────

    /// The per-row property map (height / change / prop-change), get-or-created.
    fn row_prop_map(&self, row_id: &str) -> Result<LoroMap> {
        Ok(self.row_props()?.get_or_create_container(row_id, LoroMap::new())?)
    }
    /// The per-row property map, read-only (None if the row has no stored props).
    fn row_prop_map_get(&self, row_id: &str) -> Result<Option<LoroMap>> {
        Ok(match self.row_props()?.get(row_id) {
            Some(ValueOrContainer::Container(Container::Map(m))) => Some(m),
            _ => None,
        })
    }

    /// Set the table-level tracked property change (`w:tblPrChange`); `None` clears it.
    pub fn set_table_prop_change(&self, pc: Option<&TablePropChange>) -> Result<()> {
        match pc {
            Some(pc) => write_prop_change(&self.map, TBL_PROP_CHANGE, pc),
            None => {
                self.map.delete(TBL_PROP_CHANGE)?;
                Ok(())
            }
        }
    }
    /// The table-level tracked property change, if any (a [`TablePropSnapshot::Table`]).
    pub fn table_prop_change(&self) -> Result<Option<TablePropChange>> {
        Ok(read_prop_change(&self.map, TBL_PROP_CHANGE, TablePropLevel::Table))
    }

    /// Set a row's tracked structural revision (`w:trPr/w:ins|del`); `None` clears it.
    pub fn set_row_change(&self, row_id: &str, c: Option<&Track>) -> Result<()> {
        let m = self.row_prop_map(row_id)?;
        match c {
            Some(c) => write_track(&m, CHANGE, c),
            None => {
                m.delete(CHANGE)?;
                Ok(())
            }
        }
    }
    /// A row's tracked structural revision, if any.
    pub fn row_change(&self, row_id: &str) -> Result<Option<Track>> {
        Ok(self.row_prop_map_get(row_id)?.and_then(|m| read_track(&m, CHANGE)))
    }
    /// Set a row's tracked property change (`w:trPrChange`); `None` clears it.
    pub fn set_row_prop_change(&self, row_id: &str, pc: Option<&TablePropChange>) -> Result<()> {
        let m = self.row_prop_map(row_id)?;
        match pc {
            Some(pc) => write_prop_change(&m, PROP_CHANGE, pc),
            None => {
                m.delete(PROP_CHANGE)?;
                Ok(())
            }
        }
    }
    /// A row's tracked property change, if any (a [`TablePropSnapshot::Row`]).
    pub fn row_prop_change(&self, row_id: &str) -> Result<Option<TablePropChange>> {
        Ok(self
            .row_prop_map_get(row_id)?
            .and_then(|m| read_prop_change(&m, PROP_CHANGE, TablePropLevel::Row)))
    }

    /// Set a cell's tracked structural revision (`w:tcPr/w:cellIns|cellDel`); `None` clears it.
    pub fn set_cell_change(&self, row_id: &str, col_id: &str, c: Option<&Track>) -> Result<()> {
        let cell = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
        match c {
            Some(c) => write_track(&cell, CHANGE, c),
            None => {
                cell.delete(CHANGE)?;
                Ok(())
            }
        }
    }
    /// A cell's tracked structural revision, if any.
    pub fn cell_change(&self, row_id: &str, col_id: &str) -> Result<Option<Track>> {
        Ok(self.cell_get(row_id, col_id)?.and_then(|cell| read_track(&cell, CHANGE)))
    }
    /// Set a cell's tracked property change (`w:tcPrChange`); `None` clears it.
    pub fn set_cell_prop_change(&self, row_id: &str, col_id: &str, pc: Option<&TablePropChange>) -> Result<()> {
        let cell = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
        match pc {
            Some(pc) => write_prop_change(&cell, PROP_CHANGE, pc),
            None => {
                cell.delete(PROP_CHANGE)?;
                Ok(())
            }
        }
    }
    /// A cell's tracked property change, if any (a [`TablePropSnapshot::Cell`]).
    pub fn cell_prop_change(&self, row_id: &str, col_id: &str) -> Result<Option<TablePropChange>> {
        Ok(self
            .cell_get(row_id, col_id)?
            .and_then(|cell| read_prop_change(&cell, PROP_CHANGE, TablePropLevel::Cell)))
    }

    /// Restore the table-level tracked props from a [`TablePropSnapshot::Table`] - the reject path for a
    /// `w:tblPrChange` (sets style / borders / default cell margins to the before-state exactly). A
    /// non-Table snapshot is a no-op (callers pair the right level).
    pub fn restore_table_props(&self, s: &TablePropSnapshot) -> Result<()> {
        if let TablePropSnapshot::Table { style, borders, cell_margins } = s {
            self.set_style(style.as_deref())?;
            self.set_table_borders(borders)?;
            self.set_table_cell_margins(*cell_margins)?;
        }
        Ok(())
    }
    /// Restore a row's tracked props from a [`TablePropSnapshot::Row`] (the `w:trPrChange` reject).
    pub fn restore_row_props(&self, row_id: &str, s: &TablePropSnapshot) -> Result<()> {
        if let TablePropSnapshot::Row { height, height_exact } = s {
            match height {
                Some(h) => self.set_row_height(row_id, *h, *height_exact)?,
                None => self.clear_row_height(row_id)?,
            }
        }
        Ok(())
    }
    /// Restore a cell's tracked props from a [`TablePropSnapshot::Cell`] (the `w:tcPrChange` reject):
    /// width / span / vMerge / borders / margins / shading set to the before-state exactly (cleared when
    /// the snapshot has none).
    pub fn restore_cell_props(&self, row_id: &str, col_id: &str, s: &TablePropSnapshot) -> Result<()> {
        if let TablePropSnapshot::Cell { width, grid_span, vmerge, borders, margins, shading } = s {
            let cell = self.cells()?.ensure_mergeable_map(&cell_key(row_id, col_id))?;
            match width {
                Some(w) => cell.insert(W, *w as i64)?,
                None => cell.delete(W)?,
            }
            if *grid_span > 1 {
                cell.insert(GRID_SPAN, *grid_span as i64)?;
            } else {
                cell.delete(GRID_SPAN)?;
            }
            self.set_cell_vmerge(row_id, col_id, *vmerge)?;
            self.set_cell_borders(row_id, col_id, borders)?;
            self.set_cell_margins(row_id, col_id, *margins)?;
            self.set_cell_shading(row_id, col_id, shading.as_deref())?;
        }
        Ok(())
    }
}

/// Write an [`EdgeBorders`] into a nested `LoroMap` under `key` on `parent` (per-edge child maps of
/// `{sz, color}`, keyed by OOXML edge name). An all-empty edge-set deletes the key. Each present edge
/// is rewritten; edges that became absent are cleared, so this is a proper setter (LWW per edge).
fn write_edge_borders(parent: &LoroMap, key: &str, e: &EdgeBorders) -> Result<()> {
    let edges = [&e.top, &e.left, &e.bottom, &e.right, &e.inside_h, &e.inside_v];
    if edges.iter().all(|b| b.is_none()) {
        parent.delete(key)?;
        return Ok(());
    }
    let m: LoroMap = parent.get_or_create_container(key, LoroMap::new())?;
    for (name, b) in EDGE_KEYS.iter().zip(edges) {
        match b {
            Some(b) => {
                let em: LoroMap = m.get_or_create_container(name, LoroMap::new())?;
                em.insert(B_SZ, b.size_eighths as i64)?;
                em.insert(B_COLOR, b.color.as_str())?;
            }
            None => {
                m.delete(name)?;
            }
        }
    }
    Ok(())
}

/// Read an [`EdgeBorders`] from a nested `LoroMap` under `key` on `parent` (the inverse of
/// [`write_edge_borders`]); a missing key / edge yields `None` for that edge.
fn read_edge_borders(parent: &LoroMap, key: &str) -> EdgeBorders {
    let Some(ValueOrContainer::Container(Container::Map(m))) = parent.get(key) else {
        return EdgeBorders::default();
    };
    let edge = |name: &str| -> Option<Border> {
        let Some(ValueOrContainer::Container(Container::Map(em))) = m.get(name) else { return None };
        let size_eighths = match em.get(B_SZ) {
            Some(ValueOrContainer::Value(LoroValue::I64(n))) => n as u16,
            _ => 0,
        };
        let color = match em.get(B_COLOR) {
            Some(ValueOrContainer::Value(LoroValue::String(s))) => s.to_string(),
            _ => String::new(),
        };
        Some(Border { size_eighths, color })
    };
    EdgeBorders {
        top: edge("top"),
        left: edge("left"),
        bottom: edge("bottom"),
        right: edge("right"),
        inside_h: edge("insideH"),
        inside_v: edge("insideV"),
    }
}

/// Write a [`CellMargins`] into a nested `LoroMap` under `key` on `parent` (only the SET sides -
/// unset sides are absent keys, so partial margins round-trip partial); `None` deletes the key.
fn write_margins(parent: &LoroMap, key: &str, m: Option<CellMargins>) -> Result<()> {
    let Some(m) = m else {
        parent.delete(key)?;
        return Ok(());
    };
    let mm: LoroMap = parent.get_or_create_container(key, LoroMap::new())?;
    let vals = [m.top, m.left, m.bottom, m.right];
    for (name, v) in MARGIN_KEYS.iter().zip(vals) {
        match v {
            Some(v) => mm.insert(name, v as i64)?,
            None => mm.delete(name)?,
        }
    }
    Ok(())
}

/// Read a [`CellMargins`] from a nested `LoroMap` under `key` on `parent` (the inverse of
/// [`write_margins`]).
fn read_margins(parent: &LoroMap, key: &str) -> Option<CellMargins> {
    let Some(ValueOrContainer::Container(Container::Map(mm))) = parent.get(key) else { return None };
    let g = |k: &str| match mm.get(k) {
        Some(ValueOrContainer::Value(LoroValue::I64(n))) => Some(n as u32),
        _ => None,
    };
    Some(CellMargins { top: g("top"), left: g("left"), bottom: g("bottom"), right: g("right") })
}

// ── small LoroMap value readers ────────────────────────────────────────────────────────────────

fn map_string(m: &LoroMap, k: &str) -> Option<String> {
    match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
        _ => None,
    }
}
fn map_i64(m: &LoroMap, k: &str) -> i64 {
    match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::I64(n))) => n,
        _ => 0,
    }
}
fn map_i64_opt(m: &LoroMap, k: &str) -> Option<i64> {
    match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::I64(n))) => Some(n),
        _ => None,
    }
}
fn map_bool(m: &LoroMap, k: &str) -> bool {
    matches!(m.get(k), Some(ValueOrContainer::Value(LoroValue::Bool(true))))
}

/// The stored string form of a vMerge role (`"none"` / `"restart"` / `"continue"`) - used inside a
/// cell property-change snapshot, where `None` must be stored explicitly (unlike the live cell, where
/// absence of the key means `None`).
fn vmerge_str(v: VMerge) -> &'static str {
    match v {
        VMerge::None => "none",
        VMerge::Restart => "restart",
        VMerge::Continue => "continue",
    }
}
fn vmerge_from(s: Option<&str>) -> VMerge {
    match s {
        Some("restart") => VMerge::Restart,
        Some("continue") => VMerge::Continue,
        _ => VMerge::None,
    }
}

/// Write a structural [`Track`] (row/cell `w:ins`/`w:del` revision) into a nested map under `key`. Only
/// the Ins/Del distinction round-trips (the only kinds a row/cell structural revision takes).
fn write_track(parent: &LoroMap, key: &str, t: &Track) -> Result<()> {
    let m: LoroMap = parent.get_or_create_container(key, LoroMap::new())?;
    m.insert(T_KIND, if t.kind == TrackKind::Del { "del" } else { "ins" })?;
    m.insert(T_ID, t.id as i64)?;
    m.insert(T_AUTHOR, t.author.as_str())?;
    m.insert(T_DATE, t.date.as_str())?;
    Ok(())
}
/// Read a structural [`Track`] written by [`write_track`].
fn read_track(parent: &LoroMap, key: &str) -> Option<Track> {
    let Some(ValueOrContainer::Container(Container::Map(m))) = parent.get(key) else { return None };
    let kind = match map_string(&m, T_KIND).as_deref() {
        Some("del") => TrackKind::Del,
        _ => TrackKind::Ins,
    };
    Some(Track {
        kind,
        author: map_string(&m, T_AUTHOR).unwrap_or_default(),
        date: map_string(&m, T_DATE).unwrap_or_default(),
        id: map_i64(&m, T_ID) as u64,
    })
}

/// Write a [`TablePropChange`] (author/date/id + the before-properties snapshot) into a nested map
/// under `key`. The snapshot's fields are stored flat; the level (which variant) is supplied by the
/// caller on read (it is fixed by where the change is attached).
fn write_prop_change(parent: &LoroMap, key: &str, pc: &TablePropChange) -> Result<()> {
    let m: LoroMap = parent.get_or_create_container(key, LoroMap::new())?;
    m.insert(T_AUTHOR, pc.author.as_str())?;
    m.insert(T_DATE, pc.date.as_str())?;
    m.insert(T_ID, pc.id as i64)?;
    let old: LoroMap = m.get_or_create_container(OLD, LoroMap::new())?;
    match &pc.old {
        TablePropSnapshot::Table { style, borders, cell_margins } => {
            match style {
                Some(s) => old.insert(TBL_STYLE, s.as_str())?,
                None => old.delete(TBL_STYLE)?,
            }
            write_edge_borders(&old, CELL_BORDERS, borders)?;
            write_margins(&old, CELL_MARGINS, *cell_margins)?;
        }
        TablePropSnapshot::Row { height, height_exact } => {
            match height {
                Some(h) => old.insert(ROW_H, *h as i64)?,
                None => old.delete(ROW_H)?,
            }
            old.insert(ROW_EXACT, *height_exact)?;
        }
        TablePropSnapshot::Cell { width, grid_span, vmerge, borders, margins, shading } => {
            match width {
                Some(w) => old.insert(W, *w as i64)?,
                None => old.delete(W)?,
            }
            old.insert(GRID_SPAN, *grid_span as i64)?;
            old.insert(VMERGE, vmerge_str(*vmerge))?;
            write_edge_borders(&old, CELL_BORDERS, borders)?;
            write_margins(&old, CELL_MARGINS, *margins)?;
            match shading {
                Some(s) => old.insert(CELL_SHADING, s.as_str())?,
                None => old.delete(CELL_SHADING)?,
            }
        }
    }
    Ok(())
}
/// Read a [`TablePropChange`] written by [`write_prop_change`]; `level` selects the snapshot variant.
fn read_prop_change(parent: &LoroMap, key: &str, level: TablePropLevel) -> Option<TablePropChange> {
    let Some(ValueOrContainer::Container(Container::Map(m))) = parent.get(key) else { return None };
    let Some(ValueOrContainer::Container(Container::Map(old))) = m.get(OLD) else { return None };
    let snapshot = match level {
        TablePropLevel::Table => TablePropSnapshot::Table {
            style: map_string(&old, TBL_STYLE),
            borders: read_edge_borders(&old, CELL_BORDERS),
            cell_margins: read_margins(&old, CELL_MARGINS),
        },
        TablePropLevel::Row => TablePropSnapshot::Row {
            height: map_i64_opt(&old, ROW_H).map(|n| n as u32),
            height_exact: map_bool(&old, ROW_EXACT),
        },
        TablePropLevel::Cell => TablePropSnapshot::Cell {
            width: map_i64_opt(&old, W).map(|n| n as u32),
            grid_span: map_i64(&old, GRID_SPAN).max(1) as usize,
            vmerge: vmerge_from(map_string(&old, VMERGE).as_deref()),
            borders: read_edge_borders(&old, CELL_BORDERS),
            margins: read_margins(&old, CELL_MARGINS),
            shading: map_string(&old, CELL_SHADING),
        },
    };
    Some(TablePropChange {
        author: map_string(&m, T_AUTHOR).unwrap_or_default(),
        date: map_string(&m, T_DATE).unwrap_or_default(),
        id: map_i64(&m, T_ID) as u64,
        old: snapshot,
    })
}

/// The sparse cell-map key for a coordinate.
fn cell_key(row_id: &str, col_id: &str) -> String {
    format!("{row_id}:{col_id}")
}

/// Read a movable list of strings in order (skipping any non-string entry).
fn list_strings(list: &LoroMovableList) -> Vec<String> {
    let mut out = Vec::with_capacity(list.len());
    for i in 0..list.len() {
        if let Some(ValueOrContainer::Value(LoroValue::String(s))) = list.get(i) {
            out.push(s.to_string());
        }
    }
    out
}

/// Whether a container value is a table grid (its map carries a `row_order`) - used later to find
/// tables in a generic container walk.
pub fn is_table_grid(v: &ValueOrContainer) -> bool {
    matches!(v, ValueOrContainer::Container(Container::Map(m)) if m.get(ROW_ORDER).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loro::{ExportMode, LoroDoc};

    fn build_2x2(doc: &LoroDoc) -> Result<TableGrid> {
        let g = TableGrid::open(doc.get_map("t"))?;
        g.push_row("r1")?;
        g.push_row("r2")?;
        g.push_col("c1")?;
        g.push_col("c2")?;
        g.set_cell("r1", "c1", "A1")?;
        g.set_cell("r1", "c2", "B1")?;
        g.set_cell("r2", "c1", "A2")?;
        g.set_cell("r2", "c2", "B2")?;
        doc.commit();
        Ok(g)
    }

    #[test]
    fn builds_and_reads_a_grid() -> Result<()> {
        let doc = LoroDoc::new();
        let g = build_2x2(&doc)?;
        assert_eq!(g.dims()?, (2, 2));
        assert_eq!(g.row_ids()?, ["r1", "r2"]);
        assert_eq!(g.col_ids()?, ["c1", "c2"]);
        assert_eq!(g.grid_text()?, [["A1", "B1"], ["A2", "B2"]]);
        Ok(())
    }

    fn fork(doc: &LoroDoc) -> Result<LoroDoc> {
        let d = LoroDoc::new();
        d.import(&doc.export(ExportMode::Snapshot)?).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(d)
    }
    fn sync(a: &LoroDoc, b: &LoroDoc) -> Result<()> {
        a.import(&b.export(ExportMode::Snapshot)?).map_err(|e| anyhow::anyhow!("{e}"))?;
        b.import(&a.export(ExportMode::Snapshot)?).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    }

    /// THE money test: two peers insert a column at the same index concurrently. Both columns survive,
    /// every row stays consistent (no ragged rows), and the two replicas converge to an identical
    /// column order - the property the per-row node-tree model cannot give.
    #[test]
    fn concurrent_column_insert_converges() -> Result<()> {
        let a = LoroDoc::new();
        let ga = build_2x2(&a)?;

        let b = fork(&a)?;
        let gb = TableGrid::open(b.get_map("t"))?;

        // Concurrent: A inserts "cA" between c1 and c2; B inserts "cB" at the same spot.
        ga.insert_col(1, "cA")?;
        a.commit();
        gb.insert_col(1, "cB")?;
        b.commit();

        sync(&a, &b)?;
        let cols_a = ga.col_ids()?;
        assert_eq!(cols_a, gb.col_ids()?, "replicas converged on one column order");
        assert_eq!(cols_a.len(), 4, "both inserted columns survive (c1, cA, cB, c2 in some order)");
        assert!(cols_a.contains(&"cA".to_string()) && cols_a.contains(&"cB".to_string()));
        // The grid stays rectangular: every row sees exactly the converged column set.
        for line in ga.grid_text()? {
            assert_eq!(line.len(), 4, "no ragged row");
        }
        Ok(())
    }

    /// Two peers insert a row at the same index concurrently - both survive, deterministic order.
    #[test]
    fn concurrent_row_insert_converges() -> Result<()> {
        let a = LoroDoc::new();
        let ga = build_2x2(&a)?;
        let b = fork(&a)?;
        let gb = TableGrid::open(b.get_map("t"))?;

        ga.insert_row(1, "rA")?;
        a.commit();
        gb.insert_row(1, "rB")?;
        b.commit();

        sync(&a, &b)?;
        assert_eq!(ga.row_ids()?, gb.row_ids()?, "row order converged");
        assert_eq!(ga.row_ids()?.len(), 4);
        Ok(())
    }

    /// Concurrent first-touch of the *same* cell by two peers converges to one cell (no clobber that
    /// drops a peer's write outright; the cell content is a normal CRDT merge afterwards).
    #[test]
    fn concurrent_cell_first_touch_converges() -> Result<()> {
        let a = LoroDoc::new();
        let ga = build_2x2(&a)?;
        // Add an empty column on both, synced, so the cell is unmaterialized on both.
        ga.push_col("c3")?;
        a.commit();
        let b = fork(&a)?;
        let gb = TableGrid::open(b.get_map("t"))?;

        ga.set_cell("r1", "c3", "fromA")?;
        a.commit();
        gb.set_cell("r1", "c3", "fromB")?;
        b.commit();

        sync(&a, &b)?;
        // Converged to ONE cell container; the block list merged both first-touch writes (no
        // clobber-to-nothing). Both replicas agree.
        let ta = ga.cell_text("r1", "c3")?.expect("cell materialized");
        assert_eq!(Some(ta.clone()), gb.cell_text("r1", "c3")?, "cell content converged");
        assert!(
            ta.contains("fromA") && ta.contains("fromB"),
            "both concurrent first-touch writes preserved (no clobber): {ta}"
        );
        Ok(())
    }

    /// A cell holds real multi-paragraph block content (the document's `Run`/style model), round-tripped.
    #[test]
    fn cell_holds_multiple_paragraphs() -> Result<()> {
        let doc = LoroDoc::new();
        let g = build_2x2(&doc)?;
        g.set_cell_paragraphs(
            "r1",
            "c1",
            &[
                Paragraph {
                    style: Some("Heading1".into()),
                    props: ParaProps::default(),
                    runs: vec![Run::plain("Title")],
                    prop_change: None,
                    mark_change: None,
                },
                Paragraph {
                    style: None,
                    props: ParaProps::default(),
                    runs: vec![Run::plain("Body line.")],
                    prop_change: None,
                    mark_change: None,
                },
            ],
        )?;
        doc.commit();

        let paras = g.cell_paragraphs("r1", "c1")?;
        assert_eq!(paras.len(), 2);
        assert_eq!(paras[0].style.as_deref(), Some("Heading1"));
        assert_eq!(paras[0].runs.iter().map(|r| r.text.as_str()).collect::<String>(), "Title");
        assert_eq!(paras[1].runs.iter().map(|r| r.text.as_str()).collect::<String>(), "Body line.");
        assert_eq!(g.cell_text("r1", "c1")?.as_deref(), Some("Title\nBody line."));
        Ok(())
    }

    /// Structural geometry (cell span / vMerge / width, table style, column width, row height)
    /// round-trips, has sane defaults, and concurrent per-key property edits converge.
    #[test]
    fn structural_properties_round_trip_and_converge() -> Result<()> {
        let a = LoroDoc::new();
        let ga = build_2x2(&a)?;
        ga.set_cell_grid_span("r1", "c1", 2)?;
        ga.set_cell_vmerge("r2", "c1", VMerge::Restart)?;
        ga.set_cell_width("r1", "c1", 1440)?;
        ga.set_style(Some("TableGrid"))?;
        ga.set_col_width("c1", 2000)?;
        ga.set_row_height("r1", 300, true)?;
        a.commit();

        assert_eq!(ga.cell_grid_span("r1", "c1")?, 2);
        assert_eq!(ga.cell_vmerge("r2", "c1")?, VMerge::Restart);
        assert_eq!(ga.cell_width("r1", "c1")?, Some(1440));
        assert_eq!(ga.style()?.as_deref(), Some("TableGrid"));
        assert_eq!(ga.col_width("c1")?, Some(2000));
        assert_eq!(ga.row_height("r1")?, Some((300, true)));
        // Defaults for untouched cells.
        assert_eq!(ga.cell_grid_span("r1", "c2")?, 1);
        assert_eq!(ga.cell_vmerge("r1", "c2")?, VMerge::None);
        assert_eq!(ga.cell_width("r1", "c2")?, None);

        // Concurrent property edits on different keys converge (LWW per key).
        let b = fork(&a)?;
        let gb = TableGrid::open(b.get_map("t"))?;
        ga.set_col_width("c2", 1111)?;
        a.commit();
        gb.set_cell_grid_span("r2", "c2", 3)?;
        b.commit();
        sync(&a, &b)?;
        assert_eq!(ga.col_width("c2")?, gb.col_width("c2")?);
        assert_eq!(ga.col_width("c2")?, Some(1111));
        assert_eq!(ga.cell_grid_span("r2", "c2")?, gb.cell_grid_span("r2", "c2")?);
        assert_eq!(ga.cell_grid_span("r2", "c2")?, 3);
        Ok(())
    }

    /// The container model exports to `<w:tbl>` OOXML (tables-crdt T2 codec): style, grid columns,
    /// rows/cells, and cell content all serialize.
    #[test]
    fn exports_to_ooxml_tbl() -> Result<()> {
        let doc = LoroDoc::new();
        let g = build_2x2(&doc)?;
        g.set_style(Some("TableGrid"))?;
        g.set_col_width("c1", 2000)?;
        g.set_col_width("c2", 3000)?;
        doc.commit();

        let xml = crate::model::export_table_grid(&g)?;
        assert!(xml.starts_with("<w:tbl>") && xml.ends_with("</w:tbl>"));
        assert!(xml.contains("<w:tblStyle w:val=\"TableGrid\"/>"));
        assert!(xml.contains("<w:gridCol w:w=\"2000\"/>"));
        assert!(xml.contains("<w:gridCol w:w=\"3000\"/>"));
        assert!(xml.contains("A1") && xml.contains("B2"));
        assert_eq!(xml.matches("<w:tr>").count(), 2, "two rows");
        assert_eq!(xml.matches("<w:tc>").count(), 4, "2x2 cells");
        Ok(())
    }

    /// A horizontal span absorbs the covered column on export: a span-2 cell + a normal cell in a
    /// 3-column row serialize as TWO `<w:tc>` (not three), with `w:gridSpan`.
    #[test]
    fn export_respects_grid_span() -> Result<()> {
        let doc = LoroDoc::new();
        let g = TableGrid::open(doc.get_map("t"))?;
        g.push_row("r1")?;
        g.push_col("c1")?;
        g.push_col("c2")?;
        g.push_col("c3")?;
        g.set_cell("r1", "c1", "wide")?; // spans c1 + c2
        g.set_cell_grid_span("r1", "c1", 2)?;
        g.set_cell("r1", "c3", "z")?; // c2 is covered -> no cell of its own
        doc.commit();

        let xml = crate::model::export_table_grid(&g)?;
        assert!(xml.contains("<w:gridSpan w:val=\"2\"/>"));
        assert_eq!(xml.matches("<w:tc>").count(), 2, "span absorbs the covered column");
        Ok(())
    }

    /// The import projection lifts an in-memory `Table` + its row-major cell paragraphs into the grid
    /// containers (reusing the existing parser), then reads + exports cleanly.
    #[test]
    fn imports_an_in_memory_table_into_the_grid() -> Result<()> {
        use crate::{Table, TableCell, TableRow};
        let cell = || TableCell { para_count: 1, grid_span: 1, ..Default::default() };
        let table = Table {
            col_widths: vec![2000, 3000],
            rows: vec![
                TableRow { cells: vec![cell(), cell()], ..Default::default() },
                TableRow { cells: vec![cell(), cell()], ..Default::default() },
            ],
            style: Some("TableGrid".into()),
            ..Default::default()
        };
        let para = |t: &str| Paragraph {
            style: None,
            props: ParaProps::default(),
            runs: vec![Run::plain(t)],
            prop_change: None,
            mark_change: None,
        };
        let cell_paras = vec![para("A1"), para("B1"), para("A2"), para("B2")];

        let doc = LoroDoc::new();
        let g = TableGrid::open(doc.get_map("t"))?;
        crate::model::populate_grid_from_table(&g, &table, &cell_paras)?;
        doc.commit();

        assert_eq!(g.dims()?, (2, 2));
        assert_eq!(g.row_ids()?, ["r0", "r1"]);
        assert_eq!(g.col_ids()?, ["c0", "c1"]);
        assert_eq!(g.grid_text()?, [["A1", "B1"], ["A2", "B2"]]);
        assert_eq!(g.col_width("c0")?, Some(2000));
        assert_eq!(g.style()?.as_deref(), Some("TableGrid"));

        // It exports back to <w:tbl>.
        let xml = crate::model::export_table_grid(&g)?;
        assert!(xml.contains("A1") && xml.contains("B2"));
        assert_eq!(xml.matches("<w:tr>").count(), 2);
        Ok(())
    }

    /// A table is a first-class tree node interleaved with paragraphs in document order: the paragraph
    /// flat index (read_paragraphs) counts only paragraphs (cell content lives inside the table node),
    /// while body_nodes gives the interleaved order. This is the main-path representation tables-crdt
    /// migrates onto.
    #[test]
    fn table_node_interleaves_in_the_body() -> Result<()> {
        use crate::model::{append_paragraph, body_nodes, create_table_node, open_table_grid, read_paragraphs, BodyNode};

        let doc = LoroDoc::new();
        append_paragraph(&doc, &[Run::plain("Intro")], None)?;
        let tnode = create_table_node(&doc)?;
        {
            let g = open_table_grid(&doc, tnode)?;
            g.push_row("r0")?;
            g.push_col("c0")?;
            g.push_col("c1")?;
            g.set_cell("r0", "c0", "A")?;
            g.set_cell("r0", "c1", "B")?;
        }
        append_paragraph(&doc, &[Run::plain("Outro")], None)?;
        doc.commit();

        // The paragraph flat index descends into the table node's grid: top-level paragraphs plus the
        // cell paragraphs (row-major), in document order.
        let texts: Vec<String> = read_paragraphs(&doc)?
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect();
        assert_eq!(texts, ["Intro", "A", "B", "Outro"]);

        // body_nodes gives the interleaved document order.
        let nodes = body_nodes(&doc);
        assert_eq!(nodes.len(), 3);
        assert!(matches!(nodes[0], BodyNode::Paragraph(_)));
        assert!(matches!(nodes[1], BodyNode::Table(_)));
        assert!(matches!(nodes[2], BodyNode::Paragraph(_)));

        // The table node's grid reads back, and exports to <w:tbl>.
        let g = open_table_grid(&doc, tnode)?;
        assert_eq!(g.grid_text()?, [["A", "B"]]);
        assert!(crate::model::export_table_grid(&g)?.contains("<w:tbl>"));
        Ok(())
    }

    /// Borders / margins / shading round-trip through the containers at both table and cell level,
    /// have empty defaults, and clearing removes them (T2.4 property parity).
    #[test]
    fn border_margin_shading_round_trip() -> Result<()> {
        let doc = LoroDoc::new();
        let g = build_2x2(&doc)?;
        let b = |c: &str| Some(Border { size_eighths: 8, color: c.into() });
        let tbl_borders = EdgeBorders {
            top: b("000000"),
            left: b("000000"),
            bottom: b("000000"),
            right: b("000000"),
            inside_h: b("888888"),
            inside_v: b("888888"),
        };
        let cell_borders = EdgeBorders { top: b("FF0000"), bottom: b("FF0000"), ..Default::default() };
        let m = CellMargins { top: Some(15), left: Some(108), bottom: Some(15), right: Some(108) };

        g.set_table_borders(&tbl_borders)?;
        g.set_table_cell_margins(Some(m))?;
        g.set_cell_borders("r1", "c1", &cell_borders)?;
        g.set_cell_margins("r1", "c1", Some(m))?;
        g.set_cell_shading("r1", "c1", Some("FFFF00"))?;
        doc.commit();

        // Defaults for an untouched cell.
        assert_eq!(g.cell_borders("r2", "c2")?.top, None);
        assert_eq!(g.cell_margins("r2", "c2")?, None);
        assert_eq!(g.cell_shading("r2", "c2")?, None);

        // Round-trip.
        let tb = g.table_borders()?;
        assert_eq!(tb.top, b("000000"));
        assert_eq!(tb.inside_h, b("888888"));
        assert_eq!(g.table_cell_margins()?, Some(m));
        let cb = g.cell_borders("r1", "c1")?;
        assert_eq!(cb.top, b("FF0000"));
        assert_eq!(cb.bottom, b("FF0000"));
        assert_eq!(cb.left, None);
        assert_eq!(g.cell_margins("r1", "c1")?, Some(m));
        assert_eq!(g.cell_shading("r1", "c1")?.as_deref(), Some("FFFF00"));

        // Clearing removes them (a proper setter, not append-only).
        g.set_cell_borders("r1", "c1", &EdgeBorders::default())?;
        g.set_cell_margins("r1", "c1", None)?;
        g.set_cell_shading("r1", "c1", None)?;
        doc.commit();
        assert_eq!(g.cell_borders("r1", "c1")?.top, None);
        assert_eq!(g.cell_margins("r1", "c1")?, None);
        assert_eq!(g.cell_shading("r1", "c1")?, None);
        Ok(())
    }

    /// Tracked structural + property revisions round-trip through the grid containers at table / row /
    /// cell level, and clear cleanly (T2.5).
    #[test]
    fn tracked_revisions_round_trip() -> Result<()> {
        let doc = LoroDoc::new();
        let g = build_2x2(&doc)?;
        let tk = |kind, id| Track { kind, author: "Ann".into(), date: "2026-01-02T03:04:05Z".into(), id };
        let tbl_pc = TablePropChange {
            author: "Ann".into(),
            date: "D".into(),
            id: 1,
            old: TablePropSnapshot::Table {
                style: Some("Old".into()),
                borders: EdgeBorders { top: Some(Border { size_eighths: 4, color: "AAAAAA".into() }), ..Default::default() },
                cell_margins: Some(CellMargins {
                    top: Some(1),
                    left: Some(2),
                    bottom: Some(3),
                    right: Some(4),
                }),
            },
        };
        let row_pc = TablePropChange {
            author: "Ann".into(),
            date: "D".into(),
            id: 2,
            old: TablePropSnapshot::Row { height: Some(200), height_exact: false },
        };
        let cell_pc = TablePropChange {
            author: "Ann".into(),
            date: "D".into(),
            id: 3,
            old: TablePropSnapshot::Cell {
                width: Some(720),
                grid_span: 2,
                vmerge: VMerge::Restart,
                borders: EdgeBorders::default(),
                margins: None,
                shading: Some("EEEEEE".into()),
            },
        };

        g.set_table_prop_change(Some(&tbl_pc))?;
        g.set_row_change("r1", Some(&tk(TrackKind::Ins, 10)))?;
        g.set_row_prop_change("r1", Some(&row_pc))?;
        g.set_cell_change("r2", "c1", Some(&tk(TrackKind::Del, 11)))?;
        g.set_cell_prop_change("r2", "c1", Some(&cell_pc))?;
        doc.commit();

        // Defaults: untouched levels report nothing.
        assert!(g.row_change("r2")?.is_none());
        assert!(g.cell_change("r1", "c2")?.is_none());

        // Round-trip.
        let got_tbl = g.table_prop_change()?.expect("table prop change");
        assert_eq!(got_tbl.id, 1);
        assert!(matches!(got_tbl.old, TablePropSnapshot::Table { ref style, .. } if style.as_deref() == Some("Old")));
        let rc = g.row_change("r1")?.expect("row change");
        assert_eq!((rc.kind, rc.id), (TrackKind::Ins, 10));
        let rpc = g.row_prop_change("r1")?.expect("row prop change");
        assert!(matches!(rpc.old, TablePropSnapshot::Row { height: Some(200), height_exact: false }));
        let cc = g.cell_change("r2", "c1")?.expect("cell change");
        assert_eq!((cc.kind, cc.id), (TrackKind::Del, 11));
        let cpc = g.cell_prop_change("r2", "c1")?.expect("cell prop change");
        assert!(matches!(
            cpc.old,
            TablePropSnapshot::Cell { width: Some(720), grid_span: 2, vmerge: VMerge::Restart, .. }
        ));

        // Clearing removes them.
        g.set_row_change("r1", None)?;
        g.set_cell_prop_change("r2", "c1", None)?;
        g.set_table_prop_change(None)?;
        doc.commit();
        assert!(g.row_change("r1")?.is_none());
        assert!(g.cell_prop_change("r2", "c1")?.is_none());
        assert!(g.table_prop_change()?.is_none());
        // The row's other revision data survives a sibling clear.
        assert!(g.row_prop_change("r1")?.is_some());
        Ok(())
    }

    /// T2.6: cell content carries comment + move annotations on its text, and the codec emits the
    /// OOXML range markers (cell-local comments via per-cell spans; moves via the run's track). The
    /// hosting doc must declare the mark styles first (CollabDoc does this in `new()`/import; an
    /// isolated grid test configures them by hand).
    #[test]
    fn cell_comment_and_move_markers_round_trip() -> Result<()> {
        use loro::{ExpandType, StyleConfig, StyleConfigMap};
        let doc = LoroDoc::new();
        let mut styles = StyleConfigMap::new();
        styles.insert("cmt~7".into(), StyleConfig { expand: ExpandType::None });
        styles.insert("mvf".into(), StyleConfig { expand: ExpandType::None });
        doc.config_text_style(styles);

        let g = TableGrid::open(doc.get_map("t"))?;
        g.push_row("r1")?;
        g.push_col("c1")?;
        let commented = Run { comments: vec![7], ..Run::plain("noted") };
        let moved = Run {
            track: Some(Track { kind: TrackKind::MoveFrom, author: "Ann".into(), date: "D".into(), id: 9 }),
            ..Run::plain("relocated")
        };
        g.set_cell_paragraphs(
            "r1",
            "c1",
            &[Paragraph {
                style: None,
                props: ParaProps::default(),
                runs: vec![Run::plain("a "), commented, Run::plain(" b "), moved],
                prop_change: None,
                mark_change: None,
            }],
        )?;
        doc.commit();

        // The marks survive the loro round-trip onto cell text.
        let read = g.cell_paragraphs("r1", "c1")?;
        assert!(read[0].runs.iter().any(|r| r.comments.contains(&7)), "comment mark persisted");
        assert!(
            read[0].runs.iter().any(|r| r.track.as_ref().is_some_and(|t| t.kind == TrackKind::MoveFrom)),
            "move mark persisted"
        );

        // And the codec emits the OOXML range markers.
        let xml = crate::model::export_table_grid(&g)?;
        assert!(xml.contains("<w:commentRangeStart w:id=\"7\"/>"), "{xml}");
        assert!(xml.contains("<w:commentRangeEnd w:id=\"7\"/>"));
        assert!(xml.contains("<w:commentReference w:id=\"7\"/>"));
        // The range-marker pair gets a synthesized id (SYNTH_MARK_ID_BASE) - the wrapper keeps
        // the revision id 9; reusing it for the markers was a document-wide uniqueness violation.
        assert!(xml.contains("<w:moveFromRangeStart w:id=\"900000000\" w:name=\"mv9\""), "{xml}");
        assert!(xml.contains("<w:moveFromRangeEnd w:id=\"900000000\"/>"));
        assert!(xml.contains("<w:moveFrom w:id=\"9\""), "wrapper keeps the revision id: {xml}");
        Ok(())
    }

    /// A move is a first-class op (no duplicate), and converges.
    #[test]
    fn row_move_converges() -> Result<()> {
        let doc = LoroDoc::new();
        let g = build_2x2(&doc)?;
        g.push_row("r3")?;
        doc.commit();
        g.move_row(2, 0)?; // r3 to the front
        doc.commit();
        assert_eq!(g.row_ids()?, ["r3", "r1", "r2"]);
        Ok(())
    }
}
