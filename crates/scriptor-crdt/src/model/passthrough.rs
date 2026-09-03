//! Verbatim capture of content the model does not understand.
//! 
//! Runs holding OLE objects, charts, SmartArt, shapes or content controls are
//! captured as raw XML spans and re-emitted byte-for-byte on export, so a round-trip
//! never silently drops them. See `docs/passthrough.md`.

use super::*;

/// One captured verbatim-passthrough item: the raw `<w:r>...</w:r>` bytes and the 0-based body
/// paragraph index it belongs to (aligning with [`parse_images`] / `block_seq` order). `track` carries
/// an enclosing `<w:ins>`/`<w:del>` revision (mirrors [`DrawImage::track`]) so a tracked embedded object
/// keeps its redline and resolves through accept/reject like any other tracked run.
pub struct RawItem {
    pub xml: String,
    pub para_index: usize,
    pub track: Option<Track>,
    /// Codepoints of modeled text preceding this run inside its paragraph.
    ///
    /// The placeholder is inserted here rather than appended, so a captured run stays where it was.
    /// Appending moved an object that sat between two text runs to the end of its paragraph and
    /// merged the text across the gap it left - `BEFORE | object | AFTER` came back as
    /// `BEFOREAFTER | object`.
    pub text_offset: usize,
}

/// Run children the model reproduces on export. A run built only from these needs no capture.
///
/// `w:rPr` is the marker for run properties; its subtree is skipped rather than walked, since
/// formatting is modeled wholesale and walking it would flag every `<w:b/>` as unmodeled and capture
/// the entire document. `w:lastRenderedPageBreak` is a layout cache Word rewrites on open.
///
/// `w:br` is deliberately absent: only `w:type="page"` and `w:type="column"` are modeled onto the
/// paragraph, so a plain `<w:br/>` line break is judged by attribute in [`classify_run_child`].
const MODELED_RUN_CHILDREN: &[&[u8]] = &[
    b"w:rPr",
    b"w:t",
    b"w:delText",
    b"w:instrText",
    b"w:delInstrText",
    b"w:tab",
    b"w:fldChar",
    b"w:commentReference",
    b"w:drawing",
    b"w:object",
    b"w:pict",
    b"w:control",
    b"mc:AlternateContent",
    b"w:lastRenderedPageBreak",
];

/// Classify one child of an open `<w:r>`: does the model reproduce it?
///
/// `rpr_depth` tracks nesting inside `<w:rPr>`, whose subtree is formatting rather than content.
fn classify_run_child(
    name: &[u8],
    e: &quick_xml::events::BytesStart<'_>,
    rpr_depth: &mut usize,
    has_unmodeled: &mut bool,
) {
    if name == b"w:rPr" {
        *rpr_depth += 1;
        return;
    }
    if *rpr_depth > 0 {
        return;
    }
    if name == b"w:br" {
        let kind = e
            .attributes()
            .flatten()
            .find(|a| a.key.as_ref() == b"w:type")
            .map(|a| String::from_utf8_lossy(&a.value).into_owned());
        if !matches!(kind.as_deref(), Some("page") | Some("column")) {
            *has_unmodeled = true;
        }
        return;
    }
    if !MODELED_RUN_CHILDREN.contains(&name) {
        *has_unmodeled = true;
    }
}

