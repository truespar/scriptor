//! Table placement, including rows that straddle a page boundary.
//! 
//! Word splits a row across pages by default unless `w:cantSplit` says otherwise, and
//! it splits at line granularity rather than moving the whole row. That is what
//! `split_cell_blocks` implements: cut each cell's blocks at the last line that fits
//! and carry the remainder to the next page.

use crate::*;

impl Renderer {
    /// Lay out one table: scale grid columns to the content width, then place each row's cells with a
    /// page break between rows that overflow. Records each cell's page-local rect + its blocks.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn layout_table(
        &mut self,
        table: &TableData,
        content_w: f32,
        ml: f32,
        mt: f32,
        page_bottom: f32,
        page_h: u32,
        gap: u32,
        page: &mut u32,
        y: &mut f32,
        cells: &mut Vec<CellPlacement>,
        lines: &mut Vec<LineBox>,
        hashes: &mut Vec<u64>,
        change_bars: &mut Vec<ChangeBar>,
    ) {
        let ncols = table.col_widths.len().max(1);
        let authored: f32 = table.col_widths.iter().sum();
        // Absolute grid: Word never scales a table to the column - the authored widths stand, and
        // `w:jc` positions the whole grid (a wider-than-column table spills into the margin).
        let col_px: Vec<f32> = if authored > 0.0 {
            table.col_widths.clone()
        } else {
            vec![content_w / ncols as f32; ncols]
        };
        let total: f32 = col_px.iter().sum();
        let x0 = ml
            + match table.justify {
                1 => (content_w - total) / 2.0,
                2 => content_w - total,
                _ => 0.0,
            };
        // Per grid column: the cells[] index of the open vertical-merge cell (extended by `continue`).
        let mut open: Vec<Option<usize>> = vec![None; col_px.len() + 8];

        for row in &table.rows {
            // Cell grid column, x, width (by grid span).
            let mut specs: Vec<(usize, f32, f32, usize, &CellData)> =
                Vec::with_capacity(row.cells.len());
            let mut cx = x0;
            let mut col = 0usize;
            for cell in &row.cells {
                let span = cell.grid_span.max(1);
                let end = (col + span).min(col_px.len());
                let w: f32 = col_px.get(col..end).map(|s| s.iter().sum()).unwrap_or(0.0);
                let w = if w > 0.5 { w } else { content_w / ncols as f32 };
                specs.push((col, cx, w, span, cell));
                cx += w;
                col += span;
            }

            // Page break BEFORE the row (skip while a vertical merge is open - keep it whole). A
            // `w:pageBreakBefore` cell paragraph (direct or via its style) forces its ROW to the top
            // of a new page, like Word (tdf89377) - with the same "not already at the top"
            // suppression as the body rule.
            let any_open = open.iter().any(|o| o.is_some());
            let row_breaks = specs
                .iter()
                .any(|(_, _, _, _, cell)| !cell.vmerge_continue && cell.blocks.iter().any(|b| b.page_break_before));
            if row_breaks && *y > mt && !any_open {
                *page += 1;
                *y = mt;
            }

            // The row's cell content still to place, parallel to `specs`. Word SPLITS a row whose
            // content exceeds the space left on the page (the default; `w:cantSplit` / an exact
            // height / an open vertical merge keep it whole) - pushing the whole row wasted the
            // page tail and inflated multi-page table documents by whole pages. Each pass of the
            // loop places one fragment: what fits on this page, then the remainder from the top of
            // the next.
            let mut work: Vec<(Vec<Block>, Vec<usize>)> = specs
                .iter()
                .map(|(_, _, _, _, cell)| (cell.blocks.clone(), cell.para_ids.clone()))
                .collect();
            // Each term is a separate reason a row may NOT split; kept as an explicit list of
            // negations because that reads as the Word rules it encodes, not as one folded boolean.
            #[allow(clippy::nonminimal_bool)]
            let splittable = !(row.exact && row.min_height > 0.0)
                && !row.cant_split
                && !any_open
                && !specs.iter().any(|(_, _, _, _, c)| c.vmerge_continue);
            let mut first_fragment = true;
            loop {
                // Height of the content still to place (tallest cell: content + its margins).
                let content_h = {
                    let mut h = 0.0_f32;
                    for (k, (_, _, w, _, cell)) in specs.iter().enumerate() {
                        if cell.vmerge_continue {
                            continue;
                        }
                        let [mtp, mlf, mbt, mrt] = cell.margins;
                        let cw = (w - mlf - mrt).max(1.0);
                        h = h.max(self.stacked_height(&work[k].0, cw) + mtp + mbt);
                    }
                    h
                };
                let row_h = if row.exact && row.min_height > 0.0 {
                    row.min_height
                } else if first_fragment {
                    content_h.max(row.min_height).max(8.0)
                } else {
                    content_h.max(8.0)
                };
                let avail = page_bottom - *y;

                // Decide this pass: the whole remainder (breaking the page first when it doesn't
                // fit and may not split), or a page-filling fragment with the rest carried over.
                let (frag_h, rest) = if row_h <= avail {
                    (row_h, None)
                } else if !splittable {
                    // A row taller than a whole page just overflows (legacy behavior).
                    if *y > mt && !any_open {
                        *page += 1;
                        *y = mt;
                    }
                    (row_h, None)
                } else if avail < 24.0 && *y > mt {
                    // The sliver left is too small to be worth a fragment - start on the next page.
                    *page += 1;
                    *y = mt;
                    continue;
                } else {
                    let mut fit_cells = Vec::with_capacity(specs.len());
                    let mut rest_cells = Vec::with_capacity(specs.len());
                    let mut any_fit = false;
                    for (k, (_, _, w, _, cell)) in specs.iter().enumerate() {
                        if cell.vmerge_continue {
                            fit_cells.push((Vec::new(), Vec::new()));
                            rest_cells.push((Vec::new(), Vec::new()));
                            continue;
                        }
                        let [mtp, mlf, _mbt, mrt] = cell.margins;
                        let cw = (w - mlf - mrt).max(1.0);
                        let (f, r) = self.split_cell_blocks(&work[k].0, &work[k].1, cw, avail - mtp);
                        any_fit |= !f.0.is_empty();
                        fit_cells.push(f);
                        rest_cells.push(r);
                    }
                    if !any_fit {
                        // Nothing fits in the sliver (a tall first line): retry on a fresh page,
                        // or - already at the top - place whole and overflow (can't shrink a line).
                        if *y > mt {
                            *page += 1;
                            *y = mt;
                            continue;
                        }
                        (row_h, None)
                    } else {
                        work = fit_cells;
                        (avail, Some(rest_cells))
                    }
                };

                while hashes.len() <= *page as usize {
                    hashes.push(FNV_OFFSET);
                }
                hashes[*page as usize] = fnv_bytes(hashes[*page as usize], &(*y as i64).to_le_bytes());

                for (k, &(col0, cx, w, span, cell)) in specs.iter().enumerate() {
                    if cell.vmerge_continue {
                        // Absorbed into the cell above in this column: extend its height.
                        if let Some(idx) = open.get(col0).copied().flatten() {
                            cells[idx].h += frag_h;
                        }
                        continue;
                    }
                    let (frag_blocks, frag_ids) = &work[k];
                    hashes[*page as usize] = fnv_bytes(hashes[*page as usize], &(cx as i64).to_le_bytes());
                    let idx = cells.len();
                    cells.push(CellPlacement {
                        page: *page,
                        x: cx,
                        y: *y,
                        w,
                        h: frag_h,
                        margins: cell.margins,
                        borders: cell.borders,
                        shading: cell.shading,
                        blocks: frag_blocks.clone(),
                        para_ids: frag_ids.clone(),
                    });
                    for c in col0..(col0 + span).min(open.len()) {
                        open[c] = if cell.vmerge_restart { Some(idx) } else { None };
                    }

                    // Caret geometry for the cell's paragraphs: stack them inside the cell content
                    // box exactly as `paint_cell` does, and shape each into visual lines (absolute y
                    // for the overlay). NB: cell paragraphs are painted by the cell pass (via
                    // `CellPlacement`), NOT the body-placement pass - so we emit `lines` (for
                    // hit-test/caret) but no `Placement` (that would double-paint them as full-width
                    // body blocks). Fold the content into the page fingerprint so editing a cell
                    // repaints its page. A continuation fragment's blocks carry `byte_offset` so the
                    // caret stops stay paragraph-global.
                    let [mtp, mlf, _mbt, mrt] = cell.margins;
                    let cw = (w - mlf - mrt).max(1.0);
                    let cx_content = cx + mlf;
                    let page_origin = (*page * (page_h + gap)) as f32;
                    let mut yy = *y + mtp; // page-local
                    let mut prev_blk: Option<&Block> = None;
                    for (bi, block) in frag_blocks.iter().enumerate() {
                        // Consolidated gap - the same stacking as `stacked_height` / `paint_cell`,
                        // so the caret geometry matches the painted lines.
                        yy += stack_gap(prev_blk, block);
                        prev_blk = Some(block);
                        let para_id = frag_ids.get(bi).copied().unwrap_or(usize::MAX);
                        hashes[*page as usize] = fold_block(hashes[*page as usize], block, yy);
                        // A cell paragraph's `w:ind` indents within the cell's text column, exactly
                        // like the body pass (the NOBA price rows indent left=300tw so their
                        // floating checkbox sits in front of the text instead of under it).
                        let il = block.indent_left_px.max(0.0);
                        let ir = block.indent_right_px.max(0.0);
                        let bx = cx_content + il;
                        let bw = (cw - il - ir).max(1.0);
                        let is_empty = block.spans.iter().all(|s| s.text.is_empty());
                        let (bh, geom) = if is_empty {
                            (empty_line_height(block), Vec::new())
                        } else {
                            self.shape_block_lines(block, bw, bx)
                        };
                        let abs_top = page_origin + yy;
                        // Change-bars for a tracked paragraph inside a cell: drawn at the page's
                        // left margin (Word bars at the margin, not the cell), beside only the
                        // changed visual lines.
                        if is_empty {
                            lines.push(LineBox {
                                para: para_id,
                                y: abs_top,
                                height: empty_line_height(block),
                                stops: vec![CaretStop { byte: block.byte_offset, x: bx }],
                            });
                            if !block.change_ranges.is_empty() {
                                change_bars.push(ChangeBar { page: *page, y: yy, height: bh, para: para_id });
                            }
                        } else {
                            for (rel, lh, mut stops) in geom {
                                for s in stops.iter_mut() {
                                    s.byte += block.byte_offset;
                                }
                                if line_has_change(&block.change_ranges, &stops) {
                                    change_bars.push(ChangeBar { page: *page, y: yy + rel, height: lh, para: para_id });
                                }
                                lines.push(LineBox { para: para_id, y: abs_top + rel, height: lh, stops });
                            }
                        }
                        // The space-after folds into the next block's consolidated gap.
                        yy += bh;
                    }
                }
                *y += frag_h;

                match rest {
                    Some(r) if r.iter().any(|(b, _)| !b.is_empty()) => {
                        *page += 1;
                        *y = mt;
                        work = r;
                        first_fragment = false;
                    }
                    _ => break,
                }
            }
        }
        *y += 6.0; // small gap after the table
    }

    /// Partition a cell's block run at inner height `budget` (px, below the cell's top margin), for
    /// a table row splitting across a page boundary: blocks (and, at the boundary, LINES) that fit
    /// stay; the remainder returns as the continuation fragment. The straddling block's spans are
    /// sliced at the first non-fitting line's start byte: the head keeps the paragraph's marker and
    /// spacing-before (its spacing-after is dropped - the paragraph continues); the tail drops the
    /// marker (already painted), aligns at the hanging-text edge, and carries `byte_offset` so its
    /// caret stops stay paragraph-global. Both fragment lists stay parallel to their `para_ids`.
    #[allow(clippy::type_complexity)]
    pub(crate) fn split_cell_blocks(
        &mut self,
        blocks: &[Block],
        para_ids: &[usize],
        w: f32,
        budget: f32,
    ) -> ((Vec<Block>, Vec<usize>), (Vec<Block>, Vec<usize>)) {
        let mut fit: (Vec<Block>, Vec<usize>) = (Vec::new(), Vec::new());
        let mut rest: (Vec<Block>, Vec<usize>) = (Vec::new(), Vec::new());
        let mut used = 0.0_f32;
        let mut prev: Option<&Block> = None;
        for (i, b) in blocks.iter().enumerate() {
            let pid = para_ids.get(i).copied().unwrap_or(usize::MAX);
            if !rest.0.is_empty() {
                rest.0.push(b.clone());
                rest.1.push(pid);
                continue;
            }
            // The same consolidated-gap stacking as `stacked_height`, so the fitted fragment's
            // measured height stays within the page budget.
            let gap = stack_gap(prev, b);
            let bw = (w - b.indent_left_px.max(0.0) - b.indent_right_px.max(0.0)).max(1.0);
            let full = gap + self.block_height(b, bw);
            if used + full + b.space_after_px <= budget + 0.5 {
                used += full;
                prev = Some(b);
                fit.0.push(b.clone());
                fit.1.push(pid);
                continue;
            }
            // The straddling block. An empty (spacer) paragraph has a single indivisible line -
            // move it whole.
            if b.spans.iter().all(|s| s.text.is_empty()) {
                rest.0.push(b.clone());
                rest.1.push(pid);
                continue;
            }
            let inner = budget - used - gap;
            let (_, geom) = self.shape_block_lines(b, bw, 0.0);
            // The byte where the first NON-fitting line starts; None = every line fits (rounding -
            // treat as fitting whole).
            let mut cut: Option<usize> = None;
            for (rel, lh, stops) in &geom {
                if rel + lh <= inner + 0.5 {
                    continue;
                }
                cut = Some(stops.iter().map(|s| s.byte).min().unwrap_or(0));
                break;
            }
            match cut {
                None => {
                    fit.0.push(b.clone());
                    fit.1.push(pid);
                    used += full;
                    prev = Some(b);
                }
                Some(0) => {
                    // Not even the first line fits - the whole block moves.
                    rest.0.push(b.clone());
                    rest.1.push(pid);
                }
                Some(cut) => {
                    let text_len: usize = b.spans.iter().map(|s| s.text.len()).sum();
                    let head = Block {
                        spans: slice_block(b, 0, cut).spans,
                        space_after_px: 0.0,
                        ..b.clone()
                    };
                    // Continuation lines of a hung list paragraph align at the hanging-text edge;
                    // with the marker dropped, fold that edge into the indent.
                    let hang =
                        if !b.marker.is_empty() && b.hang_px > 0.0 { b.hang_px } else { 0.0 };
                    let tail = Block {
                        spans: slice_block(b, cut, text_len).spans,
                        byte_offset: b.byte_offset + cut,
                        space_before_px: 0.0,
                        marker: String::new(),
                        hang_px: 0.0,
                        indent_left_px: b.indent_left_px + hang,
                        ..b.clone()
                    };
                    fit.0.push(head);
                    fit.1.push(pid);
                    rest.0.push(tail);
                    rest.1.push(pid);
                }
            }
        }
        (fit, rest)
    }
}
