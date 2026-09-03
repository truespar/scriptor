//! Table serialization, from both the grid and the legacy in-memory table.
//! 
//! Also carries the grid codec: populating the loro grid from a `Table` and reading
//! one back, which is what makes table structure a CRDT citizen rather than a blob.

use super::*;

/// Serialize a table (`<w:tbl>`): properties, grid, and each row's cells. Cell paragraphs come from
/// `paras` starting at `*cursor`, which advances by each cell's `para_count` (document order).
/// Re-emit any table captured verbatim from this cell that sat after `n` of the cell's paragraphs.
///
/// A nested table is opaque bytes (the model cannot hold a table inside a cell), so it goes back out
/// exactly as it came in, in its original position. See [`NestedBlock`].
fn push_nested_at(s: &mut String, cell: &TableCell, n: usize) {
    for nb in cell.nested.iter().filter(|nb| nb.after_para == n) {
        s.push_str(&nb.xml);
    }
}

pub(crate) fn tbl_xml(t: &Table, paras: &[Paragraph], cursor: &mut usize, sp: &ExportSpans) -> String {
    let mut s = String::from("<w:tbl>");
    s.push_str(&tblpr_xml(t));
    s.push_str("<w:tblGrid>");
    for w in &t.col_widths {
        s.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
    }
    s.push_str("</w:tblGrid>");
    for row in &t.rows {
        s.push_str("<w:tr>");
        // `w:trPr`: height (if any) then the tracked row revision (CT_TrPr schema order: trHeight
        // precedes ins/del). Emitted only when there's something to put in it.
        let mut trpr = String::new();
        // CT_TrPr order: cantSplit precedes trHeight.
        if row.cant_split {
            trpr.push_str("<w:cantSplit/>");
        }
        if let Some(h) = row.height {
            let rule = if row.height_exact { "exact" } else { "atLeast" };
            trpr.push_str(&format!("<w:trHeight w:val=\"{h}\" w:hRule=\"{rule}\"/>"));
        }
        // CT_TrPr order: jc follows trHeight and precedes the tracked revisions.
        if let Some(j) = &row.justify {
            trpr.push_str(&format!("<w:jc w:val=\"{}\"/>", xml_escape(j)));
        }
        if let Some(c) = &row.change {
            trpr.push_str(&row_change_xml(c));
        }
        // The tracked row-property revision comes last in CT_TrPr (after ins/del).
        if let Some(pc) = &row.prop_change {
            trpr.push_str(&table_prop_change_xml(pc));
        }
        if !trpr.is_empty() {
            s.push_str(&format!("<w:trPr>{trpr}</w:trPr>"));
        }
        for cell in &row.cells {
            s.push_str("<w:tc>");
            s.push_str(&tcpr_xml(cell));
            let mut emitted = 0;
            for i in 0..cell.para_count {
                push_nested_at(&mut s, cell, i);
                if let Some(p) = paras.get(*cursor) {
                    s.push_str(&para_xml(p, *cursor, sp));
                    emitted += 1;
                }
                *cursor += 1;
            }
            push_nested_at(&mut s, cell, cell.para_count);
            if emitted == 0 && cell.nested.is_empty() {
                s.push_str("<w:p/>"); // a cell must contain at least one paragraph
            }
            s.push_str("</w:tc>");
        }
        s.push_str("</w:tr>");
    }
    s.push_str("</w:tbl>");
    s
}

