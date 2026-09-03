//! Serialization back to OOXML, and byte-stable round-trips.

use super::*;

/// A document with a styled paragraph, a bold run, a tracked insertion and a tracked deletion
/// survives import -> export -> re-import unchanged at the model level.
#[test]
fn import_export_roundtrip_preserves_model() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t xml:space="preserve">Title</w:t></w:r></w:p>
<w:p>
  <w:r><w:t xml:space="preserve">Plain and </w:t></w:r>
  <w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">bold</w:t></w:r>
  <w:r><w:t xml:space="preserve"> and </w:t></w:r>
  <w:ins w:id="7" w:author="Agent" w:date="2026-06-17T00:00:00Z"><w:r><w:t xml:space="preserve">inserted</w:t></w:r></w:ins>
  <w:del w:id="8" w:author="Reviewer" w:date="2026-06-17T01:00:00Z"><w:r><w:delText xml:space="preserve">removed</w:delText></w:r></w:del>
</w:p>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let p = doc.paragraphs()?;
    assert_eq!(p.len(), 2, "expected two paragraphs");

    assert_eq!(p[0].style.as_deref(), Some("Heading1"));
    assert_eq!(p[0].runs, vec![plain("Title")]);

    let runs = &p[1].runs;
    assert_eq!(runs[0], plain("Plain and "));
    assert_eq!(
        runs[1],
        Run {
            text: "bold".into(),
            bold: true,
            italic: false,
            underline: false,
            strike: false,
            size: None,
            color: None,
            font: None,
            highlight: None,
            vert_align: None,
            lang: None,
            char_style: None,
            shading: None,
            track: None,
            fmt_change: None,
            comments: Vec::new(),
            field: None,
            bookmarks: Vec::new(),
            point_bookmarks: Vec::new(),
            end_point_bookmarks: Vec::new(),
            link: None,
            image: None,
            raw: None,
        }
    );
    assert_eq!(runs[2], plain(" and "));
    assert_eq!(
        runs[3].track,
        Some(Track {
            kind: TrackKind::Ins,
            author: "Agent".into(),
            date: "2026-06-17T00:00:00Z".into(),
            id: 7,
        })
    );
    assert_eq!(runs[3].text, "inserted");
    assert_eq!(
        runs[4].track,
        Some(Track {
            kind: TrackKind::Del,
            author: "Reviewer".into(),
            date: "2026-06-17T01:00:00Z".into(),
            id: 8,
        })
    );
    assert_eq!(runs[4].text, "removed");

    // Export and re-import: the model must be identical (semantic round-trip).
    let xml2 = doc.to_document_xml()?;
    let doc2 = CollabDoc::from_document_xml(xml2.as_bytes())?;
    assert_eq!(doc.paragraphs()?, doc2.paragraphs()?, "round-trip changed the model");
    Ok(())
}

/// Highlight + superscript/subscript apply, query back, survive the OOXML round-trip (`w:highlight`
/// / `w:vertAlign`), and Clear Formatting strips all inline run formatting.
#[test]
fn highlight_vertalign_and_clear_round_trip() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("E=mc2 highlighted")], None)?; // 17 chars
    doc.apply_run_format(0, 4..5, &RunFormat::vert_align("superscript"), "sup")?;
    doc.apply_run_format(0, 6..17, &RunFormat::highlight("yellow"), "hl")?;
    assert_eq!(doc.selection_format(0, 4, 5)?.vert_align.as_deref(), Some("superscript"));
    assert_eq!(doc.selection_format(0, 6, 17)?.highlight.as_deref(), Some("yellow"));

    // Round-trip through OOXML.
    let xml = doc.to_document_xml()?;
    assert!(xml.contains("<w:vertAlign w:val=\"superscript\"/>"), "vertAlign emitted: {xml}");
    assert!(xml.contains("<w:highlight w:val=\"yellow\"/>"), "highlight emitted");
    let doc2 = CollabDoc::from_document_xml(xml.as_bytes())?;
    let runs = &doc2.paragraphs()?[0].runs;
    assert!(runs.iter().any(|r| r.vert_align.as_deref() == Some("superscript")), "vertAlign lost");
    assert!(runs.iter().any(|r| r.highlight.as_deref() == Some("yellow")), "highlight lost");

    // Clear Formatting strips both over the range.
    doc2.clear_run_format(0, 0, 17, "clear")?;
    assert_eq!(doc2.selection_format(0, 4, 5)?.vert_align, None, "vertAlign cleared");
    assert_eq!(doc2.selection_format(0, 6, 17)?.highlight, None, "highlight cleared");
    Ok(())
}

/// A run's proofing language (`w:lang`) is imported and re-emitted on export, so it isn't dropped
/// when `document.xml` is rewritten (Word tags most runs with a language).
#[test]
fn run_lang_round_trips() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:rPr><w:lang w:val="sv-SE"/></w:rPr><w:t>Hej</w:t></w:r></w:p></w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    assert_eq!(doc.paragraphs()?[0].runs[0].lang.as_deref(), Some("sv-SE"), "lang imported");
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:lang w:val=\"sv-SE\"/>"), "lang re-emitted: {out}");
    Ok(())
}

