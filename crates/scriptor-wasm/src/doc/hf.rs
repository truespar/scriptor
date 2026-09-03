//! Headers, footers, and the active story.
//! 
//! Selects which header/footer part applies to a page (per section, honouring
//! titlePg), routes edits into the right child document while the caret is in one,
//! and dims the region that is not currently being edited, as Word does.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// How far to dim a region's ink: solid (`0`) when it's the active story, greyed otherwise - Word's
    /// active/inactive header-footer treatment.
    pub(crate) fn region_dim(&self, region: Region) -> f32 {
        if self.active_region == region { 0.0 } else { DIM_INACTIVE }
    }

    /// The `hf_sets` indices page `page` shows, `[header, footer]`. Pages past the map (the
    /// frame-extended tail) carry the last mapped page's parts forward.
    pub(crate) fn page_hf_at(&self, page: u32) -> [Option<usize>; 2] {
        self.page_hf
            .get(page as usize)
            .or(self.page_hf.last())
            .copied()
            .unwrap_or([None, None])
    }

    /// The header/footer set the caret's page shows for `region` - what "the header" means for
    /// hit-testing, caret text, and edit routing (per-section: the same click on another page can
    /// resolve to a different part).
    pub(crate) fn active_hf_set(&self, region: Region) -> Option<&HfSet> {
        let [h, f] = self.page_hf_at(self.active_hf_page);
        match region {
            Region::Header => h.and_then(|i| self.hf_sets.get(i)),
            Region::Footer => f.and_then(|i| self.hf_sets.get(i)),
            Region::Body => None,
        }
    }

    /// The child document behind the caret's page's header/footer band (see [`Self::active_hf_set`]).
    /// Before the first relayout `hf_sets` is empty; fall back to the document's effective default
    /// part so API edits (a `setHeaderText` followed by typing, tests) still route.
    pub(crate) fn active_hf_doc(&self) -> Option<&scriptor_crdt::CollabDoc> {
        match self.active_hf_set(self.active_region) {
            Some(set) => self.doc.hf_part_doc(&set.part),
            None if self.hf_sets.is_empty() => match self.active_region {
                Region::Header => self.doc.header_doc(),
                Region::Footer => self.doc.footer_doc(),
                Region::Body => None,
            },
            None => None,
        }
    }

    pub(crate) fn active_hf_doc_mut(&mut self, footer: bool) -> Option<&mut scriptor_crdt::CollabDoc> {
        let region = if footer { Region::Footer } else { Region::Header };
        match self.active_hf_set(region) {
            Some(set) => {
                let part = set.part.clone();
                self.doc.hf_part_doc_mut(&part)
            }
            None if self.hf_sets.is_empty() => {
                if footer { self.doc.footer_doc_mut() } else { self.doc.header_doc_mut() }
            }
            None => None,
        }
    }

    /// The x where an INLINE header/footer picture actually paints. `place_float` pins inline
    /// pictures at the content left; the paint path re-places them from the anchor paragraph's
    /// resolved alignment (the centred-logo footer) - hit-rects must cover the SAME box, else the
    /// visible logo isn't clickable and an invisible rect at the band's left steals footer clicks.
    pub(crate) fn inline_hf_x(&self, set_idx: usize, enc_id: u64, w: f32, geom: &FloatGeom, x: f32) -> f32 {
        let Some(set) = self.hf_sets.get(set_idx) else { return x };
        let Some(d) = self.doc.hf_part_doc(&set.part) else { return x };
        let local = img_local(enc_id);
        let anchor = d.paragraphs().ok().and_then(|paras| {
            paras.iter().position(|pp| pp.runs.iter().any(|r| r.image == Some(local)))
        });
        if let Some(b) = anchor.and_then(|i| set.blocks.get(i)) {
            let cw = (geom.page_w - geom.ml - geom.mr).max(1.0);
            match b.align {
                scriptor_layout::BlockAlign::Center => return geom.ml + (cw - w) / 2.0,
                scriptor_layout::BlockAlign::Right => return geom.ml + cw - w,
                _ => {}
            }
        }
        x
    }

    /// The dimming region a picture story byte belongs to (`IMG_BODY`, or `1 + hf_sets index`).
    pub(crate) fn img_region(&self, story: u8) -> Region {
        match story {
            IMG_BODY => Region::Body,
            s => match self.hf_sets.get(s as usize - 1) {
                Some(set) if set.header => Region::Header,
                Some(_) => Region::Footer,
                None => Region::Body,
            },
        }
    }

    /// Editable header/footer pictures across every part's child story, as
    /// `(encoded_id, context, placement)`. The id bands by the part's `hf_sets` index (story
    /// `1 + i`) so a hit-test result or an edit op routes back to the owning part via
    /// [`Self::image_doc`]. The context (part name + role) decides which pages the picture paints on.
    pub(crate) fn hf_images(&self) -> Vec<(u64, scriptor_crdt::ImageContext, scriptor_crdt::ImagePlacement)> {
        let mut out = Vec::new();
        for (i, set) in self.hf_sets.iter().enumerate() {
            let Some(d) = self.doc.hf_part_doc(&set.part) else { continue };
            let ctx = scriptor_crdt::ImageContext::Hf { part: set.part.clone(), header: set.header };
            let mut v: Vec<(u64, scriptor_crdt::ImagePlacement)> = d.image_placements().into_iter().collect();
            v.sort_by_key(|(id, _)| *id);
            for (id, p) in v {
                out.push((img_enc(1 + i as u8, id), ctx.clone(), p));
            }
        }
        out
    }

    /// The child story + child-local image id behind an encoded image id (see [`Self::hf_images`]). A
    /// body id (`< IMG_STORY`) maps to the body doc unchanged; a header/footer id routes to the part
    /// whose `hf_sets` band owns it. `None` if no story holds it.
    pub(crate) fn image_doc(&self, enc_id: u64) -> Option<(&scriptor_crdt::CollabDoc, u64)> {
        let local = img_local(enc_id);
        let doc = match img_story(enc_id) {
            IMG_BODY => Some(&self.doc),
            s => self
                .hf_sets
                .get(s as usize - 1)
                .and_then(|set| self.doc.hf_part_doc(&set.part)),
        };
        doc.map(|d| (d, local))
    }

    /// Set which story the caret is in (from a namespaced paragraph index), so undo/redo route to the
    /// right child document. The JS shell calls this on every selection change.
    #[wasm_bindgen(js_name = setActiveStory)]
    pub fn set_active_story(&mut self, para: u32) {
        self.active_region = decode_region(para as usize).0;
    }

    /// Set the page whose header/footer instance the caret is on (the JS shell computes it from the
    /// click on a multi-page document). Lets the caret resolve to that instance, not always page 1.
    #[wasm_bindgen(js_name = setHeaderFooterPage)]
    pub fn set_header_footer_page(&mut self, page: u32) {
        self.active_hf_page = page;
    }

    /// The default header as plain text (one line per paragraph).
    #[wasm_bindgen(js_name = headerText)]
    pub fn header_text(&self) -> String {
        self.doc.header_text()
    }

    /// The default footer as plain text.
    #[wasm_bindgen(js_name = footerText)]
    pub fn footer_text(&self) -> String {
        self.doc.footer_text()
    }

    /// Replace the default header with plain `text`. Re-paint after.
    #[wasm_bindgen(js_name = setHeaderText)]
    pub fn set_header_text(&mut self, text: &str) {
        self.doc.set_header_text(text);
    }

    /// Replace the default footer with plain `text`. Re-paint after.
    #[wasm_bindgen(js_name = setFooterText)]
    pub fn set_footer_text(&mut self, text: &str) {
        self.doc.set_footer_text(text);
    }

    /// Ensure a default header story exists - creating an empty one (a single blank paragraph) if the
    /// document has none - and return the namespaced paragraph index of its first paragraph, so the
    /// shell can drop the caret into the header to edit it (Word's Insert > Header). Idempotent: an
    /// existing header is left untouched (its content preserved). Re-layout + paint after.
    #[wasm_bindgen(js_name = ensureHeader)]
    pub fn ensure_header(&mut self) -> u32 {
        if self.doc.header_doc().is_none() {
            self.doc.set_header_text("");
        }
        self.active_region = Region::Header;
        HEADER_BASE as u32
    }

    /// Ensure a default footer story exists (see [`ensure_header`](Self::ensure_header)) and return
    /// the namespaced index of its first paragraph.
    #[wasm_bindgen(js_name = ensureFooter)]
    pub fn ensure_footer(&mut self) -> u32 {
        if self.doc.footer_doc().is_none() {
            self.doc.set_footer_text("");
        }
        self.active_region = Region::Footer;
        FOOTER_BASE as u32
    }
}
