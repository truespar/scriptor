//! Direct text and formatting edits.

use super::*;

/// `parse_textboxes` finds an anchored rotated text box (the legal margin stamp): text from the
/// cached field result only (no `w:instrText` field code), the run's font/size/colour, the
/// `vert270` flow, and the anchor geometry - while the `mc:Fallback` VML copy of the SAME box is
/// skipped (reading both would emit it twice).
#[test]
fn parse_textboxes_reads_rotated_stamp_once() {
    let xml = br#"<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:v="urn:schemas-microsoft-com:vml">
<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>
<wp:anchor behindDoc="0">
<wp:positionH relativeFrom="column"><wp:posOffset>-647700</wp:posOffset></wp:positionH>
<wp:positionV relativeFrom="page"><wp:posOffset>5080000</wp:posOffset></wp:positionV>
<wp:extent cx="198120" cy="1778000"/>
<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp>
<wps:txbx><w:txbxContent><w:p>
<w:r><w:rPr><w:rFonts w:ascii="Arial"/><w:color w:val="A0A0A0"/><w:sz w:val="12"/></w:rPr><w:fldChar w:fldCharType="begin"/></w:r>
<w:r><w:instrText xml:space="preserve"> DOCPROPERTY ID \* MERGEFORMAT </w:instrText></w:r>
<w:r><w:fldChar w:fldCharType="separate"/></w:r>
<w:r><w:rPr><w:rFonts w:ascii="Arial"/><w:color w:val="A0A0A0"/><w:sz w:val="12"/></w:rPr><w:t>LEGAL#507340303v5</w:t></w:r>
<w:r><w:fldChar w:fldCharType="end"/></w:r>
</w:p></w:txbxContent></wps:txbx>
<wps:bodyPr rot="0" vert="vert270" anchor="t"/>
</wps:wsp></a:graphicData></a:graphic>
</wp:anchor>
</w:drawing></mc:Choice><mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>LEGAL#507340303v5</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p></w:ftr>"#;
    let boxes = model::parse_textboxes(xml);
    assert_eq!(boxes.len(), 1, "Choice read once, Fallback skipped: {boxes:?}");
    let b = &boxes[0];
    assert_eq!(b.text, "LEGAL#507340303v5", "cached result only, no field code");
    assert_eq!(b.font.as_deref(), Some("Arial"));
    assert_eq!(b.size_half_points, 12);
    assert_eq!(b.color.as_deref(), Some("A0A0A0"));
    assert_eq!(b.vert, 2, "vert270 = bottom-to-top");
    assert_eq!((b.x_emu, b.y_emu), (-647700, 5080000));
    assert_eq!((b.w_emu, b.h_emu), (198120, 1778000));
    assert_eq!((b.h_from.as_str(), b.v_from.as_str()), ("column", "page"));
}

/// Applying a run-formatting command marks the range; the selection-format query reports it
/// (and reports "mixed" -> None when a selection spans differing runs). Underline + font
/// survive an export/import round-trip too.
#[test]
fn run_format_command_and_selection_query() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?; // 12 chars

    // Bold + underline "cat" (chars 4..7).
    doc.apply_run_format(0, 4..7, &RunFormat { bold: Some(true), underline: Some(true), ..Default::default() }, "fmt")?;

    // The selection over "cat" reports bold + underline; the whole paragraph is "mixed".
    let on = doc.selection_format(0, 4, 7)?;
    assert_eq!(on.bold, Some(true));
    assert_eq!(on.underline, Some(true));
    let whole = doc.selection_format(0, 0, 12)?;
    assert_eq!(whole.bold, None, "a selection spanning bold + non-bold is mixed");

    // Set a font over the whole paragraph; un-bold "cat".
    doc.apply_run_format(0, 0..12, &RunFormat::font("Georgia"), "font")?;
    doc.apply_run_format(0, 4..7, &RunFormat::bold(false), "unbold")?;
    assert_eq!(doc.selection_format(0, 0, 12)?.font.as_deref(), Some("Georgia"));
    assert_eq!(doc.selection_format(0, 4, 7)?.bold, Some(false));

    // Underline + font survive the OOXML round-trip.
    let xml = doc.to_document_xml()?;
    let doc2 = CollabDoc::from_document_xml(xml.as_bytes())?;
    let runs = &doc2.paragraphs()?[0].runs;
    assert!(runs.iter().any(|r| r.underline), "underline lost on round-trip");
    assert!(runs.iter().all(|r| r.font.as_deref() == Some("Georgia")), "font lost on round-trip");
    Ok(())
}

/// Deleting your own un-accepted insertion is detected (Word removes it outright instead of
/// stacking a `w:del`); a delete spanning other text is not.
#[test]
fn own_insertion_detection() -> Result<()> {
    let doc = CollabDoc::from_document_xml(
        br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>The cat.</w:t></w:r></w:p></w:body></w:document>"#,
    )?;
    doc.suggest_insertion(0, 4, "big ", "Alice", "2026-06-20T00:00:00Z", "ins")?; // "The big cat."
    // The 4 chars of "big " are your own insertion.
    assert!(doc.range_is_own_insertion(0, 4, 8, "Alice")?);
    // A different author's delete of the same range is not "your own".
    assert!(!doc.range_is_own_insertion(0, 4, 8, "Other")?);
    // A range spilling into the original (untracked) text is not all-own-insertion.
    assert!(!doc.range_is_own_insertion(0, 4, 9, "Alice")?);
    Ok(())
}

/// An explicit `w:color w:val="auto"` is kept as the literal "auto" (not folded to unset), so it
/// renders near-black AND overrides an inherited colour - e.g. an auto run inside a styled table
/// stays black instead of picking up the table style's colour. A real colour is kept; a missing
/// colour stays None (inherits).
#[test]
fn auto_run_colour_is_preserved_not_dropped() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:rPr><w:color w:val="auto"/></w:rPr><w:t>auto</w:t></w:r>
     <w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>red</w:t></w:r>
     <w:r><w:t>plain</w:t></w:r></w:p>
</w:body></w:document>"#;
    let runs = CollabDoc::from_document_xml(xml)?.paragraphs()?[0].runs.clone();
    let by = |t: &str| runs.iter().find(|r| r.text == t).unwrap().color.clone();
    assert_eq!(by("auto").as_deref(), Some("auto"), "explicit auto is preserved");
    assert_eq!(by("red").as_deref(), Some("FF0000"), "a real colour is kept");
    assert_eq!(by("plain"), None, "no colour stays unset (inherits)");
    Ok(())
}