/// Paragraph formatting (alignment / line spacing / indents) applies, queries back, and survives
/// an OOXML round-trip.
#[test]
fn paragraph_format_command_and_roundtrip() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Centered, spaced, indented.")], None)?;
    doc.apply_paragraph_format(
        0,
        &ParaProps {
            align: Some(Align::Center),
            line_spacing: Some(360), // 1.5
            indent_left: Some(720),  // 0.5"
            ..Default::default()
        },
        "para",
    )?;

    let got = doc.paragraph_format(0)?;
    assert_eq!(got.align, Some(Align::Center));
    assert_eq!(got.line_spacing, Some(360));
    assert_eq!(got.indent_left, Some(720));

    let xml = doc.to_document_xml()?;
    let doc2 = CollabDoc::from_document_xml(xml.as_bytes())?;
    let p = doc2.paragraph_format(0)?;
    assert_eq!(p.align, Some(Align::Center), "alignment lost on round-trip");
    assert_eq!(p.line_spacing, Some(360), "line spacing lost on round-trip");
    assert_eq!(p.indent_left, Some(720), "indent lost on round-trip");
    Ok(())
}

/// A from-scratch document saves to `.docx` bytes and reopens with its content intact (exercises
/// the minimal-package scaffold + the in-memory zip writer).
#[test]
fn save_new_doc_roundtrips_via_bytes() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Saved and reopened.")], None)?;
    let bytes = doc.to_docx_bytes()?;
    let reopened = CollabDoc::from_docx_bytes(&bytes)?;
    let p = reopened.paragraphs()?;
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].runs.iter().map(|r| r.text.as_str()).collect::<String>(), "Saved and reopened.");
    Ok(())
}

/// An encrypted / password-protected .docx is an OLE compound-file, not a zip - report it clearly
/// instead of failing with a confusing "not a zip" error (and, on the native CLI, panicking while
/// building the JsError).
#[test]
fn encrypted_docx_reports_a_clear_error() {
    let cfb = [0xD0u8, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0, 0, 0];
    let err = match CollabDoc::from_docx_bytes(&cfb) {
        Ok(_) => panic!("expected an error for an encrypted file"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("encrypted"), "got: {err}");
}

/// An unmodeled embedded OLE object (`<w:object>`) survives a `document.xml` round-trip verbatim:
/// its enclosing `<w:r>` is captured as a `raw~{id}` passthrough placeholder on import and
/// re-emitted byte-for-byte on export, with the placeholder char never surfacing as `<w:t>` text.
/// See `docs/passthrough.md`.
#[test]
fn embedded_object_round_trips_as_verbatim_passthrough() -> Result<()> {
    // A dedicated run holding an Excel OLE object: a VML preview shape + the `<o:OLEObject>`, both
    // referencing package parts by `r:id`. Nothing here is modeled, so v1 preserves the whole run.
    let object_run = "<w:r><w:object w:dxaOrig=\"1440\" w:dyaOrig=\"1440\">\
<v:shape id=\"_x0000_i1025\" type=\"#_x0000_t75\" style=\"width:72pt;height:72pt\" o:ole=\"\">\
<v:imagedata r:id=\"rId5\" o:title=\"\"/></v:shape>\
<o:OLEObject Type=\"Embed\" ProgID=\"Excel.Sheet.12\" ShapeID=\"_x0000_i1025\" \
DrawAspect=\"Content\" ObjectID=\"_1699\" r:id=\"rId6\"/></w:object></w:r>";
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:o=\"urn:schemas-microsoft-com:office:office\" \
xmlns:v=\"urn:schemas-microsoft-com:vml\"><w:body>\
<w:p><w:r><w:t>Before</w:t></w:r></w:p>\
<w:p>{object_run}</w:p>\
<w:p><w:r><w:t>After</w:t></w:r></w:p>\
</w:body></w:document>"
    );
    let doc = CollabDoc::from_document_xml(xml.as_bytes())?;

    // Export re-emits the object run byte-for-byte, and the placeholder codepoint is not text.
    let out = doc.to_document_xml()?;
    assert!(out.contains(object_run), "object run not re-emitted verbatim:\n{out}");
    assert!(!out.contains(&crate::model::IMAGE_PLACEHOLDER.to_string()), "placeholder char leaked as text");

    // Re-import: the middle paragraph's run carries the passthrough id (and no surrounding neighbour
    // ate the mark), and the object still round-trips a second time.
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    let paras = doc2.paragraphs()?;
    assert!(paras[1].runs.iter().any(|r| r.raw.is_some()), "passthrough run lost on re-import");
    assert!(doc2.to_document_xml()?.contains(object_run), "object dropped on second round-trip");
    Ok(())
}

