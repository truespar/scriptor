//! Suggesting, accepting and rejecting tracked changes.

use super::*;

/// `revision` advances on any edit (the freshness token).
#[test]
fn revision_advances_on_edits() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Hi.")], None)?;
    let r1 = doc.revision();
    doc.append_paragraph(&[plain("Bye.")], None)?;
    assert!(doc.revision() > r1, "revision must advance");
    Ok(())
}

/// `list_changes` enumerates tracked changes one-per-id with kind + author + text.
#[test]
fn list_changes_enumerates_tracked_changes() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    doc.suggest_insertion(0, 4, "quick ", "Agent", "2026-06-23T00:00:00Z", "ins")?;
    doc.suggest_deletion(0, 0..3, "Agent", "2026-06-23T00:00:00Z", "del")?; // "The"

    let changes = doc.list_changes()?;
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().any(|c| c.kind == "ins" && c.text == "quick "));
    assert!(changes.iter().any(|c| c.kind == "del"));
    assert!(changes.iter().all(|c| c.author == "Agent"));
    Ok(())
}

/// The header is a child document: a tracked insertion made in it survives export to `<w:hdr>`
/// XML (the save round-trip), proving header redline persists like the body's.
#[test]
fn header_tracked_edit_round_trips_to_xml() -> Result<()> {
    let mut doc = CollabDoc::new();
    doc.set_header_text("Title"); // create the header child story
    let h = doc.header_doc().expect("header exists after set_header_text");
    h.suggest_insertion(0, 5, " X", "Alice", "2026-06-21T00:00:00Z", "edit header")?;

    let paras = doc.header();
    assert_eq!(paras.len(), 1);
    let xml = model::export_hdr_ftr_xml(&paras, true, &std::collections::HashMap::new());
    assert!(xml.contains("<w:hdr"), "wraps in w:hdr: {xml}");
    assert!(xml.contains("<w:ins"), "tracked insertion survives export: {xml}");
    assert!(xml.contains("Title"), "original text survives: {xml}");
    Ok(())
}

/// One peer removing a picture while another resizes it converges without a split brain: both
/// replicas agree on the paragraphs *and* the placements after a two-way merge.
#[test]
fn concurrent_image_remove_and_resize_converge() -> Result<()> {
    let a = CollabDoc::new();
    a.append_paragraph(&[plain("Fig.")], None)?;
    let png = vec![0x89u8, b'P', b'N', b'G', 1];
    let id = a.insert_image(0, 4, png, "image/png", 1000, 1000, "insert")?;
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;

    a.remove_image(id, "A removes")?; // drops the run + placement
    b.set_image_size(id, 2000, 2000, "B resizes")?;
    let (sa, sb) = (a.snapshot()?, b.snapshot()?);
    a.merge(&sb)?;
    b.merge(&sa)?;
    assert_eq!(a.paragraphs()?, b.paragraphs()?, "peers diverged on the text");
    assert_eq!(a.image_placements(), b.image_placements(), "peers diverged on placements");
    Ok(())
}

/// A tracked embedded object (an OLE `<w:object>` inside a `<w:ins>`) keeps its insertion redline on
/// import and resolves through native accept/reject like any tracked run: **accept** keeps the object
/// as a bare run (the `<w:ins>` wrapper gone), **reject** removes it entirely. See
/// `docs/passthrough.md`.
#[test]
fn tracked_embedded_object_resolves_via_accept_reject() -> Result<()> {
    let object_run = "<w:r><w:object w:dxaOrig=\"1440\" w:dyaOrig=\"1440\">\
<o:OLEObject Type=\"Embed\" ProgID=\"Excel.Sheet.12\" r:id=\"rId6\"/></w:object></w:r>";
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
xmlns:o=\"urn:schemas-microsoft-com:office:office\"><w:body>\
<w:p><w:r><w:t>Before</w:t></w:r></w:p>\
<w:p><w:ins w:id=\"7\" w:author=\"Agent\" w:date=\"2026-01-01T00:00:00Z\">{object_run}</w:ins></w:p>\
<w:p><w:r><w:t>After</w:t></w:r></w:p>\
</w:body></w:document>"
    );
    // Import: the passthrough run carries the insertion track, and export re-wraps it in `<w:ins>`.
    let doc = CollabDoc::from_document_xml(xml.as_bytes())?;
    let paras = doc.paragraphs()?;
    let raw_run = paras[1].runs.iter().find(|r| r.raw.is_some()).expect("passthrough run");
    assert!(
        matches!(raw_run.track.as_ref().map(|t| t.kind), Some(model::TrackKind::Ins)),
        "the object keeps its insertion redline"
    );
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:ins w:id=\"7\"") && out.contains(object_run), "tracked object inside w:ins:\n{out}");

    // Accept-all: the insertion is accepted -> a bare object run, no `<w:ins>`.
    let accepted = CollabDoc::from_document_xml(xml.as_bytes())?;
    accepted.accept_all("accept")?;
    let acc = accepted.to_document_xml()?;
    assert!(acc.contains(object_run), "accepted object survives:\n{acc}");
    assert!(!acc.contains("<w:ins"), "the insertion wrapper is gone after accept:\n{acc}");

    // Reject-all: the inserted object is removed entirely.
    let rejected = CollabDoc::from_document_xml(xml.as_bytes())?;
    rejected.reject_all("reject")?;
    let rej = rejected.to_document_xml()?;
    assert!(!rej.contains("<w:object"), "rejected insertion removes the object:\n{rej}");
    Ok(())
}

