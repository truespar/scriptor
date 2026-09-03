//! Style resolution and runtime style edits.

use super::*;

/// A style-definition edit (Modify-Style) survives a snapshot round-trip: the override lives in
/// the loro op-log (STYLE_OVERRIDES), so a doc rebuilt from a snapshot - the wasm `fromSnapshot`
/// path: `new()` then `merge` - reflects it. This is the property the old in-memory `StyleTable`
/// lacked (a snapshot carried no styles.xml).
#[test]
fn style_edit_persists_through_a_snapshot() -> Result<()> {
    let a = CollabDoc::new();
    a.set_style_props("Heading1", &StyleProps { size: Some(40), ..StyleProps::default() })?;
    assert_eq!(a.styles().resolve(Some("Heading1")).size, Some(40), "edit live in the source doc");

    // Mirror wasm fromSnapshot: a fresh doc, then merge the snapshot bytes.
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;
    assert_eq!(
        b.styles().resolve(Some("Heading1")).size,
        Some(40),
        "the style edit survived the snapshot rebuild"
    );
    Ok(())
}

/// A style edit is undoable (it commits a single loro op): undo reverts the effective table to the
/// pre-edit base, and the dirty flag forces the next `styles()` read to rebuild from it.
#[test]
fn style_edit_undoes() -> Result<()> {
    let mut a = CollabDoc::new();
    let base = a.styles().resolve(Some("Heading1")).size;
    a.set_style_props("Heading1", &StyleProps { size: Some(40), ..StyleProps::default() })?;
    assert_eq!(a.styles().resolve(Some("Heading1")).size, Some(40), "edit applied");
    assert!(a.undo()?, "there was something to undo");
    assert_eq!(
        a.styles().resolve(Some("Heading1")).size,
        base,
        "undo reverted the style edit to the base definition"
    );
    Ok(())
}

/// A new custom paragraph style (Word's New-Style) appears in the gallery, resolves to its props
/// through its `basedOn` chain, and survives a snapshot rebuild (it's loro state via STYLE_ADDED +
/// STYLE_OVERRIDES).
#[test]
fn add_style_appears_resolves_and_survives_snapshot() -> Result<()> {
    let a = CollabDoc::new();
    a.add_style(
        "MyQuote",
        "My Quote",
        Some("Normal"),
        &StyleProps { italic: Some(true), size: Some(28), color: Some("0070C0".into()), ..StyleProps::default() },
    )?;
    assert!(
        a.style_gallery().iter().any(|(id, name)| id == "MyQuote" && name == "My Quote"),
        "added style is offered in the gallery with its name"
    );
    let r = a.styles().resolve(Some("MyQuote"));
    assert_eq!((r.italic, r.size, r.color.as_deref()), (Some(true), Some(28), Some("0070C0")));

    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;
    let rb = b.styles().resolve(Some("MyQuote"));
    assert_eq!((rb.italic, rb.size), (Some(true), Some(28)), "added style survived the snapshot");
    assert!(b.style_gallery().iter().any(|(id, _)| id == "MyQuote"), "and is in the peer's gallery");
    Ok(())
}

/// `parse_styles` captures each style's display name + flags the paragraph quick-styles for the
/// gallery (skipping character/table styles + non-qFormat ones).
#[test]
fn paragraph_style_gallery_parses_quick_styles() {
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:qFormat/></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:qFormat/></w:style>
<w:style w:type="paragraph" w:styleId="TOC1"><w:name w:val="toc 1"/></w:style>
<w:style w:type="character" w:styleId="Emphasis"><w:name w:val="Emphasis"/><w:qFormat/></w:style>
</w:styles>"#;
    let table = model::parse_styles(xml);
    assert_eq!(table.gallery, vec!["Title".to_string(), "Heading1".to_string()],
        "only paragraph quick-styles, in order (TOC1 lacks qFormat, Emphasis is a character style)");
    assert_eq!(table.names.get("Heading1").map(|s| s.as_str()), Some("heading 1"));
}