/// Nested block wrappers (an outer `<w:sdt>` around an inner `<w:sdt>` around a paragraph) re-emit in
/// the correct order (outer opens first, inner closes first), and a leading paragraph inserted before
/// them does not shift the wrapping (anchored to the block nodes, not to indices). See `docs/passthrough.md`.
#[test]
fn nested_block_wrappers_round_trip_and_survive_edits() -> Result<()> {
    let inner_prefix = "<w:sdt><w:sdtPr><w:tag w:val=\"inner\"/></w:sdtPr><w:sdtContent>";
    let outer_prefix = "<w:sdt><w:sdtPr><w:tag w:val=\"outer\"/></w:sdtPr><w:sdtContent>";
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>\
<w:p><w:r><w:t>Top</w:t></w:r></w:p>{outer_prefix}{inner_prefix}\
<w:p><w:r><w:t>Core</w:t></w:r></w:p></w:sdtContent></w:sdt></w:sdtContent></w:sdt></w:body></w:document>"
    );
    // The opens nest outer-then-inner, right before Core; the closes nest inner-then-outer after it.
    let nested_open = format!("{outer_prefix}{inner_prefix}<w:p>");
    let nested_close = "</w:p></w:sdtContent></w:sdt></w:sdtContent></w:sdt>";
    let doc = CollabDoc::from_document_xml(xml.as_bytes())?;
    let out0 = doc.to_document_xml()?;
    assert!(out0.contains(&nested_open), "opens nest outer-then-inner:\n{out0}");
    assert!(out0.contains(nested_close), "closes nest inner-then-outer:\n{out0}");

    // Insert a new first paragraph: the wrappers must still enclose "Core", not the new paragraph.
    doc.split_paragraph(0, 0, "split")?; // Top=0 -> ""(0) + "Top"(1); Core is now block 2
    doc.insert_text(0, 0, "New", "type")?;
    let out = doc.to_document_xml()?;
    assert!(out.contains(&nested_open) && out.contains(nested_close), "wrappers stay anchored to Core:\n{out}");
    // "New" is a leading paragraph OUTSIDE the wrappers (it appears before the nested opening).
    let new_pos = out.find("New").expect("New present");
    assert!(new_pos < out.find(&nested_open).unwrap(), "the new leading paragraph is outside the wrappers");
    Ok(())
}

/// Every prefix the export emits must be DECLARED in the exported part. A captured chart
/// (`c:`) or WordprocessingShape (`wps:`) passthrough span relied on declarations that lived
/// on the SOURCE document's root; `DOC_HEAD` used to declare only w/r/o/v/w10/m/mc, so this
/// exact document exported as namespace-non-well-formed XML - "unreadable content" in Word.
#[test]
fn export_declares_every_emitted_namespace_prefix() -> Result<()> {
    let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><w:body>
<w:p><w:r><w:drawing><wp:inline><wp:extent cx="5000" cy="5000"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId5"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:p><w:r><w:drawing><wp:anchor><wp:extent cx="9000" cy="4000"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>inside</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml.as_bytes())?;
    let out = doc.to_document_xml()?;
    assert!(out.contains("<c:chart") && out.contains("<wps:wsp>"), "spans re-emitted: {out}");
    assert_ns_well_formed(&out);
    Ok(())
}

/// An inline picture imports into an editable image run + placement (size + crop), exports back to a
/// `<w:drawing>`, and re-imports stably - the editable-image round-trip (images-editing P1b).
#[test]
fn reattaching_source_parts_restores_passthrough_on_export() -> Result<()> {
    let sample = include_bytes!("../../tests/fixtures/sample.docx");

    // Edit, snapshot, then reconstruct via merge: the loro snapshot carries
    // the content + the edit but NOT source_parts.
    let doc = CollabDoc::from_docx_bytes(sample)?;
    doc.insert_text(0, 0, "XMARK", "test")?;
    let snap = doc.snapshot()?;
    let mut reopened = CollabDoc::new();
    reopened.merge(&snap)?;
    assert!(reopened.source_parts.is_empty(), "a loro-reconstructed doc has no source parts");

    // Reattach the origin's parts plus a synthetic passthrough part; both the
    // edit (modeled, via document.xml) and the passthrough survive export.
    let mut parts = scriptor_ooxml::read_parts_bytes(sample)?;
    parts.push(scriptor_ooxml::Part {
        name: "docProps/custom.xml".to_string(),
        data: b"<passthrough-marker/>".to_vec(),
    });
    reopened.set_source_parts(parts);

    let out = scriptor_ooxml::read_parts_bytes(&reopened.to_docx_bytes()?)?;
    let custom = out
        .iter()
        .find(|p| p.name == "docProps/custom.xml")
        .expect("the reattached passthrough part survives export");
    assert_eq!(custom.data, b"<passthrough-marker/>", "passthrough bytes are verbatim");

    let doc_xml = out.iter().find(|p| p.name == "word/document.xml").expect("document.xml");
    assert!(
        String::from_utf8_lossy(&doc_xml.data).contains("XMARK"),
        "the modeled edit is in the re-rendered document.xml"
    );
    Ok(())
}

/// A header nobody edited is written back byte-for-byte, table and all.
///
/// Save re-renders a header from its child story, and that story is a flat paragraph list: a
/// `<w:tbl>` comes back as loose `<w:p>`s, losing rows, cells and borders. It used to do that on
/// every save, so merely opening a document with a letterhead table in the header and pressing
/// Ctrl+S destroyed it. Untouched parts are now passed through instead.
#[test]
fn an_unedited_header_is_not_re_rendered() -> Result<()> {
    let rich = include_bytes!("../../tests/fixtures/rich.docx");
    let part = |bytes: &[u8], name: &str| -> Result<Vec<u8>> {
        Ok(scriptor_ooxml::read_parts_bytes(bytes)?
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
            .data)
    };

    let before = part(rich, "word/header1.xml")?;
    assert!(
        String::from_utf8_lossy(&before).contains("<w:tbl>"),
        "fixture precondition: the header must contain a table for this test to mean anything"
    );

    // Open and save, changing nothing.
    let doc = CollabDoc::from_docx_bytes(rich)?;
    let after = part(&doc.to_docx_bytes()?, "word/header1.xml")?;

    // Structure first, so a regression reports "the table is gone" rather than dumping two
    // multi-kilobyte byte vectors at whoever broke it.
    let tags = |b: &[u8], tag: &str| String::from_utf8_lossy(b).matches(tag).count();
    assert_eq!(tags(&after, "<w:tbl>"), tags(&before, "<w:tbl>"), "the header table was lost");
    assert_eq!(tags(&after, "<w:tc>"), tags(&before, "<w:tc>"), "the header table's cells were lost");

    assert_eq!(before, after, "an unedited header must be passed through verbatim");
    Ok(())
}