/// A field DELETED under Track Changes re-exports Word's structure: the markers ride inside
/// `w:del` wrappers, the instruction as `w:delInstrText` (previously unparsed, so a deleted
/// field re-exported with an EMPTY instruction), the cached result as `w:delText`, and every
/// wrapper id unique - the field markers split the source revision into fragments that all
/// reused one id (tdf70234; n830205 inflated Word's revision count 129 -> 190).
#[test]
fn deleted_field_round_trips_tracked() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Date: </w:t></w:r><w:del w:id="0" w:author="A" w:date="2019-06-03T14:31:00Z"><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:delInstrText xml:space="preserve"> DATE </w:delInstrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:delText xml:space="preserve">2019-06-03</w:delText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:del></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    assert!(
        out.contains("<w:delInstrText xml:space=\"preserve\"> DATE </w:delInstrText>"),
        "deleted instruction preserved: {out}"
    );
    assert!(out.contains("<w:delText xml:space=\"preserve\">2019-06-03</w:delText>"), "cached result kept: {out}");

    // Structure: every field marker sits inside a w:del wrapper, and all w:del ids are unique.
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(out.as_bytes());
    let mut buf = Vec::new();
    let mut del_depth = 0usize;
    let mut del_ids: Vec<String> = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).expect("well-formed XML") {
            Event::Start(e) => match e.name().as_ref() {
                b"w:del" => {
                    del_depth += 1;
                    if let Some(id) = e.attributes().flatten().find(|a| a.key.as_ref() == b"w:id") {
                        del_ids.push(String::from_utf8_lossy(&id.value).into_owned());
                    }
                }
                b"w:delInstrText" => {
                    assert!(del_depth > 0, "delInstrText outside a deletion:\n{out}");
                }
                _ => {}
            },
            Event::Empty(e) if e.name().as_ref() == b"w:fldChar" => {
                assert!(del_depth > 0, "a deleted field's fldChar outside a deletion:\n{out}");
            }
            Event::End(e) if e.name().as_ref() == b"w:del" => del_depth -= 1,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    let unique: std::collections::HashSet<_> = del_ids.iter().collect();
    assert_eq!(unique.len(), del_ids.len(), "all w:del ids unique: {del_ids:?}\n{out}");

    // Re-import: the instruction and the deleted result survive a second round-trip.
    let out2 = CollabDoc::from_document_xml(out.as_bytes())?.to_document_xml()?;
    assert!(out2.contains("<w:delInstrText xml:space=\"preserve\"> DATE </w:delInstrText>"), "{out2}");
    assert!(out2.contains(">2019-06-03<"), "{out2}");
    Ok(())
}

/// One source `w:del` wrapping several runs (formatting varies inside) must re-export as ONE
/// `w:del` - per-run wrappers duplicated the revision id N times (a document-wide uniqueness
/// violation) and inflated Word's revision count to match (tdf70234; n830205 went 129 -> 190
/// revisions in Word's own count).
#[test]
fn adjacent_same_revision_runs_share_one_wrapper() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:del w:id="5" w:author="A" w:date="2021-05-10T10:52:00Z"><w:r><w:delText xml:space="preserve">plain </w:delText></w:r><w:r><w:rPr><w:b/></w:rPr><w:delText>bold</w:delText></w:r></w:del><w:r><w:t xml:space="preserve"> after.</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    assert_eq!(out.matches("<w:del ").count(), 1, "one shared wrapper: {out}");
    assert_eq!(out.matches("<w:delText").count(), 2, "both runs inside it: {out}");
    // Stable a second time.
    let out2 = CollabDoc::from_document_xml(out.as_bytes())?.to_document_xml()?;
    assert_eq!(out, out2, "second round-trip is stable");
    Ok(())
}

/// Move range markers get their own synthesized id pair: rangeStart/rangeEnd share a fresh id
/// (from SYNTH_MARK_ID_BASE), the move wrapper keeps its revision id, and the from/to halves
/// still pair by `w:name`. Reusing the wrapper's id put three elements on one id - the
/// duplicate the validator flagged on every TC-table move doc.
#[test]
fn move_range_markers_get_distinct_ids() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:moveFromRangeStart w:id="1" w:name="m1"/><w:moveFrom w:id="2" w:author="A" w:date="D"><w:r><w:t xml:space="preserve">moved</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="1"/></w:p>
<w:p><w:moveToRangeStart w:id="3" w:name="m1"/><w:moveTo w:id="4" w:author="A" w:date="D"><w:r><w:t xml:space="preserve">moved</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="3"/><w:r><w:t xml:space="preserve"> here.</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    // Each marker pair carries one synthesized id, allocated in emission order.
    assert!(out.contains("<w:moveFromRangeStart w:id=\"900000000\""), "from start: {out}");
    assert!(out.contains("<w:moveFromRangeEnd w:id=\"900000000\""), "from end pairs with start: {out}");
    assert!(out.contains("<w:moveToRangeStart w:id=\"900000001\""), "to start: {out}");
    assert!(out.contains("<w:moveToRangeEnd w:id=\"900000001\""), "to end pairs with start: {out}");
    // The wrapper elements keep their (non-synthesized) revision ids.
    assert!(!out.contains("<w:moveFrom w:id=\"900000000\""), "wrapper id untouched: {out}");
    assert!(!out.contains("<w:moveTo w:id=\"900000001\""), "wrapper id untouched: {out}");
    Ok(())
}

