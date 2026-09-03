//! Header and footer stories, sections, and page background.

use super::*;

/// "Different First Page": a section with a default header (rId8), a first-page header (rId9), a
/// first-page footer (rId10) and `<w:titlePg/>` imports the first-page variants into their own
/// stories, sets `title_pg`, and leaves the (absent) default footer empty.
#[test]
fn different_first_page_imports_first_header_and_footer() -> Result<()> {
    let part = |name: &str, data: String| scriptor_ooxml::Part { name: name.into(), data: data.into_bytes() };
    let ns = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
    let document = format!(
        "<w:document {ns}><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr>\
<w:headerReference w:type=\"default\" r:id=\"rId8\"/>\
<w:headerReference w:type=\"first\" r:id=\"rId9\"/>\
<w:footerReference w:type=\"first\" r:id=\"rId10\"/>\
<w:titlePg/></w:sectPr></w:body></w:document>"
    );
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId8\" Type=\"h\" Target=\"header1.xml\"/>\
<Relationship Id=\"rId9\" Type=\"h\" Target=\"header2.xml\"/>\
<Relationship Id=\"rId10\" Type=\"f\" Target=\"footer1.xml\"/></Relationships>"
        .to_string();
    let hf = |tag: &str, text: &str| format!("<w:{tag} {ns}><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:{tag}>");
    let doc = CollabDoc::from_parts(vec![
        part("word/document.xml", document),
        part("word/_rels/document.xml.rels", rels),
        part("word/header1.xml", hf("hdr", "DefaultHeader")),
        part("word/header2.xml", hf("hdr", "FirstHeader")),
        part("word/footer1.xml", hf("ftr", "FirstFooter")),
    ])?;

    let text = |ps: &[Paragraph]| -> String {
        ps.iter().flat_map(|p| p.runs.iter().map(|r| r.text.clone())).collect()
    };
    assert!(doc.title_pg(), "titlePg recognized");
    assert!(text(&doc.header()).contains("DefaultHeader"), "default header imported");
    assert!(text(&doc.header_first()).contains("FirstHeader"), "first-page header imported");
    assert!(text(&doc.footer_first()).contains("FirstFooter"), "first-page footer imported");
    assert!(doc.footer().is_empty(), "no default footer in this section");

    // The references + <w:titlePg/> round-trip through document.xml.
    let xml = doc.to_document_xml()?;
    assert!(xml.contains("<w:titlePg/>"), "titlePg emitted: {xml}");
    assert!(xml.contains("w:headerReference w:type=\"first\""), "first header ref emitted");
    assert!(xml.contains("w:footerReference w:type=\"first\""), "first footer ref emitted");
    assert!(CollabDoc::from_document_xml(xml.as_bytes())?.title_pg(), "titlePg survives re-import");
    Ok(())
}

