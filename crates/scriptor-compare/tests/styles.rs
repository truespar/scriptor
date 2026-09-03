//! Style-rename robustness: two documents that are the same content re-saved with the style ids
//! localized vs. English (`Brdtext` <-> `BodyText`, `Rubrik1` <-> `Heading1`) must NOT flood the
//! redline with `w:pPrChange` noise - those ids denote one style (same `w:name`). Regression for the
//! NOBA drafts, which produced ~180 spurious paragraph-format changes for a one-word edit.

use scriptor_compare::{compare, CompareOptions};

/// Build a minimal, valid `.docx` whose paragraphs each carry `(style_id)` and whose `styles.xml`
/// maps that id to `style_name` (`w:name`). One `<w:p>` per (style_id, text) entry.
fn docx(paras: &[(&str, &str)], styles: &[(&str, &str)]) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

    let mut body = String::new();
    for (style, text) in paras {
        body.push_str(&format!(
            r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
        ));
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );

    let mut style_defs = String::new();
    for (id, name) in styles {
        style_defs.push_str(&format!(
            r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="{name}"/></w:style>"#
        ));
    }
    let styles_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{style_defs}</w:styles>"#
    );

    let part = |name: &str, data: String| scriptor_ooxml::Part { name: name.into(), data: data.into_bytes() };
    let parts = vec![
        part("[Content_Types].xml", content_types.into()),
        part("_rels/.rels", root_rels.into()),
        part("word/_rels/document.xml.rels", doc_rels.into()),
        part("word/document.xml", document),
        part("word/styles.xml", styles_xml),
    ];
    scriptor_ooxml::write_parts_bytes(&parts).unwrap()
}

#[test]
fn style_id_rename_with_same_name_is_not_a_change() {
    // A: Swedish ids; B: English ids. Same `w:name`, identical text -> nothing changed.
    let a = docx(
        &[("Brdtext", "The Supplier shall provide the Services."), ("Rubrik1", "Heading")],
        &[("Brdtext", "Body Text"), ("Rubrik1", "heading 1")],
    );
    let b = docx(
        &[("BodyText", "The Supplier shall provide the Services."), ("Heading1", "Heading")],
        &[("BodyText", "Body Text"), ("Heading1", "heading 1")],
    );
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    assert_eq!(result.manifest.changes.len(), 0, "style-id rename must not redline: {:?}", result.manifest.changes);
}

#[test]
fn genuine_restyle_is_still_reported() {
    // Same text, but paragraph 1 really changes style (Body Text -> Heading 1): a real format change.
    let a = docx(&[("BodyText", "A clause.")], &[("BodyText", "Body Text"), ("Heading1", "heading 1")]);
    let b = docx(&[("Heading1", "A clause.")], &[("BodyText", "Body Text"), ("Heading1", "heading 1")]);
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    assert_eq!(result.manifest.changes.len(), 1, "a real restyle should be one change: {:?}", result.manifest.changes);
}

#[test]
fn real_edit_survives_alongside_a_rename() {
    // A one-word text edit plus a wholesale style-id rename: exactly one change (the edit), no noise.
    let a = docx(&[("Brdtext", "Payment within thirty days.")], &[("Brdtext", "Body Text")]);
    let b = docx(&[("BodyText", "Payment within sixty days.")], &[("BodyText", "Body Text")]);
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    let kinds: Vec<_> = result.manifest.changes.iter().map(|c| format!("{:?}", c.kind)).collect();
    assert!(
        result.manifest.changes.iter().all(|c| !format!("{:?}", c.kind).contains("Format")),
        "no format-change noise expected, got {kinds:?}"
    );
}
