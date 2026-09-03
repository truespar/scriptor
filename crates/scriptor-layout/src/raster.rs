//! Glyph rasterization into an RGBA buffer.
//! 
//! Takes shaped lines and draws them: glyph coverage blended over the page, plus the
//! highlight fills and underline/strike decorations that sit with the text. The
//! rotated variant paints into a temporary canvas and transposes it, which is how a
//! vertical margin stamp is drawn.

use crate::*;

impl Renderer {
    /// Lay out `text` wrapped to `width` px and rasterize it onto an opaque white page of
    /// `width`x`height` px. Returns non-premultiplied RGBA8 (`width*height*4` bytes), ready for the
    /// browser's `ImageData` + `putImageData`.
    pub fn render_rgba(&mut self, text: &str, width: u32, height: u32, font_size: f32) -> Vec<u8> {
        let metrics = Metrics::new(font_size, font_size * line_height_factor(DEFAULT_FAMILY));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let mut view = buffer.borrow_with(&mut self.font_system);
        view.set_size(Some(width as f32), Some(height as f32));
        view.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        view.shape_until_scroll(false);

        // Opaque white page; composite dark ink over it (straight-alpha src-over).
        let mut pixels = vec![0xFFu8; (width as usize) * (height as usize) * 4];
        let ink = Color::rgb(0x1a, 0x1a, 0x1a);
        let (w, h) = (width as i32, height as i32);
        view.draw(&mut self.swash_cache, ink, |x, y, bw, bh, color| {
            let a = color.a() as u32;
            if a == 0 {
                return;
            }
            let inv = 255 - a;
            for dy in 0..bh as i32 {
                for dx in 0..bw as i32 {
                    let (px, py) = (x + dx, y + dy);
                    if px < 0 || py < 0 || px >= w || py >= h {
                        continue;
                    }
                    let idx = ((py as usize) * (width as usize) + (px as usize)) * 4;
                    pixels[idx] = ((color.r() as u32 * a + pixels[idx] as u32 * inv) / 255) as u8;
                    pixels[idx + 1] =
                        ((color.g() as u32 * a + pixels[idx + 1] as u32 * inv) / 255) as u8;
                    pixels[idx + 2] =
                        ((color.b() as u32 * a + pixels[idx + 2] as u32 * inv) / 255) as u8;
                    pixels[idx + 3] = 255;
                }
            }
        });
        pixels
    }