/// A MULTI-SECTION document resolves headers/footers PER SECTION with Word's carry-forward
/// inheritance (the legal-template shape): section 1 defines default header/footer + a
/// first-page footer, NO titlePg; section 2 defines only its own default header + titlePg. So
/// section 2's first page inherits section 1's FIRST-page footer (the rotated-stamp scenario)
/// while its later pages run section 2's header over section 1's default footer. Saving writes
/// each part from its OWN story - the old role-keyed save overwrote section 1's header file
/// with section 2's content.
#[test]
fn multi_section_headers_resolve_per_section_and_save_part_keyed() -> Result<()> {
    let part = |name: &str, data: String| scriptor_ooxml::Part { name: name.into(), data: data.into_bytes() };
    let ns = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
    let document = format!(
        "<w:document {ns}><w:body>\
<w:p><w:pPr><w:sectPr>\
<w:headerReference w:type=\"default\" r:id=\"rId1\"/>\
<w:footerReference w:type=\"default\" r:id=\"rId2\"/>\
<w:footerReference w:type=\"first\" r:id=\"rId3\"/>\
<w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
<w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>\
</w:sectPr></w:pPr><w:r><w:t>Section one.</w:t></w:r></w:p>\
<w:p><w:r><w:t>Section two.</w:t></w:r></w:p>\
<w:sectPr>\
<w:headerReference w:type=\"default\" r:id=\"rId4\"/>\
<w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
<w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>\
<w:titlePg/></w:sectPr></w:body></w:document>"
    );
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"h\" Target=\"header1.xml\"/>\
<Relationship Id=\"rId2\" Type=\"f\" Target=\"footer1.xml\"/>\
<Relationship Id=\"rId3\" Type=\"f\" Target=\"footer2.xml\"/>\
<Relationship Id=\"rId4\" Type=\"h\" Target=\"header2.xml\"/></Relationships>"
        .to_string();
    let hf = |tag: &str, text: &str| format!("<w:{tag} {ns}><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:{tag}>");
    let doc = CollabDoc::from_parts(vec![
        part("word/document.xml", document),
        part("word/_rels/document.xml.rels", rels),
        part("word/header1.xml", hf("hdr", "H1")),
        part("word/header2.xml", hf("hdr", "H2")),
        part("word/footer1.xml", hf("ftr", "F1")),
        part("word/footer2.xml", hf("ftr", "F2first")),
    ])?;

    assert_eq!(doc.num_sections(), 2, "one entry per sectPr");
    let s1 = doc.section_hf(0);
    assert_eq!(s1.header_default.as_deref(), Some("word/header1.xml"));
    assert_eq!(s1.footer_default.as_deref(), Some("word/footer1.xml"));
    assert_eq!(s1.footer_first.as_deref(), Some("word/footer2.xml"));
    assert!(!s1.title_pg, "section 1 has no titlePg");
    let s2 = doc.section_hf(1);
    assert_eq!(s2.header_default.as_deref(), Some("word/header2.xml"), "section 2's own header");
    assert_eq!(s2.footer_default.as_deref(), Some("word/footer1.xml"), "default footer inherited");
    assert_eq!(
        s2.footer_first.as_deref(),
        Some("word/footer2.xml"),
        "FIRST footer inherited - what puts the stamp on section 2's opening page"
    );
    assert!(s2.title_pg, "section 2 uses Different First Page");

    // Part-keyed stories: each file's own content, not last-reference-wins.
    let text_of = |part: &str| -> String {
        doc.hf_part_doc(part)
            .and_then(|d| d.paragraphs().ok())
            .unwrap_or_default()
            .iter()
            .flat_map(|p| p.runs.iter().map(|r| r.text.clone()))
            .collect()
    };
    assert_eq!(text_of("word/header1.xml"), "H1");
    assert_eq!(text_of("word/header2.xml"), "H2");

    // Save: every part re-serializes from its OWN story (H1 stays H1 - the old save wrote H2
    // into header1.xml too), and reopening resolves the same per-section bindings.
    let reopened = CollabDoc::from_docx_bytes(&doc.to_docx_bytes()?)?;
    let rtext = |part: &str| -> String {
        reopened
            .hf_part_doc(part)
            .and_then(|d| d.paragraphs().ok())
            .unwrap_or_default()
            .iter()
            .flat_map(|p| p.runs.iter().map(|r| r.text.clone()))
            .collect()
    };
    assert_eq!(rtext("word/header1.xml"), "H1", "section 1's header file kept its content");
    assert_eq!(rtext("word/header2.xml"), "H2");
    assert_eq!(rtext("word/footer2.xml"), "F2first");
    assert_eq!(reopened.num_sections(), 2);
    assert_eq!(reopened.section_hf(1).footer_first.as_deref(), Some("word/footer2.xml"));
    Ok(())
}

/// The header/footer root used to declare only `xmlns:w`, so an image run's `r:embed` made
/// the rebuilt part namespace-non-well-formed on save (Word repair on any doc with a header
/// logo).
#[test]
fn header_export_declares_every_emitted_namespace_prefix() {
    let para = model::Paragraph {
        style: None,
        props: model::ParaProps::default(),
        runs: vec![model::Run {
            image: Some(7),
            ..model::Run::plain(model::IMAGE_PLACEHOLDER.to_string())
        }],
        prop_change: None,
        mark_change: None,
    };
    let mut images = std::collections::HashMap::new();
    images.insert(
        7u64,
        model::ImagePlacement {
            media: "word/media/image1.png".into(),
            w_emu: 500,
            h_emu: 400,
            ..Default::default()
        },
    );
    let out = model::export_hdr_ftr_xml(&[para], true, &images);
    assert!(out.contains("r:embed=\"rIdImg7\""), "image rel: {out}");
    assert_ns_well_formed(&out);
}

