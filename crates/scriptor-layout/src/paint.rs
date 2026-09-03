//! Painting a page.
//! 
//! Composites one page's pixels from an already-computed layout: the sheet, the block
//! runs, table cells and their borders. It reads no document state off the renderer -
//! everything it needs arrives as parameters - which is why a page can be repainted
//! alone when its fingerprint changes.

use crate::*;

impl Renderer {
    /// Rasterize a single page (0-based `page_index`) of a laid-out document: an opaque white sheet
    /// `page_width`x`page_height` px with that page's text drawn on it. Returns RGBA8
    /// (`page_width*page_height*4`). The browser blits it at `y = page_index*(page_height+gap)`;
    /// unchanged pages are never re-rasterized. (Gutters between sheets are the browser's gray
    /// backdrop.)
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn paint_page(
        &mut self,
        blocks: &[Block],
        layout: &DocLayout,
        page_index: u32,
        header: &[Block],
        footer: &[Block],
        header_y: f32,
        footer_dist: f32,
        images: &[PageImage],
        frames: &[PageFrame],
        balloons: &[BalloonPlacement],
        dim_body: f32,
        dim_header: f32,
        dim_footer: f32,
    ) -> Vec<u8> {
        let (page_w, page_h) = (layout.page_width, layout.page_height);
        let mut pixels = vec![0xFFu8; (page_w as usize) * (page_h as usize) * 4];
        // The page sheet: white, or the document's `w:background` fill (the top-4 worst docs of the
        // first pixel-diff baseline were all an unpainted page colour - a ~100% per-page diff).
        if let Some([r, g, b]) = self.page_background {
            for px in pixels.as_chunks_mut::<4>().0 {
                px[0] = r;
                px[1] = g;
                px[2] = b;
            }
        }
        // Word greys whichever region isn't being edited (the header/footer + logo while you work in
        // the body; the body once you enter a header/footer). Each region's text dims via `self.dim`
        // (glyph coverage), its shading/bars via `dim_rgb`, and its pictures via `PageImage.dim`.
        self.dim = dim_body;

        // `behindDoc` images paint first, so the text + other content land on top of them.
        for img in images.iter().filter(|i| i.behind) {
            self.composite_image(img, page_w, page_h, &mut pixels);
        }

        // Collect placements for this page first (avoids borrowing `layout` across `&mut self`).
        let on_page: Vec<(usize, f32)> = layout
            .placements
            .iter()
            .filter(|p| p.page == page_index)
            .map(|p| (p.block, p.y))
            .collect();
        for (block_idx, y) in on_page {
            let Some(block) = blocks.get(block_idx) else { continue };
            // Same per-block indent box as layout_doc, so paint matches the caret geometry.
            let il = block.indent_left_px.max(0.0);
            let ir = block.indent_right_px.max(0.0);
            let bx = layout.margin_left + il;
            let bw = (layout.content_width - il - ir).max(1.0);
            if block.shading.is_some() || block.borders.is_some() {
                let h = self.block_height(block, bw);
                if let Some(fill) = block.shading {
                    fill_solid(
                        &mut pixels, page_w, page_h,
                        bx.round() as i32, y.round() as i32, bw.round() as i32, h.round() as i32,
                        dim_rgb(fill, dim_body),
                    );
                }
                // The paragraph's own border box (`w:pBdr`); an empty bordered paragraph still boxes.
                if let Some(bd) = &block.borders {
                    paint_para_borders(bd, bx, y, bw, h, page_w, page_h, &mut pixels, dim_body);
                }
            }
            if block.spans.iter().all(|s| s.text.is_empty()) {
                continue;
            }
            self.raster_block(block, bw, bx, y, page_w, page_h, &mut pixels);
        }

        // Table cells on this page: every fill first, then every border + text - a later row's
        // shading must never bury the collapsed border it shares with the row above (the bottom
        // edge straddles the boundary; see draw_cell_borders).
        let page_cells: Vec<CellPlacement> =
            layout.cells.iter().filter(|c| c.page == page_index).cloned().collect();
        for c in &page_cells {
            if let Some(fill) = c.shading {
                let (x, y) = (c.x.round() as i32, c.y.round() as i32);
                let (x1, y1) = ((c.x + c.w).round() as i32, (c.y + c.h).round() as i32);
                fill_solid(&mut pixels, page_w, page_h, x, y, x1 - x, y1 - y, dim_rgb(fill, dim_body));
            }
        }
        for c in &page_cells {
            draw_cell_borders(c, page_w, page_h, &mut pixels);
            let [mt, ml, _mb, mr] = c.margins;
            let cw = (c.w - ml - mr).max(1.0);
            self.paint_cell(&c.blocks, c.x + ml, c.y + mt, cw, page_w, page_h, &mut pixels);
        }

        // Text frames on this page: a positioned box of paragraphs (body text already wrapped around
        // it). Painted over the body via the same block-flow as a cell, at the frame's resolved origin,
        // then its border box at the frame's full height (so a tall `w:h` frame's rectangle matches
        // Word even when the text is short).
        for f in frames.iter().filter(|f| f.page == page_index) {
            self.paint_cell(&f.blocks, f.x, f.y, f.w, page_w, page_h, &mut pixels);
            if let Some(b) = &f.border {
                paint_para_borders(b, f.x, f.y, f.w, f.h, page_w, page_h, &mut pixels, 0.0);
            }
        }

        // Margin change-bars: a thin vertical line in the left margin beside every changed paragraph
        // on this page (Word's "changed lines" bar). Layout-neutral - it sits in the margin gutter.
        let bw = layout.change_bar_w.round().max(1.0) as i32;
        let bx = layout.change_bar_x.round() as i32;
        for bar in layout.change_bars.iter().filter(|b| b.page == page_index) {
            fill_solid(
                &mut pixels, page_w, page_h,
                bx, bar.y.round() as i32, bw, bar.height.round().max(1.0) as i32,
                dim_rgb(CHANGE_BAR_RGB, dim_body),
            );
        }

        // Revision balloons: bubbles in the right-margin band (content was narrowed to reserve it),
        // each connected to its paragraph by a thin elbow line. The band sits just right of the body.
        // Balloons are review chrome, not body content - always solid.
        self.dim = 0.0;
        if layout.balloon_band > 0.0 {
            let pad = 6.0_f32;
            let band_left = layout.margin_left + layout.content_width;
            let bx = band_left + pad;
            let bw = (layout.balloon_band - 2.0 * pad).max(1.0);
            let cw = (bw - 2.0 * pad).max(1.0); // text width inside the box padding
            for b in balloons.iter().filter(|b| b.page == page_index) {
                // Elbow connector: a hairline from the body's right edge in to the band at the anchor
                // line, then down/up to the (possibly stacked) balloon's top.
                fill_solid(
                    &mut pixels, page_w, page_h,
                    band_left.round() as i32, b.anchor_y.round() as i32,
                    (bx - band_left).round().max(1.0) as i32, 1, BALLOON_BORDER,
                );
                let (vy, vh) = if b.y >= b.anchor_y {
                    (b.anchor_y, b.y - b.anchor_y)
                } else {
                    (b.y + b.height, b.anchor_y - (b.y + b.height))
                };
                fill_solid(
                    &mut pixels, page_w, page_h,
                    bx.round() as i32, vy.round() as i32, 1, vh.round().max(0.0) as i32,
                    BALLOON_BORDER,
                );
                // Box: faint fill + a 1px border (four edges).
                let (ix, iy, iw, ih) =
                    (bx.round() as i32, b.y.round() as i32, bw.round() as i32, b.height.round().max(1.0) as i32);
                fill_solid(&mut pixels, page_w, page_h, ix, iy, iw, ih, BALLOON_BG);
                fill_solid(&mut pixels, page_w, page_h, ix, iy, iw, 1, BALLOON_BORDER);
                fill_solid(&mut pixels, page_w, page_h, ix, iy + ih - 1, iw, 1, BALLOON_BORDER);
                fill_solid(&mut pixels, page_w, page_h, ix, iy, 1, ih, BALLOON_BORDER);
                fill_solid(&mut pixels, page_w, page_h, ix + iw - 1, iy, 1, ih, BALLOON_BORDER);
                // Content (the struck, author-coloured deleted text), inset by the padding.
                let mut yy = b.y + pad;
                for blk in &b.blocks {
                    self.raster_block(blk, cw, bx + pad, yy, page_w, page_h, &mut pixels);
                    yy += self.block_height(blk, cw);
                }
            }
        }

        // Header + footer (drawn in the top / bottom margins of every page). Header stacks down from
        // `header_y`; footer is bottom-aligned so its last line sits `footer_dist` from the edge.
        let cw = layout.content_width;
        let x = layout.margin_left;
        let (cbx, cbw) = (layout.change_bar_x, layout.change_bar_w);
        if !header.is_empty() {
            self.dim = dim_header;
            self.paint_block_run(header, cw, x, header_y, page_w, page_h, &mut pixels, cbx, cbw);
        }
        if !footer.is_empty() {
            self.dim = dim_footer;
            let total: f32 = footer.iter().map(|b| self.block_height(b, cw)).sum();
            let start = page_h as f32 - footer_dist - total;
            self.paint_block_run(footer, cw, x, start, page_w, page_h, &mut pixels, cbx, cbw);
        }

        // Foreground images (inline / in-front anchored) paint last, over the text. Each carries its
        // own `dim` (matching its region), so this is independent of `self.dim`.
        for img in images.iter().filter(|i| !i.behind) {
            self.composite_image(img, page_w, page_h, &mut pixels);
        }

        // Passthrough placeholder boxes (unmodeled OLE / chart / shape): a neutral filled rectangle +
        // a caption where the object sits, so the view isn't a blank gap. Body content, so they dim
        // with the body region (see `docs/passthrough.md`).
        self.dim = dim_body;
        for ph in layout.placeholders.iter().filter(|p| p.page == page_index) {
            self.paint_placeholder(ph.x, ph.y, ph.w, ph.h, &ph.label, page_w, page_h, &mut pixels);
        }

        self.dim = 0.0; // leave the renderer un-dimmed for the next paint
        pixels
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_cell(&mut self, blocks: &[Block], x: f32, y: f32, w: f32, page_w: u32, page_h: u32, pixels: &mut [u8]) {
        let mut yy = y;
        let mut prev: Option<&Block> = None;
        for b in blocks {
            // Consolidated inter-paragraph gap - the same rule as `stacked_height`, so paint
            // matches the cell's sized box and the caret geometry.
            yy += stack_gap(prev, b);
            prev = Some(b);
            // The block's `w:ind` indents within the cell / frame text column - the same box the
            // caret pass shaped, so paint matches the caret geometry.
            let il = b.indent_left_px.max(0.0);
            let ir = b.indent_right_px.max(0.0);
            let bx = x + il;
            let bw = (w - il - ir).max(1.0);
            let text_empty = b.spans.iter().all(|s| s.text.is_empty());
            // An object-only paragraph (a logo / passthrough box in a frame / cell) reserves no blank
            // text line - its inline objects ARE its content, stacked below any text otherwise.
            let only_objects = text_empty && (!b.inline_images.is_empty() || !b.placeholders.is_empty());
            let text_h = if only_objects { 0.0 } else { self.block_height(b, bw) };
            // A text-empty block still paints when it carries a list marker - an empty numbered
            // paragraph's number IS its visible content (e.g. a "Nr" column of bare headings).
            if !text_empty || !b.marker.is_empty() {
                self.raster_block(b, bw, bx, yy, page_w, page_h, pixels);
            }
            // Inline pictures stack below the text (a figure, or a logo inside a frame / table cell).
            let mut iy = yy + text_h;
            for img in &b.inline_images {
                let (iw, ih) = (img.w.max(1.0), img.h.max(1.0));
                let pi = PageImage {
                    key: img.key.clone(),
                    x: bx,
                    y: iy,
                    w: iw,
                    h: ih,
                    behind: false,
                    crop: img.crop,
                    page: 0,
                    id: None,
                    dim: self.dim,
                };
                self.composite_image(&pi, page_w, page_h, pixels);
                iy += ih;
            }
            // Passthrough placeholder boxes stack the same way (an OLE / chart / shape inside a cell).
            for ph in &b.placeholders {
                let (pw, ph_h) = (ph.w.max(1.0), ph.h.max(1.0));
                self.paint_placeholder(bx, iy, pw, ph_h, &ph.label, page_w, page_h, pixels);
                iy += ph_h;
            }
            // The paragraph's border box wraps its text + pictures (frames lift their box to the frame).
            if let Some(bd) = &b.borders {
                let content_h = (iy - yy).max(text_h);
                paint_para_borders(bd, bx, yy, bw, content_h, page_w, page_h, pixels, 0.0);
            }
            // The space-after folds into the NEXT block's consolidated gap (see `stack_gap`).
            yy = iy;
        }
    }

    /// Paint a list of blocks stacking downward from `start_y` (used for header / footer). A changed
    /// paragraph gets the same margin change-bar as the body (`bar_x` / `bar_w` device px) - header /
    /// footer don't flow through `layout_doc`, so the bar is drawn here where they actually paint.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_block_run(
        &mut self,
        blocks: &[Block],
        content_w: f32,
        x: f32,
        start_y: f32,
        page_w: u32,
        page_h: u32,
        pixels: &mut [u8],
        bar_x: f32,
        bar_w: f32,
    ) {
        let mut y = start_y;
        for block in blocks {
            // The block's `w:ind` applies within the band - SIGNED, because Word's Header styles
            // widen the header box (and its rule) into both page margins with negative indents,
            // and the tab stops that right-align the page number measure from that origin.
            let il = block.indent_left_px;
            let ir = block.indent_right_px;
            let bx = x + il;
            let bw = (content_w - il - ir).max(1.0);
            let hgt = self.block_height(block, bw);
            if !block.spans.iter().all(|s| s.text.is_empty()) {
                self.raster_block(block, bw, bx, y, page_w, page_h, pixels);
            }
            // The paragraph's border box (Word's Header style draws its horizontal rule as a style
            // pBdr bottom edge - it must paint in the band, not just in body flow).
            if let Some(bd) = &block.borders {
                paint_para_borders(bd, bx, y, bw, hgt, page_w, page_h, pixels, self.dim);
            }
            if block.has_change {
                fill_solid(
                    pixels, page_w, page_h,
                    bar_x.round() as i32, y.round() as i32,
                    bar_w.round().max(1.0) as i32, hgt.round().max(1.0) as i32,
                    CHANGE_BAR_RGB,
                );
            }
            y += hgt;
        }
    }
}
