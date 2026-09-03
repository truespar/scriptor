//! The style table and paragraph-to-style binding.
//! 
//! The effective table is the imported `styles.xml` with runtime edits folded in, so a
//! style change reflows every paragraph using it without touching those paragraphs.

use crate::*;

impl CollabDoc {
    /// The **effective** style table: the imported `word/styles.xml` (document defaults + named
    /// styles) with any runtime style-definition edits ([`model::STYLE_OVERRIDES`]) folded in. The
    /// edits reconcile in from loro on read - so a `set_style_props` this session, a peer's edit
    /// arrived over `merge`, or one restored from a snapshot is reflected before any resolve/preview/
    /// export, without the caller remembering a refresh step. Returns a `Ref` guard (interior-mutable
    /// cache); deref for `&StyleTable`. Mirrors [`Self::numbering`].
    pub fn styles(&self) -> std::cell::Ref<'_, StyleTable> {
        // Rebuild from the parsed base + the loro overrides only when something may have changed
        // (the dirty flag), so an unedited document's relayout doesn't re-clone the table each
        // keystroke. `try_borrow_mut` guards against a nested read (a `Ref` still alive): when it
        // fails an outer call already reconciled this read, so skipping is correct.
        if self.styles_dirty.get()
            && let Ok(mut eff) = self.styles.try_borrow_mut()
        {
            *eff = self.styles_base.clone();
            eff.apply_overrides(&model::read_style_overrides(&self.doc));
            eff.apply_added_styles(&model::read_added_styles(&self.doc));
            self.styles_dirty.set(false);
        }
        self.styles.borrow()
    }

    /// Edit a style's *definition* (Word's Modify-Style): merge `props`' set fields into style `id`'s
    /// per-field override in the loro [`model::STYLE_OVERRIDES`] map, so every paragraph resolving
    /// through `id` re-renders with the new properties. The edit persists in the op-log, syncs to
    /// peers, and is undoable. Direct, not a tracked revision - Word doesn't redline a style-definition
    /// change (it's a document setting, not paragraph content). `&self` (interior mutability), like the
    /// other edit verbs. Caller re-renders (the next `styles()` reconciles the change in).
    pub fn set_style_props(&self, id: &str, props: &StyleProps) -> Result<()> {
        model::write_style_override(&self.doc, id, props)?;
        self.doc.commit();
        self.styles_dirty.set(true);
        Ok(())
    }

    /// Add a new paragraph style `id` (Word's New-Style / Save-Selection-as-a-Style): record its
    /// identity (`name` + optional `based_on`) in the [`model::STYLE_ADDED`] map and its formatting in
    /// [`model::STYLE_OVERRIDES`], so it resolves, appears in the gallery, persists in the op-log,
    /// syncs, undoes, and exports - like any built-in. The caller then applies it to a paragraph via
    /// [`set_paragraph_style`](Self::set_paragraph_style). Direct, not tracked. `&self` (interior
    /// mutability). Caller re-renders.
    pub fn add_style(&self, id: &str, name: &str, based_on: Option<&str>, props: &StyleProps) -> Result<()> {
        model::write_added_style(&self.doc, id, name, based_on)?;
        model::write_style_override(&self.doc, id, props)?;
        self.doc.commit();
        self.styles_dirty.set(true);
        Ok(())
    }

    /// Paragraph `para`'s named style id (`w:pStyle`), or `None` for the default (Normal).
    pub fn paragraph_style(&self, para: usize) -> Option<String> {
        model::paragraph_style(&self.doc, para)
    }

    /// The Styles gallery: `(id, display name)` for each quick-style paragraph style the document
    /// defines (Title / Subtitle / Heading N / Normal / ...), in document order. `Normal` is always
    /// offered (prepended if the doc didn't flag it) so there's a way back to the base style.
    pub fn style_gallery(&self) -> Vec<(String, String)> {
        let styles = self.styles();
        let name = |id: &str| styles.names.get(id).cloned().unwrap_or_else(|| id.to_string());
        let mut out: Vec<(String, String)> = Vec::new();
        if !styles.gallery.iter().any(|id| id == "Normal") {
            out.push(("Normal".to_string(), name("Normal")));
        }
        for id in &styles.gallery {
            out.push((id.clone(), name(id)));
        }
        out
    }

    /// The resolved visual properties of paragraph style `id` (`""` -> defaults), for the Styles
    /// gallery's live previews: size (half-points), bold, italic, hex colour, and font family - each
    /// the value the style would render at (its own `basedOn` chain over docDefaults).
    pub fn resolve_style(&self, id: &str) -> model::StyleProps {
        self.styles().resolve((!id.is_empty()).then_some(id))
    }

    /// Set / clear paragraph `para`'s named style (`w:pStyle`) **directly** (no revision). `style = None`
    /// resets it to Normal.
    pub fn set_paragraph_style(&self, para: usize, style: Option<&str>, audit: &str) -> Result<()> {
        model::set_paragraph_style(&self.doc, para, style)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Suggest a style change: set / clear paragraph `para`'s style as a tracked `w:pPrChange`
    /// attributed to `author`/`date` (old style + props recorded for reject). Returns the revision id.
    pub fn suggest_paragraph_style(
        &self,
        para: usize,
        style: Option<&str>,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        model::suggest_paragraph_style(&self.doc, para, style, author, date, id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Add / refresh `word/styles.xml` so the document's styles ship on save. An imported styles.xml is
    /// preserved, with (a) each edited style's modeled props patched in place (Modify-Style, unmodeled
    /// children + unedited spacing attributes intact) and (b) any canonical quick style it lacked
    /// appended (the doc's own definitions otherwise win); a from-scratch doc gets a full styles.xml
    /// (from the *effective* table, so style edits ride along) + registration in `[Content_Types].xml` +
    /// `document.xml.rels`.
    pub(crate) fn write_styles_parts(&self, parts: &mut Vec<scriptor_ooxml::Part>) {
        let table = self.styles();
        let overrides = model::read_style_overrides(&self.doc);
        let xml = match parts.iter().find(|p| p.name == "word/styles.xml") {
            Some(p) => {
                let src = String::from_utf8_lossy(&p.data).into_owned();
                model::merge_styles_into_xml(&src, &table, &overrides)
            }
            None => model::export_styles_xml(&table),
        };
        set_part(parts, "word/styles.xml", xml.into_bytes());
        patch_content_types(
            parts,
            "/word/styles.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
        );
        patch_doc_rels(
            parts,
            "styles.xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
        );
    }
}
