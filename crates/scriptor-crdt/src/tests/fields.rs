//! Table of contents, bookmarks and hyperlinks.

use super::*;

/// A TOC-style field whose cached result spans hyperlinks across several paragraphs must
/// re-export with every fldChar / instrText marker OUTSIDE the `w:hyperlink` elements. The
/// old emission order put begin/instrText/separate inside the first entry's hyperlink and
/// the end inside the last entry's - locally schema-valid (hyperlink content is EG_PContent),
/// but Word enforces field pairing semantics and refused to OPEN the file (TOC_field_f and
/// four sibling corpus docs; caught by scripts/word-verify.ps1, invisible to ooxml-validate).
#[test]
fn field_markers_stay_outside_hyperlinks() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> TOC \o "1-3" \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:hyperlink w:anchor="_Toc1" w:history="1"><w:r><w:t>Heading 1</w:t></w:r><w:r><w:t xml:space="preserve">	2</w:t></w:r></w:hyperlink></w:p>
<w:p><w:hyperlink w:anchor="_Toc2" w:history="1"><w:r><w:t>Heading 2</w:t></w:r></w:hyperlink></w:p>
<w:p><w:hyperlink w:anchor="_Toc3" w:history="1"><w:r><w:t>Heading 3</w:t></w:r></w:hyperlink><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    assert!(out.contains("w:fldCharType=\"begin\"") && out.contains("w:fldCharType=\"end\""), "field re-wrapped: {out}");
    assert!(out.contains("<w:hyperlink w:anchor=\"_Toc1\""), "hyperlinks kept: {out}");

    // Walk the output: no field marker may appear while a hyperlink element is open.
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(out.as_bytes());
    let mut buf = Vec::new();
    let mut link_depth = 0usize;
    loop {
        match reader.read_event_into(&mut buf).expect("well-formed XML") {
            Event::Start(e) => match e.name().as_ref() {
                b"w:hyperlink" => link_depth += 1,
                b"w:fldChar" | b"w:instrText" => {
                    assert_eq!(link_depth, 0, "field marker inside a hyperlink:\n{out}");
                }
                _ => {}
            },
            Event::Empty(e) if matches!(e.name().as_ref(), b"w:fldChar" | b"w:instrText") => {
                assert_eq!(link_depth, 0, "field marker inside a hyperlink:\n{out}");
            }
            Event::End(e) if e.name().as_ref() == b"w:hyperlink" => link_depth -= 1,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

/// Several distinct bookmarks starting at the same run (a stack of TOC bookmarks on a heading)
/// all round-trip with their own id + name - the model used to keep one id per run (Option),
/// collapsing them and duplicating one id across their disjoint spans (wDateValueFormat's
/// _Toc bookmarks). Each bookmark emits exactly one start + one end.
#[test]
fn overlapping_bookmarks_round_trip() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:bookmarkStart w:id="3" w:name="_Toc1"/><w:bookmarkStart w:id="4" w:name="_Toc2"/><w:bookmarkStart w:id="5" w:name="_Toc3"/><w:r><w:t>Heading</w:t></w:r><w:bookmarkEnd w:id="5"/><w:bookmarkEnd w:id="4"/><w:bookmarkEnd w:id="3"/></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    for (id, name) in [(3, "_Toc1"), (4, "_Toc2"), (5, "_Toc3")] {
        assert_eq!(
            out.matches(&format!("<w:bookmarkStart w:id=\"{id}\" w:name=\"{name}\"/>")).count(),
            1,
            "bookmark {id}/{name} starts exactly once: {out}"
        );
        assert_eq!(out.matches(&format!("<w:bookmarkEnd w:id=\"{id}\"/>")).count(), 1, "bookmark {id} ends once: {out}");
    }
    // Stable a second time.
    let out2 = CollabDoc::from_document_xml(out.as_bytes())?.to_document_xml()?;
    assert_eq!(out, out2, "overlapping-bookmark round-trip is stable");
    Ok(())
}

/// A PAGE / NUMPAGES field round-trips as FIELD MARKUP. Import collapses the field to a
/// placeholder char (render-substituted per page); export used to emit that char as literal
/// `w:t` text, so every save replaced the page number with a U+E000 tofu character and the
/// field was gone. Covers both field forms: simple (`w:fldSimple`) and complex
/// (`w:fldChar` begin/separate/end).
#[test]
fn page_field_round_trips_as_field_markup() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:fldSimple w:instr=" PAGE "><w:r><w:t>7</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> of </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> NUMPAGES </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>9</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:fldSimple w:instr=\" PAGE \""), "PAGE re-wrapped as a field: {out}");
    assert!(out.contains("<w:fldSimple w:instr=\" NUMPAGES \""), "NUMPAGES re-wrapped: {out}");
    assert!(
        !out.contains('\u{E000}') && !out.contains('\u{E001}'),
        "no placeholder tofu in the saved XML: {out}"
    );
    assert!(out.contains(">Page <") && out.contains("> of <"), "surrounding text kept: {out}");
    // Stable a second time: the re-imported fldSimple collapses to the same placeholders.
    let out2 = CollabDoc::from_document_xml(out.as_bytes())?.to_document_xml()?;
    assert_eq!(out, out2, "second round-trip is stable");
    Ok(())
}

