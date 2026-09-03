//! Headers and footers, and the sections that select them.
//! 
//! Each distinct header/footer part is imported once as a child document, so two
//! sections sharing a part share the story and an edit shows in both, which is what
//! Word does. Resolution follows Word's carry-forward rule: a section with no
//! reference of its own inherits the previous section's, while `titlePg` leaves the
//! first-page slot blank rather than falling back to the default.

use crate::*;

impl CollabDoc {
    /// The **effective** part name for a header/footer slot of the LAST section (the body-final
    /// `sectPr` - what "the document's header" means for the single-section common case, and what the
    /// old last-reference-wins import resolved to for multi-section documents).
    fn effective_hf_part(&self, header: bool, first: bool) -> Option<&str> {
        let sec = self.sections_hf.last()?;
        match (header, first) {
            (true, false) => sec.header_default.as_deref(),
            (true, true) => sec.header_first.as_deref(),
            (false, false) => sec.footer_default.as_deref(),
            (false, true) => sec.footer_first.as_deref(),
        }
    }

    /// The child document behind an effective slot of the last section.
    fn effective_hf_doc(&self, header: bool, first: bool) -> Option<&CollabDoc> {
        let part = self.effective_hf_part(header, first)?;
        self.hf_docs.get(part).map(|p| &*p.doc)
    }

    /// The default header's paragraphs, materialized from its child document (empty if none).
    pub fn header(&self) -> Vec<Paragraph> {
        self.header_doc().map(|d| d.paragraphs().unwrap_or_default()).unwrap_or_default()
    }

    /// The default footer's paragraphs, materialized from its child document (empty if none).
    pub fn footer(&self) -> Vec<Paragraph> {
        self.footer_doc().map(|d| d.paragraphs().unwrap_or_default()).unwrap_or_default()
    }

    /// The header's child document, for editing it through the same path as the body (`None` if the
    /// document has no header). The footer's child document is [`footer_doc`](Self::footer_doc).
    /// "The" header is the last section's effective default part - exact for the single-section
    /// common case; per-section consumers address parts by name via [`hf_part_doc`](Self::hf_part_doc).
    pub fn header_doc(&self) -> Option<&CollabDoc> {
        self.effective_hf_doc(true, false)
    }

    /// The footer's child document, for editing (None if the document has no footer).
    pub fn footer_doc(&self) -> Option<&CollabDoc> {
        self.effective_hf_doc(false, false)
    }

    /// Mutable access to the header / footer child documents - needed to undo/redo within a story
    /// (each child owns its own `UndoManager`, so `Ctrl+Z` must reach the right one).
    pub fn header_doc_mut(&mut self) -> Option<&mut CollabDoc> {
        let part = self.effective_hf_part(true, false)?.to_string();
        self.hf_part_doc_mut(&part)
    }

    pub fn footer_doc_mut(&mut self) -> Option<&mut CollabDoc> {
        let part = self.effective_hf_part(false, false)?.to_string();
        self.hf_part_doc_mut(&part)
    }

    /// Whether any section uses a separate first-page header/footer (`<w:titlePg/>`). Rendering is
    /// per-section ([`SectionHf::title_pg`]); this document-level view feeds the synthesized
    /// single-section export.
    pub fn title_pg(&self) -> bool {
        self.title_pg
    }

    /// The first-page header's paragraphs (empty when there's no first-page header). Painted on page 1
    /// instead of the default when [`title_pg`](Self::title_pg).
    pub fn header_first(&self) -> Vec<Paragraph> {
        self.header_first_doc().map(|d| d.paragraphs().unwrap_or_default()).unwrap_or_default()
    }

    /// The first-page footer's paragraphs (empty when there's no first-page footer).
    pub fn footer_first(&self) -> Vec<Paragraph> {
        self.footer_first_doc().map(|d| d.paragraphs().unwrap_or_default()).unwrap_or_default()
    }

    /// The first-page header / footer child documents (for editing them through the body path).
    pub fn header_first_doc(&self) -> Option<&CollabDoc> {
        self.effective_hf_doc(true, true)
    }

    pub fn footer_first_doc(&self) -> Option<&CollabDoc> {
        self.effective_hf_doc(false, true)
    }

    pub fn header_first_doc_mut(&mut self) -> Option<&mut CollabDoc> {
        let part = self.effective_hf_part(true, true)?.to_string();
        self.hf_part_doc_mut(&part)
    }

