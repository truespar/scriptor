//! Page setup and export.
//! 
//! Size, margins and orientation, plus handing the document back as OOXML bytes.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// The current document serialized to OOXML `word/document.xml` (the edited body). Hook for
    /// "save"; full `.docx` re-packaging (re-zip with the source's other parts) is a follow-up.
    #[wasm_bindgen(js_name = toDocumentXml)]
    pub fn to_document_xml(&self) -> Result<String, JsError> {
        self.doc.to_document_xml().map_err(to_js)
    }

    /// Save the whole document to `.docx` bytes - the original package re-zipped with the edited
    /// body + header/footer parts (or a minimal package for a from-scratch document).
    #[wasm_bindgen(js_name = toDocx)]
    pub fn to_docx(&self) -> Result<Vec<u8>, JsError> {
        self.doc.to_docx_bytes().map_err(to_js)
    }

    /// Set the page size in twips (Letter = 12240x15840, A4 = 11906x16838).
    #[wasm_bindgen(js_name = setPageSize)]
    pub fn set_page_size(&mut self, width: u32, height: u32) {
        self.doc.set_page_size(width, height);
    }

    /// Set the page margins in twips (1 inch = 1440).
    #[wasm_bindgen(js_name = setMargins)]
    pub fn set_margins(&mut self, top: u32, right: u32, bottom: u32, left: u32) {
        self.doc.set_margins(top, right, bottom, left);
    }

    /// Set page orientation (true = landscape); swaps the page dimensions if needed.
    #[wasm_bindgen(js_name = setLandscape)]
    pub fn set_landscape(&mut self, landscape: bool) {
        self.doc.set_landscape(landscape);
    }

    /// Page geometry in twips: `[width, height, marginTop, marginRight, marginBottom, marginLeft,
    /// headerDist, footerDist]`. For the ruler + the Layout tab's page-size / margin controls.
    #[wasm_bindgen(js_name = pageGeometry)]
    pub fn page_geometry(&self) -> Vec<u32> {
        let p = self.doc.page_geometry();
        // The page-setup UI consumes this as an unsigned twip array; a negative (imported) margin has
        // no UI affordance, so surface it as 0. Pagination uses the true signed value in `layout`.
        vec![
            p.width,
            p.height,
            p.margin_top.max(0) as u32,
            p.margin_right.max(0) as u32,
            p.margin_bottom.max(0) as u32,
            p.margin_left.max(0) as u32,
            p.header_dist,
            p.footer_dist,
        ]
    }
}