/// The header/footer export path shares `run_xml`, so a footer's "Page {PAGE} of {NUMPAGES}"
/// re-wraps as fields there too - the canonical victim: footers are rebuilt on every browser
/// save, so the tofu corruption hit documents that were never edited at all.
#[test]
fn footer_page_field_exports_as_field_markup() {
    let para = model::Paragraph {
        style: None,
        props: model::ParaProps::default(),
        runs: vec![model::Run::plain(format!("Page {FIELD_PAGE} of {FIELD_NUMPAGES}"))],
        prop_change: None,
        mark_change: None,
    };
    let out = model::export_hdr_ftr_xml(&[para], false, &std::collections::HashMap::new());
    assert!(out.contains("<w:fldSimple w:instr=\" PAGE \""), "PAGE field: {out}");
    assert!(out.contains("<w:fldSimple w:instr=\" NUMPAGES \""), "NUMPAGES field: {out}");
    assert!(
        !out.contains('\u{E000}') && !out.contains('\u{E001}'),
        "no placeholder tofu in the footer part: {out}"
    );
}

/// A non-PAGE field (a TOC) round-trips: its instruction + the begin/separate/end markers survive
/// export (so Word can still update it), its cached result is preserved as text + marked as the
/// field's range, and a re-import keeps it. Body text outside the field is untouched.
#[test]
fn toc_field_round_trips() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="TOC1"/></w:pPr>
  <w:r><w:fldChar w:fldCharType="begin"/></w:r>
  <w:r><w:instrText xml:space="preserve"> TOC \o &quot;1-3&quot; \h </w:instrText></w:r>
  <w:r><w:fldChar w:fldCharType="separate"/></w:r>
  <w:r><w:t>Chapter 1</w:t></w:r><w:r><w:t>3</w:t></w:r>
</w:p>
<w:p><w:r><w:t>Chapter 2</w:t></w:r><w:r><w:t>5</w:t></w:r>
  <w:r><w:fldChar w:fldCharType="end"/></w:r>
