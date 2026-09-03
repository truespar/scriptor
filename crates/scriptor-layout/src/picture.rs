//! Pictures on the page.
//! 
//! Decodes and caches image bytes, composites them with crop and scaling clipped to
//! the display box, and draws the neutral labelled box that stands in for content the
//! engine does not render. Metafiles (WMF/EMF) come through here too, via the vector
//! interpreter in `metafile`.

use crate::*;

impl Renderer {
    /// Decode + cache an image (PNG/JPEG bytes) under `key` (idempotent). Call before painting pages
    /// that reference it. Unsupported/corrupt data is silently skipped (the image just won't show).
    pub fn register_image(&mut self, key: &str, bytes: &[u8]) {
        if self.image_cache.contains_key(key) {
            return;
        }
        // WMF / EMF are GDI record streams the `image` crate can't read - route them to the metafile
        // decoder (embedded bitmap -> pixel-exact; vector -> tiny-skia geometry + collected text runs
        // that we rasterize here through the font system).
        if metafile::is_metafile(bytes) {
            if let Some((mut img, texts)) = metafile::decode(bytes) {
                if !texts.is_empty() {
                    self.rasterize_meta_text(&mut img, &texts);
                }
                self.image_cache.insert(key.to_string(), img);
            }
            return;
        }
        if let Ok(img) = image::load_from_memory(bytes) {
            self.image_cache.insert(key.to_string(), img.to_rgba8());
        }
    }