/// Serialize a loro-backed [`TableGrid`](crate::table_crdt::TableGrid) to `<w:tbl>` XML - the
/// container-model export codec (tables-crdt T2). Walks `row_order` x `col_order`, emitting one
/// `<w:tc>` per cell with its gridSpan / vMerge / width, skipping the grid columns a horizontal span
/// absorbs (a spanning cell occupies `gridSpan` columns, so the covered columns get no cell of their
/// own). Cell content comes from each cell's own block paragraphs (reusing the body's paragraph + run
/// serializers). Tracked structural + property revisions (row/cell `w:ins`/`w:del`, `tblPrChange` /
/// `trPrChange` / `tcPrChange`) round-trip via the grid containers, as do cell-local **comment** + move
/// range markers. Cell-anchored **field / bookmark / hyperlink** markers need the document's anchor maps
/// (instr / name / target by id) - use [`export_table_grid_anchored`] for those; this convenience form
/// passes empty maps (comment + move markers still emit from the run marks).
pub fn export_table_grid(grid: &crate::table_crdt::TableGrid) -> Result<String> {
    let empty = std::collections::HashMap::new();
    let no_images = std::collections::HashMap::new();
    let no_raw = std::collections::HashMap::new();
    let ids = IdAlloc::new();
    // A standalone table: compute spans over its own cell paragraphs (no surrounding document), then
    // export against those, starting at flat index 0.
    let all_paras = table_cell_paras(grid)?;
    let (copens, ccloses) = comment_spans(&all_paras);
    let (fopens, fcloses) = field_spans(&all_paras);
    let (bopens, bcloses) = bookmark_spans(&all_paras);
    let sp = ExportSpans {
        ids: &ids,
        copens: &copens,
        ccloses: &ccloses,
        fopens: &fopens,
        fcloses: &fcloses,
        bopens: &bopens,
        bcloses: &bcloses,
        fields: &empty,
        bookmarks: &empty,
        links: &empty,
        images: &no_images,
        raw: &no_raw,
    };
    export_table_grid_anchored(grid, &sp, 0)
}

/// Every cell's paragraphs in row-major order (skipping the columns a horizontal `gridSpan`
/// absorbs) - the flat sequence the export walks, so its span index lines up with the caller's.
fn table_cell_paras(grid: &crate::table_crdt::TableGrid) -> Result<Vec<Paragraph>> {
    let rows = grid.row_ids()?;
    let cols = grid.col_ids()?;
    let mut out = Vec::new();
    for r in &rows {
        let mut ci = 0usize;
        while ci < cols.len() {
            let c = &cols[ci];
            out.extend(grid.cell_paragraphs(r, c)?);
            ci += (grid.cell_grid_span(r, c)?.max(1)) as usize;
        }
    }
    Ok(out)
}

