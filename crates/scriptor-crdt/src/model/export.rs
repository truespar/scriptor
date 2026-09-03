//! CRDT -> `document.xml`.
//! 
//! Serializes the model back to OOXML: paragraphs and runs with their properties,
//! tables from the grid, drawings, headers and footers, and the section properties
//! that close the body. Tracked changes, comments, fields, bookmarks and hyperlinks
//! are emitted as the range wrappers Word expects, computed up front as per-run open
//! and close tables so a run knows what to open before it writes itself.

use super::*;

// ── export (CRDT -> document.xml) ────────────────────────────────────────────

mod drawing;
mod headers;
mod paragraph;
mod spans;
mod tables;

pub use headers::*;
pub use tables::*;
pub(crate) use drawing::*;
pub(crate) use paragraph::*;
pub(crate) use spans::*;

// The namespace declarations carried on every rebuilt root (`<w:document>`, `<w:hdr>`, `<w:ftr>`).
// This must be the FULL set Word itself declares on `document.xml`: verbatim passthrough
// (`docs/passthrough.md`) re-emits captured spans - charts (`c:`), WordprocessingShapes (`wps:`/
// `wpg:`), drawing anchors (`wp:`/`a:`), extension attributes (`w14:`/`w15:`) - that relied on
// ancestor declarations in the source document. A prefix missing here makes the exported part
// namespace-non-well-formed, which Word reports as "unreadable content"; an extra declaration is
// harmless. `a`/`pic`/`c`/`dgm` are belt-and-braces for non-Word producers that declare them at
// the root instead of inline. `mc:Ignorable` mirrors Word's own, so consumers skip the extension
// namespaces they do not understand instead of rejecting them.
macro_rules! word_ns_attrs {
    () => {
        concat!(
            "xmlns:wpc=\"http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas\" ",
            "xmlns:cx=\"http://schemas.microsoft.com/office/drawing/2014/chartex\" ",
            "xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" ",
            "xmlns:o=\"urn:schemas-microsoft-com:office:office\" ",
            "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" ",
            "xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\" ",
            "xmlns:v=\"urn:schemas-microsoft-com:vml\" ",
            "xmlns:wp14=\"http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing\" ",
            "xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" ",
            "xmlns:w10=\"urn:schemas-microsoft-com:office:word\" ",
            "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" ",
            "xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\" ",
            "xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\" ",
            "xmlns:w16se=\"http://schemas.microsoft.com/office/word/2015/wordml/symex\" ",
            "xmlns:w16cid=\"http://schemas.microsoft.com/office/word/2016/wordml/cid\" ",
            "xmlns:w16=\"http://schemas.microsoft.com/office/word/2018/wordml\" ",
            "xmlns:w16cex=\"http://schemas.microsoft.com/office/word/2018/wordml/cex\" ",
            "xmlns:w16sdtdh=\"http://schemas.microsoft.com/office/word/2020/wordml/sdtdatahash\" ",
            "xmlns:wpg=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingGroup\" ",
            "xmlns:wpi=\"http://schemas.microsoft.com/office/word/2010/wordprocessingInk\" ",
            "xmlns:wne=\"http://schemas.microsoft.com/office/word/2006/wordml\" ",
            "xmlns:wps=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingShape\" ",
            "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" ",
            "xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" ",
            "xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" ",
            "xmlns:dgm=\"http://schemas.openxmlformats.org/drawingml/2006/diagram\" ",
            "mc:Ignorable=\"w14 w15 w16se w16cid w16 w16cex w16sdtdh wp14\""
        )
    };
}
pub(crate) const WORD_NS_ATTRS: &str = word_ns_attrs!();
const DOC_HEAD: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n",
    "<w:document ",
    word_ns_attrs!(),
    ">",
    "<w:body>"
);
const DOC_TAIL: &str = "</w:body></w:document>";

