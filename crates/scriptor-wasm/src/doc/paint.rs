//! Rasterizing pages for the canvas.
//! 
//! Paints a page to RGBA, with a small LRU cache and a band variant that returns only
//! the rows that changed, so a keystroke does not ship a whole page back to JS.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Rasterize a single page (0-based) of the current layout: an opaque white sheet with that
    /// page's text. Returns RGBA8 (`page_width*page_height*4`); the browser blits it at
    /// `y = index*(page_height+gap)`. Call after [`relayout`], only for pages whose fingerprint
    /// changed. Also refreshes the page's raster cache, so a later [`Self::paint_page_band`] diffs
    /// against exactly what the canvas shows.
    #[wasm_bindgen(js_name = paintPage)]
    pub fn paint_page(&mut self, index: u32) -> Vec<u8> {
        let pixels = self.rasterize_page(index);
        self.paint_cache_store(index, &pixels);
        pixels
    }

    /// [`Self::paint_page`], returning only the vertical band of rows that actually CHANGED since
    /// the page's last raster: an 8-byte little-endian header `[y0, y1)` followed by `(y1-y0)` rows
    /// of RGBA. Typing edits one paragraph, so shipping the whole ~3-4MB page across the wasm->JS
    /// boundary per keystroke was mostly wasted transfer + GC; the band is pixel-diffed against the
    /// cached previous raster, so it can never miss a visual change (no raster cached, or a size
    /// change, degrades to the full page; nothing changed returns an empty `[0, 0)` band). Only
    /// valid when the caller's canvas still shows this page's previous raster.
    #[wasm_bindgen(js_name = paintPageBand)]
    pub fn paint_page_band(&mut self, index: u32) -> Vec<u8> {
        let pixels = self.rasterize_page(index);
        let row = (self.layout.page_width as usize) * 4;
        let rows = pixels.len().checked_div(row).unwrap_or(0);
        let band = match self.paint_cache.get(&index) {
            Some(prev) if prev.len() == pixels.len() && row > 0 => {
                let mut y0 = 0;
                while y0 < rows && prev[y0 * row..(y0 + 1) * row] == pixels[y0 * row..(y0 + 1) * row] {
                    y0 += 1;
                }
                if y0 == rows {
                    (0, 0) // pixel-identical: nothing to ship
                } else {
                    let mut y1 = rows;
                    while y1 > y0 + 1
                        && prev[(y1 - 1) * row..y1 * row] == pixels[(y1 - 1) * row..y1 * row]
                    {
                        y1 -= 1;
                    }
                    (y0, y1)
                }
            }
            _ => (0, rows),
        };
        let (y0, y1) = band;
        let mut out = Vec::with_capacity(8 + (y1 - y0) * row);
        out.extend_from_slice(&(y0 as u32).to_le_bytes());
        out.extend_from_slice(&(y1 as u32).to_le_bytes());
        out.extend_from_slice(&pixels[y0 * row..y1 * row]);
        self.paint_cache_store(index, &pixels);
        out
    }

    /// Keep page `index`'s raster for the band diff, evicting the oldest entries past a byte cap
    /// (the view only band-paints pages its canvas window holds, so a handful suffice).
    pub(crate) fn paint_cache_store(&mut self, index: u32, pixels: &[u8]) {
        const CAP_BYTES: usize = 32 * 1024 * 1024;
        self.paint_order.retain(|&p| p != index);
        self.paint_order.push(index);
        self.paint_cache.insert(index, pixels.to_vec());
        let mut held: usize = self.paint_cache.values().map(Vec::len).sum();
        while held > CAP_BYTES && self.paint_order.len() > 1 {
            let oldest = self.paint_order.remove(0);
            if let Some(v) = self.paint_cache.remove(&oldest) {
                held -= v.len();
            }
        }
    }

    pub(crate) fn rasterize_page(&mut self, index: u32) -> Vec<u8> {
        let total = self.layout.pages.len().max(1) as u32;
        let pageno = index + 1;
        let images = self.page_images(index);

        // The page's own header/footer parts (per-section selection + titlePg, resolved in
        // `relayout`) - `None` paints a blank band.
        let [h_set, f_set] = self.page_hf_at(index);
        let hdr_src: &[scriptor_layout::Block] =
            h_set.map(|i| &self.hf_sets[i].blocks[..]).unwrap_or(&[]);
        let ftr_src: &[scriptor_layout::Block] =
            f_set.map(|i| &self.hf_sets[i].blocks[..]).unwrap_or(&[]);
        // Substitute computed-field placeholders (PAGE/NUMPAGES) for this page. Header/footer are
        // small, so always; the body only when it actually has a field placeholder.
        let header = substitute_fields(hdr_src, pageno, total);
        let footer = substitute_fields(ftr_src, pageno, total);
        let body_sub;
        let body: &[scriptor_layout::Block] = if self.has_body_fields {
            body_sub = substitute_fields(&self.blocks, pageno, total);
            &body_sub
        } else {
            &self.blocks
        };

        let (dim_body, dim_header, dim_footer) = (
            self.region_dim(Region::Body),
            self.region_dim(Region::Header),
            self.region_dim(Region::Footer),
        );
        let mut pixels = self.renderer.paint_page(
            body,
            &self.layout,
            index,
            &header,
            &footer,
            self.header_y,
            self.footer_dist_px,
            &images,
            &self.frames,
            &self.balloon_placements,
            dim_body,
            dim_header,
            dim_footer,
        );
        // Anchored text-box stamps (the rotated legal-margin stamp in a footer): painted over the
        // page for the parts THIS page shows.
        self.paint_stamps(index, &mut pixels);
        pixels
    }

    /// Paint the header/footer parts' anchored text boxes that fall on page `index` - each is a
    /// positioned (often 90-degree-rotated) single-line stamp, anchored like a floating picture:
    /// offsets from the page edge (`page`) or the content box (`margin` / `column`).
    pub(crate) fn paint_stamps(&mut self, index: u32, pixels: &mut [u8]) {
        if self.doc.textboxes().is_empty() {
            return;
        }
        let scale = self.scale_last;
        let emu_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * scale;
        let (page_w, page_h) = (self.layout.page_width, self.layout.page_height);
        let [h_set, f_set] = self.page_hf_at(index);
        let stamps: Vec<scriptor_crdt::PlacedTextBox> = self.doc.textboxes().to_vec();
        for tb in stamps {
            let scriptor_crdt::ImageContext::Hf { part, header } = &tb.context else { continue };
            let set_idx = self.hf_sets.iter().position(|s| &s.part == part);
            if set_idx.is_none() || (if *header { h_set } else { f_set }) != set_idx {
                continue;
            }
            let x = match tb.h_from.as_str() {
                "page" => emu_px(tb.x_emu),
                _ => self.ml_px + emu_px(tb.x_emu), // margin / column: from the content-box left
            };
            let y = match tb.v_from.as_str() {
                "page" => emu_px(tb.y_emu),
                _ => self.mt_px + emu_px(tb.y_emu),
            };
            let size_px = if tb.size_half_points > 0 {
                (tb.size_half_points as f32 / 2.0) * (96.0 / 72.0) * scale
            } else {
                10.0 * (96.0 / 72.0) * scale
            };
            let color = tb.color.as_deref().map(parse_hex).unwrap_or([0x1a, 0x1a, 0x1a]);
            let family = scriptor_layout::resolve_family(tb.font.as_deref().unwrap_or(""));
            let dim = self.region_dim(if *header { Region::Header } else { Region::Footer });
            self.renderer.paint_text_stamp(
                &tb.text,
                family,
                size_px,
                color,
                tb.vert,
                x,
                y,
                emu_px(tb.w_emu),
                emu_px(tb.h_emu),
                page_w,
                page_h,
                pixels,
                dim,
            );
        }
    }
}
