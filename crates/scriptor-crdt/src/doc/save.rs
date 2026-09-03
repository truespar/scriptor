//! Writing the document back out as a `.docx`.
//! 
//! Regenerates `document.xml` from the model, refreshes the parts the model owns
//! (styles, numbering, comments, headers and footers), and patches the relationship
//! and content-type parts so images and hyperlinks still resolve. Everything the
//! model does not understand is carried through untouched from the source package.

use crate::*;

impl CollabDoc {
    /// Serialize the modeled content to a valid, Word-openable `word/document.xml` (body + sectPr +
    /// header/footer references) by walking the loro block tree's body order ([`model::body_nodes`]):
    /// top-level paragraphs interleaved with table nodes, each table read from its grid (tables-crdt).
    pub fn to_document_xml(&self) -> Result<String> {
        let refs: Vec<model::HfRef> = self.hf.iter().map(|(r, _)| r.clone()).collect();
        let mut xml = model::export_document_xml_via_nodes(&self.doc, &self.page, &refs, self.title_pg)?;
        // `w:background` is document-level metadata (not in the op log): re-emit it in its schema
        // slot - the first child of `<w:document>`, right before `<w:body>` (the exporter's fixed
        // prologue ends with that tag). Dropping it silently lost the page colour on save.
        if let Some(bg) = &self.background {
            xml = xml.replacen("<w:body>", &format!("<w:background w:color=\"{bg}\"/><w:body>"), 1);
        }
        Ok(xml)
    }

    /// Serialize the whole document to `.docx` bytes (the browser save path). Re-zips the original
    /// OPC parts with an updated `word/document.xml` and the edited header/footer parts, preserving
    /// every other part verbatim. A from-scratch document gets a minimal valid package (body only;
    /// materializing brand-new headers for a fresh doc on save is a follow-up).
    pub fn to_docx_bytes(&self) -> Result<Vec<u8>> {
        // A header / footer part present in the model but never referenced by a section (added to a
        // document that had none) is materialized on save: a new part + rel + content-type override +
        // a `sectPr` reference. `(is_header, part_name, rId)`, the rId pre-allocated so the same id
        // goes into both the relationship and the reference.
        let rels_src = self
            .source_parts
            .iter()
            .find(|p| p.name == "word/_rels/document.xml.rels")
            .map(|p| String::from_utf8_lossy(&p.data).into_owned())
            .unwrap_or_else(|| String::from_utf8_lossy(DOC_RELS_MIN).into_owned());
        let mut next_rid = max_rid(&rels_src) + 1;
        let mut new_hf: Vec<(bool, String, String)> = Vec::new();
        for (part_name, hfp) in &self.hf_docs {
            if !self.hf.iter().any(|(_, pn)| pn == part_name) {
                new_hf.push((hfp.is_header, part_name.clone(), format!("rId{next_rid}")));
                next_rid += 1;
            }
        }

        // The full sectPr reference list: the imported refs + any newly-materialized ones. Built here
        // (rather than via `to_document_xml`, which reads only `self.hf`) so the new references ship.
        let mut refs: Vec<model::HfRef> = self.hf.iter().map(|(r, _)| r.clone()).collect();
        for (is_header, _, r_id) in &new_hf {
            refs.push(model::HfRef {
                is_header: *is_header,
                kind: "default".to_string(),
                r_id: r_id.clone(),
            });
        }
        let document_xml = model::export_document_xml_via_nodes(&self.doc, &self.page, &refs, self.title_pg)?;

        let mut parts = if self.source_parts.is_empty() {
            let mut p = self.minimal_parts()?;
            set_part(&mut p, "word/document.xml", document_xml.into_bytes());
            p
        } else {
            let mut p = self.source_parts.clone();
            set_part(&mut p, "word/document.xml", document_xml.into_bytes());
            p
        };
        // Refresh the imported header/footer parts from the model - including their editable pictures
        // (a resized/cropped/moved header logo): emit each image run's `<w:drawing>` from the child
        // story's placements and wire its `rIdImg{id}` blip into THIS part's own `.rels` (+ content
        // type + media bytes), mirroring the body. Without this, header/footer pictures are dropped.
        // Iterates the part map (each part exactly once) - the old per-REFERENCE walk wrote one
        // role's story into every same-role part, so saving a multi-section document overwrote
        // section 1's header file with section 2's content.
        for (part_name, hfp) in &self.hf_docs {
            if !self.hf.iter().any(|(_, pn)| pn == part_name) {
                continue; // unreferenced (fresh) part - written with a rel + reference below
            }
            // Never edited, and the source package still holds it: keep those bytes. Re-rendering
            // would rebuild the part from a flat paragraph list, which flattens a table in a header
            // to loose paragraphs - real loss on a document that was only opened and saved. The
            // `parts` guard matters because a document reconstructed from a loro snapshot has no
            // source parts, and skipping there would leave the section reference dangling.
            if !hfp.dirty && parts.iter().any(|p| p.name == *part_name) {
                continue;
            }
            let child = self.hf_child(part_name);
            let paras = child.map(|c| c.paragraphs().unwrap_or_default()).unwrap_or_default();
            let images = child.map(|c| c.image_placements()).unwrap_or_default();
            set_part(&mut parts, part_name, model::export_hdr_ftr_xml(&paras, hfp.is_header, &images).into_bytes());
            let base = part_name.strip_prefix("word/").unwrap_or(part_name);
            let rels_name = format!("word/_rels/{base}.rels");
            for (id, p) in &images {
                if p.media.starts_with("rid:") {
                    continue;
                }
                let target = p.media.strip_prefix("word/").unwrap_or(&p.media);
                patch_internal_rel_in(&mut parts, &rels_name, &format!("rIdImg{id}"), target, IMAGE_REL);
                if let Some(ext) = std::path::Path::new(&p.media).extension().and_then(|e| e.to_str())
                    && let Some(ct) = image_content_type(ext)
                {
                    patch_content_type_default(&mut parts, &ext.to_ascii_lowercase(), ct);
                }
                if !parts.iter().any(|pt| pt.name == p.media)
                    && let Some(bytes) = self.inserted_media_bytes(&p.media)
                {
                    set_part(&mut parts, &p.media, bytes);
                }
            }
        }
        // Write + register the newly-materialized header/footer parts (no images - a fresh H/F).
        for (is_header, part_name, r_id) in &new_hf {
            let paras = self
                .hf_child(part_name)
                .map(|c| c.paragraphs().unwrap_or_default())
                .unwrap_or_default();
            let no_images = std::collections::HashMap::new();
            set_part(&mut parts, part_name, model::export_hdr_ftr_xml(&paras, *is_header, &no_images).into_bytes());
            let (ct, rel_type) = if *is_header {
                (
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
                )
            } else {
                (
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml",
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer",
                )
            };
            patch_content_types(&mut parts, &format!("/{part_name}"), ct);
            let target = part_name.strip_prefix("word/").unwrap_or(part_name);
            patch_doc_rels_with_id(&mut parts, r_id, target, rel_type);
        }
        self.write_comment_parts(&mut parts);
        self.write_numbering_parts(&mut parts);
        self.write_styles_parts(&mut parts);
        self.patch_link_and_image_rels(&mut parts);
        scriptor_ooxml::write_parts_bytes(&parts)
    }