/// The hyperlink edit ops: add over a range, query at a point, survive a `.docx` round-trip, and
/// remove (unmark + drop the target).
#[test]
fn add_remove_hyperlink_round_trips() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Click here please")], None)?; // "here" = chars 6..10
    let id = doc.add_hyperlink(0, 6, 10, "https://example.com/", "link")?;
    assert_eq!(
        doc.link_at(0, 7)?.map(|(_, t)| t).as_deref(),
        Some("https://example.com/"),
        "the link is queryable inside its range"
    );
    assert_eq!(doc.link_at(0, 1)?, None, "text outside the link has none");
    assert!(doc.paragraphs()?[0].runs.iter().any(|r| r.link == Some(id)), "a run carries the mark");

    // Survives a full .docx round-trip (the external rel both ways).
    let re = CollabDoc::from_docx_bytes(&doc.to_docx_bytes()?)?;
    assert_eq!(
        re.link_at(0, 7)?.map(|(_, t)| t).as_deref(),
        Some("https://example.com/"),
        "the link survives save + reopen"
    );

    // Remove it: the mark + target are gone.
    assert!(doc.remove_hyperlink(0, 7, "unlink")?, "a link was removed");
    assert_eq!(doc.link_at(0, 7)?, None, "no link after removal");
    assert!(doc.paragraphs()?[0].runs.iter().all(|r| r.link.is_none()), "no run carries a link mark");
    Ok(())
}

/// Accept/reject on the live model mirrors `scriptor_ooxml::resolve`: accept ins keeps the text
/// (drops the mark), reject ins removes it; accept del removes the text, reject del keeps it.
#[test]
fn accept_reject_resolves_tracked_changes() -> Result<()> {
    let text = |d: &CollabDoc| -> String {
        d.paragraphs().unwrap()[0].runs.iter().map(|r| r.text.clone()).collect()
    };
    let tracked = |d: &CollabDoc| -> usize {
        d.paragraphs().unwrap()[0].runs.iter().filter(|r| r.track.is_some()).count()
    };

    // Accept an insertion: text stays, mark gone.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat.")], None)?;
    let ins = doc.suggest_insertion(0, 4, "quick ", "Alice", "2026-06-20T00:00:00Z", "ins")?;
    assert_eq!(text(&doc), "The quick cat.");
    assert!(doc.accept_revision(ins, "accept")?);
    assert_eq!(text(&doc), "The quick cat.");
    assert_eq!(tracked(&doc), 0, "accepted insertion is no longer a tracked change");

    // Reject an insertion: text removed.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat.")], None)?;
    let ins = doc.suggest_insertion(0, 4, "quick ", "Alice", "2026-06-20T00:00:00Z", "ins")?;
    assert!(doc.reject_revision(ins, "reject")?);
    assert_eq!(text(&doc), "The cat.");

    // Accept a deletion: text removed. Reject a deletion: text restored (mark gone).
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let del = doc.suggest_deletion(0, 4..8, "Alice", "2026-06-20T00:00:00Z", "del")?; // "cat "
    assert_eq!(text(&doc), "The cat sat."); // retained while pending
    assert!(doc.accept_revision(del, "accept")?);
    assert_eq!(text(&doc), "The sat.");

    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let del = doc.suggest_deletion(0, 4..8, "Alice", "2026-06-20T00:00:00Z", "del")?;
    assert!(doc.reject_revision(del, "reject")?);
    assert_eq!(text(&doc), "The cat sat.");
    assert_eq!(tracked(&doc), 0);

    // accept_all clears everything; navigation + track_at find the change.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("A B C.")], None)?;
    doc.suggest_insertion(0, 0, "X", "Alice", "2026-06-20T00:00:00Z", "ins")?; // "XA B C."
    doc.suggest_deletion(0, 3..4, "Alice", "2026-06-20T00:00:00Z", "del")?; // mark "B"
    // From caret 0 (sitting on the insertion), "next" skips it and lands on the deletion.
    assert_eq!(doc.next_change(0, 0)?, Some((0, 3)), "next change after the insertion");
    assert!(doc.track_at(0, 0)?.is_some(), "caret 0 is over the insertion");
    assert_eq!(doc.accept_all("accept all")?, 2);
    assert!(doc.track_at(0, 0)?.is_none());
    Ok(())
}