/// A table nested inside a table cell survives a save, text and structure both.
///
/// A cell owns a contiguous slice of the flat paragraph list, which cannot express a table, so the
/// importer skipped a nested table's paragraphs silently - "not modeled" written as a `_ => {}` arm.
/// Every word inside one was lost, and for several corpus documents that was the whole document:
/// `fdo80097.docx` came back with all 1,105 of its characters gone. 22 documents in total.
///
/// Preserving comes before modelling: the nested `<w:tbl>` is captured verbatim and re-emitted where
/// it sat. It is opaque - not editable, not laid out - exactly like an OLE object.
#[test]
fn a_nested_table_survives_a_save() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
<w:tr><w:tc>
  <w:p><w:r><w:t>OUTER BEFORE</w:t></w:r></w:p>
  <w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
    <w:tr><w:tc><w:p><w:r><w:t>INNER TEXT</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
  <w:p><w:r><w:t>OUTER AFTER</w:t></w:r></w:p>
</w:tc></w:tr></w:tbl>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    assert!(out.contains("INNER TEXT"), "the nested table's text was dropped");
    assert!(out.contains("OUTER BEFORE") && out.contains("OUTER AFTER"), "the cell's own text");
    // Two tables out, as in: the outer one and the nested one.
    assert_eq!(out.matches("<w:tbl>").count(), 2, "the nested table must be emitted exactly once");
    // And it must sit between the cell's paragraphs, not be appended after them.
    let at = |n: &str| out.find(n).unwrap_or_else(|| panic!("{n} missing"));
    assert!(at("OUTER BEFORE") < at("INNER TEXT"), "nested table moved before the text above it");
    assert!(at("INNER TEXT") < at("OUTER AFTER"), "nested table moved past the text below it");
    Ok(())
}

/// A text box keeps its text when it also contains a picture.
///
/// `parse_images` collects pictures found inside a `w:txbxContent` - it must, so the renderer paints
/// them - which made the box's own run look like an ordinary picture run to the capture oracle. The
/// run was declined for verbatim capture and the modeled image path emitted the picture alone at
/// body level, hoisting it out of the box and dropping every word in it. `fdo76591.docx` lost all
/// 666 of its characters that way; 11 corpus documents in total.
#[test]
fn a_text_box_holding_a_picture_keeps_its_text() -> Result<()> {
    let xml = br##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><w:body>
<w:p><w:r><w:pict><v:shape id="s1" type="#_x0000_t202" style="width:100pt;height:100pt"><v:textbox><w:txbxContent>
  <w:p><w:r><w:t>BOXED WORDS</w:t></w:r></w:p>
  <w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><a:blip r:embed="rId9"/></wp:inline></w:drawing></w:r></w:p>
</w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>
</w:body></w:document>"##;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    assert!(out.contains("BOXED WORDS"), "the text box's words were dropped");
    assert!(out.contains("w:txbxContent"), "the text box itself was dropped");
    // The picture must not also be hoisted out as a standalone body picture: it belongs to the box,
    // and the box is re-emitted verbatim, so emitting it again would duplicate it.
    assert_eq!(out.matches("rId9").count(), 1, "the picture must appear exactly once");
    Ok(())
}

/// A shape's picture *fill* is not a picture, so a text box that has one keeps its text.
///
/// `<a:blip>` appears both in `<pic:blipFill>` (a real picture) and in `<a:blipFill>` under a
/// shape's `<wps:spPr>`, where it is the shape's background. Treating the second as a picture had
/// the same effect as the text-box case above: the run was not captured and the box was dropped.
#[test]
fn a_shape_picture_fill_is_not_a_body_picture() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><w:body>
<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><a:graphic><a:graphicData><wps:wsp><wps:spPr><a:blipFill><a:blip r:embed="rIdFill"/></a:blipFill></wps:spPr><wps:txbx><w:txbxContent>
  <w:p><w:r><w:t>FILLED BOX</w:t></w:r></w:p>
</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    assert!(out.contains("FILLED BOX"), "the text box's words were dropped");
    assert!(out.contains("a:blipFill"), "the shape's fill was dropped");
    Ok(())
}