    /// Rasterize one block into `pixels` (a `width`x`height` page-local buffer) with its top-left at
    /// (`x`,`y`). Re-shapes the block (cheap) and alpha-blends its glyphs over the page.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn raster_block(
        &mut self,
        block: &Block,
        content_w: f32,
        x: f32,
        y: f32,
        width: u32,
        height: u32,
        pixels: &mut [u8],
    ) {
        if !block.marker.is_empty() && block.hang_px > 0.0 {
            return self.raster_block_hung(block, content_w, x, y, width, height, pixels);
        }
        if block_has_tab(block) {
            return self.raster_block_tabbed(block, content_w, x, y, width, height, pixels);
        }
        // Two-sided frame wrap: paint each column region's slice at its own origin.
        if !block.wrap_holes.is_empty() {
            for (sub, rx, ry, rw) in self.flow_regions(block, content_w, x) {
                self.raster_block(&sub, rw, rx, y + ry, width, height, pixels);
            }
            return;
        }
        let max_size = block.spans.iter().map(|s| s.size_px).fold(1.0_f32, f32::max);
        let line_h = block.block_line_height(max_size);
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(max_size, line_h));
        {
            let mut view = buffer.borrow_with(&mut self.font_system);
            view.set_size(Some(content_w), None);
            let (spans, _marker_len, _trailing_len) = Self::rich_spans(block);
            view.set_rich_text(spans, &Attrs::new(), Shaping::Advanced, block.align.to_cosmic());
            view.shape_until_scroll(false);
        } // drop the view (releasing the font-system borrow) - the buffer stays shaped.

        let (w, h) = (width as i32, height as i32);
        let (x_off, y_off) = (x as i32, y as i32);

        // Highlight fills sit BEHIND the glyphs - paint them before the glyph pass.
        for run in buffer.layout_runs() {
            let base_y = y_off + run.line_y as i32;
            highlight_run(run.glyphs, &block.spans, x_off as f32, base_y, pixels, width, height);
        }

        // Glyph pass. We raster each glyph by hand (rather than the high-level `Buffer::draw`) so a
        // super/subscript span can carry a vertical baseline shift: each glyph's metadata holds
        // (span index + 1) in bits 2.. (see `rich_spans`); look up that span's `baseline_shift` and
        // raise (superscript) / lower (subscript) the glyph. Mirrors cosmic-text's own draw loop.
        let default_ink = Color::rgb(0x1a, 0x1a, 0x1a);
        let spans = &block.spans;
        // Dim toward white by scaling glyph coverage (a dimmed inactive header/footer or body).
        let dim_mul = ((1.0 - self.dim).clamp(0.0, 1.0) * 255.0) as u32;
        for run in buffer.layout_runs() {
            let base_y = y_off + run.line_y as i32;
            for glyph in run.glyphs.iter() {
                let si = glyph.metadata >> 2;
                let shift = if si == 0 {
                    0.0
                } else {
                    spans.get(si - 1).map(|s| s.baseline_shift).unwrap_or(0.0)
                };
                let physical = glyph.physical((0.0, 0.0), 1.0);
                let gcolor = glyph.color_opt.unwrap_or(default_ink);
                let gx0 = x_off + physical.x;
                let gy0 = base_y + physical.y - shift as i32;
                self.swash_cache.with_pixels(
                    &mut self.font_system,
                    physical.cache_key,
                    gcolor,
                    |ox, oy, color| {
                        let a = (color.a() as u32 * dim_mul) / 255;
                        if a == 0 {
                            return;
                        }
                        let inv = 255 - a;
                        let (px, py) = (gx0 + ox, gy0 + oy);
                        if px < 0 || py < 0 || px >= w || py >= h {
                            return;
                        }
                        let idx = ((py as usize) * (width as usize) + (px as usize)) * 4;
                        pixels[idx] = ((color.r() as u32 * a + pixels[idx] as u32 * inv) / 255) as u8;
                        pixels[idx + 1] = ((color.g() as u32 * a + pixels[idx + 1] as u32 * inv) / 255) as u8;
                        pixels[idx + 2] = ((color.b() as u32 * a + pixels[idx + 2] as u32 * inv) / 255) as u8;
                        pixels[idx + 3] = 255;
                    },
                );
            }
        }

        // Underline / strike: cosmic-text shapes but draws no decorations. Each glyph carries the
        // bits we set in `spans_of` (0 = underline, 1 = strike). Draw ONE continuous line per run of
        // consecutive decorated glyphs - a rect per glyph leaves sub-pixel gaps (looks dashed).
        let ink = dim_rgb([0x1a_u8, 0x1a, 0x1a], self.dim);
        for run in buffer.layout_runs() {
            let thick = (run.line_height * 0.07).max(1.0) as i32;
            let u_y = y_off + (run.line_top + run.line_height * 0.92) as i32;
            let s_y = y_off + (run.line_top + run.line_height * 0.58) as i32;
            decoration_line(run.glyphs, 1, x_off as f32, u_y, thick, pixels, width, height, ink);
            decoration_line(run.glyphs, 2, x_off as f32, s_y, thick, pixels, width, height, ink);
        }
    }

    /// Rasterize a block containing literal tabs (see [`Self::shape_block_tabbed`]): draw each
    /// segment at its tab stop via the normal raster path; the last segment wraps within the
    /// remaining width.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn raster_block_tabbed(
        &mut self,
        block: &Block,
        content_w: f32,
        x: f32,
        y: f32,
        width: u32,
        height: u32,
        pixels: &mut [u8],
    ) {
        let right = x + content_w;
        let segs = split_segments(&block.spans);
        let n = segs.len();
        let mut pen = x;
        for (i, (seg, _start)) in segs.into_iter().enumerate() {
            let mut kind = 0u8;
            if i > 0 {
                let (np, nk) =
                    next_tab_stop(pen, x, &block.tab_stops_px, &block.tab_kinds, block.default_tab_px);
                pen = np;
                kind = nk;
            }
            if seg.is_empty() {
                continue;
            }
            let w = tab_segment_width(right - pen, content_w);
            let sb = seg_block(block, seg);
            let is_last = i == n - 1;
            let segw = self.first_line_width(&sb, w);
            // Mirror the alignment shift from `shape_block_tabbed` so paint matches caret geometry.
            let offset = tab_align_offset(kind, segw, pen, x);
            self.raster_block(&sb, w, pen + offset, y, width, height, pixels);
            if !is_last {
                pen += segw + offset;
            }
        }
    }

    /// Rasterize a hanging-indent list item (see [`Self::shape_block_hung`]): paint the marker at the
    /// block's left edge (left-aligned in the hanging gap, on the first line), then the text + its
    /// wrapped lines at `x + hang_px`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn raster_block_hung(
        &mut self,
        block: &Block,
        content_w: f32,
        x: f32,
        y: f32,
        width: u32,
        height: u32,
        pixels: &mut [u8],
    ) {
        let hang = block.hang_px.max(0.0);
        if let Some(span) = marker_span(block) {
            let mb = Block {
                spans: vec![span],
                marker: String::new(),
                hang_px: 0.0,
                align: BlockAlign::Left,
                ..block.clone()
            };
            self.raster_block(&mb, hang.max(1.0), x, y, width, height, pixels);
        }
        let tb = Block { marker: String::new(), hang_px: 0.0, ..block.clone() };
        self.raster_block(&tb, (content_w - hang).max(1.0), x + hang, y, width, height, pixels);
    }

    /// Paint a single-line text stamp whose page box is `(x, y, box_w, box_h)`, optionally rotated -
    /// the anchored text-box render path (a legal template's rotated margin stamp in a footer).
    /// `vert`: `0` horizontal, `1` = `vert` (top-to-bottom, glyph tops facing page-right), `2` =
    /// `vert270` (bottom-to-top, glyph tops facing page-left - Word's `bodyPr vert="vert270"`).
    /// The text is rasterized horizontally into a temp canvas along the box's long axis, then
    /// transpose-blitted; a vertical stamp reads from the box's BOTTOM (vert270) / TOP (vert) edge,
    /// matching Word's rotated-frame text origin. `dim` mirrors the region dimming.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_text_stamp(
        &mut self,
        text: &str,
        family: &str,
        size_px: f32,
        color: [u8; 3],
        vert: u8,
        x: f32,
        y: f32,
        box_w: f32,
        box_h: f32,
        page_w: u32,
        page_h: u32,
        pixels: &mut [u8],
        dim: f32,
    ) {
        if text.is_empty() || size_px <= 0.5 {
            return;
        }
        let run_len = if vert == 0 { box_w } else { box_h };
        let line_h = (size_px * FALLBACK_LINE_HEIGHT).ceil().max(1.0);
        let tw = (run_len.ceil() as u32).clamp(1, 4096);
        let th = (line_h as u32).clamp(1, 512);
        let mut tmp = vec![0xFFu8; tw as usize * th as usize * 4];
        let block = Block {
            spans: vec![Span {
                text: text.to_string(),
                size_px,
                bold: false,
                italic: false,
                underline: false,
                strike: false,
                color,
                highlight: None,
                baseline_shift: 0.0,
                family: if family.is_empty() { DEFAULT_FAMILY.to_string() } else { family.into() },
            }],
            line_mult: 1.0,
            ..Default::default()
        };
        let saved_dim = self.dim;
        self.dim = dim;
        self.raster_block(&block, tw as f32, 0.0, 0.0, tw, th, &mut tmp);
        self.dim = saved_dim;

        // Transpose-blit the non-white (inked) temp pixels onto the page. The temp canvas is white
        // and stamps land in the page margins (also white), so a straight copy keeps the glyph
        // anti-aliasing exact.
        let (pw, ph) = (page_w as i32, page_h as i32);
        for ty in 0..th as i32 {
            for tx in 0..tw as i32 {
                let si = ((ty as usize) * (tw as usize) + tx as usize) * 4;
                let (r, g, b) = (tmp[si], tmp[si + 1], tmp[si + 2]);
                if r == 0xFF && g == 0xFF && b == 0xFF {
                    continue;
                }
                let (dx, dy) = match vert {
                    // vert270: reading start at the box bottom, glyph tops at the box left edge.
                    2 => (x as i32 + ty, y as i32 + box_h as i32 - 1 - tx),
                    // vert: reading start at the box top, glyph tops at the box right edge.
                    1 => (x as i32 + box_w.min(line_h) as i32 - 1 - ty, y as i32 + tx),
                    _ => (x as i32 + tx, y as i32 + ty),
                };
                if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                    continue;
                }
                let di = ((dy as usize) * (page_w as usize) + dx as usize) * 4;
                pixels[di] = r;
                pixels[di + 1] = g;
                pixels[di + 2] = b;
            }
        }
    }
}