/// [`export_table_grid`] driven by the **document-global** span tables (`sp`) indexed from
/// `flat_start` (the flat paragraph index of this table's first cell paragraph). Using the whole
/// document's spans - not a per-cell or per-table recomputation - is what lets a comment / bookmark /
/// field range that spans cells, rows, or even the body-table boundary open once at its first run and
/// close once at its last. Emitting a fresh range per cell repeated the same id everywhere the range
/// touched, a document-wide uniqueness violation Word and the validator reject. Mirrors `tbl_xml`,
/// which walks the same global `sp` by a flat cursor.
pub(crate) fn export_table_grid_anchored(
    grid: &crate::table_crdt::TableGrid,
    sp: &ExportSpans,
    flat_start: usize,
) -> Result<String> {
    let rows = grid.row_ids()?;
    let cols = grid.col_ids()?;
    let mut flat = flat_start; // flat index of the next cell paragraph into the global spans

    let mut s = String::from("<w:tbl>");

    // tblPr (style + auto width + borders + default cell margins + tracked tblPrChange), in CT_TblPr
    // schema order.
    let mut tblpr = String::new();
    if let Some(style) = grid.style()? {
        tblpr.push_str(&format!("<w:tblStyle w:val=\"{}\"/>", xml_escape(&style)));
    }
    tblpr.push_str("<w:tblW w:w=\"0\" w:type=\"auto\"/>");
    if let Some(j) = grid.justify()? {
        tblpr.push_str(&format!("<w:jc w:val=\"{}\"/>", xml_escape(&j)));
    }
    tblpr.push_str(&edge_borders_xml("tblBorders", &grid.table_borders()?, true));
    if let Some(m) = grid.table_cell_margins()? {
        tblpr.push_str(&cellmar_xml("tblCellMar", &m));
    }
    if let Some(look) = grid.look()? {
        tblpr.push_str(&format!("<w:tblLook{look}/>"));
    }
    // The tracked table-property revision comes last in CT_TblPr.
    if let Some(pc) = grid.table_prop_change()? {
        tblpr.push_str(&table_prop_change_xml(&pc));
    }
    s.push_str(&format!("<w:tblPr>{tblpr}</w:tblPr>"));

    // tblGrid: one gridCol per column (width 0 = unset/auto).
    s.push_str("<w:tblGrid>");
    for c in &cols {
        let w = grid.col_width(c)?.unwrap_or(0);
        s.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
    }
    s.push_str("</w:tblGrid>");

    for r in &rows {
        s.push_str("<w:tr>");
        // `w:trPr` in CT_TrPr schema order: trHeight, then the tracked row revision (ins/del), then
        // the tracked row-property revision - emitted only when non-empty (mirrors the legacy tbl_xml).
        let mut trpr = String::new();
        // CT_TrPr order: cantSplit precedes trHeight.
        if grid.row_cant_split(r)? {
            trpr.push_str("<w:cantSplit/>");
        }
        if let Some((h, exact)) = grid.row_height(r)? {
            let rule = if exact { "exact" } else { "atLeast" };
            trpr.push_str(&format!("<w:trHeight w:val=\"{h}\" w:hRule=\"{rule}\"/>"));
        }
        // CT_TrPr order: jc follows trHeight and precedes the tracked revisions.
        if let Some(j) = grid.row_justify(r)? {
            trpr.push_str(&format!("<w:jc w:val=\"{}\"/>", xml_escape(&j)));
        }
        if let Some(c) = grid.row_change(r)? {
            trpr.push_str(&row_change_xml(&c));
        }
        if let Some(pc) = grid.row_prop_change(r)? {
            trpr.push_str(&table_prop_change_xml(&pc));
        }
        if !trpr.is_empty() {
            s.push_str(&format!("<w:trPr>{trpr}</w:trPr>"));
        }
        let mut ci = 0usize;
        while ci < cols.len() {
            let c = &cols[ci];
            s.push_str("<w:tc>");
            let mut tcpr = String::new();
            if let Some(w) = grid.cell_width(r, c)? {
                tcpr.push_str(&format!("<w:tcW w:w=\"{w}\" w:type=\"dxa\"/>"));
            }
            let span = grid.cell_grid_span(r, c)?;
            if span > 1 {
                tcpr.push_str(&format!("<w:gridSpan w:val=\"{span}\"/>"));
            }
            match grid.cell_vmerge(r, c)? {
                VMerge::Restart => tcpr.push_str("<w:vMerge w:val=\"restart\"/>"),
                VMerge::Continue => tcpr.push_str("<w:vMerge/>"),
                VMerge::None => {}
            }
            tcpr.push_str(&edge_borders_xml("tcBorders", &grid.cell_borders(r, c)?, false));
            if let Some(shd) = grid.cell_shading(r, c)? {
                tcpr.push_str(&format!(
                    "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
                    xml_escape(&shd)
                ));
            }
            if let Some(m) = grid.cell_margins(r, c)? {
                tcpr.push_str(&cellmar_xml("tcMar", &m));
            }
            // The tracked cell revision (cellIns/cellDel) then the tracked cell-property revision come
            // last in CT_TcPr.
            if let Some(ch) = grid.cell_change(r, c)? {
                tcpr.push_str(&cell_change_xml(&ch));
            }
            if let Some(pc) = grid.cell_prop_change(r, c)? {
                tcpr.push_str(&table_prop_change_xml(&pc));
            }
            if !tcpr.is_empty() {
                s.push_str(&format!("<w:tcPr>{tcpr}</w:tcPr>"));
            }
            // Tables nested in this cell, kept verbatim because the model cannot hold one, re-emitted
            // between the cell's paragraphs exactly where they sat. See `NestedBlock`.
            let nested = grid.cell_nested(r, c)?;
            let push_nested = |s: &mut String, n: usize| {
                for nb in nested.iter().filter(|nb| nb.after_para == n) {
                    s.push_str(&nb.xml);
                }
            };
            let paras = grid.cell_paragraphs(r, c)?;
            if paras.is_empty() {
                push_nested(&mut s, 0);
                if nested.is_empty() {
                    s.push_str("<w:p/>"); // a cell must hold at least one paragraph
                }
            } else {
                // Emit cell paragraphs through the body serializer using the document-global spans
                // (`sp`) indexed by the flat cell-paragraph position, so comment / bookmark / field
                // ranges spanning cells (or the body-table boundary) open + close exactly once. Move
                // ranges + hyperlinks stay on the run marks. A picture in a cell re-emits its
                // `<w:drawing>` from the shared image map.
                for (i, p) in paras.iter().enumerate() {
                    push_nested(&mut s, i);
                    s.push_str(&para_xml(p, flat, sp));
                    flat += 1;
                }
                push_nested(&mut s, paras.len());
            }
            s.push_str("</w:tc>");
            ci += (span.max(1)) as usize; // a horizontal span absorbs the next (span-1) columns
        }
        s.push_str("</w:tr>");
    }

    s.push_str("</w:tbl>");
    Ok(s)
}