/// A from-scratch document seeds Word's built-in quick styles, so the Styles gallery is populated
/// (Normal first, then No Spacing / Heading 1-3 / Title / Subtitle / Quote / ...) before any are
/// used - matching Word's blank-doc gallery instead of showing only "Normal".
#[test]
fn seeded_doc_offers_word_quick_styles() {
    let doc = CollabDoc::new();
    let gallery = doc.style_gallery();
    let ids: Vec<&str> = gallery.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids.first(), Some(&"Normal"), "Normal leads the gallery");
    for want in ["NoSpacing", "Heading1", "Heading2", "Heading3", "Title", "Subtitle", "Quote", "IntenseQuote", "ListParagraph"] {
        assert!(ids.contains(&want), "gallery offers {want}: {ids:?}");
    }
    // Display names are the human-facing Word names, not the ids.
    let name = |id: &str| gallery.iter().find(|(i, _)| i == id).map(|(_, n)| n.as_str());
    assert_eq!(name("NoSpacing"), Some("No Spacing"));
    assert_eq!(name("Heading1"), Some("Heading 1"));
    assert_eq!(name("IntenseQuote"), Some("Intense Quote"));
}

/// The seeded built-ins carry default-theme formatting so they render as a real hierarchy: Heading 1
/// is large + accent-coloured + Calibri Light; Title is bigger still. (Resolved over docDefaults.)
#[test]
fn seeded_heading_styles_resolve_to_props() {
    let doc = CollabDoc::new();
    let h1 = doc.resolve_style("Heading1");
    assert_eq!(h1.size, Some(32));
    assert_eq!(h1.color.as_deref(), Some("2E74B5"));
    assert_eq!(h1.font.as_deref(), Some("Calibri Light"));
    assert_eq!(doc.resolve_style("Title").size, Some(56));
    // Normal inherits the Calibri 11pt docDefault.
    assert_eq!(doc.resolve_style("Normal").size, Some(22));
    assert_eq!(doc.resolve_style("Normal").font.as_deref(), Some("Calibri"));
}

/// A from-scratch document writes a real `word/styles.xml` on save, so its seeded styles survive a
/// full `.docx` round-trip (the gallery + resolved props come back after reopen).
#[test]
fn new_doc_styles_round_trip() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("A title")], None)?;
    doc.set_paragraph_style(0, Some("Title"), "style")?;
    let bytes = doc.to_docx_bytes()?;
    let reopened = CollabDoc::from_docx_bytes(&bytes)?;
    assert_eq!(reopened.paragraph_style(0).as_deref(), Some("Title"), "applied style survives");
    let ids: Vec<String> = reopened.style_gallery().into_iter().map(|(id, _)| id).collect();
    assert!(ids.contains(&"Heading1".to_string()), "gallery survives: {ids:?}");
    assert_eq!(reopened.resolve_style("Heading1").size, Some(32), "heading props survive");
    Ok(())
}

/// A bullet list applied to a document with no list definitions synthesizes one (the marker renders
/// live), and it survives a full `.docx` save + reopen (numbering.xml is materialized).
#[test]
fn synthesized_list_renders_and_round_trips() -> Result<()> {
    // Numbering-level: synthesize + reuse + the save XML.
    let mut n = Numbering::default();
    let bullet = n.ensure_list(ListFormat::Bullet);
    assert!(n.level(bullet, 0).is_some(), "bullet level 0 exists");
    assert!(n.has_synth());
    assert_eq!(
        n.ensure_list(ListFormat::Bullet),
        bullet,
        "a second bullet request reuses the same definition"
    );
    let number = n.ensure_list(ListFormat::Decimal);
    assert_ne!(number, bullet, "a numbered list is a distinct definition");
    // The decimal list cycles its format by depth (Word's 1. a. i. outline), so demoting shows
    // letters then roman numerals - not decimal at every level.
    assert_eq!(n.level(number, 0).unwrap().fmt, "decimal", "level 0 is decimal");
    assert_eq!(n.level(number, 1).unwrap().fmt, "lowerLetter", "level 1 is a/b/c");
    assert_eq!(n.level(number, 2).unwrap().fmt, "lowerRoman", "level 2 is i/ii/iii");
    assert_eq!(n.level(number, 3).unwrap().fmt, "decimal", "the cycle repeats at level 3");
    // A picked number format is a distinct definition and applies uniformly at every level.
    let roman = n.ensure_list(ListFormat::LowerRoman);
    assert_ne!(roman, number, "a picked format is its own definition");
    assert_eq!(n.ensure_list(ListFormat::LowerRoman), roman, "re-picking reuses it");
    assert_eq!(n.level(roman, 0).unwrap().fmt, "lowerRoman", "uniform roman at level 0");
    assert_eq!(n.level(roman, 3).unwrap().fmt, "lowerRoman", "uniform roman at depth too");
    let xml = n.synth_xml();
    assert!(xml.contains("w:numFmt w:val=\"bullet\""), "bullet def emitted: {xml}");
    assert!(xml.contains("w:numFmt w:val=\"decimal\""), "decimal def emitted: {xml}");
    assert!(xml.contains("w:numFmt w:val=\"lowerLetter\""), "letter level emitted: {xml}");
    assert!(xml.contains("w:numFmt w:val=\"lowerRoman\""), "roman level emitted: {xml}");

    // Whole-document: a fresh doc (no numbering.xml) + a bullet list, saved + reopened.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Item")], None)?;
    let num_id = doc.ensure_list(ListFormat::Bullet);
    doc.set_numbering(0, Some(num_id), Some(0), "bullets")?;
    assert!(doc.numbering().level(num_id, 0).is_some(), "synthesized level queryable for render");

    let bytes = doc.to_docx_bytes()?;
    let reopened = CollabDoc::from_docx_bytes(&bytes)?;
    assert_eq!(reopened.paragraph_format(0)?.num_id, Some(num_id), "paragraph still in the list");
    assert!(
        reopened.numbering().level(num_id, 0).is_some(),
        "numbering.xml was materialized so the marker resolves on reopen"
    );
    Ok(())
}

