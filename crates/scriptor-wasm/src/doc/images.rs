//! The editable picture surface.
//! 
//! Insert, resize, crop, float and remove, plus the hit-testing and rectangles the
//! view needs to draw selection handles.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// The editable picture id whose hit-rect contains the canvas point `(x, y)` (absolute px), or
    /// `None`. The topmost match wins (floats over inline) - the view uses this for click-to-select.
    #[wasm_bindgen(js_name = imageAtPoint)]
    pub fn image_at_point(&self, x: f32, y: f32) -> Option<u64> {
        hit_image(&self.image_rects, x, y)
    }

    /// Picture `id`'s rect on the canvas as `[x, y, w, h]` (absolute px), or `None` if it isn't placed
    /// (e.g. off-page). The view draws the selection box + resize handles from this.
    #[wasm_bindgen(js_name = imageRect)]
    pub fn image_rect(&self, id: u64) -> Option<Box<[f32]>> {
        // A header/footer picture repeats on every page (and a Different First Page doc shares the
        // image id between the first + default header stories), so several hits carry the same id.
        // Prefer the instance on the page the caret/click is on (`active_hf_page`, set on click) so the
        // selection box lands on the picture the user actually clicked - not the first page's copy.
        let r = self
            .image_rects
            .iter()
            .find(|r| r.id == id && r.page == self.active_hf_page)
            .or_else(|| self.image_rects.iter().find(|r| r.id == id))?;
        Some(vec![r.x, r.y, r.w, r.h].into_boxed_slice())
    }

    /// Insert a picture at codepoint `off` in **body** paragraph `para`: `bytes` (MIME `mime`, e.g.
    /// `image/png`) ship as a fresh `word/media` part on save, shown at `w_emu` x `h_emu` (EMU; the
    /// caller derives these from the decoded natural size via [`px_to_emu`]). Returns the new picture
    /// id. Re-layout + repaint after. Images live on the body story only (header/footer not supported).
    #[wasm_bindgen(js_name = insertImage)]
    pub fn insert_image(
        &self,
        para: u32,
        off: u32,
        bytes: &[u8],
        mime: &str,
        w_emu: f64,
        h_emu: f64,
    ) -> Result<u64, JsError> {
        let (region, p) = decode_region(para as usize);
        if region != Region::Body {
            return Err(JsError::new("images are supported on the body story only"));
        }
        // Under Track Changes the picture is a tracked insertion (its run carries w:ins), attributed to
        // the current author + timestamp; otherwise a direct insert.
        if self.track_changes {
            self.doc
                .suggest_insert_image(
                    p, off as usize, bytes.to_vec(), mime, w_emu as i64, h_emu as i64,
                    &self.author_name, &self.now, "insert image",
                )
                .map_err(to_js)
        } else {
            self.doc
                .insert_image(p, off as usize, bytes.to_vec(), mime, w_emu as i64, h_emu as i64, "insert image")
                .map_err(to_js)
        }
    }

    /// Resize picture `id` to `w_emu` x `h_emu` (EMU). Returns whether it existed. Re-layout after.
    #[wasm_bindgen(js_name = setImageSize)]
    pub fn set_image_size(&self, id: u64, w_emu: f64, h_emu: f64) -> Result<bool, JsError> {
        let Some((d, local)) = self.image_doc(id) else { return Ok(false) };
        d.set_image_size(local, w_emu as i64, h_emu as i64, "resize image").map_err(to_js)
    }

    /// Set picture `id`'s crop (`<a:srcRect>` l/t/r/b, thousandths of a percent, 0..100000 - the share
    /// of each edge to cut). Returns whether it existed. Re-layout + repaint after.
    #[wasm_bindgen(js_name = setImageCrop)]
    pub fn set_image_crop(&self, id: u64, l: i32, t: i32, r: i32, b: i32) -> Result<bool, JsError> {
        let Some((d, local)) = self.image_doc(id) else { return Ok(false) };
        d.set_image_crop(local, l as i64, t as i64, r as i64, b as i64, "crop image")
            .map_err(to_js)
    }

    /// Reset picture `id`'s crop (Word's "Reset Crop"): clear `<a:srcRect>` and restore the display
    /// extent so the whole image reappears at the same scale. Returns whether it was cropped.
    /// Re-layout + repaint after.
    #[wasm_bindgen(js_name = resetImageCrop)]
    pub fn reset_image_crop(&self, id: u64) -> Result<bool, JsError> {
        let Some((d, local)) = self.image_doc(id) else { return Ok(false) };
        d.reset_image_crop(local, "reset crop").map_err(to_js)
    }

    /// Make picture `id` floating (positioned + text-wrapped) or inline (in the flow). `wrap` is the
    /// wrap type (`square` / `tight` / `topAndBottom` / `through` / `none`); `behind` paints it under
    /// the text. Returns whether it existed. Re-layout after.
    #[wasm_bindgen(js_name = setImageFloating)]
    pub fn set_image_floating(
        &self,
        id: u64,
        floating: bool,
        wrap: &str,
        behind: bool,
    ) -> Result<bool, JsError> {
        let Some((d, local)) = self.image_doc(id) else { return Ok(false) };
        d.set_image_floating(local, floating, wrap, behind, "wrap image").map_err(to_js)
    }

    /// Position floating picture `id`: `h_from`/`v_from` are the `relativeFrom` origins
    /// (`column`/`page`/`margin`/...) and `x_emu`/`y_emu` the offset from it (EMU). Clears any
    /// alignment (an explicit offset wins, as in Word's drag-to-move). No-op on an inline picture.
    /// Returns whether it existed and was floating. Re-layout after.
    #[wasm_bindgen(js_name = setImagePosition)]
    pub fn set_image_position(
        &self,
        id: u64,
        h_from: &str,
        x_emu: f64,
        v_from: &str,
        y_emu: f64,
    ) -> Result<bool, JsError> {
        let Some((d, local)) = self.image_doc(id) else { return Ok(false) };
        d.set_image_position(local, h_from, x_emu as i64, v_from, y_emu as i64, "move image")
            .map_err(to_js)
    }

    /// Remove picture `id`. Under Track Changes this is a tracked deletion (the run is marked `w:del`,
    /// retained until accepted); otherwise the run + placement are dropped outright. Returns whether it
    /// existed. Re-layout after.
    #[wasm_bindgen(js_name = removeImage)]
    pub fn remove_image(&self, id: u64) -> Result<bool, JsError> {
        let Some((d, local)) = self.image_doc(id) else { return Ok(false) };
        if self.track_changes {
            d.suggest_remove_image(local, &self.author_name, &self.now, "remove image").map_err(to_js)
        } else {
            d.remove_image(local, "remove image").map_err(to_js)
        }
    }

    /// The raw (encoded, uncropped) media bytes of picture `id`, or `None`. The view decodes these to
    /// show the full image behind the crop window in crop mode (the page canvas only has the cropped
    /// bitmap).
    #[wasm_bindgen(js_name = imageMedia)]
    pub fn image_media(&self, id: u64) -> Option<Vec<u8>> {
        let (d, local) = self.image_doc(id)?;
        let p = d.image_placement(local)?;
        self.doc.image_bytes(&p.media) // media parts live in the root doc, shared across stories
    }

    /// Picture `id`'s crop as `[l, t, r, b]` (`<a:srcRect>`, thousandths of a percent - the share of
    /// each edge cut), or `None`. The view seeds the crop window from this.
    #[wasm_bindgen(js_name = imageCrop)]
    pub fn image_crop(&self, id: u64) -> Option<Box<[i32]>> {
        let (d, local) = self.image_doc(id)?;
        let p = d.image_placement(local)?;
        Some(vec![p.crop_l as i32, p.crop_t as i32, p.crop_r as i32, p.crop_b as i32].into_boxed_slice())
    }

    /// Picture `id`'s wrap state as a single token for the Wrap Text menu + drag logic: `inline`
    /// (in the flow), `square` / `tight` / `through` / `topAndBottom` (floating, text wraps), `behind`
    /// (floating, behind the text), or `front` (floating, in front of the text). `None` if `id` is
    /// unknown.
    #[wasm_bindgen(js_name = imageWrapState)]
    pub fn image_wrap_state(&self, id: u64) -> Option<String> {
        let (d, local) = self.image_doc(id)?;
        let p = d.image_placement(local)?;
        Some(if !p.floating {
            "inline".to_string()
        } else if p.behind {
            "behind".to_string()
        } else if p.wrap.is_empty() || p.wrap == "none" {
            "front".to_string()
        } else {
            p.wrap.clone()
        })
    }

    /// Map each editable picture id to the flat body-paragraph index whose run carries it (its anchor).
    /// Used to fix a floating picture's page + paragraph-relative origin.
    pub(crate) fn image_anchor_paras(&self) -> std::collections::HashMap<u64, usize> {
        let mut m = std::collections::HashMap::new();
        if let Ok(paras) = self.doc.paragraphs() {
            for (pi, p) in paras.iter().enumerate() {
                for r in &p.runs {
                    if let Some(id) = r.image {
                        m.entry(id).or_insert(pi);
                    }
                }
            }
        }
        m
    }
}