/// A run whose entire content is something the model does not represent is carried through verbatim.
///
/// `parse_passthrough` used to capture a run only when a whitelist fired - `w:object`, `w:control`,
/// `w:drawing`, `w:pict`, `mc:AlternateContent`. Everything else was neither modeled nor captured, so
/// a footnote reference, a `w:sym` or a plain `<w:br/>` line break imported as an empty run and
/// exported as nothing: a footnote survived in `footnotes.xml` while the reference pointing at it
/// disappeared, leaving it orphaned and invisible in Word.
///
/// This could not ship until placeholders were positional - see
/// `a_captured_object_keeps_its_position_in_the_paragraph`. Capturing a mid-paragraph run while
/// placeholders appended at the paragraph end reordered content and broke comment anchoring.
#[test]
fn a_run_of_purely_unmodeled_content_is_captured() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Cited</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="2"/></w:r></w:p>
<w:p><w:r><w:t>arrow </w:t></w:r><w:r><w:rPr><w:sz w:val="18"/></w:rPr><w:sym w:font="Wingdings" w:char="F0E0"/></w:r><w:r><w:t> tail</w:t></w:r></w:p>
<w:p><w:r><w:t>before</w:t></w:r><w:r><w:br/></w:r><w:r><w:t>after</w:t></w:r></w:p>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    for (needle, what) in [
        ("w:footnoteReference", "the footnote reference"),
        ("w:id=\"2\"", "the footnote's id"),
        ("w:sym", "the symbol run"),
        ("F0E0", "the symbol's codepoint"),
        ("<w:br/>", "the plain line break"),
    ] {
        assert!(out.contains(needle), "saving dropped {what}");
    }
    // Ordinary prose stays modeled - capturing a text run would make it opaque - and the captured
    // runs stay between the text they sat between rather than moving to the paragraph end.
    let at = |n: &str| out.find(n).unwrap_or_else(|| panic!("{n} missing"));
    assert!(at("arrow") < at("w:sym"), "the symbol must stay after the text before it");
    assert!(at("w:sym") < at("tail"), "the symbol must stay before the text after it");
    assert!(at("before") < at("<w:br/>") && at("<w:br/>") < at("after"), "the break must stay put");
    Ok(())
}

/// A captured embedded object stays where it was in its paragraph.
///
/// Its placeholder used to be appended at the paragraph's end rather than inserted at the run's
/// position, so an object sitting between two text runs was *moved* and the text either side merged
/// across the gap it left: `BEFORE | object | AFTER` came back as `BEFOREAFTER | object`. Silent
/// reordering, and worse than a visible loss because nothing looks missing.
///
/// It went unnoticed because the capture whitelist only fires on OLE objects, charts, shapes and
/// content controls, which nearly always occupy a paragraph of their own - there, "end of paragraph"
/// and "where the run was" are the same place.
#[test]
fn a_captured_object_keeps_its_position_in_the_paragraph() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>BEFORE</w:t></w:r><w:r><w:object><w:OLEObject ProgID="Test"/></w:object></w:r><w:r><w:t>AFTER</w:t></w:r></w:p>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    let before = out.find("BEFORE").expect("the leading text");
    let object = out.find("OLEObject").expect("the captured object");
    let after = out.find("AFTER").expect("the trailing text");
    assert!(before < object, "the object must not move ahead of the text before it");
    assert!(object < after, "the object must not be moved past the text after it");
    assert!(
        !out.contains("BEFOREAFTER"),
        "the text either side of the object must not merge across it"
    );
    Ok(())
}

/// Two captured objects in one paragraph both land in the right place.
///
/// Each insertion shifts the offsets after it, so the import offsets every placeholder by the count
/// already placed in that paragraph. Getting that wrong puts the second object one codepoint early.
#[test]
fn two_captured_objects_in_one_paragraph_keep_their_order() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>AAA</w:t></w:r><w:r><w:object><w:OLEObject ProgID="First"/></w:object></w:r><w:r><w:t>BBB</w:t></w:r><w:r><w:object><w:OLEObject ProgID="Second"/></w:object></w:r><w:r><w:t>CCC</w:t></w:r></w:p>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    let at = |n: &str| out.find(n).unwrap_or_else(|| panic!("{n} missing"));
    assert!(at("AAA") < at("First"), "AAA before the first object");
    assert!(at("First") < at("BBB"), "first object before BBB");
    assert!(at("BBB") < at("Second"), "BBB before the second object");
    assert!(at("Second") < at("CCC"), "second object before CCC");
    Ok(())
}

/// A collapsed bookmark - `<w:bookmarkStart/><w:bookmarkEnd/>` with nothing between - survives a save.
///
/// This is the normal shape for a cross-reference target: Word writes `_Ref…` as a bare insertion
/// point, not a range. A range mark cannot hold a zero-width span, so `mark_bookmark_range` skipped
/// it and the bookmark simply vanished, silently breaking every cross-reference pointing at it. 79
/// bookmarks across 47 corpus documents were going that way.
///
/// Covers both anchors: one collapsed bookmark before a run, and one past the paragraph's last
/// codepoint (nothing modeled follows it, so it anchors to the final codepoint and emits after).
#[test]
fn collapsed_bookmarks_survive_a_save() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:bookmarkStart w:id="1" w:name="_Ref357264347"/><w:bookmarkEnd w:id="1"/><w:r><w:t>Target</w:t></w:r><w:bookmarkStart w:id="2" w:name="TrailingPoint"/><w:bookmarkEnd w:id="2"/></w:p>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    for name in ["_Ref357264347", "TrailingPoint"] {
        assert!(out.contains(name), "the collapsed bookmark {name} was dropped");
    }
    // Each must still be a collapsed PAIR, not widened into a range around the run.
    for id in [1, 2] {
        let pair = format!("<w:bookmarkEnd w:id=\"{id}\"/>");
        let start = out.find(&format!("w:id=\"{id}\" w:name=")).expect("start");
        let end = out.find(&pair).expect("end");
        let between = &out[start..end];
        assert!(
            !between.contains("<w:t>"),
            "bookmark {id} was widened to span text instead of staying collapsed"
        );
    }
    Ok(())
}

