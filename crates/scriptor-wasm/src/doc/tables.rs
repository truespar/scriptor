//! Table editing from the browser.
//! 
//! Row and column structure, merges and splits, and the cell properties the ribbon
//! exposes. Each call addresses the grid by the paragraph the caret sits in.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Table context for paragraph `para`: `[row, col, rowCount, colCount]` (cell indices), or an
    /// empty array when the paragraph isn't inside a table. Drives the table context menu.
    #[wasm_bindgen(js_name = tableContext)]
    pub fn table_context(&self, para: u32) -> Vec<u32> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Vec::new(); // header/footer have no tables in v1
        }
        match self.doc.table_context(local) {
            Some((r, c, nr, nc)) => vec![r as u32, c as u32, nr as u32, nc as u32],
            None => Vec::new(),
        }
    }

    /// The body paragraph index of the first paragraph of the cell one step forward (`forward=true`) /
    /// backward from the caret's cell, or `-1` when `para` isn't in a cell / is at the table's last /
    /// first cell. Drives Tab / Shift+Tab cell navigation. Body only (header/footer have no tables in v1).
    #[wasm_bindgen(js_name = cellStep)]
    pub fn cell_step(&self, para: u32, forward: bool) -> i32 {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return -1;
        }
        self.doc.cell_step(local, forward).map(|p| p as i32).unwrap_or(-1)
    }

    /// Insert a table row above (`below=false`) / below (`below=true`) the caret's row. With
    /// Track-Changes on it's recorded as a tracked insertion (`w:trPr/w:ins`); otherwise direct.
    /// Returns the new caret paragraph, or `-1` if `para` isn't in a table. Re-layout + re-paint after.
    #[wasm_bindgen(js_name = insertTableRow)]
    pub fn insert_table_row(&self, para: u32, below: bool) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        let out = if self.track_changes {
            self.doc.suggest_insert_table_row(local, below, &self.author_name, &self.now, "table: insert row")
        } else {
            self.doc.insert_table_row(local, below, "table: insert row")
        };
        Ok(out.map_err(to_js)?.map(|c| c as i32).unwrap_or(-1))
    }

    /// Delete the caret's table row. With Track-Changes on it's *marked* (`w:trPr/w:del`, the row
    /// survives until accepted); otherwise it's removed (and the table if it was the last row). Returns
    /// the caret paragraph after the edit, or `-1` if `para` isn't in a table. Re-layout + re-paint.
    #[wasm_bindgen(js_name = deleteTableRow)]
    pub fn delete_table_row(&self, para: u32) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        let out = if self.track_changes {
            self.doc.suggest_delete_table_row(local, &self.author_name, &self.now, "table: delete row")
        } else {
            self.doc.delete_table_row(local, "table: delete row")
        };
        Ok(out.map_err(to_js)?.map(|c| c as i32).unwrap_or(-1))
    }

    /// Insert a table column left (`right=false`) / right (`right=true`) of the caret's cell. With
    /// Track-Changes on it's a tracked insertion (`w:tcPr/w:cellIns` on each new cell); otherwise
    /// direct. Returns the new caret paragraph, or `-1` if not in a table. Re-layout + re-paint after.
    #[wasm_bindgen(js_name = insertTableColumn)]
    pub fn insert_table_column(&self, para: u32, right: bool) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        let out = if self.track_changes {
            self.doc.suggest_insert_table_column(local, right, &self.author_name, &self.now, "table: insert column")
        } else {
            self.doc.insert_table_column(local, right, "table: insert column")
        };
        Ok(out.map_err(to_js)?.map(|c| c as i32).unwrap_or(-1))
    }

    /// Delete the caret's table column. With Track-Changes on it's *marked* (`w:tcPr/w:cellDel` on the
    /// column's cells, retained until accepted); otherwise removed (and the table if it empties).
    /// Returns the caret paragraph after the edit, or `-1` if not in a table. Re-layout + re-paint.
    #[wasm_bindgen(js_name = deleteTableColumn)]
    pub fn delete_table_column(&self, para: u32) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        let out = if self.track_changes {
            self.doc.suggest_delete_table_column(local, &self.author_name, &self.now, "table: delete column")
        } else {
            self.doc.delete_table_column(local, "table: delete column")
        };
        Ok(out.map_err(to_js)?.map(|c| c as i32).unwrap_or(-1))
    }

    /// Move the caret's table row up (`up=true`) / down one position (a direct structural reorder).
    /// Returns the caret paragraph after the move, or `-1` if not in a table / the move runs off the
    /// edge. Re-layout + re-paint after.
    #[wasm_bindgen(js_name = moveTableRow)]
    pub fn move_table_row(&self, para: u32, up: bool) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        Ok(self
            .doc
            .move_table_row(local, up, "table: move row")
            .map_err(to_js)?
            .map(|c| c as i32)
            .unwrap_or(-1))
    }

    /// Move the caret's table column left (`left=true`) / right one position. Returns the caret
    /// paragraph after the move, or `-1` if not in a table / the move runs off the edge.
    #[wasm_bindgen(js_name = moveTableColumn)]
    pub fn move_table_column(&self, para: u32, left: bool) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        Ok(self
            .doc
            .move_table_column(local, left, "table: move column")
            .map_err(to_js)?
            .map(|c| c as i32)
            .unwrap_or(-1))
    }

    /// Merge the caret's cell with the `count - 1` cells to its right (horizontal `w:gridSpan` merge).
    /// Returns the caret after the merge, or `-1` if not in a table / not enough cells. Re-layout after.
    #[wasm_bindgen(js_name = mergeCellsRight)]
    pub fn merge_cells_right(&self, para: u32, count: u32) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        Ok(self
            .doc
            .merge_cells_right(local, count as usize, "table: merge cells")
            .map_err(to_js)?
            .map(|c| c as i32)
            .unwrap_or(-1))
    }

    /// Split (unmerge) the caret's horizontally-merged cell back into single columns. Returns the caret,
    /// or `-1` if not in a table / the cell isn't merged.
    #[wasm_bindgen(js_name = splitCellHorizontal)]
    pub fn split_cell_horizontal(&self, para: u32) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        Ok(self
            .doc
            .split_cell_horizontal(local, "table: split cell")
            .map_err(to_js)?
            .map(|c| c as i32)
            .unwrap_or(-1))
    }

    /// Merge the caret's cell with the `count - 1` cells below it (vertical `w:vMerge` merge). Returns
    /// the caret after the merge, or `-1` if not in a table / not enough rows. Re-layout after.
    #[wasm_bindgen(js_name = mergeCellsDown)]
    pub fn merge_cells_down(&self, para: u32, count: u32) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        Ok(self
            .doc
            .merge_cells_down(local, count as usize, "table: merge cells")
            .map_err(to_js)?
            .map(|c| c as i32)
            .unwrap_or(-1))
    }

    /// Split (unmerge) the caret's vertically-merged cell. Returns the caret, or `-1` if not in a table /
    /// the cell isn't a vertical-merge anchor.
    #[wasm_bindgen(js_name = splitCellVertical)]
    pub fn split_cell_vertical(&self, para: u32) -> Result<i32, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(-1);
        }
        Ok(self
            .doc
            .split_cell_vertical(local, "table: split cell")
            .map_err(to_js)?
            .map(|c| c as i32)
            .unwrap_or(-1))
    }

    /// The caret cell's shading fill (RGB hex, no `#`), or `""` if not in a cell / no shading - lets the
    /// UI pre-select the current colour.
    #[wasm_bindgen(js_name = cellShading)]
    pub fn cell_shading(&self, para: u32) -> String {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return String::new();
        }
        self.doc.cell_shading(local).unwrap_or_default()
    }

    /// Set the caret cell's shading fill (`fill` = RGB hex without `#`; `""` clears it). With
    /// Track-Changes on it's a tracked cell-property change (`w:tcPrChange`); otherwise direct. Returns
    /// whether the caret was in a table cell (so the caller re-layouts). Re-layout + re-paint after.
    #[wasm_bindgen(js_name = setCellShading)]
    pub fn set_cell_shading(&self, para: u32, fill: &str) -> Result<bool, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(false);
        }
        let fill = (!fill.is_empty()).then(|| fill.to_string());
        let out = if self.track_changes {
            self.doc.suggest_cell_shading(local, fill, &self.author_name, &self.now, "table: cell shading")
        } else {
            self.doc.set_cell_shading(local, fill, "table: cell shading")
        };
        out.map_err(to_js)
    }

    /// Set the caret row's height in twips (`twips = 0` clears it; `exact` = exact rule, else at-least).
    /// Tracked as `w:trPrChange` when Track-Changes is on. Returns whether the caret was in a table row.
    #[wasm_bindgen(js_name = setRowHeight)]
    pub fn set_row_height(&self, para: u32, twips: u32, exact: bool) -> Result<bool, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(false);
        }
        let height = (twips > 0).then_some(twips);
        let out = if self.track_changes {
            self.doc.suggest_row_height(local, height, exact, &self.author_name, &self.now, "table: row height")
        } else {
            self.doc.set_row_height(local, height, exact, "table: row height")
        };
        out.map_err(to_js)
    }

    /// Set a uniform single-line border on every edge of the caret's table (`size_eighths` = line
    /// weight in eighths of a point, `0` removes all borders; `color` = RGB hex without `#`). Tracked as
    /// `w:tblPrChange` when Track-Changes is on. Returns whether the caret was in a table.
    #[wasm_bindgen(js_name = setTableBorders)]
    pub fn set_table_borders(&self, para: u32, size_eighths: u32, color: &str) -> Result<bool, JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(false);
        }
        let border = (size_eighths > 0).then(|| scriptor_crdt::Border {
            size_eighths: size_eighths as u16,
            color: if color.is_empty() { "000000".into() } else { color.to_string() },
        });
        let out = if self.track_changes {
            self.doc.suggest_table_borders(local, border, &self.author_name, &self.now, "table: borders")
        } else {
            self.doc.set_table_borders(local, border, "table: borders")
        };
        out.map_err(to_js)
    }
}