/// Populate a [`TableGrid`](crate::table_crdt::TableGrid) from an in-memory [`Table`] + its cell
/// paragraphs in row-major order (the import projection, tables-crdt T2). Reuses the existing parser:
/// `import_document_xml` already turns a `<w:tbl>` into a `Table` (structure) + cell paragraphs (in the
/// flat flow); this lifts that into the loro containers - generating stable `r{i}` / `c{i}` ids,
/// mapping each row-major cell to the grid column it starts at (advancing by `gridSpan`, so a spanning
/// cell's covered columns get no cell), and copying content + geometry. `cell_paras` must be exactly the
/// table's cell paragraphs, row-major, cell by cell. Caller commits.
pub fn populate_grid_from_table(
    grid: &crate::table_crdt::TableGrid,
    t: &Table,
    cell_paras: &[Paragraph],
) -> Result<()> {
    // Column count: the grid's declared columns, or the widest row's summed spans.
    let widest = t
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.grid_span.max(1)).sum::<usize>())
        .max()
        .unwrap_or(0);
    let ncols = t.col_widths.len().max(widest);
    let col_ids: Vec<String> = (0..ncols).map(|i| format!("c{i}")).collect();
    for (i, cid) in col_ids.iter().enumerate() {
        grid.push_col(cid)?;
        if let Some(w) = t.col_widths.get(i) {
            grid.set_col_width(cid, *w)?;
        }
    }
    if let Some(style) = &t.style {
        grid.set_style(Some(style))?;
    }
    grid.set_look(t.look.as_deref())?;
    grid.set_justify(t.justify.as_deref())?;
    grid.set_table_borders(&t.borders)?;
    grid.set_table_cell_margins(t.cell_margins)?;
    if let Some(pc) = &t.prop_change {
        grid.set_table_prop_change(Some(pc))?;
    }

    let mut pcursor = 0usize;
    for (ri, row) in t.rows.iter().enumerate() {
        let rid = format!("r{ri}");
        grid.push_row(&rid)?;
        if let Some(h) = row.height {
            grid.set_row_height(&rid, h, row.height_exact)?;
        }
        if let Some(j) = &row.justify {
            grid.set_row_justify(&rid, Some(j))?;
        }
        if row.cant_split {
            grid.set_row_cant_split(&rid, true)?;
        }
        if let Some(c) = &row.change {
            grid.set_row_change(&rid, Some(c))?;
        }
        if let Some(pc) = &row.prop_change {
            grid.set_row_prop_change(&rid, Some(pc))?;
        }
        let mut col_cursor = 0usize;
        for cell in &row.cells {
            let cid = col_ids.get(col_cursor).cloned().unwrap_or_else(|| format!("c{col_cursor}"));
            let span = cell.grid_span.max(1);
            let end = (pcursor + cell.para_count).min(cell_paras.len());
            let paras = cell_paras.get(pcursor..end).unwrap_or(&[]);
            pcursor += cell.para_count;
            grid.set_cell_paragraphs(&rid, &cid, paras)?;
            if cell.grid_span > 1 {
                grid.set_cell_grid_span(&rid, &cid, cell.grid_span as u32)?;
            }
            if cell.vmerge != VMerge::None {
                grid.set_cell_vmerge(&rid, &cid, cell.vmerge)?;
            }
            if let Some(w) = cell.width {
                grid.set_cell_width(&rid, &cid, w)?;
            }
            grid.set_cell_borders(&rid, &cid, &cell.borders)?;
            grid.set_cell_margins(&rid, &cid, cell.margins)?;
            grid.set_cell_shading(&rid, &cid, cell.shading.as_deref())?;
            if let Some(c) = &cell.change {
                grid.set_cell_change(&rid, &cid, Some(c))?;
            }
            if let Some(pc) = &cell.prop_change {
                grid.set_cell_prop_change(&rid, &cid, Some(pc))?;
            }
            if !cell.nested.is_empty() {
                grid.set_cell_nested(&rid, &cid, &cell.nested)?;
            }
            col_cursor += span;
        }
    }
    Ok(())
}