    /// Decode-cached image -> cropped (`<a:srcRect>`) -> resized -> alpha-blended onto the page at the
    /// placement rect.
    pub(crate) fn composite_image(&self, img: &PageImage, page_w: u32, page_h: u32, pixels: &mut [u8]) {
        let Some(src) = self.image_cache.get(&img.key) else { return };
        // The `srcRect` window (thousandths of a percent off each edge) maps onto the display box.
        // Positive insets crop *in* (the kept region fills the box); NEGATIVE insets extend *out* (the
        // image is placed within the box with blank padding - Word uses this to keep a logo's aspect by
        // padding the box). Either way: scale the whole image so its `[l..1-r] x [t..1-b]` window fills
        // the box, then blit clipped to the box. The general form subsumes the plain (no-crop) case.
        let (bw, bh) = (img.w.round().max(1.0) as f64, img.h.round().max(1.0) as f64); // box, px
        let frac = |v: i64| v as f64 / 100_000.0; // signed: negative = pad
        let (fl, ft, fr, fb) = (frac(img.crop[0]), frac(img.crop[1]), frac(img.crop[2]), frac(img.crop[3]));
        let (sw_frac, sh_frac) = (1.0 - fl - fr, 1.0 - ft - fb);
        if sw_frac <= 0.0 || sh_frac <= 0.0 {
            return; // degenerate crop (cuts away everything)
        }
        // The full image's draw-rect within the page: its `srcRect` window lands on the box.
        let img_w = (bw / sw_frac).round().max(1.0) as u32;
        let img_h = (bh / sh_frac).round().max(1.0) as u32;
        let img_x0 = (img.x as f64 - (fl / sw_frac) * bw).round() as i32;
        let img_y0 = (img.y as f64 - (ft / sh_frac) * bh).round() as i32;
        // The display box clips the blit, so a negative crop's overflow stays blank + a positive crop's
        // cut region is dropped.
        let (bx0, by0) = (img.x.round() as i32, img.y.round() as i32);
        let (bx1, by1) = (bx0 + bw as i32, by0 + bh as i32);
        let (pw, ph) = (page_w as i32, page_h as i32);
        // An oversized draw-rect (a mis-sized shape, an extreme crop at high zoom) must never resize
        // the FULL image: the target buffer alone can exhaust memory - and overflows `usize` on
        // wasm32, panicking the paint (a VML group misparse once asked for a 171k x 171k resize).
        // Past this area, map the page-visible window back to a source crop and scale only that;
        // below it, keep the exact full-image path so normal content renders byte-identically.
        const FULL_RESIZE_CAP_PX: u64 = 16 * 1024 * 1024;
        let (resized, ox, oy) = if (img_w as u64) * (img_h as u64) <= FULL_RESIZE_CAP_PX {
            (image::imageops::resize(src, img_w, img_h, image::imageops::FilterType::Triangle), img_x0, img_y0)
        } else {
            let vx0 = img_x0.max(bx0).max(0);
            let vy0 = img_y0.max(by0).max(0);
            let vx1 = img_x0.saturating_add(img_w.min(i32::MAX as u32) as i32).min(bx1).min(pw);
            let vy1 = img_y0.saturating_add(img_h.min(i32::MAX as u32) as i32).min(by1).min(ph);
            if vx0 >= vx1 || vy0 >= vy1 {
                return; // nothing of the image lands on the page
            }
            // The visible dest rect mapped back to a source window (f64 - the full-rect sizes may
            // not fit i32 math), clamped into the bitmap.
            let to_src = |d: i32, o: i32, full: u32, dim: u32| -> f64 {
                (d - o) as f64 / full as f64 * dim as f64
            };
            let sx0 = to_src(vx0, img_x0, img_w, src.width()).floor().clamp(0.0, (src.width() - 1) as f64) as u32;
            let sy0 = to_src(vy0, img_y0, img_h, src.height()).floor().clamp(0.0, (src.height() - 1) as f64) as u32;
            let sx1 = to_src(vx1, img_x0, img_w, src.width()).ceil().clamp(1.0, src.width() as f64) as u32;
            let sy1 = to_src(vy1, img_y0, img_h, src.height()).ceil().clamp(1.0, src.height() as f64) as u32;
            let crop = image::imageops::crop_imm(src, sx0, sy0, (sx1 - sx0).max(1), (sy1 - sy0).max(1)).to_image();
            let win = image::imageops::resize(
                &crop,
                (vx1 - vx0) as u32,
                (vy1 - vy0) as u32,
                image::imageops::FilterType::Triangle,
            );
            (win, vx0, vy0)
        };
        // A dimmed picture (an inactive header/footer's logo) blends at reduced alpha, fading toward
        // the white page - the image counterpart of the dimmed text.
        let dim_mul = ((1.0 - img.dim).clamp(0.0, 1.0) * 255.0) as u32;
        for (sx, sy, px) in resized.enumerate_pixels() {
            let (dx, dy) = (ox + sx as i32, oy + sy as i32);
            if dx < bx0 || dy < by0 || dx >= bx1 || dy >= by1 {
                continue; // outside the display box
            }
            if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                continue; // off the page
            }
            let a = (px[3] as u32 * dim_mul) / 255;
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            let idx = ((dy as usize) * (page_w as usize) + (dx as usize)) * 4;
            pixels[idx] = ((px[0] as u32 * a + pixels[idx] as u32 * inv) / 255) as u8;
            pixels[idx + 1] = ((px[1] as u32 * a + pixels[idx + 1] as u32 * inv) / 255) as u8;
            pixels[idx + 2] = ((px[2] as u32 * a + pixels[idx + 2] as u32 * inv) / 255) as u8;
            pixels[idx + 3] = 255;
        }
    }

    /// Paint one passthrough placeholder box (a neutral fill + soft border + muted, vertically-centred
    /// caption) at (`x`,`y`) sized `w`x`h` device px - the visual stand-in for an unmodeled object (OLE
    /// / chart / shape) that the engine can't render. Shared by the body ([`paint_page`]) and cell /
    /// frame ([`paint_cell`]) paths. Uses `self.dim` for the caption (so it greys with its region).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_placeholder(&mut self, x: f32, y: f32, w: f32, h: f32, label: &str, page_w: u32, page_h: u32, pixels: &mut [u8]) {
        let (ix, iy) = (x.round() as i32, y.round() as i32);
        let (iw, ih) = (w.round().max(1.0) as i32, h.round().max(1.0) as i32);
        fill_solid(pixels, page_w, page_h, ix, iy, iw, ih, PLACEHOLDER_BG);
        fill_solid(pixels, page_w, page_h, ix, iy, iw, 1, PLACEHOLDER_BORDER);
        fill_solid(pixels, page_w, page_h, ix, iy + ih - 1, iw, 1, PLACEHOLDER_BORDER);
        fill_solid(pixels, page_w, page_h, ix, iy, 1, ih, PLACEHOLDER_BORDER);
        fill_solid(pixels, page_w, page_h, ix + iw - 1, iy, 1, ih, PLACEHOLDER_BORDER);
        // Muted caption, vertically centred and left-inset a little. Sized down for a short box.
        let size_px = 12.0_f32.min(h * 0.5).max(7.0);
        let line_h = size_px * line_height_factor(DEFAULT_FAMILY);
        let block = Block {
            spans: vec![Span {
                text: label.to_string(),
                size_px,
                bold: false,
                italic: false,
                underline: false,
                strike: false,
                color: PLACEHOLDER_TEXT,
                highlight: None,
                baseline_shift: 0.0,
                family: resolve_family(DEFAULT_FAMILY).to_string(),
            }],
            line_mult: 1.0,
            ..Default::default()
        };
        let ty = y + ((h - line_h) / 2.0).max(0.0);
        let cw = (w - 12.0).max(1.0);
        self.raster_block(&block, cw, x + 6.0, ty, page_w, page_h, pixels);
    }

    /// Rasterize the text runs a metafile collected (EMF `ExtTextOut`) onto its decoded image, through
    /// the font system - tiny-skia can't draw text, so the metafile decoder hands back positioned runs
    /// and we shape + blit them here. `t.x`/`t.y` are the cell top-left in image px; `t.size_px` the
    /// device font size; the glyphs alpha-blend (coverage) in the metafile colour.
    pub(crate) fn rasterize_meta_text(&mut self, img: &mut image::RgbaImage, texts: &[metafile::MetaText]) {
        let (iw, ih) = (img.width() as i32, img.height() as i32);
        for t in texts {
            if t.size_px < 1.0 {
                continue;
            }
            let fam: Option<String> = t.family.as_deref().map(|f| resolve_family(f).to_string());
            let line_h = t.size_px * 1.25;
            let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(t.size_px, line_h));
            {
                let mut view = buffer.borrow_with(&mut self.font_system);
                view.set_size(Some(1.0e6), None); // single unwrapped run
                let mut attrs = Attrs::new();
                if let Some(f) = &fam {
                    attrs = attrs.family(Family::Name(f));
                }
                if t.bold {
                    attrs = attrs.weight(Weight::BOLD);
                }
                if t.italic {
                    attrs = attrs.style(Style::Italic);
                }
                view.set_rich_text([(t.text.as_str(), attrs)], &Attrs::new(), Shaping::Advanced, None);
                view.shape_until_scroll(false);
            }
            let color = Color::rgb(t.rgb[0], t.rgb[1], t.rgb[2]);
            for run in buffer.layout_runs() {
                let base_y = t.y as i32 + run.line_y as i32;
                for glyph in run.glyphs.iter() {
                    let physical = glyph.physical((t.x, 0.0), 1.0);
                    let (gx0, gy0) = (physical.x, base_y + physical.y);
                    self.swash_cache.with_pixels(
                        &mut self.font_system,
                        physical.cache_key,
                        color,
                        |ox, oy, col| {
                            let cov = col.a() as u32;
                            if cov == 0 {
                                return;
                            }
                            let (px, py) = (gx0 + ox, gy0 + oy);
                            if px < 0 || py < 0 || px >= iw || py >= ih {
                                return;
                            }
                            let inv = 255 - cov;
                            let p = img.get_pixel_mut(px as u32, py as u32);
                            p.0[0] = ((col.r() as u32 * cov + p.0[0] as u32 * inv) / 255) as u8;
                            p.0[1] = ((col.g() as u32 * cov + p.0[1] as u32 * inv) / 255) as u8;
                            p.0[2] = ((col.b() as u32 * cov + p.0[2] as u32 * inv) / 255) as u8;
                            p.0[3] = p.0[3].max(cov as u8);
                        },
                    );
                }
            }
        }
    }
}
