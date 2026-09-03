//! Building a document from OOXML.
//! 
//! Parses a `.docx` package (or a bare `document.xml`) into the CRDT: paragraphs and
//! runs into the block tree, tracked changes and comments into Peritext marks, images
//! and unmodeled content into their side maps, and the header/footer parts into child
//! documents. This is the one place that writes nearly every field of `CollabDoc`.

use crate::*;

impl CollabDoc {
    /// Build a document from `word/document.xml` bytes (the modeled subset; see [`model`]). Captures
    /// comment anchor ranges (`w:commentRangeStart`/`End`) as `cmt~{id}` marks; the comment *bodies*
    /// are loaded separately from `word/comments.xml` (see [`from_parts`](Self::from_parts)).
    pub fn from_document_xml(xml: &[u8]) -> Result<Self> {
        let mut me = Self::new();
        me.title_pg = model::title_page(xml); // Different First Page (the header/footer parts load in from_parts)
        let (_, anchors) = model::import_document_xml(&me.doc, xml)?;
        // Pictures (`w:drawing`) become editable image runs: a placeholder char per drawing, anchored
        // to its paragraph, with geometry/crop/placement in the `images` map. Ids are 0..n in document
        // order; media holds the embed rel (`rid:{rId}`) until `from_parts` resolves it to a media part.
        let draws = model::parse_images(xml);
        let image_ids: Vec<u64> = (0..draws.len() as u64).collect();
        // Unmodeled embedded objects (`w:object` / `w:control`) are preserved verbatim: each captured
        // run gets a `raw~{id}` placeholder anchored to its paragraph, its XML in the `rawxml` map. See
        // `docs/passthrough.md`.
        let raws = model::parse_passthrough(xml);
        let raw_ids: Vec<u64> = (0..raws.len() as u64).collect();
        let any = !anchors.comments.is_empty()
            || !anchors.fields.is_empty()
            || !anchors.bookmarks.is_empty()
            || !anchors.hyperlinks.is_empty()
            || !draws.is_empty()
            || !raws.is_empty();
        if any {
            // Configure every anchor's mark key before applying them (cmt~ / fld~ / bkm~ / lnk~ / img~ /
            // raw~).
            let cids: Vec<u64> = anchors.comments.iter().map(|a| a.id).collect();
            let fids: Vec<u64> = anchors.fields.iter().map(|a| a.id).collect();
            let bids: Vec<u64> = anchors.bookmarks.iter().map(|a| a.id).collect();
            let lids: Vec<u64> = anchors.hyperlinks.iter().map(|a| a.id).collect();
            configure_marks_with(&me.doc, &cids, &fids, &bids, &lids, &image_ids, &raw_ids);
            model::apply_comment_anchors(&me.doc, &anchors.comments)?;
            model::apply_field_anchors(&me.doc, &anchors.fields)?;
            model::apply_bookmark_anchors(&me.doc, &anchors.bookmarks)?;
            model::apply_hyperlink_anchors(&me.doc, &anchors.hyperlinks)?;
            // Anchor each drawing after the text-range anchors (it appends a placeholder at the
            // paragraph's end, which can't disturb a fixed-range mark applied above). Clamp the
            // anchor paragraph to the live block range: a picture sharing a paragraph with a text
            // box, or any future parse skew, must never abort the whole import - it degrades to
            // anchoring on the nearest real paragraph instead.
            let block_count = model::block_seq(&me.doc).len();
            for (i, di) in draws.iter().enumerate() {
                if block_count == 0 {
                    break;
                }
                // A picture inside a text box is part of that box, and the box's run is captured
                // verbatim. Giving it a body placeholder as well emitted it twice - once inside the
                // captured XML and once hoisted to body level - and, before the capture was allowed,
                // once instead of the box entirely.
                if di.in_textbox {
                    continue;
                }
                let para = di.para_index.min(block_count - 1);
                let id = i as u64;
                let placement = model::ImagePlacement {
                    media: format!("rid:{}", di.embed),
                    w_emu: di.w_emu,
                    h_emu: di.h_emu,
                    crop_l: di.crop_l,
                    crop_t: di.crop_t,
                    crop_r: di.crop_r,
                    crop_b: di.crop_b,
                    floating: di.anchored,
                    behind: di.behind,
                    h_from: di.h_from.clone(),
                    v_from: di.v_from.clone(),
                    x_emu: di.x_emu,
                    y_emu: di.y_emu,
                    h_align: di.h_align.clone(),
                    v_align: di.v_align.clone(),
                    wrap: di.wrap.clone(),
                };
                model::write_image(&me.doc, id, &placement)?;
                match &di.track {
                    Some(t) => model::insert_image_placeholder_tracked(&me.doc, id, para, t)?,
                    None => model::insert_image_placeholder(&me.doc, id, para)?,
                }
            }
            // Anchor each captured embedded object at the position its run occupied, storing the
            // verbatim `<w:r>` XML for re-emission on export. Position rather than paragraph end:
            // appending moved an object that sat between two text runs, so `BEFORE | object | AFTER`
            // came back as `BEFOREAFTER | object`.
            //
            // `text_offset` counts modeled text only, so each placeholder already inserted into the
            // same paragraph shifts the ones after it by one codepoint. `raws` is in document order,
            // hence ascending offset within a paragraph, so a running per-paragraph count is enough.
            //
            // The text-range anchors above were applied against the same modeled-text coordinates, so
            // a placeholder landing inside one of their ranges extends that range over it - which is
            // what should happen when a commented span contains an embedded object. Marks are
            // configured `ExpandType::None`, so one landing exactly on a boundary stays outside.
            let mut placed: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
            for (i, item) in raws.iter().enumerate() {
                if block_count == 0 {
                    break;
                }
                let para = item.para_index.min(block_count - 1);
                let id = i as u64;
                let shift = placed.entry(para).or_insert(0);
                let at = item.text_offset + *shift;
                *shift += 1;
                model::write_raw(&me.doc, id, &item.xml)?;
                match &item.track {
                    Some(t) => model::insert_raw_placeholder_tracked(&me.doc, id, para, at, t)?,
                    None => model::insert_raw_placeholder(&me.doc, id, para, at)?,
                }
            }
        }
        // Block-level `<w:sdt>` / `<w:customXml>` wrappers: capture each opening verbatim and anchor it
        // to the body nodes it encloses (a `wrapopen`/`wrapclose` id list on the first / last inner
        // node's meta), so the control round-trips on export while its inner blocks stay editable. Image
        // / raw placeholders above don't add block nodes, so `body_nodes` indices still line up with the
        // `parse_block_wraps` block counter. See `docs/passthrough.md`.
        let block_wraps = model::parse_block_wraps(xml);
        if !block_wraps.is_empty() {
            let nodes = model::body_nodes(&me.doc);
            let mut opens_at: std::collections::HashMap<usize, Vec<u64>> = std::collections::HashMap::new();
            let mut closes_at: std::collections::HashMap<usize, Vec<u64>> = std::collections::HashMap::new();
            for w in &block_wraps {
                // Skip a wrapper whose block range fell outside the built tree (a parse skew) rather than
                // anchoring it wrong.
                if w.start_block < nodes.len() && w.end_block < nodes.len() {
                    model::write_block_wrap(&me.doc, w.id, &w.prefix)?;
                    opens_at.entry(w.start_block).or_default().push(w.id);
                    closes_at.entry(w.end_block).or_default().push(w.id);
                }
            }
            for (idx, node) in nodes.iter().enumerate() {
                let mut opens = opens_at.remove(&idx).unwrap_or_default();
                let mut closes = closes_at.remove(&idx).unwrap_or_default();
                if opens.is_empty() && closes.is_empty() {
                    continue;
                }
                opens.sort_unstable(); // outer-first (ascending capture id)
                closes.sort_unstable_by(|a, b| b.cmp(a)); // inner-first (descending capture id)
                let meta = model::block_node_meta(&me.doc, node)?;
                model::set_block_wrap_anchors(&meta, &opens, &closes)?;
            }
        }
        // Per-section properties: attach each `<w:sectPr>` verbatim to its carrier paragraph (and the
        // body-final one document-level), so multi-section page geometry / columns / header-footer
        // refs round-trip instead of collapsing into one synthesized final sectPr. Runs before commit
        // so the writes land in the same version as the paragraphs they annotate.
        model::apply_section_props(&me.doc, xml)?;
        me.doc.commit();
        me.page = model::parse_page_geometry(xml);
        me.sections = model::parse_sections(xml);
        me.clear_undo(); // the initial load is the baseline, not an undoable edit
        Ok(me)
    }