    /// Wire the rebuilt `document.xml`'s synthesized relationship ids into the package: an
    /// external-hyperlink rel per `rIdLnk{id}` and an image rel (+ content-type default + media
    /// bytes for inserted pictures) per `rIdImg{id}`. Every path that writes a package around the
    /// rebuilt document needs this - the browser save (`to_docx_bytes`) and the template export
    /// (`export_docx`) alike - because a dangling `r:id` is a package-structure conformance error
    /// (Word shows broken images / dead links).
    fn patch_link_and_image_rels(&self, parts: &mut Vec<scriptor_ooxml::Part>) {
        // Relationship type for an external hyperlink's `r:id`. Internal `#anchor` links need no rel.
        const HLINK_REL: &str =
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
        // External hyperlinks: inject a rel per external target (`rIdLnk{id}` -> URL), matching the
        // `r:id` the export emitted.
        for (id, target) in model::read_hyperlinks(&self.doc) {
            if !target.starts_with('#') {
                patch_external_rel(parts, &format!("rIdLnk{id}"), &target, HLINK_REL);
            }
        }
        // Pictures: wire each blip (`r:embed="rIdImg{id}"`) to its media part + ensure the extension's
        // content type. An imported picture's bytes already live in the source parts; an inserted
        // picture's bytes are injected from `pending_media`. (Unresolved `rid:` entries - a bytes-only
        // load with no rels - are skipped.)
        for (id, p) in model::read_images(&self.doc) {
            if p.media.starts_with("rid:") {
                continue;
            }
            let target = p.media.strip_prefix("word/").unwrap_or(&p.media);
            patch_internal_rel(parts, &format!("rIdImg{id}"), target, IMAGE_REL);
            if let Some(ext) = std::path::Path::new(&p.media).extension().and_then(|e| e.to_str())
                && let Some(ct) = image_content_type(ext) {
                    patch_content_type_default(parts, &ext.to_ascii_lowercase(), ct);
                }
            // Inserted-picture bytes (not in the source parts) ship from the in-session buffer or the
            // CRDT `media` map (the latter survives a reopen-from-op-log).
            if !parts.iter().any(|pt| pt.name == p.media)
                && let Some(bytes) = self.inserted_media_bytes(&p.media) {
                    set_part(parts, &p.media, bytes);
                }
        }
    }

    fn minimal_parts(&self) -> Result<Vec<scriptor_ooxml::Part>> {
        let document = self.to_document_xml()?;
        Ok(vec![
            scriptor_ooxml::Part { name: "[Content_Types].xml".into(), data: CONTENT_TYPES_MIN.into() },
            scriptor_ooxml::Part { name: "_rels/.rels".into(), data: ROOT_RELS_MIN.into() },
            scriptor_ooxml::Part { name: "word/document.xml".into(), data: document.into_bytes() },
            scriptor_ooxml::Part {
                name: "word/_rels/document.xml.rels".into(),
                data: DOC_RELS_MIN.into(),
            },
        ])
    }

    /// Write a `.docx`: take every part of `template` verbatim except `word/document.xml`, which
    /// is replaced by this document's serialized content. Reusing the source as the template keeps
    /// its styles / numbering / relationships, so the result opens cleanly in Word.
    pub fn export_docx(&self, template: &Path, out: &Path) -> Result<()> {
        let mut parts = scriptor_ooxml::read_parts(template)?;
        let document = parts
            .iter_mut()
            .find(|p| p.name == "word/document.xml")
            .context("template is missing word/document.xml")?;
        document.data = self.to_document_xml()?.into_bytes();
        // The rebuilt document.xml references synthesized rel ids (`rIdImg{id}` / `rIdLnk{id}`)
        // that the template's rels never contain - without patching them in, every exported doc
        // with a picture or external hyperlink carried dangling relationships (broken images /
        // dead links; the schema gate flags it as a package-structure error).
        self.patch_link_and_image_rels(&mut parts);
        scriptor_ooxml::write_parts(out, &parts)
    }
}