/// A bookmark that wraps text still round-trips as a range, not as two collapsed points.
#[test]
fn a_ranged_bookmark_still_spans_its_text() -> Result<()> {
    let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:bookmarkStart w:id="3" w:name="Wrapped"/><w:r><w:t>inside</w:t></w:r><w:bookmarkEnd w:id="3"/></w:p>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    let start = out.find("w:name=\"Wrapped\"").expect("start");
    let end = out.find("<w:bookmarkEnd w:id=\"3\"/>").expect("end");
    assert!(start < end, "the range must open before it closes");
    assert!(
        out[start..end].contains("inside"),
        "the bookmark must still span its text"
    );
    Ok(())
}

/// A single-section document keeps the section properties the model does not represent.
///
/// The body-final `<w:sectPr>` used to be synthesized from scratch for a single-section document -
/// header/footer refs, `pgSz`, `pgMar`, `titlePg` and nothing else. Everything the original carried
/// was discarded on a save that changed nothing: `w:cols` (576 corpus documents, 18 of them a real
/// multi-column layout), `w:type` (whether the section starts a new page), `w:pgNumType`,
/// `w:docGrid`, `w:bidi`. It now merges into the imported section instead.
#[test]
fn a_single_section_keeps_unmodeled_section_properties() -> Result<()> {
    let document = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="even" r:id="rId9"/><w:type w:val="continuous"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1417" w:right="1417" w:bottom="1417" w:left="1417" w:header="708" w:footer="708" w:gutter="0"/><w:pgNumType w:start="7"/><w:cols w:num="2" w:space="708"/><w:bidi/><w:docGrid w:linePitch="360"/></w:sectPr>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(document)?;
    let out = doc.to_document_xml()?;

    for (needle, what) in [
        ("w:num=\"2\"", "the two-column layout"),
        ("w:cols", "the columns element"),
        ("w:val=\"continuous\"", "the section break type"),
        ("w:pgNumType", "the page numbering restart"),
        ("w:docGrid", "the document grid"),
        ("w:bidi", "the right-to-left flag"),
        ("w:type=\"even\"", "the even-page header reference"),
    ] {
        assert!(out.contains(needle), "saving dropped {what}");
    }
    // Still schema-ordered: pgSz before cols, cols before docGrid.
    let at = |n: &str| out.find(n).unwrap_or_else(|| panic!("{n} missing"));
    assert!(at("<w:pgSz") < at("<w:cols"), "pgSz must precede cols");
    assert!(at("<w:cols") < at("<w:docGrid"), "cols must precede docGrid");
    Ok(())
}

/// An attribute value we consider malformed is still reproduced exactly.
///
/// `ST_SignedTwipsMeasure` is an Int32, but writers emit a negative char space as its two's
/// complement u32 - `4294961151` for `-6145`. Word reinterprets it; the Open XML SDK validator
/// rejects the file, and 13 corpus documents arrive that way.
///
/// It is tempting to re-spell the value on the way out, since the meaning is identical and the
/// output would then validate. That is still rewriting OOXML nobody asked us to touch, and the
/// invalidity belongs to the input, not to us: a document that arrives invalid must leave the same
/// way. The alternative - what the code used to do - was deleting `w:docGrid` outright, which
/// bought a passing validator with silent data loss.
#[test]
fn a_malformed_attribute_value_is_reproduced_not_repaired() -> Result<()> {
    let document = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Body</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:docGrid w:type="lines" w:linePitch="312" w:charSpace="4294961151"/></w:sectPr>
</w:body></w:document>"#;

    let doc = CollabDoc::from_document_xml(document)?;
    let out = doc.to_document_xml()?;

    assert!(out.contains("w:docGrid"), "the grid must survive");
    assert!(out.contains("w:charSpace=\"4294961151\""), "the value must be reproduced verbatim");
    assert!(out.contains("w:linePitch=\"312\""), "the other grid attributes must be untouched");
    Ok(())
}

/// A comment nobody edited is written back byte-for-byte, formatting and all.
///
/// A comment body is modeled as plain text, so re-emitting `comments.xml` from the model discards
/// run formatting, paragraph properties and any table inside the comment. That is an accepted limit
/// for a comment somebody edited. It was not acceptable for a document that was merely opened and
/// saved, which is what used to happen: `write_comment_parts` regenerated the part whenever the
/// document had any comments at all.
#[test]
fn an_unedited_comment_part_is_not_re_emitted() -> Result<()> {
    let (pkg, original) = commented_package()?;

    let doc = CollabDoc::from_docx_bytes(&pkg)?;
    let after = comments_part(&doc.to_docx_bytes()?)?;

    // Structure first, so a regression names what was lost instead of dumping the part.
    let s = String::from_utf8_lossy(&after);
    for (needle, what) in [
        ("<w:b/>", "bold run formatting"),
        ("FF0000", "run colour"),
        ("w:jc", "paragraph centring"),
        ("<w:tbl>", "a table inside the comment"),
    ] {
        assert!(s.contains(needle), "re-emitting the comment lost {what}");
    }
    assert_eq!(after, original, "an unedited comments part must be passed through verbatim");
    Ok(())
}

