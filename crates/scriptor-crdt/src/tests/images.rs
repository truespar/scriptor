//! Pictures: placement, media, and tracked insert and remove.

use super::*;

/// Two peers editing the *same* picture converge: A resizes (w/h) while B crops + floats (disjoint
/// fields) - both edits survive on both replicas; then concurrent resizes of the *same* field
/// converge to one agreed value (LWW per field, the placement map's whole point).
#[test]
fn concurrent_image_edits_converge() -> Result<()> {
    let a = CollabDoc::new();
    a.append_paragraph(&[plain("Fig.")], None)?;
    let png = vec![0x89u8, b'P', b'N', b'G', 1];
    let id = a.insert_image(0, 4, png, "image/png", 1000, 1000, "insert")?;
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;
    assert_eq!(a.image_placements(), b.image_placements());

    // Disjoint fields: A's resize and B's crop + float all survive.
    a.set_image_size(id, 2000, 1500, "A resize")?;
    b.set_image_crop(id, 5000, 0, 5000, 0, "B crop")?;
    b.set_image_floating(id, true, "square", false, "B float")?;
    let (sa, sb) = (a.snapshot()?, b.snapshot()?);
    a.merge(&sb)?;
    b.merge(&sa)?;
    assert_eq!(a.image_placements(), b.image_placements(), "peers diverged");
    let p = a.image_placement(id).unwrap();
    assert_eq!((p.w_emu, p.h_emu), (2000, 1500), "A's resize survived");
    assert_eq!((p.crop_l, p.crop_r), (5000, 5000), "B's crop survived");
    assert!(p.floating && p.wrap == "square", "B's float survived");

    // Same field, concurrent: converges to one value, identical on both peers.
    a.set_image_size(id, 3000, 3000, "A resize 2")?;
    b.set_image_size(id, 4000, 4000, "B resize 2")?;
    let (sa, sb) = (a.snapshot()?, b.snapshot()?);
    a.merge(&sb)?;
    b.merge(&sa)?;
    assert_eq!(a.image_placements(), b.image_placements(), "LWW resize diverged");
    Ok(())
}

/// A non-picture `<w:drawing>` (a chart - has `wp:extent` but no `<a:blip>`, so it yields no
/// modeled picture) survives a `document.xml` round-trip verbatim via the passthrough path, and a
/// **real** inline picture in the same document is not double-emitted (the `parse_images` oracle
/// keeps it on the modeled image path). See `docs/passthrough.md`.
#[test]
fn non_picture_drawing_round_trips_as_passthrough() -> Result<()> {
    let chart_run = "<w:r><w:drawing><wp:inline><wp:extent cx=\"5400000\" cy=\"3000000\"/>\
<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\">\
<c:chart r:id=\"rId7\"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>";
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><w:body>\
<w:p><w:r><w:t>Before</w:t></w:r></w:p>\
<w:p>{chart_run}</w:p>\
<w:p><w:r><w:t>After</w:t></w:r></w:p>\
</w:body></w:document>"
    );
    let doc = CollabDoc::from_document_xml(xml.as_bytes())?;
    let out = doc.to_document_xml()?;
    assert!(out.contains(chart_run), "chart drawing not re-emitted verbatim:\n{out}");
    // Stable a second time (the placeholder + captured span survive re-import).
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    assert!(doc2.paragraphs()?[1].runs.iter().any(|r| r.raw.is_some()), "chart passthrough lost on re-import");
    assert!(doc2.to_document_xml()?.contains(chart_run), "chart dropped on second round-trip");
    Ok(())
}

#[test]
fn inserted_image_bytes_survive_snapshot_reopen() -> Result<()> {
    // Inserted media must live in the CRDT, not just the in-session pending_media,
    // or a reopen-from-op-log (a server reloading from a persisted op-log) loses
    // the bytes. (image_bytes does not decode, so any stable bytes exercise it.)
    let bytes = b"\x89PNG\r\n\x1a\n-fake-but-stable-image-bytes".to_vec();
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Hi.")], None)?;
    let id = doc.insert_image(0, 0, bytes.clone(), "image/png", 9525, 9525, "insert pic")?;
    let part = doc.image_placement(id).expect("placement").media;
    assert_eq!(doc.image_bytes(&part).as_deref(), Some(bytes.as_slice()), "bytes available in-session");

    // Reopen purely from the snapshot (fresh CollabDoc: empty pending_media, no source_parts).
    let snap = doc.snapshot()?;
    let reopened = CollabDoc::new();
    reopened.merge(&snap)?;
    assert_eq!(
        reopened.image_bytes(&part).as_deref(),
        Some(bytes.as_slice()),
        "inserted image bytes survive snapshot + reopen via the CRDT media map"
    );
    Ok(())
}