/// Tracked run formatting (`w:rPrChange`): the run keeps the new formatting + records the old;
/// accept drops the mark (keeps new), reject restores the old; survives an export round-trip.
#[test]
fn tracked_formatting_resolves_and_round_trips() -> Result<()> {
    let para_text = |d: &CollabDoc| -> String {
        d.paragraphs().unwrap()[0].runs.iter().map(|r| r.text.clone()).collect()
    };

    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let id = doc.suggest_format(0, 4..7, &RunFormat::bold(true), "Alice", "2026-06-20T00:00:00Z", "fmt")?;
    // "cat" is now bold and carries a fmt_change recording it was not bold before.
    let cat = doc.paragraphs()?[0].runs.iter().find(|r| r.text == "cat").cloned().expect("cat run");
    assert!(cat.bold);
    let fc = cat.fmt_change.as_ref().expect("a tracked format change");
    assert_eq!(fc.author, "Alice");
    assert!(!fc.old.bold, "old props record it was not bold");
    assert_eq!(doc.track_at(0, 5)?.expect("over the change").track.kind, TrackKind::Fmt);

    // Survives export -> re-import (w:rPrChange round-trips).
    let doc2 = CollabDoc::from_document_xml(doc.to_document_xml()?.as_bytes())?;
    let cat2 = doc2.paragraphs()?[0].runs.iter().find(|r| r.text == "cat").cloned().expect("cat");
    assert!(cat2.bold);
    assert_eq!(cat2.fmt_change.as_ref().map(|f| f.id), Some(id));
    assert!(!cat2.fmt_change.as_ref().unwrap().old.bold);

    // Reject: restore the original (un-bold) formatting, drop the mark (runs re-merge to plain).
    assert!(doc2.reject_revision(id, "reject")?);
    let runs = doc2.paragraphs()?[0].runs.clone();
    assert!(runs.iter().all(|r| !r.bold && r.fmt_change.is_none()), "reject restored plain formatting");
    assert_eq!(para_text(&doc2), "The cat sat.");

    // Accept (fresh doc): keep the new formatting, drop the mark.
    let doc3 = CollabDoc::new();
    doc3.append_paragraph(&[plain("The cat sat.")], None)?;
    let id3 = doc3.suggest_format(0, 4..7, &RunFormat::bold(true), "Alice", "2026-06-20T00:00:00Z", "fmt")?;
    assert!(doc3.accept_revision(id3, "accept")?);
    let cat = doc3.paragraphs()?[0].runs.iter().find(|r| r.text == "cat").cloned().expect("cat");
    assert!(cat.bold && cat.fmt_change.is_none(), "accept keeps bold, drops the mark");
    Ok(())
}

/// Tracked paragraph formatting (`w:pPrChange`): the paragraph keeps the new props + records the
/// old; accept drops the change, reject restores the old; survives an export round-trip.
#[test]
fn tracked_paragraph_formatting_resolves_and_round_trips() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Para.")], None)?;
    let id = doc.suggest_paragraph_format(
        0,
        &ParaProps { align: Some(Align::Center), ..Default::default() },
        "Alice",
        "2026-06-20T00:00:00Z",
        "ppr",
    )?;
    let p = doc.paragraphs()?[0].clone();
    assert_eq!(p.props.align, Some(Align::Center));
    let pc = p.prop_change.as_ref().expect("a tracked paragraph-property change");
    assert_eq!(pc.author, "Alice");
    assert_eq!(pc.old.align, None, "old props record no alignment");
    assert_eq!(doc.track_at(0, 0)?.expect("over the paragraph").track.kind, TrackKind::Fmt);

    // Round-trips through export/import (w:pPrChange).
    let doc2 = CollabDoc::from_document_xml(doc.to_document_xml()?.as_bytes())?;
    let p2 = doc2.paragraphs()?[0].clone();
    assert_eq!(p2.props.align, Some(Align::Center));
    assert_eq!(p2.prop_change.as_ref().map(|c| c.id), Some(id));
    assert_eq!(p2.prop_change.as_ref().unwrap().old.align, None);

    // Reject: restore the original alignment (none), drop the change.
    assert!(doc2.reject_revision(id, "reject")?);
    let p3 = doc2.paragraphs()?[0].clone();
    assert_eq!(p3.props.align, None, "reject restored the original alignment");
    assert!(p3.prop_change.is_none());

    // Accept (fresh doc): keep the new alignment, drop the change.
    let doc3 = CollabDoc::new();
    doc3.append_paragraph(&[plain("Para.")], None)?;
    let id3 = doc3.suggest_paragraph_format(
        0,
        &ParaProps { align: Some(Align::Right), ..Default::default() },
        "Alice",
        "2026-06-20T00:00:00Z",
        "ppr",
    )?;
    assert!(doc3.accept_revision(id3, "accept")?);
    let p = doc3.paragraphs()?[0].clone();
    assert_eq!(p.props.align, Some(Align::Right));
    assert!(p.prop_change.is_none(), "accept keeps the new props, drops the change");
    Ok(())
}