</w:p>
<w:p><w:r><w:t>Body text</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let paras = doc.paragraphs()?;
    // The cached-result runs (paras 0-1) carry the field mark; the body paragraph doesn't.
    assert!(paras[0].runs.iter().all(|r| r.field.is_some()), "TOC line 1 runs are in the field");
    assert!(paras[1].runs.iter().all(|r| r.field.is_some()), "TOC line 2 runs are in the field");
    assert!(paras[2].runs.iter().all(|r| r.field.is_none()), "body text is outside the field");

    // Export re-wraps the field: the instruction + all three fldChar markers + the result text.
    let out = doc.to_document_xml()?;
    assert!(out.contains(r#"<w:fldChar w:fldCharType="begin"/>"#), "begin emitted: {out}");
    assert!(out.contains(r#"<w:fldChar w:fldCharType="separate"/>"#), "separate emitted");
    assert!(out.contains(r#"<w:fldChar w:fldCharType="end"/>"#), "end emitted");
    assert!(out.contains("TOC"), "the TOC instruction survives");
    assert!(out.contains("Chapter 1") && out.contains("Chapter 2"), "result text survives");

    // A re-import keeps the field intact (the result runs are still marked).
    let reopened = CollabDoc::from_document_xml(out.as_bytes())?;
    let rp = reopened.paragraphs()?;
    assert!(rp[0].runs.iter().all(|r| r.field.is_some()), "field survives re-import");
    assert!(rp[2].runs.iter().all(|r| r.field.is_none()), "body still outside");
    Ok(())
}

/// Building a TOC from the document's headings: `headings()` reads level + text from the style ids;
/// `insert_toc_entries` + page-number appends + `finish_toc` produce TOC-styled, field-wrapped lines
/// that export as a real `TOC` field and round-trip. The original headings shift down, untouched.
#[test]
fn generate_toc_from_headings() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Intro")], Some("Heading1"))?;
    doc.append_paragraph(&[plain("Details")], Some("Heading2"))?;
    doc.append_paragraph(&[plain("Body")], None)?;
    let h = doc.headings();
    assert_eq!(h.len(), 2, "two headings");
    assert_eq!((h[0].1, h[0].2.as_str()), (1, "Intro"));
    assert_eq!((h[1].1, h[1].2.as_str()), (2, "Details"));

    let entries: Vec<(u8, String)> = h.iter().map(|(_, l, t)| (*l, t.clone())).collect();
    doc.insert_toc_entries(0, &entries)?;
    let len = |d: &CollabDoc, i: usize| -> usize {
        d.paragraphs().unwrap()[i].runs.iter().map(|r| r.text.chars().count()).sum()
    };
    doc.insert_text(0, len(&doc, 0), "1", "pg")?; // page number for line 1
    doc.insert_text(1, len(&doc, 1), "2", "pg")?;
    let fid = doc.finish_toc(0, entries.len(), " TOC \\o \"1-3\" \\h ")?;
    assert_eq!(fid, 0, "the first field gets id 0");

    let p = doc.paragraphs()?;
    assert_eq!(p[0].style.as_deref(), Some("TOC1"), "TOC line 1 styled");
    assert_eq!(p[1].style.as_deref(), Some("TOC2"), "TOC line 2 styled");
    assert!(p[0].runs.iter().all(|r| r.field.is_some()), "TOC line 1 wrapped as a field");
    assert_eq!(p[2].style.as_deref(), Some("Heading1"), "original heading shifted down");

    let out = doc.to_document_xml()?;
    assert!(out.contains("TOC"), "the TOC field instruction is emitted: {out}");
    assert!(out.contains(r#"<w:fldChar w:fldCharType="begin"/>"#), "field begin emitted");
    assert!(out.contains("Intro") && out.contains("Details"), "entry text present");

    let re = CollabDoc::from_document_xml(out.as_bytes())?;
    assert!(re.paragraphs()?[0].runs.iter().all(|r| r.field.is_some()), "field survives re-import");
    Ok(())
}

/// Bookmarks + hyperlinks (internal `w:anchor` + external `r:id`->URL) survive a full `.docx`
/// round-trip: the marks reattach, the bookmark name + internal anchor + external URL are preserved
/// (the external one through the document rels both ways).
#[test]
fn bookmarks_and_hyperlinks_round_trip() -> Result<()> {
    let doc_xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p>
  <w:bookmarkStart w:id="1" w:name="target"/><w:r><w:t>Anchor</w:t></w:r><w:bookmarkEnd w:id="1"/>
  <w:hyperlink w:anchor="target"><w:r><w:t>jump</w:t></w:r></w:hyperlink>
  <w:hyperlink r:id="rId5"><w:r><w:t>site</w:t></w:r></w:hyperlink>
</w:p>
</w:body></w:document>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/></Relationships>"#;
    let parts = vec![
        scriptor_ooxml::Part { name: "word/document.xml".into(), data: doc_xml.to_vec() },
        scriptor_ooxml::Part {
            name: "word/_rels/document.xml.rels".into(),
            data: rels.to_vec(),
        },
    ];
    let doc = CollabDoc::from_parts(parts)?;
    // The marks reattached to the right runs.
    let p = doc.paragraphs()?;
    assert!(p[0].runs.iter().any(|r| !r.bookmarks.is_empty()), "a run carries the bookmark mark");
    assert_eq!(p[0].runs.iter().filter(|r| r.link.is_some()).count(), 2, "two hyperlinked runs");
    // The external r:id resolved to its URL via the document rels.
    let targets: Vec<String> = model::read_hyperlinks(&doc.doc).into_values().collect();
    assert!(targets.iter().any(|t| t == "#target"), "internal anchor stored: {targets:?}");
    assert!(targets.iter().any(|t| t == "https://example.com/"), "external URL resolved: {targets:?}");

    // Full .docx round-trip: bytes -> reopen. Marks + targets + bookmark name all survive (the
    // external link via the injected External relationship both directions).
    let re = CollabDoc::from_docx_bytes(&doc.to_docx_bytes()?)?;
    let rp = re.paragraphs()?;
    assert!(rp[0].runs.iter().any(|r| !r.bookmarks.is_empty()), "bookmark survives the round-trip");
    assert_eq!(rp[0].runs.iter().filter(|r| r.link.is_some()).count(), 2, "both links survive");
    let rtargets: Vec<String> = model::read_hyperlinks(&re.doc).into_values().collect();
    assert!(rtargets.iter().any(|t| t == "#target"), "internal anchor survives: {rtargets:?}");
    assert!(rtargets.iter().any(|t| t == "https://example.com/"), "external URL survives: {rtargets:?}");
    let names: Vec<String> = model::read_bookmarks(&re.doc).into_values().collect();
    assert!(names.iter().any(|n| n == "target"), "bookmark name survives: {names:?}");
    Ok(())
}

/// `add_bookmark` over a range stores the name + `bkm~{id}` mark, is queryable by name via
/// `bookmark_paragraph`, and survives a `.docx` round-trip.
#[test]
fn add_bookmark_round_trips() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Jump target here")], None)?;
    let id = doc.add_bookmark(0, 0, 4, "Target", "bm")?; // bookmark "Jump"
    assert!(doc.paragraphs()?[0].runs.iter().any(|r| r.bookmarks.contains(&id)), "a run carries the mark");
    assert_eq!(doc.bookmark_paragraph("Target"), Some(0), "queryable by name");

    let re = CollabDoc::from_docx_bytes(&doc.to_docx_bytes()?)?;
    assert!(re.paragraphs()?[0].runs.iter().any(|r| !r.bookmarks.is_empty()), "bookmark survives reopen");
    assert!(model::read_bookmarks(&re.doc).values().any(|n| n == "Target"), "name survives reopen");
    Ok(())
}

/// The TOC update cycle at the model level (what `updateToc` orchestrates in wasm): build the TOC
/// lines, anchor each heading with a `_Toc{n}` bookmark + an internal entry hyperlink, wrap as a
/// field. `toc_field_range` locates it; a TOC entry run carries BOTH a field + a link mark (so it
/// renders in TOC style, not hyperlink blue); `remove_toc` deletes the block, clears the `_Toc`
/// anchors, and returns where it was, leaving the headings so it can be regenerated clean.
#[test]
fn toc_update_cycle() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Alpha")], Some("Heading1"))?;
    doc.append_paragraph(&[plain("Beta")], Some("Heading2"))?;
    doc.append_paragraph(&[plain("Body")], None)?;

    // Mirror wasm `insert_toc_at`: stub lines -> per heading anchor + entry link -> wrap as a field.
    let build = |d: &CollabDoc| -> Result<u64> {
        let entries: Vec<(u8, String)> = d.headings().iter().map(|(_, l, t)| (*l, t.clone())).collect();
        let count = entries.len();
        d.insert_toc_entries(0, &entries)?;
        let after = d.headings(); // headings now at their shifted indices
        for (seq, (para, (hidx, _, _))) in (d.next_toc_seq()..).zip(after.iter().enumerate()) {
            let hlen: usize = d.paragraphs()?[*hidx].runs.iter().map(|r| r.text.chars().count()).sum();
            let name = format!("_Toc{seq}");
            d.add_bookmark(*hidx, 0, hlen, &name, "anchor")?;
            let stub: usize = d.paragraphs()?[para].runs.iter().map(|r| r.text.chars().count()).sum();
            d.insert_text(para, stub, "1", "pg")?; // page number
            d.add_hyperlink(para, 0, stub + 1, &format!("#{name}"), "entry")?;
        }
        d.finish_toc(0, count, " TOC \\o \"1-3\" \\h ")
    };

    let fid = build(&doc)?;
    assert_eq!(doc.toc_field_range()?, Some((fid, 0, 2)), "TOC field located");
    let p = doc.paragraphs()?;
    assert!(
        p[0].runs.iter().any(|r| r.field == Some(fid) && r.link.is_some()),
        "a TOC entry run is both a field result and a link"
    );
    assert_eq!(doc.link_at(0, 0)?.map(|(_, t)| t).as_deref(), Some("#_Toc0"), "entry links to the anchor");
    assert!(p[2].runs.iter().any(|r| !r.bookmarks.is_empty()), "the first heading is anchored");
    assert_eq!(doc.bookmark_paragraph("_Toc0"), Some(2), "the anchor resolves to the heading");

    // Update: remove + rebuild. The block + field + `_Toc` anchors are gone; the headings survive.
    assert_eq!(doc.remove_toc("upd")?, Some(0), "removed, returns the start index");
    assert_eq!(doc.toc_field_range()?, None, "no TOC field after removal");
    assert!(
        model::read_bookmarks(&doc.doc).values().all(|n| !n.starts_with("_Toc")),
        "the _Toc anchors were cleared"
    );
    assert_eq!(doc.paragraphs()?[0].style.as_deref(), Some("Heading1"), "headings shifted back up");
    assert_eq!(doc.next_toc_seq(), 0, "the anchor sequence resets once cleared");

    let fid2 = build(&doc)?;
    assert_eq!(doc.toc_field_range()?, Some((fid2, 0, 2)), "the regenerated TOC is located");
    Ok(())
}

/// The template export (`export_docx` - the CLI `remodel` / agent path) must patch the
/// synthesized `rIdImg{id}` / `rIdLnk{id}` rels in exactly like the browser save: it used to
/// replace only `document.xml`, leaving every picture blip and external hyperlink pointing at
/// relationship ids that did not exist in the template's rels (broken images / dead links in
/// Word; the schema gate flagged the whole corpus).
#[test]
fn export_docx_patches_image_and_link_rels() -> Result<()> {
    let part = |name: &str, data: &str| scriptor_ooxml::Part { name: name.into(), data: data.as_bytes().to_vec() };
    let document = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:hyperlink r:id="rId4" w:history="1"><w:r><w:t>site</w:t></w:r></w:hyperlink><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="200"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId5"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let cts = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let parts = vec![
        part("[Content_Types].xml", cts),
        part("word/document.xml", document),
        part("word/_rels/document.xml.rels", rels),
        scriptor_ooxml::Part { name: "word/media/image1.png".into(), data: vec![0x89, b'P', b'N', b'G', 1, 2, 3] },
    ];
    let bytes = scriptor_ooxml::write_parts_bytes(&parts)?;

    let dir = std::env::temp_dir().join(format!("scriptor-export-docx-rels-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let template = dir.join("template.docx");
    let out_path = dir.join("out.docx");
    std::fs::write(&template, &bytes)?;

    let doc = CollabDoc::from_docx_bytes(&bytes)?;
    doc.export_docx(&template, &out_path)?;
    let out = scriptor_ooxml::read_parts(&out_path)?;
    std::fs::remove_dir_all(&dir).ok();

    let doc_out = out
        .iter()
        .find(|p| p.name == "word/document.xml")
        .map(|p| String::from_utf8_lossy(&p.data).into_owned())
        .unwrap_or_default();
    let rels_out = out
        .iter()
        .find(|p| p.name == "word/_rels/document.xml.rels")
        .map(|p| String::from_utf8_lossy(&p.data).into_owned())
        .unwrap_or_default();
    // Every r:id the rebuilt document references exists in the written rels.
    assert!(doc_out.contains("r:embed=\"rIdImg0\""), "blip uses the synthesized id: {doc_out}");
    assert!(rels_out.contains("Id=\"rIdImg0\""), "image rel patched in: {rels_out}");
    assert!(doc_out.contains("r:id=\"rIdLnk0\""), "hyperlink uses the synthesized id: {doc_out}");
    assert!(rels_out.contains("Id=\"rIdLnk0\""), "hyperlink rel patched in: {rels_out}");
    assert!(rels_out.contains("Target=\"https://example.com/\""), "link target kept: {rels_out}");
    Ok(())
}

/// A block-level `w:bookmarkEnd` between paragraphs (Word emits `_Hlk*` auto-bookmarks this way)
/// must anchor to the END of the paragraph it follows - not the start of the next one with the
/// previous paragraph's length as a stale offset. The regression: the end was recorded as
/// `(next_para, prev_para_len)`; when the next paragraph was shorter, marking past its length
/// panicked loro and the whole import failed. Here para 0 is far longer than para 1, so the bad
/// offset would overflow para 1.
#[test]
fn block_level_bookmark_end_between_paragraphs_anchors_to_prev() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:bookmarkStart w:id="1" w:name="bk"/><w:r><w:t xml:space="preserve">world first paragraph long</w:t></w:r></w:p>
<w:bookmarkEnd w:id="1"/>
<w:p><w:r><w:t xml:space="preserve">Short</w:t></w:r></w:p>
</w:body></w:document>"#;
    // Before the fix this panicked inside loro (mark past text length).
    let doc = CollabDoc::from_document_xml(xml)?;
    // The bookmark lands in the long first paragraph, not the short second one.
    assert_eq!(doc.bookmark_paragraph("bk"), Some(0), "bookmark anchored to para 0");
    let paras = doc.paragraphs()?;
    assert_eq!(paras[1].runs.iter().map(|r| r.text.as_str()).collect::<String>(), "Short");
    assert!(
        !paras[1].runs.iter().any(|r| !r.bookmarks.is_empty()),
        "the short second paragraph carries no bookmark mark"
    );
    Ok(())
}