/// Read a loro-backed [`TableGrid`](crate::table_crdt::TableGrid) back into an in-memory [`Table`] - the
/// read projection the derived `body()`, the renderer, and the flat-index locator consume. Mirrors
/// [`export_table_grid`]'s column walk (one visible cell per `<w:tc>`, skipping the grid columns a
/// horizontal `gridSpan` absorbs), and sets each visible cell's `para_count` to its block count - so the
/// visible-cell flat walk (`flat_before_item` / `body_locate` / `cell_flat_start`) lines up exactly with
/// [`block_seq`] (which descends every `(row, col)`; a span-covered column simply has no blocks). The
/// inverse of [`populate_grid_from_table`]; with it, `grid_to_table` round-trips structure + geometry +
/// tracked revisions, so the editing/render layer sees the same `Table` it did from the old in-memory body.
pub fn grid_to_table(grid: &crate::table_crdt::TableGrid) -> Result<Table> {
    let rows = grid.row_ids()?;
    let cols = grid.col_ids()?;
    let col_widths: Vec<u32> =
        cols.iter().map(|c| grid.col_width(c).ok().flatten().unwrap_or(0)).collect();
    let mut out_rows = Vec::with_capacity(rows.len());
    for r in &rows {
        let (height, height_exact) = match grid.row_height(r)? {
            Some((h, e)) => (Some(h), e),
            None => (None, false),
        };
        let mut cells = Vec::new();
        let mut ci = 0usize;
        while ci < cols.len() {
            let c = &cols[ci];
            let span = grid.cell_grid_span(r, c)?.max(1) as usize;
            cells.push(TableCell {
                para_count: grid.cell_block_count(r, c)?,
                grid_span: span,
                vmerge: grid.cell_vmerge(r, c)?,
                borders: grid.cell_borders(r, c)?,
                margins: grid.cell_margins(r, c)?,
                width: grid.cell_width(r, c)?,
                shading: grid.cell_shading(r, c)?,
                change: grid.cell_change(r, c)?,
                prop_change: grid.cell_prop_change(r, c)?,
                nested: grid.cell_nested(r, c)?,
            });
            ci += span;
        }
        out_rows.push(TableRow {
            cells,
            height,
            height_exact,
            cant_split: grid.row_cant_split(r)?,
            justify: grid.row_justify(r)?,
            change: grid.row_change(r)?,
            prop_change: grid.row_prop_change(r)?,
        });
    }
    Ok(Table {
        col_widths,
        rows: out_rows,
        style: grid.style()?,
        justify: grid.justify()?,
        borders: grid.table_borders()?,
        cell_margins: grid.table_cell_margins()?,
        look: grid.look()?,
        prop_change: grid.table_prop_change()?,
    })
}