/// A list synthesized at runtime survives a reopen THROUGH THE LORO OP-LOG (not just on export): its
/// definition's identity lives in the `NUM_SYNTH` loro map, so building a fresh `CollabDoc` and
/// `merge`-ing a snapshot rebuilds the def - the reopened doc resolves `level()`, keeps the
/// paragraph's `numId`, and emits the `<w:num>` + abstract on save. This is the regression the
/// loro-backed numbering closes (previously the in-memory `Numbering` reset to empty on merge, so a
/// runtime list was lost on reopen + never synced to collaborators).
#[test]
fn synth_numbering_survives_reopen() -> Result<()> {
    let doc = CollabDoc::new();
    let nid = doc.ensure_list(ListFormat::Bullet);
    doc.append_paragraph(&[plain("Item")], Some("ListParagraph"))?;
    doc.set_numbering(0, Some(nid), Some(0), "bullets")?;
    let snap = doc.snapshot()?;

    // Reopen purely through the op-log: a fresh doc + a loro merge (NO .docx, NO source-parts
    // re-attach). This is the path that used to drop the runtime list.
    let reopened = CollabDoc::new();
    reopened.merge(&snap)?;

    // (a) The paragraph's numbering reference survived.
    assert_eq!(
        reopened.paragraph_format(0)?.num_id,
        Some(nid),
        "the paragraph's numId survived the op-log reopen"
    );
    // (b) The list DEFINITION survived in loro (so the marker resolves live, not just on export).
    assert!(
        reopened.numbering().level(nid, 0).is_some(),
        "the synthesized list def was rebuilt from the NUM_SYNTH loro map on merge"
    );
    assert_eq!(
        reopened.numbering().level(nid, 0).unwrap().fmt,
        "bullet",
        "the rebuilt def carries the right kind"
    );
    // (c) Export from the reopened doc emits the synthesized num + its abstract into the
    // (zipped) numbering.xml part - unzip it and assert on the actual bytes.
    let bytes = reopened.to_docx_bytes()?;
    let parts = scriptor_ooxml::read_parts_bytes(&bytes)?;
    let numbering_xml = parts
        .iter()
        .find(|p| p.name == "word/numbering.xml")
        .map(|p| String::from_utf8_lossy(&p.data).into_owned())
        .expect("the export carries a numbering.xml part for the synthesized list");
    assert!(
        numbering_xml.contains(&format!("w:numId=\"{nid}\"")),
        "to_docx_bytes emits <w:num w:numId=\"{nid}\">: {numbering_xml}"
    );
    assert!(
        numbering_xml.contains("w:numFmt w:val=\"bullet\""),
        "to_docx_bytes emits the bullet abstract: {numbering_xml}"
    );
    Ok(())
}