    /// Read a `.docx` from disk into the CRDT, modeling its `word/document.xml` + `word/styles.xml`.
    pub fn import_docx(path: &Path) -> Result<Self> {
        Self::from_parts(scriptor_ooxml::read_parts(path)?)
    }

    /// Build a document from the in-memory bytes of a `.docx` (the browser / wasm path, where there
    /// is no filesystem). Mirrors [`import_docx`].
    pub fn from_docx_bytes(bytes: &[u8]) -> Result<Self> {
        // A password-protected / encrypted .docx is an OLE compound-file (CFB) wrapper, not a zip -
        // detect its magic so we return a clear message instead of a confusing "not a zip" error.
        if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
            anyhow::bail!("this document is encrypted or password-protected, which isn't supported");
        }
        Self::from_parts(scriptor_ooxml::read_parts_bytes(bytes)?)
    }

    /// Build from already-unzipped OPC parts: model `word/document.xml` and resolve
    /// `word/styles.xml` (the style hierarchy used to render heading / title sizing).
    pub(crate) fn from_parts(parts: Vec<scriptor_ooxml::Part>) -> Result<Self> {
        // The main document part is named by `_rels/.rels` (usually `word/document.xml`, but not
        // always - e.g. `word/trial.xml`); fall back to the conventional name when there's no rels.
        let doc_name = parts
            .iter()
            .find(|p| p.name == "_rels/.rels")
            .and_then(|p| model::main_document_part(&p.data))
            .unwrap_or_else(|| "word/document.xml".to_string());
        let document = parts
            .iter()
            .find(|p| p.name == doc_name)
            .or_else(|| parts.iter().find(|p| p.name == "word/document.xml"))
            .with_context(|| {
                // An `.odt` (OpenDocument) renamed `.docx` is a zip too, but with no Word main part -
                // name the real format rather than a misleading "missing document.xml".
                if parts.iter().any(|p| p.name == "content.xml" || p.name == "mimetype") {
                    "this looks like an OpenDocument (.odt) file, not a Word (.docx) document".to_string()
                } else {
                    format!("missing the main document part ({doc_name}) - is this a Word document?")
                }
            })?;
        let mut me = Self::from_document_xml(&document.data)?;
        if let Some(s) = parts.iter().find(|p| p.name == "word/styles.xml") {
            me.styles_base = model::parse_styles(&s.data);
        }
        // Paragraph-spacing compatibility: legacy documents SUM adjacent space-after +
        // space-before where modern Word consolidates to the max (see the layout flow).
        if let Some(s) = parts.iter().find(|p| p.name == "word/settings.xml") {
            me.legacy_spacing = model::settings_legacy_spacing(&s.data);
        }
        // Page background: the colour is kept for round-trip regardless; Word only PAINTS it when
        // settings.xml opts in via w:displayBackgroundShape.
        me.background = model::parse_background(&document.data);
        if me.background.is_some() {
            me.background_shown = parts
                .iter()
                .find(|p| p.name == "word/settings.xml")
                .is_some_and(|s| model::settings_display_background(&s.data));
        }
        // Always offer Word's built-in quick styles: fill any the doc didn't define (its own win), so
        // the gallery is Word-complete and an applied built-in never dangles on export.
        me.styles_base.merge_defaults();
        // Seed the effective table from the parsed base; runtime edits reconcile in on read.
        *me.styles.borrow_mut() = me.styles_base.clone();
        me.styles_dirty.set(false);

        // Document relationships (rId -> target part) - drives both header/footer refs and body
        // image blips. The rels of the main part `<dir>/<name>` live at `<dir>/_rels/<name>.rels`.
        let doc_rels_name = match doc_name.rsplit_once('/') {
            Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
            None => format!("_rels/{doc_name}.rels"),
        };
        let doc_rels = parts
            .iter()
            .find(|p| p.name == doc_rels_name)
            .map(|p| model::resolve_rels(&p.data))
            .unwrap_or_default();

        // Resolve external hyperlinks: import stored each external target as `rid:{r:id}`; map it to the
        // URL in the document rels. Internal `#anchor` targets are left as-is.
        let links = model::read_hyperlinks(&me.doc);
        let mut links_changed = false;
        for (id, target) in &links {
            if let Some(rid) = target.strip_prefix("rid:")
                && let Some(url) = doc_rels.get(rid) {
                    model::write_hyperlink(&me.doc, *id, url)?;
                    links_changed = true;
                }
        }
        if links_changed {
            me.doc.commit();
        }

        // Resolve each editable image's media: import stored the embed rel as `rid:{rId}`; map it to the
        // media part (e.g. `word/media/image1.png`) through the document rels, mirroring the hyperlink
        // resolution above. Unresolved entries (a bytes-only `from_document_xml` load) keep the rel form.
        let mut images_changed = false;
        for (id, mut p) in model::read_images(&me.doc) {
            if p.media.starts_with("rid:") {
                let rid = p.media[4..].to_string();
                if let Some(target) = doc_rels.get(&rid) {
                    p.media = target
                        .strip_prefix('/')
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("word/{target}"));
                    model::write_image(&me.doc, id, &p)?;
                    images_changed = true;
                }
            }
        }
        if images_changed {
            me.doc.commit();
        }

        // Body pictures, resolved through the document rels (the read-only render path; P2 will render
        // from the editable `images` map instead).
        collect_images(&document.data, &doc_rels, ImageContext::Body, &mut me.images);

        // Headers / footers, PER SECTION: each `<w:sectPr>` (in-paragraph terminators + the
        // body-final one, in document order) carries its own reference set + `titlePg`, and a section
        // without a reference for some slot uses the previous section's part (Word's carry-forward
        // inheritance - the mechanism that puts section 1's first-page footer on section 2's first
        // page). Resolve the effective part name per slot per section here, and import each DISTINCT
        // part ONCE as a child CollabDoc keyed by part name - two sections sharing a part share the
        // story, so an edit shows on both (exactly Word's behavior). The `even` variant is not
        // rendered (`w:evenAndOddHeaders` unmodeled); its refs + parts round-trip verbatim.
        me.title_pg = model::title_page(&document.data);
        let sect_slices = model::parse_section_props(&document.data);
        let mut sections_hf: Vec<SectionHf> = Vec::new();
        let mut carry = SectionHf::default();
        for slice in &sect_slices {
            let mut sec = SectionHf { title_pg: model::title_page(slice.as_bytes()), ..carry.clone() };
            for r in model::header_footer_refs(slice.as_bytes()) {
                let Some(target) = doc_rels.get(&r.r_id) else { continue };
                let part_name = target
                    .strip_prefix('/')
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("word/{target}"));
                match (r.is_header, r.kind.as_str()) {
                    (true, "default") => sec.header_default = Some(part_name.clone()),
                    (true, "first") => sec.header_first = Some(part_name.clone()),
                    (false, "default") => sec.footer_default = Some(part_name.clone()),
                    (false, "first") => sec.footer_first = Some(part_name.clone()),
                    _ => continue, // `even` - preserved verbatim, not imported
                }
                me.hf.push((r.clone(), part_name));
            }
            carry = SectionHf { title_pg: false, ..sec.clone() };
            sections_hf.push(sec);
        }
        if sections_hf.is_empty() {
            sections_hf.push(SectionHf::default());
        }
        me.sections_hf = sections_hf;

        // Import each referenced part once (several sections may share it).
        let mut part_roles: Vec<(String, bool)> = Vec::new();
        for sec in &me.sections_hf {
            for (name, is_header) in [
                (&sec.header_default, true),
                (&sec.header_first, true),
                (&sec.footer_default, false),
                (&sec.footer_first, false),
            ] {
                if let Some(n) = name
                    && !part_roles.iter().any(|(p, _)| p == n)
                {
                    part_roles.push((n.clone(), is_header));
                }
            }
        }
        for (part_name, is_header) in part_roles {
            let Some(part) = parts.iter().find(|p| p.name == part_name) else { continue };
            // Parse the header/footer body into a child CollabDoc (its own loro tree), so it edits
            // through the same path as the body. `from_document_xml` keys on `w:p`, so a `<w:hdr>` /
            // `<w:ftr>` fragment imports its paragraphs (and tracked-change marks) just like the body.
            let child = Box::new(CollabDoc::from_document_xml(&part.data)?);

            // Pictures in the header/footer resolve through that part's own rels
            // (`word/_rels/<base>.xml.rels`).
            let base = part_name.strip_prefix("word/").unwrap_or(&part_name);
            let hf_rels = parts
                .iter()
                .find(|p| p.name == format!("word/_rels/{base}.rels"))
                .map(|p| model::resolve_rels(&p.data))
                .unwrap_or_default();

            // Resolve the child story's editable image media through its OWN rels (the body did the
            // same above via the document rels). `from_document_xml` stored each embed as `rid:{rId}`
            // but couldn't resolve it - the rels live in a sibling part - so without this the
            // header/footer editable `images` map points at a rel id, not a media part, and can't
            // render or export. Resolving it here makes header/footer pictures editable like the body.
            let mut child_imgs_changed = false;
            for (id, mut p) in model::read_images(&child.doc) {
                if let Some(rid) = p.media.strip_prefix("rid:")
                    && let Some(target) = hf_rels.get(rid)
                {
                    p.media = target
                        .strip_prefix('/')
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("word/{target}"));
                    model::write_image(&child.doc, id, &p)?;
                    child_imgs_changed = true;
                }
            }
            if child_imgs_changed {
                child.doc.commit();
            }

            let ctx = ImageContext::Hf { part: part_name.clone(), header: is_header };
            collect_images(&part.data, &hf_rels, |_| ctx.clone(), &mut me.images);
            // Anchored text boxes (the rotated margin stamp): render-only, from the raw part XML.
            collect_textboxes(&part.data, &ctx, &mut me.textboxes);

            // Freshly imported: not edited, so save leaves the original part bytes alone.
            me.hf_docs.insert(part_name, HfPartDoc { is_header, doc: child, dirty: false });
        }

        if let Some(n) = parts.iter().find(|p| p.name == "word/numbering.xml") {
            // The imported defs are the base population; runtime-synthesized defs (if this `.docx` was
            // itself exported from a CollabDoc carrying a NUM_SYNTH map) reconcile in lazily on read.
            *me.numbering.borrow_mut() = model::parse_numbering(&n.data);
        }

        // Comments: parse the bodies + thread state into the CRDT map (their anchored ranges were
        // already applied as marks in `from_document_xml` from document.xml's commentRange markers).
        if let Some(cx) = parts.iter().find(|p| p.name == "word/comments.xml") {
            let mut parsed = model::parse_comments(&cx.data);
            if let Some(ce) = parts.iter().find(|p| p.name == "word/commentsExtended.xml") {
                model::apply_comments_extended(&ce.data, &mut parsed);
            }
            for pc in &parsed {
                model::write_comment(&me.doc, &pc.comment)?;
            }
            me.doc.commit();
            me.reconfigure_comment_marks();
            // Read back through `comments()` rather than reusing `parsed`, so the snapshot is in the
            // same shape a save will compare against - anything the round trip through the CRDT
            // normalizes is normalized on both sides, and an untouched document compares equal.
            me.imported_comments = me.comments();
        }

        me.clear_undo(); // loading comments is part of the baseline, not an undoable edit

        // Retain the original parts so a save re-zips with everything else verbatim.
        me.source_parts = parts;
        Ok(me)
    }
}