/// Tracked paragraph-mark revisions: a tracked split records an inserted ¶ (reject merges back,
/// accept keeps the split); a tracked join records a deleted ¶ non-destructively (accept merges,
/// reject keeps separate). Both round-trip through OOXML (`w:pPr/w:rPr/w:ins|w:del`).
#[test]
fn tracked_paragraph_marks_resolve_and_round_trip() -> Result<()> {
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };

    // Tracked split at codepoint 8 of "The cat sat." -> two paragraphs; first carries inserted ¶.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let id = doc.suggest_split(0, 8, "Alice", "2026-06-20T00:00:00Z", "split")?;
    assert_eq!(texts(&doc), ["The cat ", "sat."]);
    let m = doc.paragraphs()?[0].mark_change.clone().expect("inserted ¶ on the first paragraph");
    assert_eq!(m.kind, TrackKind::Ins);
    assert_eq!(m.id, id);
    assert_eq!(doc.track_at(0, 8)?.expect("at the ¶").track.kind, TrackKind::Ins);

    // Round-trips through OOXML (w:pPr/w:rPr/w:ins).
    let doc2 = CollabDoc::from_document_xml(doc.to_document_xml()?.as_bytes())?;
    assert_eq!(texts(&doc2), ["The cat ", "sat."]);
    assert_eq!(
        doc2.paragraphs()?[0].mark_change.as_ref().map(|m| m.kind),
        Some(TrackKind::Ins)
    );

    // Reject the inserted ¶ -> the split is undone (paragraphs merge back).
    assert!(doc2.reject_revision(id, "reject")?);
    assert_eq!(texts(&doc2), ["The cat sat."]);
    assert!(doc2.paragraphs()?[0].mark_change.is_none());

    // Accept the inserted ¶ (fresh) -> split stays, mark dropped.
    let doc3 = CollabDoc::new();
    doc3.append_paragraph(&[plain("The cat sat.")], None)?;
    let id3 = doc3.suggest_split(0, 8, "Alice", "2026-06-20T00:00:00Z", "split")?;
    assert!(doc3.accept_revision(id3, "accept")?);
    assert_eq!(texts(&doc3), ["The cat ", "sat."]);
    assert!(doc3.paragraphs()?[0].mark_change.is_none());

    // Tracked join (Backspace at start of "Second") -> NOT merged; "First" gets a deleted ¶.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("First")], None)?;
    doc.append_paragraph(&[plain("Second")], None)?;
    let caret = doc.suggest_join(1, "Alice", "2026-06-20T00:00:00Z", "join")?.expect("join applied");
    assert_eq!(caret, 5, "caret lands at the end of the previous paragraph");
    assert_eq!(texts(&doc), ["First", "Second"], "non-destructive: paragraphs stay separate");
    let jid = doc.paragraphs()?[0].mark_change.clone().expect("deleted ¶").id;
    assert_eq!(doc.paragraphs()?[0].mark_change.as_ref().unwrap().kind, TrackKind::Del);

    // Accept the deleted ¶ -> the paragraphs merge.
    assert!(doc.accept_revision(jid, "accept")?);
    assert_eq!(texts(&doc), ["FirstSecond"]);

    // Reject a deleted ¶ -> stays separate, mark dropped.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("First")], None)?;
    doc.append_paragraph(&[plain("Second")], None)?;
    doc.suggest_join(1, "Alice", "2026-06-20T00:00:00Z", "join")?;
    let jid = doc.paragraphs()?[0].mark_change.clone().unwrap().id;
    assert!(doc.reject_revision(jid, "reject")?);
    assert_eq!(texts(&doc), ["First", "Second"]);
    assert!(doc.paragraphs()?[0].mark_change.is_none());
    Ok(())
}

/// Insert / resize / crop / remove a picture as live edits: insert anchors an editable run + ships
/// the bytes as a fresh media part on save; resize + crop update the placement; remove drops the run
/// and the placement (images-editing P1d).
#[test]
fn insert_resize_crop_remove_image() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Caption.")], None)?;
    // Insert a PNG at the end of the paragraph (offset 8 = after "Caption.").
    let png = vec![0x89u8, b'P', b'N', b'G', 9, 9, 9];
    let id = doc.insert_image(0, 8, png.clone(), "image/png", 914400, 685800, "insert image")?;
    let p = doc.image_placement(id).expect("placement");
    assert!(p.media.starts_with("word/media/image") && p.media.ends_with(".png"));
    assert_eq!((p.w_emu, p.h_emu), (914400, 685800));
    // The placeholder run carries the image; the paragraph text gained one object char.
    assert!(doc.paragraphs()?[0].runs.iter().any(|r| r.image == Some(id)));

    // Resize + crop are live edits on the placement.
    assert!(doc.set_image_size(id, 100, 200, "resize")?);
    assert!(doc.set_image_crop(id, 5000, 0, 5000, 0, "crop")?);
    let p = doc.image_placement(id).unwrap();
    assert_eq!((p.w_emu, p.h_emu, p.crop_l, p.crop_r), (100, 200, 5000, 5000));

    // Reset Crop clears srcRect and grows the extent back (90% wide -> /0.9); a second call no-ops.
    assert!(doc.reset_image_crop(id, "reset crop")?);
    let p = doc.image_placement(id).unwrap();
    assert_eq!((p.crop_l, p.crop_t, p.crop_r, p.crop_b), (0, 0, 0, 0));
    assert_eq!((p.w_emu, p.h_emu), (111, 200)); // 100 / 0.9, height unchanged (no vertical crop)
    assert!(!doc.reset_image_crop(id, "noop")?, "no-op when already uncropped");
    // Re-crop so the rest of the test (save/reopen) exercises a cropped picture as before.
    assert!(doc.set_image_crop(id, 5000, 0, 5000, 0, "re-crop")?);
    assert!(doc.set_image_size(id, 100, 200, "re-resize")?);

    // Positioning is rejected while inline, accepted once floating; it sets the offset origin and
    // clears any alignment.
    assert!(!doc.set_image_position(id, "page", 10, "page", 20, "move")?, "no position while inline");
    assert!(doc.set_image_floating(id, true, "square", false, "float")?);
    assert!(doc.set_image_position(id, "page", 914400, "margin", 457200, "move")?);
    let p = doc.image_placement(id).unwrap();
    assert!(p.floating && p.wrap == "square");
    assert_eq!((p.h_from.as_str(), p.x_emu, p.v_from.as_str(), p.y_emu), ("page", 914400, "margin", 457200));
    assert!(p.h_align.is_empty() && p.v_align.is_empty(), "offset clears alignment");

    // Save ships the inserted bytes + the blip rel; reopening finds the picture.
    let saved = doc.to_docx_bytes()?;
    let parts = scriptor_ooxml::read_parts_bytes(&saved)?;
    assert!(parts.iter().any(|pt| pt.name == p.media && pt.data == png), "inserted media bytes shipped");
    let reopened = CollabDoc::from_docx_bytes(&saved)?;
    assert!(reopened.paragraphs()?[0].runs.iter().any(|r| r.image.is_some()), "picture survived save+reopen");

    // Remove drops the run + the placement.
    assert!(doc.remove_image(id, "remove")?);
    assert!(doc.image_placement(id).is_none());
    assert!(doc.paragraphs()?[0].runs.iter().all(|r| r.image.is_none()));
    assert_eq!(doc.paragraphs()?[0].runs.iter().map(|r| r.text.as_str()).collect::<String>(), "Caption.");
    Ok(())
}