/// The document body as `Vec<BodyItem>`, derived from the loro block tree ([`body_nodes`]): each
/// top-level paragraph node is a [`BodyItem::Paragraph`], each table node is read from its grid via
/// [`grid_to_table`]. This is the live `body()` projection now that tables are loro citizens (the
/// in-memory `Vec<BodyItem>` is no longer stored). A grid that fails to open / read is skipped.
pub fn node_body(doc: &LoroDoc) -> Vec<BodyItem> {
    let mut out = Vec::new();
    for node in body_nodes(doc) {
        match node {
            BodyNode::Paragraph(_) => out.push(BodyItem::Paragraph),
            BodyNode::Table(id) => {
                if let Ok(grid) = open_table_grid(doc, id)
                    && let Ok(t) = grid_to_table(&grid) {
                        out.push(BodyItem::Table(Box::new(t)));
                    }
            }
        }
    }
    out
}

/// The flat paragraph index of the first paragraph of cell `(row_id, col_id)` in the table node `node`,
/// or `None` if the cell isn't in the flat sequence. Used by the structural edit ops to return the caret
/// after a grid mutation (the new row/column's first cell).
pub(crate) fn cell_first_flat(doc: &LoroDoc, node: TreeID, row_id: &str, col_id: &str) -> Option<usize> {
    block_seq(doc).iter().position(|r| {
        matches!(r, BlockRef::Cell { node: n, row, col, .. } if *n == node && row == row_id && col == col_id)
    })
}

/// The flat paragraph index where table node `node`'s cell paragraphs begin (the count of flat
/// paragraphs before the table in document order), or the sequence length if the table has no cells.
/// The caret falls back here when a structural delete empties (and removes) the table.
pub(crate) fn table_first_flat(doc: &LoroDoc, node: TreeID) -> usize {
    let seq = block_seq(doc);
    seq.iter()
        .position(|r| matches!(r, BlockRef::Cell { node: n, .. } if *n == node))
        .unwrap_or(seq.len())
}

/// Delete a top-level block node (a paragraph node or a `type "table"` node) by id. Used when a
/// structural edit empties a table (its last row / column is removed). Caller commits.
pub(crate) fn delete_block_node(doc: &LoroDoc, id: TreeID) -> Result<()> {
    doc.get_tree(BLOCKS).delete(id)?;
    block_cache_invalidate(); // a top-level block (paragraph or table) was removed
    Ok(())
}

/// An empty paragraph (no style, no props, no runs) - the content a freshly inserted table cell gets.
pub(crate) fn empty_paragraph() -> Paragraph {
    Paragraph { style: None, props: ParaProps::default(), runs: Vec::new(), prop_change: None, mark_change: None }
}

/// The raw attributes of an element, re-serialized as ` k="v" ...` (for verbatim-preserved empty
/// elements like `w:tblLook`). `None` when there are none.
pub(crate) fn raw_attrs(e: &quick_xml::events::BytesStart) -> Option<String> {
    let mut s = String::new();
    for a in e.attributes().flatten() {
        let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        // decoded by hand rather than Attribute::unescape_value: that method
        // disappears when ANY crate in a consumer's build enables quick-xml's
        // `encoding` feature (cargo unifies features), and OOXML part XML is
        // always UTF-8 anyway
        let Ok(raw) = std::str::from_utf8(&a.value) else { continue };
        let Ok(v) = quick_xml::escape::unescape(raw) else { continue };
        s.push_str(&format!(" {k}=\"{}\"", xml_escape(&v)));
    }
    (!s.is_empty()).then_some(s)
}