    pub fn footer_first_doc_mut(&mut self) -> Option<&mut CollabDoc> {
        let part = self.effective_hf_part(false, true)?.to_string();
        self.hf_part_doc_mut(&part)
    }

    /// Every header/footer part in the model, as `(part_name, is_header)` in stable (name) order -
    /// the per-page selection in the renderer keys off these.
    pub fn hf_parts(&self) -> Vec<(String, bool)> {
        self.hf_docs.iter().map(|(k, v)| (k.clone(), v.is_header)).collect()
    }

    /// A header/footer part's child document, by part name (`word/footer2.xml`).
    pub fn hf_part_doc(&self, part: &str) -> Option<&CollabDoc> {
        self.hf_docs.get(part).map(|p| &*p.doc)
    }

    /// Mutable access to a header/footer part's child document.
    ///
    /// Handing out the `&mut` marks the part edited, which is what makes save re-render it. Every
    /// other `*_doc_mut` accessor routes through here, so this is the one place that has to
    /// remember. See [`HfPartDoc::dirty`] for why an unedited part must not be re-rendered.
    pub fn hf_part_doc_mut(&mut self, part: &str) -> Option<&mut CollabDoc> {
        self.hf_docs.get_mut(part).map(|p| {
            p.dirty = true;
            &mut *p.doc
        })
    }

    /// How many sections the document has (>= 1; one per `<w:sectPr>` in document order).
    pub fn num_sections(&self) -> usize {
        self.sections_hf.len()
    }

    /// Section `idx`'s effective header/footer bindings (clamped to the last section).
    pub fn section_hf(&self, idx: usize) -> &SectionHf {
        &self.sections_hf[idx.min(self.sections_hf.len() - 1)]
    }

    /// Anchored text boxes from the header/footer parts (the rotated margin stamp), render-only.
    pub fn textboxes(&self) -> &[PlacedTextBox] {
        &self.textboxes
    }

    /// The child story behind a header/footer reference, for re-serializing that part on save.
    pub(crate) fn hf_child(&self, part_name: &str) -> Option<&CollabDoc> {
        self.hf_docs.get(part_name).map(|p| &*p.doc)
    }

    /// The default header as plain text (paragraphs joined by newlines).
    pub fn header_text(&self) -> String {
        hf_text(&self.header())
    }

    /// The default footer as plain text.
    pub fn footer_text(&self) -> String {
        hf_text(&self.footer())
    }

    /// Replace the default header with plain `text` (one paragraph per line) - rebuilds its child
    /// document. Caret editing of the header otherwise goes through the same path as the body.
    /// Writes to the last section's effective default part; a document with no header yet gets a
    /// fresh part (bound into every section, materialized as part + ref on save).
    pub fn set_header_text(&mut self, text: &str) {
        self.set_hf_text(true, text);
    }

    /// Replace the default footer with plain `text` - rebuilds its child document.
    pub fn set_footer_text(&mut self, text: &str) {
        self.set_hf_text(false, text);
    }

    fn set_hf_text(&mut self, header: bool, text: &str) {
        let key = self
            .effective_hf_part(header, false)
            .map(str::to_string)
            .unwrap_or_else(|| if header { "word/header1.xml" } else { "word/footer1.xml" }.into());
        let child = Box::new(Self::doc_from_paragraphs(&hf_from_text(text)));
        // Replacing the story wholesale is as edited as it gets, so this part must be re-rendered
        // on save even though it never went through `hf_part_doc_mut`.
        self.hf_docs.insert(key.clone(), HfPartDoc { is_header: header, doc: child, dirty: true });
        for sec in &mut self.sections_hf {
            let slot = if header { &mut sec.header_default } else { &mut sec.footer_default };
            if slot.is_none() {
                *slot = Some(key.clone());
            }
        }
    }

    /// Build a child document (its own loro tree) from a paragraph list - used to (re)materialize a
    /// header/footer from plain text. Run text + style only (the plain-text edit path carries no
    /// run/paragraph properties); rich content arrives via [`from_document_xml`](Self::from_document_xml).
    fn doc_from_paragraphs(paras: &[Paragraph]) -> Self {
        let me = Self::new();
        for p in paras {
            let _ = model::append_paragraph(&me.doc, &p.runs, p.style.as_deref());
        }
        me.doc.commit();
        me.clear_undo();
        me
    }
}