/// Scan a WordprocessingML body for **unmodeled run content** the model would otherwise drop, and
/// capture each *enclosing run* (`<w:r>...</w:r>`) verbatim as a raw byte span so it round-trips
/// byte-for-byte. Three families:
///
/// - **Embedded objects** - `<w:object>` (OLE) / `<w:control>` (ActiveX): always captured.
/// - **Non-picture drawings** - `<w:drawing>` / `<w:pict>` / `<mc:AlternateContent>` that yield *no*
///   modeled picture (charts, SmartArt, WordprocessingShapes, VML lines/callouts, text boxes without a
///   `v:imagedata`). Captured **only when [`parse_images`], run on the run slice, finds no picture** -
///   otherwise a real picture would double-emit (once via the modeled image path, once here). Using
///   `parse_images` itself as the oracle guarantees the two passes can never disagree.
/// - **Anything else the model does not reproduce, in a run carrying no text** - a footnote or
///   endnote reference, `w:sym`, a plain `<w:br/>` line break, `w:ruby`, `w:ptab`, a separator. These
///   were neither modeled nor captured: the run imported as empty and exported as nothing, so a
///   footnote survived in `footnotes.xml` while the reference pointing at it disappeared.
///
/// The no-text condition is what keeps the third family safe rather than a blanket inversion.
/// Capturing a run makes it opaque - selectable and deletable, never inline-editable - which is the
/// right trade for a run whose whole content is an unmodeled element and the wrong one for ordinary
/// prose that merely carries an unmodeled child alongside its text. A text-bearing run stays modeled
/// and still loses that child; closing *that* gap needs the model to represent the element.
///
/// `<w:txbxContent>` skipping + `para_index` advance mirror [`parse_images`], so a captured item anchors
/// to the same paragraph the picture path would. See `docs/passthrough.md`.
pub fn parse_passthrough(xml: &[u8]) -> Vec<RawItem> {
    use quick_xml::events::Event;
    let xml = strip_bom(xml);
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut para_index = 0usize;
    let mut txbx_depth = 0usize;
    // Depth of open `<w:tbl>`s. A run inside a NESTED table (depth >= 2) must not be captured here:
    // the whole nested table is already kept verbatim on its enclosing cell, so capturing its runs
    // as well emitted them twice - duplicate `v:shape` ids, which the validator rejects.
    let mut tbl_depth = 0usize;
    let mut run_start: Option<usize> = None; // byte offset of the open `<w:r>`
    // Whether the open run holds an OLE/ActiveX object (always captured) or a drawing-family element
    // (captured only if it yields no modeled picture - resolved at `</w:r>` via the `parse_images` oracle).
    let mut run_has_object = false;
    let mut run_has_drawing = false;
    // Whether the open run carries a child the model does not reproduce, and how deep we are inside
    // its <w:rPr> (formatting, not content - walking it would flag every <w:b/> and capture the
    // whole document).
    let mut run_has_unmodeled = false;
    let mut rpr_depth = 0usize;
    // The tracked-change wrapper (`<w:ins>`/`<w:del>`) currently open around runs, inherited by a
    // captured object so a tracked embedded object keeps its redline (mirrors `parse_images`).
    let mut pending_track: Option<Track> = None;
    // Codepoints of modeled text seen so far in the open paragraph, and the count at the moment the
    // open run started - the position a captured run's placeholder belongs at. Counts exactly what
    // the importer puts in the text container: `w:t` / `w:delText` character data, and one codepoint
    // per `w:tab`. `w:instrText` is deliberately absent - a field's instruction is modeled
    // separately and contributes no run text.
    let mut para_text_len = 0usize;
    let mut run_text_start = 0usize;
    let mut in_text_el = false;
    loop {
        let before = reader.buffer_position() as usize;
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                b"w:txbxContent" => txbx_depth += 1,
                b"w:tbl" => tbl_depth += 1,
                b"w:ins" if txbx_depth == 0 => pending_track = revision_track(&e, TrackKind::Ins),
                b"w:del" if txbx_depth == 0 => pending_track = revision_track(&e, TrackKind::Del),
                b"w:r" if txbx_depth == 0 => {
                    run_start = Some(before);
                    run_has_object = false;
                    run_has_drawing = false;
                    run_has_unmodeled = false;
                    rpr_depth = 0;
                    run_text_start = para_text_len;
                }
                b"w:t" | b"w:delText" if txbx_depth == 0 => in_text_el = true,
                b"w:object" | b"w:control" if txbx_depth == 0 && run_start.is_some() => {
                    run_has_object = true;
                }
                b"w:drawing" | b"w:pict" | b"mc:AlternateContent"
                    if txbx_depth == 0 && run_start.is_some() =>
                {
                    run_has_drawing = true;
                }
                name if txbx_depth == 0 && run_start.is_some() => {
                    classify_run_child(name, &e, &mut rpr_depth, &mut run_has_unmodeled);
                }
                _ => {}
            },
            Event::Empty(e) if txbx_depth == 0 && run_start.is_some() => {
                match e.name().as_ref() {
                    b"w:object" | b"w:control" => run_has_object = true,
                    b"w:drawing" | b"w:pict" | b"mc:AlternateContent" => run_has_drawing = true,
                    // A tab is modeled, and counts as a codepoint of run text.
                    b"w:tab" => para_text_len += 1,
                    name => {
                        let mut depth = rpr_depth;
                        classify_run_child(name, &e, &mut depth, &mut run_has_unmodeled);
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:txbxContent" => txbx_depth = txbx_depth.saturating_sub(1),
                b"w:tbl" => tbl_depth = tbl_depth.saturating_sub(1),
                b"w:ins" | b"w:del" if txbx_depth == 0 => pending_track = None,
                b"w:t" | b"w:delText" if txbx_depth == 0 => in_text_el = false,
                // Leaving the run properties: everything after this is content again.
                b"w:rPr" if txbx_depth == 0 && run_start.is_some() => {
                    rpr_depth = rpr_depth.saturating_sub(1);
                }
                b"w:r" if txbx_depth == 0 => {
                    if let Some(start) = run_start {
                        let end = reader.buffer_position() as usize;
                        // Objects are always verbatim; a drawing-family run only when it produces no
                        // modeled picture (else the picture double-emits). `parse_images` on the run
                        // slice is the oracle - raw-name matching means it needs no ancestor namespaces.
                        //
                        // A picture found INSIDE a `<w:txbxContent>` does not count. It belongs to the
                        // text box, not to the body flow, so it must not make the box's run look like
                        // an ordinary picture run: that declined the capture, and the modeled image
                        // path then emitted the picture alone at body level, hoisting it out of the
                        // box and dropping every word in it.
                        // ... and a run whose content the model does not reproduce at all, provided
                        // it carries no text to make opaque. `run_text_start == para_text_len` is
                        // exactly that test: the paragraph's text length did not move while this run
                        // was open, so the run contributed none.
                        let capture = tbl_depth < 2 && (run_has_object
                            || (run_has_drawing
                                && !parse_images(&xml[start..end])
                                    .iter()
                                    .any(|d| !d.in_textbox))
                            || (run_has_unmodeled
                                && !run_has_drawing
                                && run_text_start == para_text_len));
                        if capture
                            && let Ok(s) = std::str::from_utf8(&xml[start..end])
                        {
                            out.push(RawItem {
                                xml: s.to_string(),
                                para_index,
                                track: pending_track.clone(),
                                text_offset: run_text_start,
                            });
                        }
                    }
                    run_start = None;
                    run_has_object = false;
                    run_has_drawing = false;
                    run_has_unmodeled = false;
                    rpr_depth = 0;
                }
                b"w:p" if txbx_depth == 0 => {
                    para_index += 1;
                    para_text_len = 0;
                    run_text_start = 0;
                }
                _ => {}
            },
            Event::Text(t) if in_text_el && txbx_depth == 0 => {
                if let Ok(s) = t.decode() {
                    para_text_len += s.chars().count();
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// One captured **block-level wrapper** - a `<w:sdt>` content control or `<w:customXml>` element that
/// wraps a run of body blocks. The wrapper's opening (`prefix`: `<w:sdt><w:sdtPr>…</w:sdtPr><w:sdtContent>`
/// or `<w:customXml …><w:customXmlPr>…</w:customXmlPr>`) is captured verbatim; its inner blocks stay
/// modeled + editable. `start_block`/`end_block` are the 0-based [`body_nodes`] indices of the first and
/// last inner block. `id` is the capture order (outer wrappers get lower ids), used to order nested
/// open/close correctly on export. The closing tag is fixed (`</w:sdtContent></w:sdt>` / `</w:customXml>`),
/// derived from `prefix` at export. See `docs/passthrough.md`.
pub struct BlockWrap {
    pub id: u64,
    pub prefix: String,
    pub start_block: usize,
    pub end_block: usize,
}

/// Scan a WordprocessingML body for **block-level `<w:sdt>` / `<w:customXml>` wrappers** (content
/// controls / custom-XML data bindings that sit *between* paragraphs) and capture each wrapper's opening
/// verbatim plus the [`body_nodes`] index range of the blocks it encloses. The inner blocks are left to
/// the normal (editable) import; export re-wraps them (see [`export_document_xml_via_nodes`]), so the
/// control round-trips **without** freezing its content. Run-level and cell/row-level `<w:sdt>` are
/// ignored (guarded by `!in_para` + `table_depth == 0`) - only body-block wrappers are modeled.
///
/// Nesting is handled by a stack: a wrapper's `prefix` is captured when its first child element starts
/// (so an outer wrapper's prefix stops before an inner `<w:sdt>`), and ids ascend with nesting depth.
pub fn parse_block_wraps(xml: &[u8]) -> Vec<BlockWrap> {
    use quick_xml::events::Event;
    let xml = strip_bom(xml);
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out: Vec<BlockWrap> = Vec::new();

    // A wrapper whose open tag was seen but whose enclosed-block range / prefix are still being built.
    struct Pending {
        id: u64,
        start_byte: usize,
        prefix: Option<String>,
        start_block: Option<usize>,
        end_block: Option<usize>,
    }
    let mut stack: Vec<Pending> = Vec::new();
    let mut next_id = 0u64;
    let mut blk = 0usize; // index the next-closed top-level block will get (= body_nodes position)
    let mut table_depth = 0usize;
    let mut txbx_depth = 0usize;
    let mut in_para = false;

    loop {
        let before = reader.buffer_position() as usize;
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        // A wrapper is only "block level" between paragraphs, outside any table or text box.
        let body_level = table_depth == 0 && txbx_depth == 0 && !in_para;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                let name = e.name();
                let n = name.as_ref();
                // The start of any block-content child (`w:p` / `w:tbl` / a nested wrapper) closes the
                // enclosing wrapper's prefix: everything up to this point is the verbatim opening.
                // Range markers close it too: a `bookmarkStart` sitting between the wrapper's opening
                // and its first paragraph (Word writes them with `displacedByCustomXml`) is ALSO
                // modeled by the importer - captured into the prefix it would re-emit TWICE, a
                // duplicate-id violation (tdf154478). The modeled marker re-emits at its anchor.
                if body_level
                    && matches!(
                        n,
                        b"w:p" | b"w:tbl" | b"w:sdt" | b"w:customXml"
                            | b"w:bookmarkStart" | b"w:bookmarkEnd"
                            | b"w:commentRangeStart" | b"w:commentRangeEnd"
                            | b"w:moveFromRangeStart" | b"w:moveFromRangeEnd"
                            | b"w:moveToRangeStart" | b"w:moveToRangeEnd"
                    )
                    && let Some(top) = stack.last_mut()
                    && top.prefix.is_none()
                    && let Ok(s) = std::str::from_utf8(&xml[top.start_byte..before])
                {
                    top.prefix = Some(s.to_string());
                }
                match n {
                    b"w:sdt" | b"w:customXml" if body_level => {
                        stack.push(Pending {
                            id: next_id,
                            start_byte: before,
                            prefix: None,
                            start_block: None,
                            end_block: None,
                        });
                        next_id += 1;
                    }
                    b"w:p" if table_depth == 0 && txbx_depth == 0 => in_para = true,
                    b"w:tbl" => table_depth += 1,
                    b"w:txbxContent" => txbx_depth += 1,
                    _ => {}
                }
            }
            // Range markers are usually self-closing - same prefix-close rule as the Start arm
            // (see the duplicate-emission note there).
            Event::Empty(e)
                if body_level
                    && matches!(
                        e.name().as_ref(),
                        b"w:bookmarkStart" | b"w:bookmarkEnd"
                            | b"w:commentRangeStart" | b"w:commentRangeEnd"
                            | b"w:moveFromRangeStart" | b"w:moveFromRangeEnd"
                            | b"w:moveToRangeStart" | b"w:moveToRangeEnd"
                    ) =>
            {
                if let Some(top) = stack.last_mut()
                    && top.prefix.is_none()
                    && let Ok(s) = std::str::from_utf8(&xml[top.start_byte..before])
                {
                    top.prefix = Some(s.to_string());
                }
            }
            Event::End(e) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:txbxContent" => txbx_depth = txbx_depth.saturating_sub(1),
                    b"w:p" if table_depth == 0 && txbx_depth == 0 => {
                        in_para = false;
                        for w in stack.iter_mut() {
                            w.start_block.get_or_insert(blk);
                            w.end_block = Some(blk);
                        }
                        blk += 1;
                    }
                    b"w:tbl" => {
                        table_depth = table_depth.saturating_sub(1);
                        if table_depth == 0 && txbx_depth == 0 {
                            for w in stack.iter_mut() {
                                w.start_block.get_or_insert(blk);
                                w.end_block = Some(blk);
                            }
                            blk += 1;
                        }
                    }
                    b"w:sdt" | b"w:customXml" if table_depth == 0 && txbx_depth == 0 && !in_para => {
                        if let Some(w) = stack.pop()
                            && let (Some(prefix), Some(sb), Some(eb)) = (w.prefix, w.start_block, w.end_block)
                        {
                            out.push(BlockWrap { id: w.id, prefix, start_block: sb, end_block: eb });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}