/// A picture inserted under Track Changes is a tracked insertion on its run (`w:ins` wrapping the
/// drawing); rejecting it removes the run, and a gc pass drops the now-anchorless placement. A
/// picture deleted under Track Changes is a tracked deletion (the run retained); accepting it
/// removes the run. The redline round-trips through document.xml.
#[test]
fn tracked_insert_and_remove_image() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Figure.")], None)?;
    let png = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3];
    let img_run = |d: &CollabDoc, id: u64| {
        d.paragraphs().unwrap()[0].runs.iter().find(|r| r.image == Some(id)).cloned()
    };

    // Insert under Track Changes: the placeholder run carries the img anchor *and* an Ins track.
    let id =
        doc.suggest_insert_image(0, 7, png.clone(), "image/png", 914400, 685800, "Ann", "D", "ins")?;
    let track = img_run(&doc, id).and_then(|r| r.track).expect("tracked insertion");
    assert_eq!(track.kind, TrackKind::Ins);

    // Export wraps the drawing's run in <w:ins>; a re-import keeps the Ins track on the picture.
    let xml = doc.to_document_xml()?;
    assert!(
        xml.find("<w:ins ").is_some_and(|i| i < xml.find("<w:drawing>").unwrap()),
        "w:ins wraps the drawing: {xml}"
    );
    let reopened = CollabDoc::from_document_xml(xml.as_bytes())?;
    assert!(
        reopened.paragraphs()?[0]
            .runs
            .iter()
            .any(|r| r.image.is_some() && r.track.as_ref().is_some_and(|t| t.kind == TrackKind::Ins)),
        "the Ins track survives the document.xml round-trip"
    );

    // Reject the insertion: the run is gone; gc drops the now-orphaned placement.
    assert!(doc.reject_revision(track.id, "reject")?);
    assert!(doc.paragraphs()?[0].runs.iter().all(|r| r.image.is_none()), "rejected picture removed");
    assert_eq!(doc.gc_orphan_images()?, 1, "orphan placement gc'd");
    assert!(doc.image_placement(id).is_none());

    // A baseline (untracked) picture, then a tracked deletion: the run stays, marked Del.
    let id2 = doc.insert_image(0, 7, png.clone(), "image/png", 100, 100, "base")?;
    assert!(doc.suggest_remove_image(id2, "Ann", "D", "del")?);
    let del = img_run(&doc, id2).and_then(|r| r.track).expect("tracked deletion");
    assert_eq!(del.kind, TrackKind::Del);
    let xml2 = doc.to_document_xml()?;
    assert!(
        xml2.find("<w:del ").is_some_and(|i| i < xml2.find("<w:drawing>").unwrap()),
        "w:del wraps the drawing: {xml2}"
    );

    // Accepting the deletion removes the run; gc drops the placement; the text is restored.
    assert!(doc.accept_revision(del.id, "accept")?);
    assert!(doc.paragraphs()?[0].runs.iter().all(|r| r.image.is_none()), "accepted deletion removed run");
    assert_eq!(doc.gc_orphan_images()?, 1);
    assert_eq!(
        doc.paragraphs()?[0].runs.iter().map(|r| r.text.as_str()).collect::<String>(),
        "Figure."
    );
    Ok(())
}