#[test]
fn inline_image_round_trips_through_document_xml() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Hi</w:t></w:r><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="685800"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId5"/><a:srcRect l="10000" t="0" r="10000" b="0"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    // The drawing became an editable image run anchored in paragraph 0.
    let paras = doc.paragraphs()?;
    let id = paras[0].runs.iter().find_map(|r| r.image).expect("an image run");
    let p = doc.image_placement(id).expect("a placement");
    assert_eq!((p.w_emu, p.h_emu), (914400, 685800), "size imported");
    assert_eq!((p.crop_l, p.crop_r), (10000, 10000), "crop imported");
    assert!(!p.floating, "inline picture");

    // Export re-emits the drawing with size + crop; a re-import is stable.
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:drawing>"), "{out}");
    assert!(out.contains("<wp:extent cx=\"914400\" cy=\"685800\"/>"));
    assert!(out.contains("<a:srcRect l=\"10000\""));
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    let id2 = doc2.paragraphs()?[0].runs.iter().find_map(|r| r.image).expect("image survived re-import");
    let p2 = doc2.image_placement(id2).expect("placement survived");
    assert_eq!((p2.w_emu, p2.h_emu, p2.crop_l), (914400, 685800, 10000));
    Ok(())
}

/// Saving a doc with a picture injects the blip's relationship (`rIdImg{id}` -> its media part),
/// ensures the media extension's content type, and keeps the media bytes - so an imported image
/// survives a full `.docx` save + reopen (images-editing P1c).
#[test]
fn image_media_survives_a_docx_save() -> Result<()> {
    let part = |name: &str, data: &str| scriptor_ooxml::Part { name: name.into(), data: data.as_bytes().to_vec() };
    // A minimal package: one paragraph with an inline picture, its rel, and the media bytes. The
    // content types deliberately omit a `png` Default so the save has to add one.
    let document = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="200"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId5"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;
    let cts = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let parts = vec![
        part("[Content_Types].xml", cts),
        part("word/document.xml", document),
        part("word/_rels/document.xml.rels", rels),
        scriptor_ooxml::Part { name: "word/media/image1.png".into(), data: vec![0x89, b'P', b'N', b'G', 1, 2, 3] },
    ];
    let bytes = scriptor_ooxml::write_parts_bytes(&parts)?;

    let doc = CollabDoc::from_docx_bytes(&bytes)?;
    // The picture imported as an editable run, its media resolved to the part name.
    let p = doc.image_placement(0).expect("image placement");
    assert_eq!(p.media, "word/media/image1.png", "media resolved from the embed rel");

    // Save: the blip rel, the media bytes, and a png content-type Default are all in the package.
    let out = scriptor_ooxml::read_parts_bytes(&doc.to_docx_bytes()?)?;
    let rels_out = out
        .iter()
        .find(|p| p.name == "word/_rels/document.xml.rels")
        .map(|p| String::from_utf8_lossy(&p.data).into_owned())
        .unwrap_or_default();
    assert!(rels_out.contains("Id=\"rIdImg0\""), "image rel injected: {rels_out}");
    assert!(rels_out.contains("Target=\"media/image1.png\""), "rel targets the media part");
    assert!(out.iter().any(|p| p.name == "word/media/image1.png"), "media bytes kept");
    let cts_out = out
        .iter()
        .find(|p| p.name == "[Content_Types].xml")
        .map(|p| String::from_utf8_lossy(&p.data).into_owned())
        .unwrap_or_default();
    assert!(cts_out.contains("Extension=\"png\""), "png content type ensured: {cts_out}");
    Ok(())
}