/// Table-level properties (`<w:tblPr>`), in CT_TblPr schema order.
fn tblpr_xml(t: &Table) -> String {
    let mut inner = String::new();
    if let Some(style) = &t.style {
        inner.push_str(&format!("<w:tblStyle w:val=\"{}\"/>", xml_escape(style)));
    }
    inner.push_str("<w:tblW w:w=\"0\" w:type=\"auto\"/>");
    if let Some(j) = &t.justify {
        inner.push_str(&format!("<w:jc w:val=\"{}\"/>", xml_escape(j)));
    }
    inner.push_str(&edge_borders_xml("tblBorders", &t.borders, true));
    if let Some(m) = t.cell_margins {
        inner.push_str(&cellmar_xml("tblCellMar", &m));
    }
    if let Some(look) = &t.look {
        inner.push_str(&format!("<w:tblLook{look}/>"));
    }
    // The tracked table-property revision comes last in CT_TblPr.
    if let Some(pc) = &t.prop_change {
        inner.push_str(&table_prop_change_xml(pc));
    }
    format!("<w:tblPr>{inner}</w:tblPr>")
}

/// Cell-level properties (`<w:tcPr>`), in CT_TcPr schema order; empty -> "".
fn tcpr_xml(cell: &TableCell) -> String {
    let mut inner = String::new();
    if let Some(w) = cell.width {
        inner.push_str(&format!("<w:tcW w:w=\"{w}\" w:type=\"dxa\"/>"));
    }
    if cell.grid_span > 1 {
        inner.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", cell.grid_span));
    }
    match cell.vmerge {
        VMerge::Restart => inner.push_str("<w:vMerge w:val=\"restart\"/>"),
        VMerge::Continue => inner.push_str("<w:vMerge/>"),
        VMerge::None => {}
    }
    inner.push_str(&edge_borders_xml("tcBorders", &cell.borders, false));
    if let Some(shd) = &cell.shading {
        inner.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
            xml_escape(shd)
        ));
    }
    if let Some(m) = cell.margins {
        inner.push_str(&cellmar_xml("tcMar", &m));
    }
    // The tracked cell revision comes last in CT_TcPr (after tcMar / vAlign), before tcPrChange.
    if let Some(c) = &cell.change {
        inner.push_str(&cell_change_xml(c));
    }
    // The tracked cell-property revision is the very last child of CT_TcPr.
    if let Some(pc) = &cell.prop_change {
        inner.push_str(&table_prop_change_xml(pc));
    }
    if inner.is_empty() {
        String::new()
    } else {
        format!("<w:tcPr>{inner}</w:tcPr>")
    }
}

/// The tracked row-revision marker for `w:trPr` - `w:ins` / `w:del` with id / author / date.
fn row_change_xml(c: &Track) -> String {
    let el = if c.kind == TrackKind::Del { "w:del" } else { "w:ins" };
    format!(
        "<{el} w:id=\"{id}\" w:author=\"{a}\"{d}/>",
        id = c.id,
        a = xml_escape(&c.author),
        d = date_attr(&c.date),
    )
}

/// The tracked cell-revision marker for `w:tcPr` - `w:cellIns` / `w:cellDel` with id / author / date.
fn cell_change_xml(c: &Track) -> String {
    let el = if c.kind == TrackKind::Del { "w:cellDel" } else { "w:cellIns" };
    format!(
        "<{el} w:id=\"{id}\" w:author=\"{a}\"{d}/>",
        id = c.id,
        a = xml_escape(&c.author),
        d = date_attr(&c.date),
    )
}