/// A move suggested on the live model marks the source (`w:moveFrom`) + inserts a destination
/// (`w:moveTo`) copy sharing one revision id; it round-trips through document.xml (re-paired by
/// `w:name`); accept drops the source and keeps the destination; reject restores the source and
/// drops the destination.
#[test]
fn tracked_move_resolves_and_round_trips() -> Result<()> {
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };
    let from_kind = |d: &CollabDoc, pi: usize, k: TrackKind| -> Option<u64> {
        d.paragraphs().unwrap()[pi]
            .runs
            .iter()
            .find_map(|r| r.track.as_ref().filter(|t| t.kind == k).map(|t| t.id))
    };

    // Two paragraphs; move "quick " (chars 4..10) from para 0 to the start of para 1.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The quick fox.")], None)?; // 14 chars
    doc.append_paragraph(&[plain("A dog.")], None)?;
    let id = doc.suggest_move(0, 4..10, 1, 0, "Alice", "2026-06-21T00:00:00Z", "move")?;

    // Source keeps the text (marked moveFrom); dest gains a moveTo copy; both share `id`.
    let mvf = doc.paragraphs()?[0]
        .runs
        .iter()
        .find(|r| r.track.as_ref().is_some_and(|t| t.kind == TrackKind::MoveFrom))
        .cloned()
        .expect("moveFrom run");
    assert_eq!(mvf.text, "quick ");
    assert_eq!(mvf.track.as_ref().unwrap().id, id);
    let mvt = doc.paragraphs()?[1]
        .runs
        .iter()
        .find(|r| r.track.as_ref().is_some_and(|t| t.kind == TrackKind::MoveTo))
        .cloned()
        .expect("moveTo run");
    assert_eq!(mvt.text, "quick ");
    assert_eq!(mvt.track.as_ref().unwrap().id, id, "both halves share one id");
    assert_eq!(texts(&doc), ["The quick fox.", "quick A dog."], "source retained until resolved");

    // Export emits move range markers + run wrappers; re-import re-pairs by name.
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:moveFromRangeStart"), "emits moveFromRangeStart: {out}");
    assert!(out.contains("<w:moveFrom "), "wraps source in w:moveFrom");
    assert!(out.contains("<w:moveTo "), "wraps dest in w:moveTo");
    assert!(out.contains("w:name=\"mv"), "shares a move name");
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    let from_id = from_kind(&doc2, 0, TrackKind::MoveFrom).expect("moveFrom survives");
    let to_id = from_kind(&doc2, 1, TrackKind::MoveTo).expect("moveTo survives");
    assert_eq!(from_id, to_id, "halves re-pair to one id on import");

    // Reject: restore the source (keep text, drop mark), drop the destination copy.
    doc2.reject_revision(from_id, "reject")?;
    assert_eq!(texts(&doc2), ["The quick fox.", "A dog."]);
    assert!(
        doc2.paragraphs()?.iter().all(|p| p.runs.iter().all(|r| r.track.is_none())),
        "no move marks after reject"
    );

    // Accept (fresh doc): drop the source text, keep the destination.
    let doc3 = CollabDoc::new();
    doc3.append_paragraph(&[plain("The quick fox.")], None)?;
    doc3.append_paragraph(&[plain("A dog.")], None)?;
    let id3 = doc3.suggest_move(0, 4..10, 1, 0, "Alice", "2026-06-21T00:00:00Z", "move")?;
    doc3.accept_revision(id3, "accept")?;
    assert_eq!(texts(&doc3), ["The fox.", "quick A dog."]);
    assert!(
        doc3.paragraphs()?.iter().all(|p| p.runs.iter().all(|r| r.track.is_none())),
        "no move marks after accept"
    );
    Ok(())
}

/// A Word-authored move (distinct ids on the run wrappers + range markers, paired only by a shared
/// `w:name`) imports as one move: both halves take a single canonical revision id, so accepting it
/// resolves source + destination together.
#[test]
fn move_imports_and_pairs_by_shared_name() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Keep </w:t></w:r><w:moveFromRangeStart w:id="1" w:name="m1"/><w:moveFrom w:id="2" w:author="A" w:date="D"><w:r><w:t xml:space="preserve">moved</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="1"/></w:p>
<w:p><w:moveToRangeStart w:id="3" w:name="m1"/><w:moveTo w:id="4" w:author="A" w:date="D"><w:r><w:t xml:space="preserve">moved</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="3"/><w:r><w:t xml:space="preserve"> here.</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let from_id = doc.paragraphs()?[0]
        .runs
        .iter()
        .find_map(|r| r.track.as_ref().filter(|t| t.kind == TrackKind::MoveFrom).map(|t| t.id))
        .expect("moveFrom");
    let to_id = doc.paragraphs()?[1]
        .runs
        .iter()
        .find_map(|r| r.track.as_ref().filter(|t| t.kind == TrackKind::MoveTo).map(|t| t.id))
        .expect("moveTo");
    assert_eq!(from_id, to_id, "shared w:name pairs the halves to one id");

    // Accepting drops the source, keeps the destination.
    doc.accept_revision(from_id, "accept")?;
    let texts: Vec<String> = doc
        .paragraphs()?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert_eq!(texts, ["Keep ", "moved here."]);
    Ok(())
}

