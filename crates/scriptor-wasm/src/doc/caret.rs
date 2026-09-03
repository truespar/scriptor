//! Caret geometry, hit-testing and text queries.
//! 
//! Maps between a point on the canvas and a position in the document, and between the
//! three coordinate spaces that exist here: byte offsets, char offsets, and the
//! visible offsets that skip runs the current review mode hides.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Map a canvas point (device px) to a caret position, returned as `[paragraph, codepoint]`.
    /// Uses the geometry from the most recent [`paint`].
    #[wasm_bindgen(js_name = hitTest)]
    pub fn hit_test(&self, x: f32, y: f32) -> Vec<u32> {
        let (para, vis_byte) = self.layout.hit_test(x, y);
        // The layout reports a *visible*-text byte; map it back to the *full* text the model speaks
        // before converting to a codepoint, so a click in a No-Markup / Original / Simple view (where
        // runs are hidden) lands at the right model offset.
        let full_byte = self.visible_to_full(para, vis_byte);
        vec![para as u32, self.byte_to_char(para, full_byte) as u32]
    }

    /// The caret one visual line above/below `(para, off)`, keeping goal column `x` (device px) -
    /// Word's ArrowUp/Down. Returns `[para, off]` (codepoints), or empty at the document edge. (The
    /// shell's old one-pixel hit-test probe snapped back inside inter-paragraph spacing, so Up/Down
    /// never crossed a paragraph boundary.)
    #[wasm_bindgen(js_name = caretLineStep)]
    pub fn caret_line_step(&self, para: u32, off: u32, x: f32, down: bool) -> Vec<u32> {
        let full_byte = self.char_to_byte(para as usize, off as usize);
        let vis_byte = self.full_to_visible(para as usize, full_byte);
        let hint = self.page_hint_for(para as usize);
        match self.layout.line_step(para as usize, vis_byte, x, down, hint) {
            Some((p, vb)) => {
                let fb = self.visible_to_full(p, vb);
                vec![p as u32, self.byte_to_char(p, fb) as u32]
            }
            None => Vec::new(),
        }
    }

    /// The caret rectangle for `(para, codepoint_offset)` as `[x, y, height]` (device px).
    #[wasm_bindgen(js_name = caretRect)]
    pub fn caret_rect(&self, para: u32, off: u32) -> Vec<f32> {
        let full_byte = self.char_to_byte(para as usize, off as usize);
        let vis_byte = self.full_to_visible(para as usize, full_byte);
        let hint = self.page_hint_for(para as usize);
        let (x, y, h) = self.layout.caret_rect(para as usize, vis_byte, hint);
        vec![x, y, h]
    }

    /// Selection highlight rectangles between two caret positions (codepoint offsets), flattened
    /// `[x, y, w, h, ...]` (device px). Empty when the selection is collapsed.
    #[wasm_bindgen(js_name = selectionRects)]
    pub fn selection_rects(&self, p1: u32, o1: u32, p2: u32, o2: u32) -> Vec<f32> {
        let b1 = self.full_to_visible(p1 as usize, self.char_to_byte(p1 as usize, o1 as usize));
        let b2 = self.full_to_visible(p2 as usize, self.char_to_byte(p2 as usize, o2 as usize));
        let hint = self.page_hint_for(p1 as usize);
        self.layout.selection_rects(p1 as usize, b1, p2 as usize, b2, hint)
    }

    /// The margin change-bar rectangles from the last layout, flattened as `[page, x, y, w, h, para]`
    /// per bar (device px; `y` is page-local within `page`). The editor hit-tests a click in the left
    /// margin against these to drive Simple-Markup click-to-expand; `para` is the namespaced paragraph
    /// the bar belongs to. Body + table-cell bars only (header/footer bars paint inline, not here).
    #[wasm_bindgen(js_name = changeBars)]
    pub fn change_bars(&self) -> Vec<f32> {
        let (x, w) = (self.layout.change_bar_x, self.layout.change_bar_w);
        let mut out = Vec::with_capacity(self.layout.change_bars.len() * 6);
        for b in &self.layout.change_bars {
            out.extend_from_slice(&[b.page as f32, x, b.y, w, b.height, b.para as f32]);
        }
        out
    }

    /// The codepoint length of paragraph `para` (for caret clamping + cross-paragraph movement).
    #[wasm_bindgen(js_name = paragraphLength)]
    pub fn paragraph_length(&self, para: u32) -> u32 {
        let (region, local) = decode_region(para as usize);
        self.region_texts(region).get(local).map(|t| t.chars().count()).unwrap_or(0) as u32
    }

    /// The 0-based page index a paragraph sits on (from the last layout) - for "Page X of N". Body
    /// paragraphs come from `placements`; table-cell paragraphs (which have no placement, only caret
    /// geometry) are found by scanning the placed cells.
    #[wasm_bindgen(js_name = paragraphPage)]
    pub fn paragraph_page(&self, para: u32) -> u32 {
        let (region, para) = decode_region(para as usize);
        if region != Region::Body {
            return 0; // header/footer repeat on every page; report page 1 for "Page X of N"
        }
        if let Some(p) = self.layout.placements.iter().find(|p| p.block == para) {
            return p.page;
        }
        self.layout
            .cells
            .iter()
            .find(|c| c.para_ids.contains(&para))
            .map(|c| c.page)
            .unwrap_or(0)
    }

    /// Number of paragraphs in the **body** (back-compat for callers that don't track regions). For
    /// region-aware caret bounds use [`paragraph_range`]. Served from the cached texts (O(1)) with a
    /// fallback to materializing the tree before the first render.
    #[wasm_bindgen(js_name = paragraphCount)]
    pub fn paragraph_count(&self) -> Result<usize, JsError> {
        if !self.para_texts.is_empty() {
            return Ok(self.para_texts.len());
        }
        Ok(self.doc.paragraphs().map_err(to_js)?.len())
    }

    /// The `[firstIndex, count]` of the story (body / header / footer) that `para` belongs to - so the
    /// JS shell can clamp caret movement to one story (a header caret can't arrow into the body). The
    /// first index is the region's namespace base; `count` is its paragraph count.
    #[wasm_bindgen(js_name = paragraphRange)]
    pub fn paragraph_range(&self, para: u32) -> Vec<u32> {
        let (region, _) = decode_region(para as usize);
        let base = region_base(region) as u32;
        let count = match region {
            Region::Body => {
                if !self.para_texts.is_empty() {
                    self.para_texts.len()
                } else {
                    self.doc.paragraphs().map(|p| p.len()).unwrap_or(0)
                }
            }
            Region::Header | Region::Footer => self.region_texts(region).len(),
        } as u32;
        vec![base, count]
    }

    /// The concatenated plain text of paragraph `index` (namespaced). Served from the cached texts
    /// when available (avoids re-materializing the tree per call).
    #[wasm_bindgen(js_name = paragraphText)]
    pub fn paragraph_text(&self, index: usize) -> Result<String, JsError> {
        let (region, local) = decode_region(index);
        if let Some(t) = self.region_texts(region).get(local) {
            return Ok(t.clone());
        }
        // Fallback (body only, before the first paint populates the cache).
        if region == Region::Body {
            let paras = self.doc.paragraphs().map_err(to_js)?;
            let p = paras
                .get(local)
                .ok_or_else(|| JsError::new("paragraph index out of range"))?;
            return Ok(p.runs.iter().map(|r| r.text.as_str()).collect());
        }
        Ok(String::new())
    }

    /// Total word count across the document, computed in a single pass. (The TS shell previously
    /// looped `paragraphText` per paragraph, which re-materialized the whole tree each time - O(n^2),
    /// and a UI freeze once tables put 100+ paragraphs in the flow.)
    #[wasm_bindgen(js_name = wordCount)]
    pub fn word_count(&self) -> usize {
        if !self.para_texts.is_empty() {
            return self.para_texts.iter().map(|t| count_words(t)).sum();
        }
        match self.doc.paragraphs() {
            Ok(paras) => paras
                .iter()
                .map(|p| {
                    let t: String = p.runs.iter().map(|r| r.text.as_str()).collect();
                    count_words(&t)
                })
                .sum(),
            Err(_) => 0,
        }
    }

    /// The cached plain texts of a story, for byte<->char conversion. Header/footer texts come from
    /// the part the caret's PAGE shows (per-section selection) - so a click into any page's band
    /// reads/edits what that page actually paints.
    pub(crate) fn region_texts(&self, region: Region) -> &[String] {
        match region {
            Region::Body => &self.para_texts,
            Region::Header | Region::Footer => {
                self.active_hf_set(region).map(|s| s.texts.as_slice()).unwrap_or(&[])
            }
        }
    }

    /// The page-instance hint for a paragraph's caret geometry: `None` for the body (each line is
    /// unique), `Some(active_hf_page)` for a header/footer (which repeats on every page, so the layout
    /// must be told which painted instance the caret is on).
    pub(crate) fn page_hint_for(&self, para: usize) -> Option<u32> {
        match decode_region(para).0 {
            Region::Body => None,
            Region::Header | Region::Footer => Some(self.active_hf_page),
        }
    }

    /// The per-paragraph visible-run maps for a region (see [`Self::body_segments`]).
    pub(crate) fn region_segments(&self, region: Region) -> &[Vec<(usize, usize)>] {
        match region {
            Region::Body => &self.body_segments,
            Region::Header | Region::Footer => {
                self.active_hf_set(region).map(|s| s.segments.as_slice()).unwrap_or(&[])
            }
        }
    }

    /// Map a *full-text* byte offset (model + JS-shell space) to the *visible-text* byte offset the
    /// caret geometry indexes, for namespaced paragraph `para`. A full offset that lands inside a
    /// hidden run (a deletion in Final, an insertion in Original) clamps to where the visible text
    /// resumes - the caret can't sit inside text that isn't drawn. Identity when nothing is hidden.
    pub(crate) fn full_to_visible(&self, para: usize, full_byte: usize) -> usize {
        let (region, local) = decode_region(para);
        let Some(segs) = self.region_segments(region).get(local) else { return full_byte };
        let mut acc = 0usize;
        for &(fs, fe) in segs {
            if full_byte < fs {
                return acc; // inside a hidden gap before this visible run
            }
            if full_byte < fe {
                return acc + (full_byte - fs);
            }
            acc += fe - fs;
        }
        acc // past the last visible run -> end of the visible text
    }

    /// The inverse of [`Self::full_to_visible`]: map a *visible-text* byte offset (from the caret
    /// geometry / a hit-test) back to the *full-text* byte offset the model + JS shell speak.
    pub(crate) fn visible_to_full(&self, para: usize, vis_byte: usize) -> usize {
        let (region, local) = decode_region(para);
        let Some(segs) = self.region_segments(region).get(local) else { return vis_byte };
        let mut rem = vis_byte;
        let mut last_full_end = 0usize;
        for &(fs, fe) in segs {
            let len = fe - fs;
            if rem < len {
                return fs + rem;
            }
            rem -= len;
            last_full_end = fe;
        }
        last_full_end // at / past the visible end -> the full offset just after the last visible run
    }

    /// The child document + region-local paragraph index for a namespaced `para`, or `None` when the
    /// region doesn't exist (e.g. a header index on a document that has no header). Header/footer
    /// indices route to the part the caret's page shows (per-section selection).
    pub(crate) fn route(&self, para: u32) -> Option<(&scriptor_crdt::CollabDoc, usize)> {
        let (region, local) = decode_region(para as usize);
        let doc = match region {
            Region::Body => Some(&self.doc),
            Region::Header | Region::Footer => match self.active_hf_set(region) {
                Some(set) => self.doc.hf_part_doc(&set.part),
                // Before the first relayout `hf_sets` is empty - fall back to the effective
                // default part so API edits (a `setHeaderText` followed by typing) still route.
                None if self.hf_sets.is_empty() => {
                    if region == Region::Footer { self.doc.footer_doc() } else { self.doc.header_doc() }
                }
                None => None,
            },
        }?;
        Some((doc, local))
    }

    pub(crate) fn byte_to_char(&self, para: usize, byte: usize) -> usize {
        let (region, local) = decode_region(para);
        match self.region_texts(region).get(local) {
            Some(t) => t.char_indices().take_while(|(b, _)| *b < byte).count(),
            None => 0,
        }
    }

    pub(crate) fn char_to_byte(&self, para: usize, ch: usize) -> usize {
        let (region, local) = decode_region(para);
        match self.region_texts(region).get(local) {
            Some(t) => t.char_indices().nth(ch).map(|(b, _)| b).unwrap_or(t.len()),
            None => 0,
        }
    }
}