/// A document that carries BOTH imported `numbering.xml` definitions (low ids, Word's own 1..N) AND
/// a runtime-synthesized list must keep them disjoint: the synth id uses the high base (>= 900000) so
/// it can never collide with an imported id, the imported defs are left intact, and a synth-of-a-kind
/// the import already provides reuses the imported def rather than duplicating it. This guards the
/// round-trip the task flagged as the key risk.
#[test]
fn imported_and_synth_numbering_do_not_collide() {
    // An imported numbering.xml with two low-id defs (decimal num 1, bullet num 2) - Word's own ids.
    let imported = br#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>
  <w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
  <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#;
    let mut num = model::parse_numbering(imported);
    assert_eq!(num.level(1, 0).unwrap().fmt, "decimal", "imported num 1 is decimal");
    assert_eq!(num.level(2, 0).unwrap().fmt, "bullet", "imported num 2 is bullet");

    // A synth-of-a-kind the import already provides reuses the imported id (no duplicate def).
    assert_eq!(num.reusable_num_id(ListFormat::Bullet), Some(2), "bullet reuses imported num 2");
    assert_eq!(num.reusable_num_id(ListFormat::Decimal), Some(1), "decimal reuses imported num 1");

    // A synth-of-a-NEW-kind (lowerRoman) the import lacks gets a fresh HIGH-base id (>= 900000),
    // disjoint from the imported low ids - no collision.
    assert!(num.reusable_num_id(ListFormat::LowerRoman).is_none(), "no roman def yet");
    let roman = num.next_synth_num_id();
    assert_eq!(roman, 900_000, "first synth id is the high base, clear of imported 1/2");
    num.insert_synth(roman, ListFormat::LowerRoman);
    assert_eq!(num.level(roman, 0).unwrap().fmt, "lowerRoman", "synth roman def built");
    // Imported defs untouched by the synth.
    assert_eq!(num.level(1, 0).unwrap().fmt, "decimal", "imported num 1 still decimal");
    assert_eq!(num.level(2, 0).unwrap().fmt, "bullet", "imported num 2 still bullet");
    // A second synth id climbs from the base, still clear of the imported range.
    assert_eq!(num.next_synth_num_id(), 900_001, "next synth id climbs from the base");
}

/// A run's character style (`w:rStyle`) is captured on the run and survives the round-trip - so a
/// doc using char styles (Hyperlink / Strong / ... - ~12% of real docs) doesn't lose them on save.
/// (The style's highlight is resolved at render; that's exercised in the wasm layer.)
#[test]
fn run_char_style_is_captured_and_round_trips() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>styled</w:t></w:r>
     <w:r><w:t>plain</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let runs = doc.paragraphs()?[0].runs.clone();
    let by = |t: &str| runs.iter().find(|r| r.text == t).unwrap().char_style.clone();
    assert_eq!(by("styled").as_deref(), Some("Strong"), "rStyle captured on the run");
    assert_eq!(by("plain"), None, "a run with no rStyle stays None");
    // Round-trip: the character style is re-emitted (was previously dropped on import).
    assert!(doc.to_document_xml()?.contains(r#"<w:rStyle w:val="Strong"/>"#), "rStyle survives export");
    Ok(())
}

#[test]
fn new_list_mints_independent_restarting_lists() -> Result<()> {
    let doc = CollabDoc::new();
    let add_item = |num_id: i32, text: &str| -> Result<()> {
        doc.append_paragraph(&[plain(text)], Some("ListParagraph"))?;
        let idx = doc.paragraphs()?.len() - 1;
        doc.set_numbering(idx, Some(num_id), Some(0), "t")?;
        Ok(())
    };

    // First ordered list.
    let a = doc.new_list(ListFormat::Decimal);
    add_item(a, "A one")?;
    add_item(a, "A two")?;
    // A non-list paragraph between them.
    doc.append_paragraph(&[plain("interlude")], None)?;
    // Second ordered list - must be INDEPENDENT (restart at 1, not continue).
    let b = doc.new_list(ListFormat::Decimal);
    add_item(b, "B one")?;

    assert_ne!(a, b, "each new_list mints a distinct numId");
    let num = doc.numbering();
    assert_ne!(
        num.abstract_id(a),
        num.abstract_id(b),
        "distinct numIds -> distinct abstracts -> independent counters (each restarts at 1)"
    );
    drop(num);

    // ensure_list, by contrast, REUSES an existing decimal def (the editor's
    // list-toggle path must not pile up definitions).
    let reused = doc.ensure_list(ListFormat::Decimal);
    assert!(reused == a || reused == b, "ensure_list reuses an existing decimal list");
    Ok(())
}