/// The tracked table-property revision marker (`w:tblPrChange` / `w:trPrChange` / `w:tcPrChange`), the
/// last child of its `w:tblPr` / `w:trPr` / `w:tcPr`. It wraps a nested `w:tblPr` / `w:trPr` / `w:tcPr`
/// holding the OLD properties (the before-state restored on reject) - serialized from the snapshot.
fn table_prop_change_xml(c: &TablePropChange) -> String {
    let (el, inner) = match &c.old {
        TablePropSnapshot::Table { style, borders, cell_margins } => {
            let mut s = String::new();
            if let Some(st) = style {
                s.push_str(&format!("<w:tblStyle w:val=\"{}\"/>", xml_escape(st)));
            }
            s.push_str("<w:tblW w:w=\"0\" w:type=\"auto\"/>");
            s.push_str(&edge_borders_xml("tblBorders", borders, true));
            if let Some(m) = cell_margins {
                s.push_str(&cellmar_xml("tblCellMar", m));
            }
            ("w:tblPrChange", format!("<w:tblPr>{s}</w:tblPr>"))
        }
        TablePropSnapshot::Row { height, height_exact } => {
            let mut s = String::new();
            if let Some(h) = height {
                let rule = if *height_exact { "exact" } else { "atLeast" };
                s.push_str(&format!("<w:trHeight w:val=\"{h}\" w:hRule=\"{rule}\"/>"));
            }
            ("w:trPrChange", format!("<w:trPr>{s}</w:trPr>"))
        }
        TablePropSnapshot::Cell { width, grid_span, vmerge, borders, margins, shading } => {
            let mut s = String::new();
            if let Some(w) = width {
                s.push_str(&format!("<w:tcW w:w=\"{w}\" w:type=\"dxa\"/>"));
            }
            if *grid_span > 1 {
                s.push_str(&format!("<w:gridSpan w:val=\"{grid_span}\"/>"));
            }
            match vmerge {
                VMerge::Restart => s.push_str("<w:vMerge w:val=\"restart\"/>"),
                VMerge::Continue => s.push_str("<w:vMerge/>"),
                VMerge::None => {}
            }
            s.push_str(&edge_borders_xml("tcBorders", borders, false));
            if let Some(shd) = shading {
                s.push_str(&format!(
                    "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
                    xml_escape(shd)
                ));
            }
            if let Some(m) = margins {
                s.push_str(&cellmar_xml("tcMar", m));
            }
            ("w:tcPrChange", format!("<w:tcPr>{s}</w:tcPr>"))
        }
    };
    format!(
        "<{el} w:id=\"{id}\" w:author=\"{a}\"{d}>{inner}</{el}>",
        id = c.id,
        a = xml_escape(&c.author),
        d = date_attr(&c.date),
    )
}

/// Serialize a set of border edges (`<w:tblBorders>` / `<w:tcBorders>`); each present edge is a
/// single line at its stored weight + colour. `inside` adds `insideH`/`insideV` (table level only).
fn edge_borders_xml(tag: &str, edges: &EdgeBorders, inside: bool) -> String {
    let edge = |name: &str, b: &Option<Border>| -> String {
        match b {
            Some(b) => format!(
                "<w:{name} w:val=\"single\" w:sz=\"{}\" w:space=\"0\" w:color=\"{}\"/>",
                b.size_eighths,
                xml_escape(&b.color)
            ),
            None => String::new(),
        }
    };
    let mut s = String::new();
    s.push_str(&edge("top", &edges.top));
    s.push_str(&edge("left", &edges.left));
    s.push_str(&edge("bottom", &edges.bottom));
    s.push_str(&edge("right", &edges.right));
    if inside {
        s.push_str(&edge("insideH", &edges.inside_h));
        s.push_str(&edge("insideV", &edges.inside_v));
    }
    if s.is_empty() {
        String::new()
    } else {
        format!("<w:{tag}>{s}</w:{tag}>")
    }
}

/// Serialize cell margins (`<w:tblCellMar>` / `<w:tcMar>`) - only the sides the source set, in
/// CT_TblCellMar order (top, left, bottom, right), so a partial margin round-trips partial.
fn cellmar_xml(tag: &str, m: &CellMargins) -> String {
    let mut s = format!("<w:{tag}>");
    for (name, v) in [("top", m.top), ("left", m.left), ("bottom", m.bottom), ("right", m.right)] {
        if let Some(v) = v {
            s.push_str(&format!("<w:{name} w:w=\"{v}\" w:type=\"dxa\"/>"));
        }
    }
    s.push_str(&format!("</w:{tag}>"));
    s
}
