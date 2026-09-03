//! Shaping and line breaking, behind a content-addressed cache.
//! 
//! Turns a [`Block`] into visual lines: resolve its spans to cosmic-text attributes,
//! break them at a width, and report the caret stops. Every measurement the rest of
//! the engine makes - block height, run height, frame height - comes through here, so
//! the cache in front of it is what keeps a keystroke cheap.

use crate::*;

impl Renderer {
    /// Build the cosmic-text attribute spans for a block - the list marker (if any) first, then the
    /// text runs (size via per-span metrics + weight / italic / color, with underline/strike tagged
    /// in glyph metadata). Returns the spans + the marker's UTF-8 byte length so callers can offset
    /// caret byte positions past it (the marker isn't part of the editable text).
    pub(crate) fn rich_spans(block: &Block) -> (Vec<(&str, Attrs<'_>)>, usize, usize) {
        let mut out: Vec<(&str, Attrs<'_>)> = Vec::with_capacity(block.spans.len() + 2);
        // cosmic-text takes a line's height from the MAX of the per-span (Attrs) metrics on that line,
        // overriding the Buffer's default metrics. So the paragraph's line spacing (`w:spacing w:line`,
        // auto / atLeast / exact) must be folded into EVERY span's line height here, not just the Buffer
        // metrics in shape/raster - otherwise a non-single line spacing silently renders as single.
        let marker_len = block.marker.len();
        if marker_len > 0 {
            let first = block.spans.first();
            let size = first.map(|s| s.size_px).unwrap_or(16.0);
            let fam = first.map(|s| s.family.as_str()).unwrap_or(DEFAULT_FAMILY);
            // Ink the marker like the first run (a white heading on a dark band keeps its number
            // white), body-dark when the paragraph has no runs.
            let col = first.map(|s| s.color).unwrap_or([0x1a, 0x1a, 0x1a]);
            let a = Attrs::new()
                .metrics(Metrics::new(size, block.span_line_height(size, fam)))
                .family(Family::Name(fam))
                .color(Color::rgb(col[0], col[1], col[2]));
            out.push((block.marker.as_str(), a));
        }
        for (i, s) in block.spans.iter().enumerate() {
            let mut a = Attrs::new()
                .metrics(Metrics::new(s.size_px, block.span_line_height(s.size_px, &s.family)))
                .family(Family::Name(&s.family));
            if s.bold {
                a = a.weight(Weight::BOLD);
            }
            if s.italic {
                a = a.style(Style::Italic);
            }
            a = a.color(Color::rgb(s.color[0], s.color[1], s.color[2]));
            // cosmic-text shapes glyphs but draws no underline/strike/highlight; tag the glyph
            // metadata so the raster pass can. bit 0 = underline, bit 1 = strike, and bits 2.. carry
            // (span index + 1) so the highlight pass can look up this span's fill (0 = marker/none).
            let deco = (s.underline as usize) | ((s.strike as usize) << 1);
            a = a.metadata(deco | ((i + 1) << 2));
            out.push((s.text.as_str(), a));
        }
        // A trailing paragraph-mark glyph (tracked ¶): painted + coloured, optionally struck, but
        // tagged with span index 0 so the highlight pass ignores it (and excluded from caret geometry
        // by the callers via `trailing_len`).
        let trailing_len = block.trailing.len();
        if trailing_len > 0 {
            let first = block.spans.first();
            let size = first.map(|s| s.size_px).unwrap_or(16.0);
            let fam = first.map(|s| s.family.as_str()).unwrap_or(DEFAULT_FAMILY);
            let c = block.trailing_color;
            let a = Attrs::new()
                .metrics(Metrics::new(size, block.span_line_height(size, fam)))
                .family(Family::Name(fam))
                .color(Color::rgb(c[0], c[1], c[2]))
                .metadata((block.trailing_strike as usize) << 1);
            out.push((block.trailing.as_str(), a));
        }
        (out, marker_len, trailing_len)
    }

    /// Shape one block to `content_w` px with its left edge at `x_left`, returning its height (px)
    /// and its visual lines as `(rel_top, height, stops)` - `rel_top` relative to the block top, and
    /// each stop's x already absolute (`x_left` + glyph x). No rasterization (the cheap layout pass).
    pub(crate) fn shape_block_lines(
        &mut self,
        block: &Block,
        content_w: f32,
        x_left: f32,
    ) -> (f32, Vec<(f32, f32, Vec<CaretStop>)>) {
        // Shaping is content-addressed: identical (block, width, x) always shapes identically, so
        // unchanged paragraphs are served from the memo and only edited ones pay rustybuzz. This is
        // what makes a keystroke O(changed paragraph) instead of O(document) for the shaping share.
        // `fold_block` is the content hash (it must fold every field shaping reads - see its doc).
        let mut key = fold_block(FNV_OFFSET, block, 0.0);
        key = fnv_bytes(key, &content_w.to_bits().to_le_bytes());
        key = fnv_bytes(key, &x_left.to_bits().to_le_bytes());
        if let Some(e) = self.shape_cache.get_mut(&key) {
            e.last_gen = self.shape_gen;
            return (e.bh, e.geom.clone());
        }
        let (bh, geom) = self.shape_block_lines_uncached(block, content_w, x_left);
        self.shape_cache.insert(key, ShapeEntry { last_gen: self.shape_gen, bh, geom: geom.clone() });
        (bh, geom)
    }

    pub(crate) fn shape_block_lines_uncached(
        &mut self,
        block: &Block,
        content_w: f32,
        x_left: f32,
    ) -> (f32, Vec<(f32, f32, Vec<CaretStop>)>) {
        if !block.marker.is_empty() && block.hang_px > 0.0 {
            return self.shape_block_hung(block, content_w, x_left);
        }
        if block_has_tab(block) {
            return self.shape_block_tabbed(block, content_w, x_left);
        }
        // Two-sided frame wrap: flow the text around the hole through the column regions.
        if !block.wrap_holes.is_empty() {
            let regions = self.flow_regions(block, content_w, x_left);
            let mut out = Vec::new();
            let mut block_h = 0.0_f32;
            for (sub, rx, ry, rw) in &regions {
                let (_, geom) = self.shape_block_lines(sub, *rw, *rx);
                for (rt, lh, stops) in geom {
                    block_h = block_h.max(ry + rt + lh);
                    out.push((ry + rt, lh, stops));
                }
            }
            return (block_h, out);
        }
        let max_size = block.spans.iter().map(|s| s.size_px).fold(1.0_f32, f32::max);
        let line_h = block.block_line_height(max_size);
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(max_size, line_h));
        let mut view = buffer.borrow_with(&mut self.font_system);
        view.set_size(Some(content_w), None);
        let (spans, marker_len, trailing_len) = Self::rich_spans(block);
        view.set_rich_text(spans, &Attrs::new(), Shaping::Advanced, block.align.to_cosmic());
        view.shape_until_scroll(false);

        // Caret stops are in MODEL text bytes - skip the leading marker glyphs (subtract its byte
        // length) and exclude any trailing ¶ glyph (bytes at/after the editable text's end).
        let text_end = marker_len + block.spans.iter().map(|s| s.text.len()).sum::<usize>();
        let editable = |start: usize| start >= marker_len && (trailing_len == 0 || start < text_end);
        let mut block_h = 0.0_f32;
        let mut out = Vec::new();
        for run in view.layout_runs() {
            block_h += run.line_height;
            let mut stops: Vec<CaretStop> = run
                .glyphs
                .iter()
                .filter(|g| editable(g.start))
                .map(|g| CaretStop { byte: g.start - marker_len, x: x_left + g.x })
                .collect();
            let (end_x, end_byte) = if trailing_len > 0 {
                // The end caret sits just after the last editable glyph (before the ¶).
                match run.glyphs.iter().rfind(|g| editable(g.start)) {
                    Some(g) => (x_left + g.x + g.w, g.end - marker_len),
                    None => {
                        let x = run
                            .glyphs
                            .iter().rfind(|g| g.start < marker_len)
                            .map(|g| x_left + g.x + g.w)
                            .unwrap_or(x_left);
                        (x, 0)
                    }
                }
            } else {
                let end_x = run.glyphs.last().map(|g| x_left + g.x + g.w).unwrap_or(x_left);
                let end_byte = run
                    .glyphs
                    .iter()
                    .map(|g| g.end)
                    .max()
                    .unwrap_or(marker_len)
                    .saturating_sub(marker_len);
                (end_x, end_byte)
            };
            stops.push(CaretStop { byte: end_byte, x: end_x });
            out.push((run.line_top, run.line_height, stops));
        }
        (block_h, out)
    }

    /// Split a paragraph that carries a [`WrapHole`] (a centre-straddling text frame) into the column
    /// regions its text flows through, in reading order: full width above the hole, the left column
    /// beside it, the right column beside it, then full width below. Each returned region is a
    /// hole-free sub-`Block` of the exact text slice it holds, plus its `(x_left, y_top, width)` - so
    /// the caller can shape / paint each with the ordinary single-rectangle path. Two-sided wrap with
    /// no new rasterizer. Only the first hole is honoured (more than one per paragraph is vanishingly
    /// rare). A column narrower than `MIN_COL` px is skipped (text can't fit), so a frame that reaches
    /// a margin degrades to one-sided / below wrap.
    pub(crate) fn flow_regions(&mut self, block: &Block, content_w: f32, x_left: f32) -> Vec<(Block, f32, f32, f32)> {
        const MIN_COL: f32 = 24.0;
        let hole = block.wrap_holes[0];
        let max_size = block.spans.iter().map(|s| s.size_px).fold(1.0_f32, f32::max);
        let line_h = block.block_line_height(max_size).max(1.0);
        let band_top = hole.top.max(0.0);
        let above_cap = (band_top / line_h).floor().max(0.0) as usize;
        let band_y = above_cap as f32 * line_h;
        let band_cap = (((hole.bot - band_y) / line_h).round() as i64).max(1) as usize;
        let below_y = band_y + band_cap as f32 * line_h;
        let left_w = hole.x0 - x_left;
        let right_w = (x_left + content_w) - hole.x1;
        // (x_left, width, y_top, max_lines), in the order text fills them.
        let mut specs: Vec<(f32, f32, f32, usize)> = vec![(x_left, content_w, 0.0, above_cap)];
        if left_w >= MIN_COL {
            specs.push((x_left, left_w, band_y, band_cap));
        }
        if right_w >= MIN_COL {
            specs.push((hole.x1, right_w, band_y, band_cap));
        }
        specs.push((x_left, content_w, below_y, usize::MAX));

        let total: usize = block.spans.iter().map(|s| s.text.len()).sum();
        let mut consumed = 0usize;
        let mut out = Vec::new();
        for (rx, rw, ry, cap) in specs {
            if consumed >= total || cap == 0 || rw < MIN_COL {
                continue;
            }
            let rest = slice_block(block, consumed, total);
            let (_, lines) = self.shape_block_lines(&rest, rw, 0.0);
            let take = cap.min(lines.len());
            if take == 0 {
                continue;
            }
            // Bytes (within `rest`) the first `take` lines consume = the end caret of line `take-1`.
            let end = lines[..take]
                .iter()
                .flat_map(|(_, _, stops)| stops.iter().map(|s| s.byte))
                .max()
                .unwrap_or(0);
            if end == 0 {
                continue;
            }
            out.push((slice_block(block, consumed, consumed + end), rx, ry, rw));
            consumed += end;
        }
        out
    }

    /// Shape geometry for a block containing literal tabs: split it into tab-delimited segments,
    /// place each at the next tab stop, and let the final segment wrap (continuation hangs under the
    /// stop). Reuses the normal (tab-free) shaper per segment.
    pub(crate) fn shape_block_tabbed(
        &mut self,
        block: &Block,
        content_w: f32,
        x_left: f32,
    ) -> (f32, Vec<(f32, f32, Vec<CaretStop>)>) {
        let right = x_left + content_w;
        let segs = split_segments(&block.spans);
        let mut pen = x_left;
        let mut line0: Vec<CaretStop> = Vec::new();
        let mut line0_h = 0.0_f32;
        let mut tail: Vec<(f32, Vec<CaretStop>)> = Vec::new(); // (height, stops) of wrapped lines

        for (i, (seg, start)) in segs.into_iter().enumerate() {
            let mut kind = 0u8;
            if i > 0 {
                let (np, nk) =
                    next_tab_stop(pen, x_left, &block.tab_stops_px, &block.tab_kinds, block.default_tab_px);
                pen = np;
                kind = nk;
            }
            if seg.is_empty() {
                continue;
            }
            let w = tab_segment_width(right - pen, content_w);
            let sb = seg_block(block, seg);
            let (_h, lines) = self.shape_block_lines(&sb, w, pen);
            for (li, (_rt, lh, stops)) in lines.iter().enumerate() {
                let mut s: Vec<CaretStop> =
                    stops.iter().map(|c| CaretStop { byte: c.byte + start, x: c.x }).collect();
                if li == 0 {
                    let end = stops.iter().map(|c| c.x).fold(pen, f32::max);
                    // Right/centre/decimal stops shift the (non-wrapping) first line so it ends at
                    // (right/decimal) or straddles (centre) the stop instead of starting on it.
                    let offset = tab_align_offset(kind, end - pen, pen, x_left);
                    if offset != 0.0 {
                        for c in &mut s {
                            c.x += offset;
                        }
                    }
                    line0.append(&mut s);
                    line0_h = line0_h.max(*lh);
                    pen = end + offset; // advance to the right edge of this segment's first line
                } else {
                    tail.push((*lh, s));
                }
            }
        }

        let mut out = vec![(0.0, line0_h.max(1.0), line0)];
        let mut y = line0_h.max(1.0);
        for (lh, stops) in tail {
            out.push((y, lh, stops));
            y += lh;
        }
        (y, out)
    }

    /// Shape a hanging-indent list item: the text (and every wrapped continuation line) aligns at
    /// `x_left + hang_px`, while the marker hangs in the gap `[x_left, x_left + hang_px)`. The marker
    /// carries no caret stops (it isn't editable text), so shaping only needs the text geometry - the
    /// text sub-block has its marker cleared, so its caret stops are already in MODEL bytes.
    pub(crate) fn shape_block_hung(
        &mut self,
        block: &Block,
        content_w: f32,
        x_left: f32,
    ) -> (f32, Vec<(f32, f32, Vec<CaretStop>)>) {
        let hang = block.hang_px.max(0.0);
        let tb = Block { marker: String::new(), hang_px: 0.0, ..block.clone() };
        self.shape_block_lines(&tb, (content_w - hang).max(1.0), x_left + hang)
    }

    /// Width (px) of a block's first laid-out line at content width `w` - used to advance the tab pen.
    pub(crate) fn first_line_width(&mut self, block: &Block, w: f32) -> f32 {
        let (_h, lines) = self.shape_block_lines(block, w, 0.0);
        lines.first().map(|(_, _, stops)| stops.iter().map(|c| c.x).fold(0.0, f32::max)).unwrap_or(0.0)
    }

    /// Stacked height (px) of a run of blocks at width `w`: the first block's space-before, each
    /// block's line height, the CONSOLIDATED inter-paragraph gaps (see [`stack_gap`] - Word's
    /// max-of-after/before rule + contextualSpacing, the same model the body flow uses; naive
    /// summing inflated every bulleted table cell by one spacing per list item), and the last
    /// block's space-after. Used to size table cells; matched by [`Renderer::paint_cell`].
    /// A block's `w:ind` narrows its wrap width within the cell's text column, like the body pass.
    pub(crate) fn stacked_height(&mut self, blocks: &[Block], w: f32) -> f32 {
        let mut h = 0.0;
        let mut prev: Option<&Block> = None;
        for b in blocks {
            let bw = (w - b.indent_left_px.max(0.0) - b.indent_right_px.max(0.0)).max(1.0);
            h += stack_gap(prev, b) + self.block_height(b, bw);
            prev = Some(b);
        }
        h + prev.map(|b| b.space_after_px).unwrap_or(0.0)
    }

    /// Shape a block to `content_w` and return its laid-out height (px) - no rasterization.
    pub(crate) fn block_height(&mut self, block: &Block, content_w: f32) -> f32 {
        // An empty paragraph still occupies one line (Word renders a blank line); cosmic-text gives
        // empty text ~0 height, so size it explicitly - consistent with `layout_doc`.
        if block.spans.iter().all(|s| s.text.is_empty()) {
            empty_line_height(block)
        } else {
            self.shape_block_lines(block, content_w, 0.0).0
        }
    }

    /// Total height (px) of a run of blocks at `content_w` - e.g. the footer's height, so an image
    /// anchored to the footer paragraph can be positioned relative to where the footer paints.
    /// Heights account for each block's (signed) indents, matching `paint_block_run`'s widths.
    pub fn run_height(&mut self, blocks: &[Block], content_w: f32) -> f32 {
        let mut h = 0.0;
        for b in blocks {
            let bw = (content_w - b.indent_left_px - b.indent_right_px).max(1.0);
            h += self.block_height(b, bw);
        }
        h
    }

    /// Paint a table cell's blocks stacked from (`x`,`y`) at width `w` - the same stacking as
    /// [`Renderer::stacked_height`] (space-before + line + space-after) so paint matches layout.
    #[allow(clippy::too_many_arguments)]
    /// Total flow height (px) of a sequence of blocks laid out at width `w` - the same accumulation
    /// `paint_cell` paints with. Used to size a text frame's box (for its wrap rect) before painting.
    pub fn frame_height(&mut self, blocks: &[Block], w: f32) -> f32 {
        let mut yy = 0.0_f32;
        let mut prev: Option<&Block> = None;
        for b in blocks {
            let bw = (w - b.indent_left_px.max(0.0) - b.indent_right_px.max(0.0)).max(1.0);
            let text_empty = b.spans.iter().all(|s| s.text.is_empty());
            let only_objects = text_empty && (!b.inline_images.is_empty() || !b.placeholders.is_empty());
            let text_h = if only_objects { 0.0 } else { self.block_height(b, bw) };
            let img_h: f32 = b.inline_images.iter().map(|i| i.h).sum();
            let ph_h: f32 = b.placeholders.iter().map(|p| p.h).sum();
            yy += stack_gap(prev, b) + text_h + img_h + ph_h;
            prev = Some(b);
        }
        yy + prev.map(|b| b.space_after_px).unwrap_or(0.0)
    }

    /// Caret geometry (visual lines) for a run of blocks stacked from `start_y` (page-local) on the
    /// page whose top is at `page_origin` (absolute px) - the header / footer counterpart to the body
    /// lines emitted in [`layout_doc`]. Stacks by `block_height` to match [`paint_block_run`], and
    /// tags each line with `base_para + block_index` so the caller can namespace header/footer
    /// paragraphs into a disjoint index range (keeping the single `(para, byte)` caret coordinate).
    pub fn block_run_lines(
        &mut self,
        blocks: &[Block],
        content_w: f32,
        x_left: f32,
        start_y: f32,
        page_origin: f32,
        base_para: usize,
    ) -> Vec<LineBox> {
        let mut out = Vec::new();
        let mut y = start_y;
        for (i, block) in blocks.iter().enumerate() {
            let para = base_para + i;
            let is_empty = block.spans.iter().all(|s| s.text.is_empty());
            let (bh, geom) = if is_empty {
                (empty_line_height(block), Vec::new())
            } else {
                self.shape_block_lines(block, content_w, x_left)
            };
            let abs_top = page_origin + y;
            if is_empty {
                out.push(LineBox {
                    para,
                    y: abs_top,
                    height: empty_line_height(block),
                    stops: vec![CaretStop { byte: 0, x: x_left }],
                });
            } else {
                for (rel, lh, stops) in geom {
                    out.push(LineBox { para, y: abs_top + rel, height: lh, stops });
                }
            }
            y += bh;
        }
        out
    }
}