/// A tracked numbering change (`w:numPr`) is recorded as a `w:pPrChange` (it reuses the
/// paragraph-property-change machinery): the old list membership is captured, it round-trips, and
/// reject restores the old numbering while accept keeps the new.
#[test]
fn tracked_numbering_change_resolves_and_round_trips() -> Result<()> {
    // A paragraph already in list #2 at level 0.
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Item")], None)?;
    doc.set_numbering(0, Some(2), Some(0), "seed")?;
    assert_eq!(doc.paragraph_format(0)?.num_id, Some(2));

    // Tracked clear: remove it from the list -> a pPrChange recording the old (list #2, level 0).
    let id = doc.suggest_numbering(0, None, None, "Alice", "2026-06-22T00:00:00Z", "unlist")?;
    let p = doc.paragraphs()?[0].clone();
    assert_eq!(p.props.num_id, None, "the new state has no list membership");
    let pc = p.prop_change.as_ref().expect("a pPrChange");
    assert_eq!(pc.id, id);
    assert_eq!(pc.old.num_id, Some(2), "old numbering recorded for reject");

    // Round-trips: export emits the pPrChange (with the old numPr); re-import keeps it.
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:pPrChange"), "pPrChange emitted: {out}");
    assert!(out.contains("<w:numId w:val=\"2\"/>"), "old numbering inside the pPrChange: {out}");
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    assert_eq!(doc2.paragraphs()?[0].props.num_id, None);
    assert_eq!(doc2.paragraphs()?[0].prop_change.as_ref().map(|c| c.old.num_id), Some(Some(2)));

    // Reject restores the list membership; accept (fresh) keeps the cleared state.
    assert!(doc2.reject_revision(id, "reject")?);
    assert_eq!(doc2.paragraph_format(0)?.num_id, Some(2), "reject restored list #2");
    assert!(doc2.paragraphs()?[0].prop_change.is_none());

    let doc3 = CollabDoc::new();
    doc3.append_paragraph(&[plain("Item")], None)?;
    doc3.set_numbering(0, Some(2), Some(0), "seed")?;
    let id3 = doc3.suggest_numbering(0, None, None, "Alice", "2026-06-22T00:00:00Z", "unlist")?;
    assert!(doc3.accept_revision(id3, "accept")?);
    assert_eq!(doc3.paragraph_format(0)?.num_id, None);
    assert!(doc3.paragraphs()?[0].prop_change.is_none());

    // The other direction: adding a list to a plain paragraph records old num_id = None.
    let doc4 = CollabDoc::new();
    doc4.append_paragraph(&[plain("Plain")], None)?;
    doc4.suggest_numbering(0, Some(3), Some(1), "Alice", "2026-06-22T00:00:00Z", "list")?;
    let p4 = doc4.paragraphs()?[0].clone();
    assert_eq!((p4.props.num_id, p4.props.num_ilvl), (Some(3), Some(1)));
    assert_eq!(p4.prop_change.as_ref().unwrap().old.num_id, None, "was not in a list before");
    Ok(())
}

/// Applying a paragraph style sets `w:pStyle`; the tracked form records a `w:pPrChange` whose old
/// style is restored on reject (it reuses the paragraph-property-change machinery).
#[test]
fn paragraph_style_set_suggest_and_reject() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Hi")], None)?;
    doc.set_paragraph_style(0, Some("Heading1"), "style")?;
    assert_eq!(doc.paragraph_style(0).as_deref(), Some("Heading1"));

    // Tracked restyle: records the old style for reject.
    let id = doc.suggest_paragraph_style(0, Some("Title"), "Alice", "D", "retitle")?;
    assert_eq!(doc.paragraph_style(0).as_deref(), Some("Title"), "new style applied");
    let pc = doc.paragraphs()?[0].prop_change.clone().expect("a pPrChange");
    assert_eq!(pc.old_style.as_deref(), Some("Heading1"), "old style recorded");
    assert!(doc.reject_revision(id, "reject")?);
    assert_eq!(doc.paragraph_style(0).as_deref(), Some("Heading1"), "reject restored the old style");
    assert!(doc.paragraphs()?[0].prop_change.is_none());
    Ok(())
}

/// A tracked move whose source spans a paragraph boundary: it lands under one revision id; accepting
/// performs the move (source removed + merged, destination keeps the moved content with its internal
/// break); rejecting restores the source and removes the destination.
#[test]
fn multi_paragraph_move_resolves_and_round_trips() -> Result<()> {
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };
    let build = || -> Result<CollabDoc> {
        let d = CollabDoc::new();
        d.append_paragraph(&[plain("AAA BBB")], None)?; // 0
        d.append_paragraph(&[plain("CCC DDD")], None)?; // 1
        d.append_paragraph(&[plain("ZZZ")], None)?; // 2
        Ok(d)
    };

    // Move "BBB" (para 0, 4..7) + the ¶ + "CCC" (para 1, 0..3) to the end of "ZZZ" (para 2, pos 3).
    let doc = build()?;
    let id = doc.suggest_move_multi(0, 4, 1, 3, 2, 3, "Agent", "2026-06-24T00:00:00Z", "reorg")?;
    assert_eq!(doc.list_changes()?.len(), 1, "one revision for the whole move");
    // Text is retained on both ends until reviewed.
    assert_eq!(texts(&doc), ["AAA BBB", "CCC DDD", "ZZZBBB", "CCC"]);

    doc.accept_revision(id, "accept")?;
    assert_eq!(texts(&doc), ["AAA  DDD", "ZZZBBB", "CCC"], "moved to the destination, source merged");

    // Reject restores the original three paragraphs.
    let doc = build()?;
    let id = doc.suggest_move_multi(0, 4, 1, 3, 2, 3, "Agent", "2026-06-24T00:00:00Z", "reorg")?;
    doc.reject_revision(id, "reject")?;
    assert_eq!(texts(&doc), ["AAA BBB", "CCC DDD", "ZZZ"], "reject restores the source");
    Ok(())
}