/// A multi-section document round-trips each section's `<w:sectPr>` VERBATIM: the in-paragraph
/// section break stays in its carrier's pPr and the body-final one at body end, each keeping
/// ITS OWN header/footer refs + page geometry. The old model collapsed every section's hf refs
/// into one synthesized final sectPr, overflowing `EG_HdrFtrReferences` - the corpus docs Word
/// then refused. Also asserts no spurious `<w:br type="page"/>` is emitted at the section
/// boundary (the sectPr is the break) and the round-trip is stable.
#[test]
fn multi_section_sectpr_round_trips_verbatim() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId10"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/><w:cols w:space="708"/></w:sectPr></w:pPr><w:r><w:t>Section one.</w:t></w:r></w:p>
<w:p><w:r><w:t>Section two body.</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rId20"/><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/><w:cols w:num="2" w:space="708"/></w:sectPr>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;

    // Two distinct sectPr elements, each with its OWN header ref - not one merged sectPr.
    assert_eq!(out.matches("<w:sectPr>").count() + out.matches("<w:sectPr ").count(), 2, "two sectPrs: {out}");
    assert!(out.contains("r:id=\"rId10\"") && out.contains("r:id=\"rId20\""), "both refs preserved: {out}");
    // The section-1 sectPr sits in a pPr and keeps its own geometry (portrait, 1 col); the final
    // is landscape 2-col. Neither merges the other's ref.
    let s1 = &out[out.find("rId10").unwrap()..];
    assert!(!s1[..s1.find("</w:sectPr>").unwrap()].contains("rId20"), "section 1 does not carry section 2's ref: {out}");
    assert!(out.contains("w:orient=\"landscape\"") && out.contains("w:num=\"2\""), "final section geometry kept: {out}");
    // The section boundary is the sectPr, not an extra page-break run: the only paragraph-level
    // break machinery here is the sectPr itself.
    assert!(!out.contains("<w:br w:type=\"page\"/>"), "no spurious section page-break run: {out}");

    // Stable a second time.
    let out2 = CollabDoc::from_document_xml(out.as_bytes())?.to_document_xml()?;
    assert_eq!(out, out2, "multi-section round-trip is stable");
    Ok(())
}

/// P5 wart 3: a header + footer added to a document that had neither is materialized on save (a new
/// part + relationship + content-type override + `sectPr` reference) and round-trips on reopen.
#[test]
fn adding_a_header_and_footer_to_a_doc_without_them_persists_on_save() -> Result<()> {
    // Save + reopen a plain doc so it has real source parts (the common path) but no header/footer.
    let base = CollabDoc::new();
    base.append_paragraph(&[plain("Body text.")], None)?;
    let mut doc = CollabDoc::from_docx_bytes(&base.to_docx_bytes()?)?;
    assert!(doc.header_text().is_empty() && doc.footer_text().is_empty(), "none to start");
    assert!(!doc.source_parts.is_empty(), "the reopened doc carries real source parts");

    doc.set_header_text("Confidential");
    doc.set_footer_text("Page 1");
    let reopened = CollabDoc::from_docx_bytes(&doc.to_docx_bytes()?)?;

    assert_eq!(reopened.header_text(), "Confidential", "new header persisted + round-tripped");
    assert_eq!(reopened.footer_text(), "Page 1", "new footer persisted + round-tripped");
    let body: String =
        reopened.paragraphs()?[0].runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(body, "Body text.", "the body survived the document.xml rewrite");
    Ok(())
}

/// The page background (`w:background w:color`) parses from document.xml, is DISPLAYED only
/// when settings.xml opts in (`w:displayBackgroundShape` - Word's gate), and re-emits in its
/// schema slot on save (the exporter previously dropped it silently).
#[test]
fn page_background_parses_gates_and_round_trips() -> Result<()> {
    let mk = |with_flag: bool| {
        let mut parts = vec![scriptor_ooxml::Part {
            name: "word/document.xml".into(),
            data: br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:background w:color="92D050"/><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>"#.to_vec(),
        }];
        if with_flag {
            parts.push(scriptor_ooxml::Part {
                name: "word/settings.xml".into(),
                data: br#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:displayBackgroundShape/></w:settings>"#.to_vec(),
            });
        }
        CollabDoc::from_parts(parts)
    };
    let shown = mk(true)?;
    assert_eq!(shown.page_background(), Some("92D050"), "the fill colour is parsed");
    assert!(shown.page_background_shown(), "displayBackgroundShape shows it");
    let hidden = mk(false)?;
    assert_eq!(hidden.page_background(), Some("92D050"), "kept for round-trip even unshown");
    assert!(!hidden.page_background_shown(), "no displayBackgroundShape -> not painted");
    let xml = shown.to_document_xml()?;
    assert!(
        xml.contains(r#"<w:background w:color="92D050"/><w:body>"#),
        "the exporter re-emits the background in its schema slot: {xml}"
    );
    Ok(())
}