/// Serialize the block tree to a valid, Word-openable `word/document.xml`, ending with a section
/// properties block built from `page` + the header/footer references `hf`. `body` gives the document
/// order (top-level paragraphs interleaved with tables); when empty (no tables) the flat paragraph
/// list is emitted directly. Tables pull their cell paragraphs from the flat list in order, so cell
/// edits round-trip back into the right `<w:tc>`.
pub fn export_document_xml(
    doc: &LoroDoc,
    page: &PageGeometry,
    hf: &[HfRef],
    body: &[BodyItem],
    title_pg: bool,
) -> Result<String> {
    let paras = read_paragraphs(doc)?;
    let (copens, ccloses) = comment_spans(&paras);
    let (fopens, fcloses) = field_spans(&paras);
    let (bopens, bcloses) = bookmark_spans(&paras);
    let fields = read_fields(doc);
    let bookmarks = read_bookmarks(doc);
    let links = read_hyperlinks(doc);
    let images = read_images(doc);
    let raw = read_raw(doc);
    let ids = IdAlloc::new();
    let sp = ExportSpans {
        ids: &ids,
        copens: &copens,
        ccloses: &ccloses,
        fopens: &fopens,
        fcloses: &fcloses,
        bopens: &bopens,
        bcloses: &bcloses,
        fields: &fields,
        bookmarks: &bookmarks,
        links: &links,
        images: &images,
        raw: &raw,
    };
    let mut out = String::from(DOC_HEAD);
    if body.is_empty() {
        for (i, para) in paras.iter().enumerate() {
            out.push_str(&para_xml(para, i, &sp));
        }
    } else {
        let mut cursor = 0usize;
        for item in body {
            match item {
                BodyItem::Paragraph => {
                    if let Some(p) = paras.get(cursor) {
                        out.push_str(&para_xml(p, cursor, &sp));
                    }
                    cursor += 1;
                }
                BodyItem::Table(t) => out.push_str(&tbl_xml(t, &paras, &mut cursor, &sp)),
            }
        }
    }
    // The body-final section. For a MULTI-section document (one or more in-paragraph sectPrs), emit
    // the imported final sectPr verbatim so every section keeps its own header/footer refs + page
    // geometry - the old collapse merged all sections' hf refs into one synthesized sectPr,
    // overflowing the schema. A SINGLE-section document keeps synthesizing from `page`/`hf`, so the
    // header/footer-editing API (which updates `hf`) still round-trips - and a lone section never
    // overflows, so there is nothing to preserve verbatim there.
    let multi_section = paras.iter().any(|p| p.props.sect_pr.is_some());
    out.push_str(&final_sectpr(doc, page, hf, title_pg, multi_section));
    out.push_str(DOC_TAIL);
    Ok(out)
}

/// Serialize the document body by walking the loro block tree's body order ([`body_nodes`]) - the
/// tables-crdt main-path export (T2.7 step 1): top-level paragraphs interleaved with table NODES, each
/// table read from its hosted [`TableGrid`](crate::table_crdt::TableGrid) via [`export_table_grid`].
///
/// The body-final `<w:sectPr>`.
///
/// A MULTI-section document emits the imported final section verbatim: each section keeps its own
/// header/footer refs and page geometry, where the old collapse merged every section's refs into one
/// synthesized `sectPr` and overflowed the schema.
///
/// A SINGLE-section document *merges* the model's page geometry and header/footer refs into the
/// imported section rather than synthesizing a fresh one. Synthesizing kept the header/footer
/// editing API round-tripping but discarded every child the model does not represent - `w:cols`,
/// `w:type`, `w:pgNumType`, `w:docGrid` and the rest - on a save that changed nothing. Merging keeps
/// both properties.
///
/// With no imported section at all (a document created here) there is nothing to merge.
fn final_sectpr(
    doc: &LoroDoc,
    page: &PageGeometry,
    hf: &[HfRef],
    title_pg: bool,
    multi_section: bool,
) -> String {
    match read_final_sect(doc) {
        Some(sp) if multi_section => sp,
        Some(sp) => merge_sectpr_xml(&sp, page, hf, title_pg),
        None => sectpr_xml(page, hf, title_pg),
    }
}