/// The other half: a real comment edit still reaches the saved part.
#[test]
fn an_edited_comment_part_is_re_emitted() -> Result<()> {
    let (pkg, original) = commented_package()?;

    let doc = CollabDoc::from_docx_bytes(&pkg)?;
    doc.add_comment_body("A brand new remark", "Bob", "2026-02-02T00:00:00Z", "test")?;
    let after = comments_part(&doc.to_docx_bytes()?)?;

    assert_ne!(after, original, "adding a comment must rewrite the part");
    assert!(
        String::from_utf8_lossy(&after).contains("A brand new remark"),
        "the new comment must reach the saved part"
    );
    Ok(())
}

/// A one-comment package whose comment carries formatting the model does not represent.
fn commented_package() -> Result<(Vec<u8>, Vec<u8>)> {
    let comments = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:comment w:id="1" w:author="Alice" w:initials="A" w:date="2026-01-01T00:00:00Z">
<w:p><w:r><w:t xml:space="preserve">plain and </w:t></w:r><w:r><w:rPr><w:b/><w:color w:val="FF0000"/></w:rPr><w:t>bold red</w:t></w:r></w:p>
<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>second paragraph centred</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>COMMENTTABLE</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:comment>
</w:comments>"#;
    let document = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:commentRangeStart w:id="1"/><w:r><w:t>Commented text</w:t></w:r><w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r></w:p>
</w:body></w:document>"#;
    let ct = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let drels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;

    let mk = |n: &str, d: &[u8]| scriptor_ooxml::Part { name: n.into(), data: d.to_vec() };
    let pkg = scriptor_ooxml::write_parts_bytes(&[
        mk("[Content_Types].xml", ct),
        mk("_rels/.rels", rels),
        mk("word/_rels/document.xml.rels", drels),
        mk("word/document.xml", document),
        mk("word/comments.xml", comments),
    ])?;
    Ok((pkg, comments.to_vec()))
}

fn comments_part(bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(scriptor_ooxml::read_parts_bytes(bytes)?
        .into_iter()
        .find(|p| p.name == "word/comments.xml")
        .expect("comments.xml")
        .data)
}

/// Saving does not rewrite what it does not understand in an imported `styles.xml`.
///
/// Unlike a header, `write_styles_parts` merges rather than regenerates: modeled props are patched
/// in place and canonical quick styles are appended, so the part grows but nothing in it is
/// rewritten. This pins that, because "styles.xml is not byte-identical after a save" reads like
/// the header bug and is not - the difference is merge versus re-render, and it is worth being able
/// to tell them apart without re-deriving it. Style-level `w:pBdr` in particular survives here; the
/// README's note about it applies to the from-scratch path, which runs only when the source package
/// has no `styles.xml` at all.
#[test]
fn an_imported_styles_part_keeps_what_the_model_does_not_represent() -> Result<()> {
    let styles = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="8" w:space="1" w:color="FF0000"/></w:pBdr></w:pPr><w:rPr><w:color w:val="2F5496"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="ZZCustom"><w:name w:val="ZZ Custom"/><w:pPr><w:widowControl w:val="0"/></w:pPr><w:unknownChild w:val="keepme"/></w:style>
</w:styles>"#;
    let document = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>
</w:body></w:document>"#;
    let ct = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let drels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

    let mk = |n: &str, d: &[u8]| scriptor_ooxml::Part { name: n.into(), data: d.to_vec() };
    let pkg = scriptor_ooxml::write_parts_bytes(&[
        mk("[Content_Types].xml", ct),
        mk("_rels/.rels", rels),
        mk("word/_rels/document.xml.rels", drels),
        mk("word/document.xml", document),
        mk("word/styles.xml", styles),
    ])?;

    let doc = CollabDoc::from_docx_bytes(&pkg)?;
    let out = scriptor_ooxml::read_parts_bytes(&doc.to_docx_bytes()?)?
        .into_iter()
        .find(|p| p.name == "word/styles.xml")
        .expect("styles.xml")
        .data;
    let s = String::from_utf8_lossy(&out);

    for needle in ["w:pBdr", "FF0000", "2F5496", "ZZCustom", "widowControl", "unknownChild"] {
        assert!(s.contains(needle), "the styles merge dropped {needle}");
    }
    Ok(())
}

/// The other half: editing a header still re-renders it, so passthrough cannot swallow a real edit.
#[test]
fn an_edited_header_is_re_rendered() -> Result<()> {
    let rich = include_bytes!("../../tests/fixtures/rich.docx");
    let header = |bytes: &[u8]| -> Result<String> {
        Ok(String::from_utf8_lossy(
            &scriptor_ooxml::read_parts_bytes(bytes)?
                .into_iter()
                .find(|p| p.name == "word/header1.xml")
                .expect("header1.xml")
                .data,
        )
        .into_owned())
    };

    let mut doc = CollabDoc::from_docx_bytes(rich)?;
    doc.set_header_text("Replaced header");
    let out = header(&doc.to_docx_bytes()?)?;

    assert!(out.contains("Replaced header"), "the edit must reach the saved part");
    assert!(
        !out.contains("Right cell"),
        "a replaced header must not keep the old content"
    );
    Ok(())
}