/// The verbatim body-final `<w:sectPr>` stored document-level at import (see [`SECTPR`]), if any.
fn read_final_sect(doc: &LoroDoc) -> Option<String> {
    match doc.get_map(SECTPR).get("final") {
        Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

/// Associate each parsed section (`parse_section_props`, verbatim, in document order) with its
/// location: the body-final one document-level ([`SECTPR`] key `"final"`), the earlier ones on their
/// carrier paragraphs (those the importer flagged `section_end` / `continuous_break`). Called after
/// import so per-section page geometry / columns / header-footer refs round-trip. A count mismatch
/// (a rare `pPrChange`-nested sectPr the importer miscounted as a real break) falls back to storing
/// only the final - which alone fixes the header/footer-ref overflow that made the multi-section
/// corpus docs schema-invalid.
pub fn apply_section_props(doc: &LoroDoc, xml: &[u8]) -> Result<()> {
    let sects = parse_section_props(xml);
    let Some((final_sect, in_para)) = sects.split_last() else { return Ok(()) };
    doc.get_map(SECTPR).insert("final", final_sect.as_str())?;

    // The carrier paragraphs, in document order (top-level paragraphs flagged as section ends).
    let tree = doc.get_tree(BLOCKS);
    let carriers: Vec<TreeID> = body_nodes(doc)
        .into_iter()
        .filter_map(|n| match n {
            BodyNode::Paragraph(id) => Some(id),
            BodyNode::Table(_) => None,
        })
        .filter(|id| {
            tree.get_meta(*id)
                .ok()
                .map(|m| {
                    meta_bool(&m, "sectEnd").unwrap_or(false)
                        || meta_bool(&m, "contSect").unwrap_or(false)
                })
                .unwrap_or(false)
        })
        .collect();

    // Only attach per-paragraph on an exact match (one in-paragraph sectPr per carrier), so a parse
    // skew never mis-labels a paragraph; the final is stored regardless.
    if carriers.len() == in_para.len() {
        for (id, sp) in carriers.iter().zip(in_para) {
            tree.get_meta(*id)?.insert("sectPr", sp.as_str())?;
        }
    }
    Ok(())
}

/// This is the node-reading equivalent of [`export_document_xml`] (which walks the in-memory
/// `Vec<BodyItem>` + the flat-flow cell paragraphs): a `BodyNode::Paragraph` consumes the next entry of
/// the paragraph-only flat list ([`read_paragraphs`], which already excludes table nodes), so the
/// annotation spans index exactly as before; a `BodyNode::Table` reads the grid.
///
/// **Not yet wired into the live save path.** Flipping `to_document_xml` onto this also requires the
/// import to build table nodes and the editing / render layer (CollabDoc's table ops + the renderer's
/// `body()`) to read the containers instead of `Vec<BodyItem>` - they share that representation, so the
/// flip is a coordinated migration (tables-crdt T2.7 + T3 + T6), not a one-sided switch. This function
/// is the proven export half (byte-equal to the legacy walk; see the doc-level equivalence test).
pub fn export_document_xml_via_nodes(
    doc: &LoroDoc,
    page: &PageGeometry,
    hf: &[HfRef],
    title_pg: bool,
) -> Result<String> {
    let paras = read_paragraphs(doc)?;
    let (copens, ccloses) = comment_spans(&paras);
    let (fopens, fcloses) = field_spans(&paras);
    let (bopens, bcloses) = bookmark_spans(&paras);
    let fields = read_fields(doc);
    let bookmarks = read_bookmarks(doc);
    let links = read_hyperlinks(doc);
    let images = read_images(doc);
    let raw = read_raw(doc);
    let ids = IdAlloc::new();
    let sp = ExportSpans {
        ids: &ids,
        copens: &copens,
        ccloses: &ccloses,
        fopens: &fopens,
        fcloses: &fcloses,
        bopens: &bopens,
        bcloses: &bcloses,
        fields: &fields,
        bookmarks: &bookmarks,
        links: &links,
        images: &images,
        raw: &raw,
    };
    let mut out = String::from(DOC_HEAD);
    // Block-level `<w:sdt>` / `<w:customXml>` wrappers: their verbatim opening is re-emitted before the
    // first enclosed block and the matching close after the last, so a content control round-trips while
    // its inner blocks stay modeled + editable. Anchored via `wrapopen`/`wrapclose` id lists on the
    // block nodes' meta (outer-first opens, inner-first closes). See `docs/passthrough.md`.
    let wraps = read_block_wraps(doc);
    let tree = doc.get_tree(BLOCKS);
    let emit_opens = |out: &mut String, meta: &LoroMap| {
        for id in block_wrap_ids(meta, "wrapopen") {
            if let Some(pfx) = wraps.get(&id) {
                out.push_str(pfx);
            }
        }
    };
    let emit_closes = |out: &mut String, meta: &LoroMap| {
        for id in block_wrap_ids(meta, "wrapclose") {
            if let Some(pfx) = wraps.get(&id) {
                out.push_str(block_wrap_suffix(pfx));
            }
        }
    };
    // `paras` is the flat block sequence (top-level paragraphs + cell paragraphs, in document order).
    // A Paragraph node consumes the next flat entry; a Table node reads its grid (cell content) and
    // advances the flat cursor past that table's cell paragraphs, so the span index stays aligned.
    let mut fc = 0usize;
    for node in body_nodes(doc) {
        match node {
            BodyNode::Paragraph(id) => {
                let meta = tree.get_meta(id)?;
                if !wraps.is_empty() {
                    emit_opens(&mut out, &meta);
                }
                if let Some(p) = paras.get(fc) {
                    out.push_str(&para_xml(p, fc, &sp));
                }
                if !wraps.is_empty() {
                    emit_closes(&mut out, &meta);
                }
                fc += 1;
            }
            BodyNode::Table(id) => {
                let meta = tree.get_meta(id)?;
                if !wraps.is_empty() {
                    emit_opens(&mut out, &meta);
                }
                let grid = open_table_grid(doc, id)?;
                // Drive the table off the document-global spans, indexed from `fc` (this table's
                // first cell paragraph in the flat sequence), so a field / bookmark / hyperlink /
                // comment / picture anchored in a cell - or a range spanning the body-table boundary -
                // re-emits exactly once.
                out.push_str(&export_table_grid_anchored(&grid, &sp, fc)?);
                for r in grid.row_ids()? {
                    for c in grid.col_ids()? {
                        fc += grid.cell_block_count(&r, &c)?;
                    }
                }
                if !wraps.is_empty() {
                    emit_closes(&mut out, &meta);
                }
            }
        }
    }
    // The body-final section. For a MULTI-section document (one or more in-paragraph sectPrs), emit
    // the imported final sectPr verbatim so every section keeps its own header/footer refs + page
    // geometry - the old collapse merged all sections' hf refs into one synthesized sectPr,
    // overflowing the schema. A SINGLE-section document keeps synthesizing from `page`/`hf`, so the
    // header/footer-editing API (which updates `hf`) still round-trips - and a lone section never
    // overflows, so there is nothing to preserve verbatim there.
    let multi_section = paras.iter().any(|p| p.props.sect_pr.is_some());
    out.push_str(&final_sectpr(doc, page, hf, title_pg, multi_section));
    out.push_str(DOC_TAIL);
    Ok(out)
}



/// Serialize one paragraph (`<w:p>` + properties + runs), emitting comment range markers
/// (`commentRangeStart`/`End` + reference run) around the runs each comment anchors, per `opens` /
/// `closes` (aligned to `para.runs`), and move range markers (`moveFromRangeStart`/`End` /
/// `moveToRangeStart`/`End`, paired across source + destination by `w:name="mv{id}"`) around each
/// contiguous run of move-tracked text.
fn para_xml(para: &Paragraph, idx: usize, sp: &ExportSpans) -> String {
    let no: Vec<Vec<u64>> = Vec::new();
    let fno: Vec<Option<u64>> = Vec::new();
    let opens = sp.copens.get(idx).unwrap_or(&no);
    let closes = sp.ccloses.get(idx).unwrap_or(&no);
    let fopens = sp.fopens.get(idx).unwrap_or(&fno);
    let fcloses = sp.fcloses.get(idx).unwrap_or(&fno);
    let bopens = sp.bopens.get(idx).unwrap_or(&no);
    let bcloses = sp.bcloses.get(idx).unwrap_or(&no);

    let mut s = String::from("<w:p>");
    s.push_str(&ppr_xml(para));

    // Adjacent runs carrying the SAME tracked change merge under ONE wrapper element - a source
    // `<w:del>` wrapping N runs imports as N runs sharing a Track, and re-exporting each in its
    // own `<w:del>` duplicated the revision id N times (a document-wide uniqueness violation the
    // validator flags; Word's revision count inflates to match). Grouping applies to plain text
    // runs only: raw/image runs wrap themselves, and a PAGE/NUMPAGES placeholder run splits into
    // `w:fldSimple` segments that must wrap each fragment individually (a fldSimple can never sit
    // inside `w:ins`/`w:del`). A group also breaks where any range marker (comment / bookmark /
    // field) opens or closes, so the marker emission below stays outside the wrapper.
    let plain_run = |r: &Run| {
        r.raw.is_none()
            && r.image.is_none()
            && !r.text.contains(FIELD_PAGE)
            && !r.text.contains(FIELD_NUMPAGES)
    };
    let no_marks_between = |i: usize| -> bool {
        fcloses.get(i).is_none_or(|o| o.is_none())
            && bcloses.get(i).is_none_or(|c| c.is_empty())
            && closes.get(i).is_none_or(|c| c.is_empty())
            && fopens.get(i + 1).is_none_or(|o| o.is_none())
            && bopens.get(i + 1).is_none_or(|c| c.is_empty())
            && opens.get(i + 1).is_none_or(|c| c.is_empty())
    };
    let grouped_with = |a: usize, b: usize| -> bool {
        let (ra, rb) = (&para.runs[a], &para.runs[b]);
        ra.track.is_some()
            && ra.track == rb.track
            && ra.link == rb.link
            && plain_run(ra)
            && plain_run(rb)
            && no_marks_between(a)
    };

    // The id of the currently open synthesized move-range pair (start emitted, end pending).
    let mut open_move_mark: Option<u64> = None;
    for (ri, run) in para.runs.iter().enumerate() {
        // Open a field: emit begin + instrText + separate before its first result run - and BEFORE
        // any hyperlink opening at this run. Field markers must sit at paragraph-content level,
        // never inside `w:hyperlink`: a TOC's begin landing inside the first entry's hyperlink and
        // its end inside a later entry's is locally schema-valid (hyperlink content is EG_PContent)
        // but Word enforces field begin/end pairing semantics and refuses to OPEN the file - the
        // class only the word-verify gate catches. (The result runs themselves are normal runs
        // that already render; this just re-wraps them as a field.)
        //
        // A field whose result runs carry a revision is a field DELETED/INSERTED under Track
        // Changes (Word deletes fields whole): its markers re-emit inside a matching revision
        // wrapper, with the instruction as `w:delInstrText` for a deletion - emitting them
        // untracked split the surrounding revision into same-id fragments (tdf70234) and lost
        // the deleted-ness of the field itself.
        if let Some(Some(id)) = fopens.get(ri) {
            let instr = sp.fields.get(id).map(String::as_str).unwrap_or("");
            let markers = |instr_el: &str| {
                format!(
                    "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
<w:r><{instr_el} xml:space=\"preserve\">{}</{instr_el}></w:r>\
<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>",
                    xml_escape(instr)
                )
            };
            match para.runs.get(ri).and_then(|r| r.track.as_ref()) {
                Some(t) => {
                    let instr_el = if t.kind.is_del_text() { "w:delInstrText" } else { "w:instrText" };
                    s.push_str(&format!(
                        "<{tag} w:id=\"{id}\" w:author=\"{author}\"{date}>{inner}</{tag}>",
                        tag = t.kind.wrapper(),
                        id = sp.ids.wrapper(t.id),
                        author = xml_escape(&t.author),
                        date = date_attr(&t.date),
                        inner = markers(instr_el),
                    ));
                }
                None => s.push_str(&markers("w:instrText")),
            }
        }
        // Open a hyperlink wrapping this + following contiguous same-link runs (a `w:hyperlink` is a
        // within-paragraph element, so the range is per-paragraph). Internal -> `w:anchor`, external
        // -> a deterministic `r:id` (`rIdLnk{id}`) whose rel `to_docx_bytes` injects.
        if let Some(link) = run.link {
            let prev_same = ri > 0 && para.runs[ri - 1].link == Some(link);
            if !prev_same {
                let attr = match sp.links.get(&link) {
                    Some(t) if t.starts_with('#') => format!(" w:anchor=\"{}\"", xml_escape(&t[1..])),
                    Some(_) => format!(" r:id=\"rIdLnk{link}\""),
                    None => String::new(),
                };
                s.push_str(&format!("<w:hyperlink{attr} w:history=\"1\">"));
            }
        }
        if let Some(ids) = opens.get(ri) {
            for id in ids {
                s.push_str(&format!("<w:commentRangeStart w:id=\"{id}\"/>"));
            }
        }
        // Collapsed bookmarks anchored at this run: start and end adjacent, ahead of the run, exactly
        // as they arrived. A cross-reference target is normally written this way.
        for id in &run.point_bookmarks {
            let name = sp.bookmarks.get(id).map(String::as_str).unwrap_or("");
            s.push_str(&format!(
                "<w:bookmarkStart w:id=\"{id}\" w:name=\"{}\"/><w:bookmarkEnd w:id=\"{id}\"/>",
                xml_escape(name)
            ));
        }
        // Open each bookmark starting before this run (its name comes from the bookmarks map).
        // Multiple bookmarks can open at the same run (a stack of TOC bookmarks on a heading).
        if let Some(ids) = bopens.get(ri) {
            for id in ids {
                let name = sp.bookmarks.get(id).map(String::as_str).unwrap_or("");
                s.push_str(&format!("<w:bookmarkStart w:id=\"{id}\" w:name=\"{}\"/>", xml_escape(name)));
            }
        }
        // Open a move range when this run begins a contiguous move region (start of the paragraph, or
        // the previous run isn't the same move half). The marker PAIR gets its own synthesized id -
        // reusing the move wrapper's revision id put three elements (rangeStart, wrapper, rangeEnd)
        // on one id, a uniqueness violation. Pairing across the from/to halves stays on `w:name`.
        if let Some(t) = run_move(run) {
            let prev_same = ri > 0
                && run_move(&para.runs[ri - 1]).is_some_and(|p| p.id == t.id && p.kind == t.kind);
            if !prev_same {
                let el = if t.kind == TrackKind::MoveFrom { "moveFromRangeStart" } else { "moveToRangeStart" };
                let mid = sp.ids.fresh();
                open_move_mark = Some(mid);
                s.push_str(&format!(
                    "<w:{el} w:id=\"{mid}\" w:name=\"mv{id}\" w:author=\"{a}\"{d}/>",
                    id = t.id,
                    a = xml_escape(&t.author),
                    d = date_attr(&t.date),
                ));
            }
        }
        // A passthrough run re-emits its captured `<w:r>...</w:r>` XML verbatim (an unmodeled embedded
        // object) instead of its placeholder text - byte-identical for an untouched object.
        if let Some(raw_id) = run.raw {
            if let Some(xml) = sp.raw.get(&raw_id) {
                // The captured span is a whole `<w:r>...</w:r>`; re-wrap it in its `<w:ins>`/`<w:del>`
                // revision when tracked, so a tracked embedded object round-trips its redline (mirrors
                // the image branch below).
                s.push_str(&match &run.track {
                    Some(t) => format!(
                        "<{tag} w:id=\"{id}\" w:author=\"{author}\"{date}>{xml}</{tag}>",
                        tag = t.kind.wrapper(),
                        id = sp.ids.wrapper(t.id),
                        author = xml_escape(&t.author),
                        date = date_attr(&t.date),
                    ),
                    None => xml.clone(),
                });
            }
        } else if let Some(img) = run.image {
            if let Some(p) = sp.images.get(&img) {
                let drawing = drawing_xml(img, p);
                s.push_str(&match &run.track {
                    Some(t) => format!(
                        "<{tag} w:id=\"{id}\" w:author=\"{author}\"{date}>{drawing}</{tag}>",
                        tag = t.kind.wrapper(),
                        id = sp.ids.wrapper(t.id),
                        author = xml_escape(&t.author),
                        date = date_attr(&t.date),
                    ),
                    None => drawing,
                });
            }
        } else if let Some(t) = run.track.as_ref().filter(|_| plain_run(run)) {
            // Tracked plain run: wrap in its revision element only at group boundaries (see the
            // grouping note above), so adjacent same-revision runs share ONE wrapper.
            if !(ri > 0 && grouped_with(ri - 1, ri)) {
                s.push_str(&format!(
                    "<{tag} w:id=\"{id}\" w:author=\"{author}\"{date}>",
                    tag = t.kind.wrapper(),
                    id = sp.ids.wrapper(t.id),
                    author = xml_escape(&t.author),
                    date = date_attr(&t.date),
                ));
            }
            s.push_str(&run_xml_untracked(run));
            if !(ri + 1 < para.runs.len() && grouped_with(ri, ri + 1)) {
                s.push_str(&format!("</{}>", t.kind.wrapper()));
            }
        } else {
            s.push_str(&run_xml(run, sp.ids));
        }
        // Close the move range when the next run isn't the same move half, with the id the
        // matching rangeStart above was given.
        if let Some(t) = run_move(run) {
            let next_same =
                para.runs.get(ri + 1).and_then(run_move).is_some_and(|n| n.id == t.id && n.kind == t.kind);
            if !next_same {
                let el = if t.kind == TrackKind::MoveFrom { "moveFromRangeEnd" } else { "moveToRangeEnd" };
                let mid = open_move_mark.take().unwrap_or(t.id);
                s.push_str(&format!("<w:{el} w:id=\"{mid}\"/>"));
            }
        }
        if let Some(ids) = bcloses.get(ri) {
            for id in ids {
                s.push_str(&format!("<w:bookmarkEnd w:id=\"{id}\"/>"));
            }
        }
        // Collapsed bookmarks that sat past the paragraph's last codepoint: emitted after the run
        // they anchored to, since there was nothing left for them to sit before.
        for id in &run.end_point_bookmarks {
            let name = sp.bookmarks.get(id).map(String::as_str).unwrap_or("");
            s.push_str(&format!(
                "<w:bookmarkStart w:id=\"{id}\" w:name=\"{}\"/><w:bookmarkEnd w:id=\"{id}\"/>",
                xml_escape(name)
            ));
        }
        if let Some(ids) = closes.get(ri) {
            for id in ids {
                s.push_str(&format!(
                    "<w:commentRangeEnd w:id=\"{id}\"/><w:r><w:commentReference w:id=\"{id}\"/></w:r>"
                ));
            }
        }
        // Close the hyperlink when the next run isn't the same link.
        if let Some(link) = run.link {
            let next_same = para.runs.get(ri + 1).is_some_and(|n| n.link == Some(link));
            if !next_same {
                s.push_str("</w:hyperlink>");
            }
        }
        // Close the field after its last result run (end fldChar) - AFTER the hyperlink close,
        // mirroring the field open above: the end marker must not sit inside a hyperlink. A
        // tracked field's end marker rides inside a matching revision wrapper (see the open).
        if let Some(Some(_id)) = fcloses.get(ri) {
            match para.runs.get(ri).and_then(|r| r.track.as_ref()) {
                Some(t) => s.push_str(&format!(
                    "<{tag} w:id=\"{id}\" w:author=\"{author}\"{date}><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></{tag}>",
                    tag = t.kind.wrapper(),
                    id = sp.ids.wrapper(t.id),
                    author = xml_escape(&t.author),
                    date = date_attr(&t.date),
                )),
                None => s.push_str("<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"),
            }
        }
    }
    // A manual page break is round-tripped as a trailing break run (we model it as "break after this
    // paragraph", so it lands at the paragraph end - the common authoring shape). A section-carrier
    // paragraph also has `page_break_after` set (the import derives it from the next section's type),
    // but its break is created by the `<w:sectPr>` now emitted verbatim in its pPr - so suppress the
    // synthetic run there, or the section boundary would carry a spurious extra page break.
    if para.props.page_break_after && para.props.sect_pr.is_none() {
        s.push_str("<w:r><w:br w:type=\"page\"/></w:r>");
    }
    // A manual column break round-trips as a trailing column-break run (kept distinct from a page
    // break so a multi-column document re-imports correctly).
    if para.props.column_break_after {
        s.push_str("<w:r><w:br w:type=\"column\"/></w:r>");
    }
    s.push_str("</w:p>");
    s
}