/// A tracked deletion spanning THREE paragraphs (a whole middle paragraph included) lands under one
/// id and, on accept, removes every spanned slice and cascades the ¶-merges so the three collapse
/// into one paragraph (start head + end tail). Rejecting restores all three.
#[test]
fn multi_paragraph_deletion_spans_three_paragraphs() -> Result<()> {
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };
    let build = || -> Result<CollabDoc> {
        let d = CollabDoc::new();
        d.append_paragraph(&[plain("Alpha beta")], None)?; // 0
        d.append_paragraph(&[plain("whole middle")], None)?; // 1 (deleted whole)
        d.append_paragraph(&[plain("gamma delta")], None)?; // 2
        Ok(d)
    };

    // Delete from offset 6 of para 0 ("Alpha |beta") through offset 6 of para 2 ("gamma |delta").
    let doc = build()?;
    let id = doc.suggest_deletion_multi(0, 6, 2, 6, "AI Agent", "2026-06-24T00:00:00Z", "trim")?;
    assert_eq!(texts(&doc), ["Alpha beta", "whole middle", "gamma delta"], "text retained");
    assert_eq!(doc.list_changes()?.len(), 1, "one revision id for the whole span");

    doc.accept_revision(id, "accept")?;
    assert_eq!(texts(&doc), ["Alpha delta"], "head + tail merged into one paragraph");

    // Reject path: all three paragraphs restored intact.
    let doc = build()?;
    let id = doc.suggest_deletion_multi(0, 6, 2, 6, "AI Agent", "2026-06-24T00:00:00Z", "trim")?;
    doc.reject_revision(id, "reject")?;
    assert_eq!(texts(&doc), ["Alpha beta", "whole middle", "gamma delta"], "reject restores");
    Ok(())
}

/// Run-level shading (`w:rPr/w:shd w:fill`) is captured on the run and round-trips - it was
/// previously dropped on import. (Rendered as a fill behind the glyphs in the wasm layer.)
#[test]
fn run_shading_is_captured_and_round_trips() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:rPr><w:shd w:val="clear" w:color="auto" w:fill="CCFF99"/></w:rPr><w:t>shaded</w:t></w:r>
     <w:r><w:t>plain</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let runs = doc.paragraphs()?[0].runs.clone();
    let by = |t: &str| runs.iter().find(|r| r.text == t).unwrap().shading.clone();
    assert_eq!(by("shaded").as_deref(), Some("CCFF99"), "run shd fill captured");
    assert_eq!(by("plain"), None, "a run with no shd stays None");
    assert!(doc.to_document_xml()?.contains(r#"w:fill="CCFF99""#), "run shd survives export");
    Ok(())
}

/// A paragraph's text frame (`w:pPr/w:framePr`) is captured (raw attrs) and round-trips; a normal
/// paragraph has none. (Layout positions the frame; this just covers the model + round-trip.)
#[test]
fn frame_pr_is_captured_and_round_trips() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:framePr w:w="2880" w:hAnchor="margin" w:wrap="around" w:xAlign="right" w:y="720"/></w:pPr><w:r><w:t>framed</w:t></w:r></w:p>
<w:p><w:r><w:t>normal</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let ps = doc.paragraphs()?;
    let f = ps[0].props.frame.as_deref().expect("frame captured");
    assert!(f.contains(r#"w:xAlign="right""#) && f.contains(r#"w:w="2880""#), "frame attrs kept: {f}");
    assert_eq!(ps[1].props.frame, None, "a normal paragraph has no frame");
    assert!(doc.to_document_xml()?.contains("<w:framePr "), "framePr survives export");
    Ok(())
}

/// A paragraph's border box (`w:pPr/w:pBdr`) is captured per edge (weight / spacing / colour),
/// a `w:val="none"` edge is dropped, and the box round-trips back to a `<w:pBdr>` on export.
#[test]
fn pbdr_is_captured_and_round_trips() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pBdr>
<w:top w:val="single" w:sz="6" w:space="1" w:color="auto"/>
<w:left w:val="single" w:sz="12" w:space="4" w:color="FF0000"/>
<w:bottom w:val="none" w:sz="0" w:space="0" w:color="auto"/>
<w:right w:val="single" w:sz="6" w:space="1" w:color="auto"/>
</w:pBdr></w:pPr><w:r><w:t>boxed</w:t></w:r></w:p>
<w:p><w:r><w:t>plain</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let ps = doc.paragraphs()?;
    let b = ps[0].props.border.as_deref().expect("border captured");
    assert!(b.contains("t=single,6,1,auto"), "top edge kept: {b}");
    assert!(b.contains("l=single,12,4,FF0000"), "left edge weight/space/colour kept: {b}");
    assert!(b.contains("r=single,6,1,auto"), "right edge kept: {b}");
    assert!(!b.contains("b="), "a w:val=none edge is dropped: {b}");
    assert_eq!(ps[1].props.border, None, "a plain paragraph has no border");
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:pBdr>"), "pBdr survives export");
    assert!(out.contains(r#"<w:left w:val="single" w:sz="12" w:space="4" w:color="FF0000"/>"#), "left edge re-emitted: {out}");
    Ok(())
}
