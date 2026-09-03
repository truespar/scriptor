//! Table structure, cell properties and tracked table revisions.

use super::*;

/// The outline maps the body with stable node ids: the id tracks its block (not the shifting
/// paragraph index), and `read_node` by that id returns the verbatim text after an edit moved it.
#[test]
fn outline_node_ids_are_stable_across_edits() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("First.")], None)?;
    doc.append_paragraph(&[plain("Second.")], None)?;

    let snap = doc.outline(40, 0, 0)?;
    assert_eq!(snap.nodes.len(), 2);
    assert_eq!(snap.total, 2);
    assert_eq!(snap.nodes[0].para, 0);
    assert_eq!(snap.nodes[1].preview, "Second.");
    assert_eq!(snap.nodes[1].kind, NodeKind::Paragraph);
    let second = snap.nodes[1].node_id.clone();

    // Split para 0 -> a new block at index 1, pushing "Second." to index 2.
    doc.split_paragraph(0, 3, "split")?;
    assert_eq!(doc.node_para(&second), Some(2), "node id tracks the block, not the index");
    assert_eq!(doc.read_node(&second)?.expect("node lives").text, "Second.");
    Ok(())
}

/// `find_text` returns each occurrence with an anchor that resolves back to the match, is
/// case-insensitive by default, and the anchors keep tracking after an edit.
#[test]
fn find_text_locates_occurrences_with_stable_anchors() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat. The CAT ran.")], None)?;
    let hits = doc.find_text("cat", false)?;
    assert_eq!(hits.len(), 2, "case-insensitive finds both 'cat' and 'CAT'");
    assert_eq!((hits[0].para, hits[0].start, hits[0].end), (0, 4, 7));
    assert_eq!(doc.resolve_range(&hits[0].anchor), Some((0, 4, 7)));
    doc.insert_text(0, 0, "Note: ", "test")?; // +6 -> the match shifts but the anchor tracks
    assert_eq!(doc.resolve_range(&hits[0].anchor), Some((0, 10, 13)));
    assert_eq!(doc.find_text("cat", true)?.len(), 1, "case-sensitive finds only lowercase");
    Ok(())
}

/// Two peers concurrently insert a table row through the live op stream (not the isolated grid):
/// the peer-namespaced row ids don't collide, so both rows survive the merge and the peers converge -
/// tables are loro citizens, so a structural edit syncs like text (tables-crdt T5, live multi-party).
#[test]
fn concurrent_table_row_inserts_converge() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>End</w:t></w:r></w:p>
</w:body></w:document>"#;
    let a = CollabDoc::from_document_xml(xml)?;
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;
    assert_eq!(a.paragraphs()?, b.paragraphs()?, "B sees the table after joining");

    // Concurrent: both insert a row below the first (caret in A1 = flat index 0).
    a.insert_table_row(0, true, "A inserts row")?;
    b.insert_table_row(0, true, "B inserts row")?;

    let (sa, sb) = (a.snapshot()?, b.snapshot()?);
    a.merge(&sb)?;
    b.merge(&sa)?;

    assert_eq!(a.paragraphs()?, b.paragraphs()?, "peers did not converge");
    let tbl = a
        .body()
        .into_iter()
        .find_map(|i| if let BodyItem::Table(t) = i { Some(t) } else { None })
        .expect("a table survived");
    assert_eq!(tbl.rows.len(), 4, "both inserted rows survived (no id collision)");
    Ok(())
}

/// Edit-vs-delete convergence on the **tracked** path (the review tool's primary delete): peer A
/// suggests deleting a row while peer B edits a cell in that row concurrently. After the merge the
/// row is *kept* (the deletion is a pending suggestion, not a hard removal) and B's edit survives -
/// the design's edit-wins requirement ("don't silently discard a concurrent edit", §4) realized by
/// tracked suggestions, which coexist with edits until a human resolves them.
#[test]
fn concurrent_tracked_delete_keeps_a_concurrent_cell_edit() -> Result<()> {
    let a = CollabDoc::from_document_xml(table_2x2_doc())?;
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;
    // Flat: Intro=0, A1=1, B1=2, A2=3, B2=4, Outro=5 (row 1 = A2/B2).
    a.suggest_delete_table_row(3, "Ann", "2026-01-01T00:00:00Z", "A suggests delete row")?;
    b.insert_text(4, 2, "!", "B edits B2")?; // B2 -> B2!

    let (sa, sb) = (a.snapshot()?, b.snapshot()?);
    a.merge(&sb)?;
    b.merge(&sa)?;

    assert_eq!(a.paragraphs()?, b.paragraphs()?, "peers did not converge");
    let texts: Vec<String> =
        a.paragraphs()?.iter().map(|p| p.runs.iter().map(|r| r.text.as_str()).collect()).collect();
    assert!(texts.iter().any(|t| t == "B2!"), "concurrent cell edit was discarded: {texts:?}");
    assert!(
        a.table_changes().iter().any(|tc| tc.is_row && tc.kind == TrackKind::Del),
        "the row deletion is still pending (not a silent hard removal)"
    );
    Ok(())
}

/// A style edit on one peer reaches another over `merge` (collaboration): the override is a loro
/// op like any other, so it syncs and the effective table reconciles it in on the next read.
#[test]
fn style_edit_merges_across_peers() -> Result<()> {
    let a = CollabDoc::new();
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?; // shared baseline

    a.set_style_props("Heading1", &StyleProps { size: Some(36), color: Some("0000FF".into()), ..StyleProps::default() })?;
    b.merge(&a.snapshot()?)?;

    let h1 = b.styles().resolve(Some("Heading1"));
    assert_eq!(h1.size, Some(36), "peer B sees the edited size");
    assert_eq!(h1.color.as_deref(), Some("0000FF"), "peer B sees the edited colour");
    Ok(())
}

/// Re-merging the same bytes is a no-op (CRDT idempotence).
#[test]
fn merge_is_idempotent() -> Result<()> {
    let a = CollabDoc::new();
    a.append_paragraph(&[plain("Hello")], None)?;
    let snap = a.snapshot()?;

    let b = CollabDoc::new();
    b.merge(&snap)?;
    let once = b.paragraphs()?;
    b.merge(&snap)?;
    assert_eq!(b.paragraphs()?, once, "re-merging the same snapshot changed state");
    Ok(())
}

/// Table cells are editable flow paragraphs: they appear in the flat paragraph list in document
/// order, edit/split work inside a cell, a cross-cell join is refused, and the table survives an
/// export -> re-import round-trip.
#[test]
fn table_cells_are_editable_flow_paragraphs() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };

    let doc = CollabDoc::from_document_xml(xml)?;
    // Cell paragraphs sit in the flat list in document order (row-major), between Intro + Outro.
    assert_eq!(texts(&doc), ["Intro", "A1", "B1", "A2", "B2", "Outro"]);

    // Edit inside a cell (B1 is flat index 2).
    doc.insert_text(2, 2, "!", "edit")?;
    assert_eq!(texts(&doc)[2], "B1!");

    // Split A2 (index 3) inside its cell -> two paragraphs in the same cell.
    doc.split_paragraph(3, 1, "split")?;
    assert_eq!(texts(&doc), ["Intro", "A1", "B1!", "A", "2", "B2", "Outro"]);

    // Joining B1! (index 2) into A1 (index 1) crosses a cell boundary -> refused.
    assert_eq!(doc.join_paragraph(2, "join")?, None);
    assert_eq!(texts(&doc), ["Intro", "A1", "B1!", "A", "2", "B2", "Outro"]);

    // Joining "2" (index 4) into "A" (index 3) is within one cell -> merges back to "A2".
    assert_eq!(doc.join_paragraph(4, "join")?, Some(1));
    assert_eq!(texts(&doc), ["Intro", "A1", "B1!", "A2", "B2", "Outro"]);

    // Export reconstructs the table; re-import yields the same flat list + a 2x2 single-para table.
    let doc2 = CollabDoc::from_document_xml(doc.to_document_xml()?.as_bytes())?;
    assert_eq!(texts(&doc2), ["Intro", "A1", "B1!", "A2", "B2", "Outro"]);
    let body = doc2.body();
    let tbl = body
        .iter()
        .find_map(|i| if let BodyItem::Table(t) = i { Some(t) } else { None })
        .expect("a table survived the round-trip");
    assert_eq!(tbl.rows.len(), 2);
    assert!(tbl.rows.iter().all(|r| r.cells.len() == 2 && r.cells.iter().all(|c| c.para_count == 1)));
    Ok(())
}

/// Paragraph-property ops (numbering / alignment / style) address the **flat** `block_seq` index -
/// the same index `paragraphs()`, the caret, anchors, and the agent tools use - so they keep working
/// on a paragraph that sits AFTER a table (where the flat index and the old top-level `ordered_roots`
/// index diverge) AND can target a paragraph INSIDE a table cell. Before the fix these ops resolved
/// against `ordered_roots` (a whole table = 1 entry), so the trailing paragraph's flat index pointed
/// past the end -> "no block at index N".
#[test]
fn paragraph_property_with_table_uses_flat_index() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Intro")], None)?; // flat 0
    // A 2x2 table: header row (H1,H2) + body row (A2,B2) -> 4 cell paragraphs at flat 1..=4.
    doc.append_table(
        &[
            vec!["H1".into(), "H2".into()],
            vec!["A2".into(), "B2".into()],
        ],
        "table",
    )?;
    doc.append_paragraph(&[plain("Outro")], None)?; // the trailing paragraph

    // Flat list: Intro, H1, H2, A2, B2, Outro. The top-level `ordered_roots` index of "Outro" is 2
    // (Intro=0, table=1, Outro=2), but its FLAT index is 5 - the divergence the bug tripped over.
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };
    assert_eq!(texts(&doc), ["Intro", "H1", "H2", "A2", "B2", "Outro"]);
    let last = doc.paragraphs()?.len() - 1;
    assert_eq!(last, 5, "the trailing paragraph's flat index is 5, not the ordered_roots index 2");

    // Numbering on the trailing paragraph's FLAT index succeeds (used to fail "no block at index 5")
    // and lands on "Outro", not somewhere in the table.
    doc.set_numbering(last, Some(7), Some(0), "list")?;
    assert_eq!(doc.paragraph_format(last)?.num_id, Some(7), "numbering landed on the flat index");

    // Alignment on the same flat index lands on the same paragraph.
    doc.apply_paragraph_format(last, &ParaProps { align: Some(Align::Center), ..Default::default() }, "align")?;
    assert_eq!(doc.paragraph_format(last)?.align, Some(Align::Center));

    // Untouched neighbours: nothing leaked onto the cells (B2 = flat 4) or Intro (flat 0).
    assert_eq!(doc.paragraph_format(4)?.num_id, None, "no numbering leaked onto a cell");
    assert_eq!(doc.paragraph_format(4)?.align, None);
    assert_eq!(doc.paragraph_format(0)?.num_id, None, "no numbering leaked onto Intro");

    // A style on the flat index of a body paragraph also resolves correctly.
    doc.set_paragraph_style(last, Some("Heading1"), "style")?;
    assert_eq!(doc.paragraph_style(last).as_deref(), Some("Heading1"));

    // Survives an OOXML round-trip: the numbering / alignment on the trailing paragraph persist.
    let doc2 = CollabDoc::from_document_xml(doc.to_document_xml()?.as_bytes())?;
    assert_eq!(texts(&doc2), ["Intro", "H1", "H2", "A2", "B2", "Outro"]);
    let last2 = doc2.paragraphs()?.len() - 1;
    assert_eq!(doc2.paragraph_format(last2)?.num_id, Some(7), "numbering survived round-trip");
    assert_eq!(doc2.paragraph_format(last2)?.align, Some(Align::Center), "alignment survived round-trip");
    Ok(())
}

/// A paragraph-property op (numbering) on a flat index that points INTO a table cell sets that cell
/// paragraph's property - the documented behavior of the unified index: the property is written to the
/// SAME `{style?, text, props...}` map whether the flat index resolves to a top-level paragraph or a
/// cell paragraph, so numbering / alignment now work inside cells too. (Whether a cell paragraph
/// *renders* list glyphs is a separate layout concern; the model stores + round-trips the property.)
#[test]
fn paragraph_property_into_table_cell_sets_cell_paragraph() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Intro")], None)?; // flat 0
    doc.append_table(
        &[
            vec!["H1".into(), "H2".into()],
            vec!["A2".into(), "B2".into()],
        ],
        "table",
    )?;
    // Flat: Intro(0), H1(1), H2(2), A2(3), B2(4). Target A2 - a cell paragraph - at flat index 3.
    let cell = 3usize;
    assert_eq!(
        doc.paragraphs()?[cell].runs.iter().map(|r| r.text.as_str()).collect::<String>(),
        "A2",
        "flat index 3 is the cell paragraph A2"
    );

    doc.set_numbering(cell, Some(9), Some(1), "cell-list")?;
    let props = doc.paragraph_format(cell)?;
    assert_eq!(props.num_id, Some(9), "numbering set on the cell paragraph");
    assert_eq!(props.num_ilvl, Some(1));

    doc.apply_paragraph_format(cell, &ParaProps { align: Some(Align::Right), ..Default::default() }, "cell-align")?;
    assert_eq!(doc.paragraph_format(cell)?.align, Some(Align::Right), "alignment set on the cell paragraph");

    // The sibling cell B2 (flat 4) is untouched - the resolver targeted exactly one cell paragraph.
    assert_eq!(doc.paragraph_format(4)?.num_id, None);
    assert_eq!(doc.paragraph_format(4)?.align, None);
    Ok(())
}

/// A block-level `<w:sdt>` content control round-trips through `document.xml` **editability-preserving**:
/// its `<w:sdtPr>` control definition is re-emitted verbatim around the inner paragraph, while the
/// inner paragraph stays a normal editable body paragraph (not frozen). See `docs/passthrough.md`.
#[test]
fn block_sdt_wrapper_round_trips_and_stays_editable() -> Result<()> {
    // Verbatim opening (the control definition) + fixed close. The inner paragraph is model-serialized
    // between them (so `xml:space` etc. follow the serializer - the documented passthrough boundary).
    let prefix = "<w:sdt><w:sdtPr><w:alias w:val=\"Party\"/><w:tag w:val=\"party\"/>\
<w:id w:val=\"123\"/><w:text/></w:sdtPr><w:sdtContent>";
    let suffix = "</w:sdtContent></w:sdt>";
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>\
<w:p><w:r><w:t>Before</w:t></w:r></w:p>{prefix}<w:p><w:r><w:t>Acme Corp</w:t></w:r></w:p>{suffix}\
<w:p><w:r><w:t>After</w:t></w:r></w:p></w:body></w:document>"
    );
    let doc = CollabDoc::from_document_xml(xml.as_bytes())?;
    // The inner paragraph is editable body text at its flat index (Before=0, Acme=1, After=2).
    let paras = doc.paragraphs()?;
    assert_eq!(paras.len(), 3);
    assert_eq!(paras[1].runs.iter().map(|r| r.text.as_str()).collect::<String>(), "Acme Corp");
    assert!(paras[1].runs.iter().all(|r| r.raw.is_none()), "inner content is a real editable run, not passthrough");

    // Export re-wraps the inner paragraph: the control definition is preserved verbatim, opening just
    // before the inner paragraph and closing just after it.
    let out = doc.to_document_xml()?;
    assert!(out.contains(&format!("{prefix}<w:p>")), "sdt opens right before the inner paragraph:\n{out}");
    assert!(out.contains(&format!("</w:p>{suffix}")), "sdt closes right after the inner paragraph:\n{out}");
    assert!(out.contains("Acme Corp"), "inner content present");

    // Editing the inner text keeps the wrapper: change "Acme Corp" -> "Beta LLC".
    doc.delete_text(1, 0..9, "clear")?;
    doc.insert_text(1, 0, "Beta LLC", "type")?;
    let out2 = doc.to_document_xml()?;
    assert!(out2.contains(&format!("{prefix}<w:p>")) && out2.contains(&format!("</w:p>{suffix}")), "wrapper survives an inner edit:\n{out2}");
    assert!(out2.contains("Beta LLC") && !out2.contains("Acme Corp"), "content updated inside the wrapper");
    Ok(())
}

/// A comment range spanning two table cells re-exports with exactly ONE commentRangeStart (in
/// the first cell) and ONE commentRangeEnd + reference (in the last) - not a start+end in every
/// cell it touches. The per-cell span recomputation emitted the same comment id in each cell, a
/// document-wide uniqueness violation Word/the validator rejected (wDateValueFormat's comment 27).
#[test]
fn comment_range_spanning_cells_emits_one_pair() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr>
    <w:tc><w:p><w:commentRangeStart w:id="7"/><w:r><w:t>A1 text</w:t></w:r></w:p></w:tc>
    <w:tc><w:p><w:r><w:t>B1 text</w:t></w:r><w:commentRangeEnd w:id="7"/><w:r><w:commentReference w:id="7"/></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>
<w:p><w:r><w:t>After.</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    assert_eq!(out.matches("<w:commentRangeStart w:id=\"7\"/>").count(), 1, "one start: {out}");
    assert_eq!(out.matches("<w:commentRangeEnd w:id=\"7\"/>").count(), 1, "one end: {out}");
    assert_eq!(out.matches("<w:commentReference w:id=\"7\"/>").count(), 1, "one reference: {out}");
    // Stable a second time.
    let out2 = CollabDoc::from_document_xml(out.as_bytes())?.to_document_xml()?;
    assert_eq!(out, out2, "cross-cell comment round-trip is stable");
    Ok(())
}

/// A bookmark + internal hyperlink anchored **inside a table cell** round-trips through
/// `document.xml`: import marks the cell's grid text (the anchor's flat index descends into the
/// cell), and the node-walk export re-emits the markers from the cell runs + the document's anchor
/// maps (tables-crdt: cell-anchored field/bookmark/link export, the T2.6 deferral closed).
#[test]
fn cell_bookmark_and_hyperlink_round_trip() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:bookmarkStart w:id="5" w:name="cellmark"/><w:hyperlink w:anchor="cellmark" w:history="1"><w:r><w:t>B1</w:t></w:r></w:hyperlink><w:bookmarkEnd w:id="5"/></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    // The cell's bookmark + internal-hyperlink markers are re-emitted inside the table.
    let start = out.find("<w:tbl>").expect("table start");
    let end = out.find("</w:tbl>").expect("table end");
    let tbl = &out[start..end];
    assert!(tbl.contains("<w:bookmarkStart w:id=\"5\" w:name=\"cellmark\"/>"), "{tbl}");
    assert!(tbl.contains("<w:bookmarkEnd w:id=\"5\"/>"), "{tbl}");
    assert!(tbl.contains("<w:hyperlink w:anchor=\"cellmark\""), "{tbl}");

    // Re-import: the cell paragraph (flat index 2 = B1) still carries the link + bookmark on its run.
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    let paras = doc2.paragraphs()?;
    assert!(paras[2].runs.iter().any(|r| r.link.is_some()), "cell hyperlink survived re-import");
    assert!(paras[2].runs.iter().any(|r| r.bookmarks.contains(&5)), "cell bookmark survived re-import");
    Ok(())
}

/// Row / column move verbs reorder the grid (a `MovableList` move) and report the caret at the moved
/// cell's new flat position; moving off the table's edge is a no-op (`None`).
#[test]
fn move_table_row_and_column() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A3</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B3</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };
    let doc = CollabDoc::from_document_xml(xml)?;
    // Flat: 0=Intro,1=A1,2=B1,3=A2,4=B2,5=A3,6=B3,7=Outro.
    assert_eq!(texts(&doc), ["Intro", "A1", "B1", "A2", "B2", "A3", "B3", "Outro"]);

    // Move row 1 (A2/B2, flat 3) up -> it swaps with row 0; caret follows to A2's new spot (flat 1).
    assert_eq!(doc.move_table_row(3, true, "move row up")?, Some(1));
    assert_eq!(texts(&doc), ["Intro", "A2", "B2", "A1", "B1", "A3", "B3", "Outro"]);

    // Top row can't move up.
    assert_eq!(doc.move_table_row(1, true, "noop")?, None);

    // Move the caret cell's column (A-column, flat 1) right -> columns swap; caret follows (flat 2).
    assert_eq!(doc.move_table_column(1, false, "move col right")?, Some(2));
    assert_eq!(texts(&doc), ["Intro", "B2", "A2", "B1", "A1", "B3", "A3", "Outro"]);

    // Not in a table -> None.
    assert_eq!(doc.move_table_row(0, true, "noop")?, None);
    Ok(())
}

/// Horizontal cell merge (`w:gridSpan`): merging the caret cell with the one to its right collapses
/// the row to a single spanning `<w:tc>` (content preserved in the survivor); split restores two cells.
#[test]
fn merge_and_split_cells_horizontal() -> Result<()> {
    let doc = CollabDoc::from_document_xml(table_2x2_doc())?;
    // Flat: Intro=0, A1=1, B1=2, A2=3, B2=4, Outro=5. Merge A1 (1) with B1.
    assert_eq!(doc.merge_cells_right(1, 2, "merge")?, Some(1));
    let t = first_table(&doc);
    assert_eq!(t.rows[0].cells.len(), 1, "row 0 collapsed to one cell");
    assert_eq!(t.rows[0].cells[0].grid_span, 2);
    assert_eq!(t.rows[1].cells.len(), 2, "row 1 untouched (merge is row-local)");
    let xml = doc.to_document_xml()?;
    assert!(xml.contains("<w:gridSpan w:val=\"2\"/>"), "{xml}");
    // Content preserved - A1 and B1 both still present (B1 appended into the merged cell).
    let texts: Vec<String> =
        doc.paragraphs()?.iter().map(|p| p.runs.iter().map(|r| r.text.as_str()).collect()).collect();
    assert!(texts.contains(&"A1".to_string()) && texts.contains(&"B1".to_string()));

    // Split it back into two columns.
    assert_eq!(doc.split_cell_horizontal(1, "split")?, Some(1));
    let t = first_table(&doc);
    assert_eq!(t.rows[0].cells.len(), 2, "split restored two cells");
    assert_eq!(t.rows[0].cells[0].grid_span, 1);
    assert!(!doc.to_document_xml()?.contains("<w:gridSpan"), "no span after split");
    Ok(())
}

/// Vertical cell merge (`w:vMerge`): merging the caret cell with the one below makes the top a
/// `restart` anchor and the lower an empty `continue` placeholder; split clears both.
#[test]
fn merge_and_split_cells_vertical() -> Result<()> {
    let doc = CollabDoc::from_document_xml(table_2x2_doc())?;
    // Merge A1 (flat 1) with the cell below it (A2).
    assert_eq!(doc.merge_cells_down(1, 2, "merge")?, Some(1));
    let t = first_table(&doc);
    assert_eq!(t.rows[0].cells[0].vmerge, model::VMerge::Restart);
    assert_eq!(t.rows[1].cells[0].vmerge, model::VMerge::Continue);
    let xml = doc.to_document_xml()?;
    assert!(xml.contains("<w:vMerge w:val=\"restart\"/>"), "{xml}");
    assert!(xml.contains("<w:vMerge/>"), "continue placeholder: {xml}");

    // Split the vertical merge.
    assert_eq!(doc.split_cell_vertical(1, "split")?, Some(1));
    let t = first_table(&doc);
    assert_eq!(t.rows[0].cells[0].vmerge, model::VMerge::None);
    assert_eq!(t.rows[1].cells[0].vmerge, model::VMerge::None);
    assert!(!doc.to_document_xml()?.contains("<w:vMerge"), "no vMerge after split");
    Ok(())
}

/// Each table-cell paragraph gets its own durable [`NodeId`] (its text container id, distinct per
/// cell), so the agent's outline distinguishes cells, `read_node` fetches the right one, and the id
/// survives edits elsewhere + round-trips through its `cid:` string form.
#[test]
fn cell_paragraphs_have_distinct_durable_node_ids() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    // Flat: 0=Intro, 1=A1, 2=B1, 3=Outro.
    let snap = doc.outline(40, 0, 0)?;
    assert_eq!(snap.nodes.len(), 4);
    let ids: Vec<String> = snap.nodes.iter().map(|n| n.node_id.to_string()).collect();
    let uniq: HashSet<&String> = ids.iter().collect();
    assert_eq!(uniq.len(), 4, "all four paragraphs have distinct node ids: {ids:?}");
    // Cell paragraphs are addressed by container id (`cid:`); body paragraphs by tree id.
    assert!(ids[1].starts_with("cid:") && ids[2].starts_with("cid:"), "cells use container ids: {ids:?}");
    assert!(!ids[0].starts_with("cid:") && !ids[3].starts_with("cid:"), "body paras use tree ids: {ids:?}");

    // read_node of the B1 cell id fetches B1 specifically (not the table's first cell).
    let b1 = doc.node_id(2).expect("B1 node id");
    assert_eq!(doc.node_para(&b1), Some(2));
    assert_eq!(doc.read_node(&b1)?.expect("B1 content").text, "B1");

    // The cell node id round-trips through its string form.
    let parsed: NodeId = ids[2].parse().expect("cell node id parses");
    assert_eq!(doc.node_para(&parsed), Some(2));

    // Durable across an edit elsewhere: insert into A1 (flat 1); B1's id still resolves to B1.
    doc.insert_text(1, 2, "!", "edit A1")?;
    assert_eq!(doc.node_para(&b1), Some(2));
    assert_eq!(doc.read_node(&b1)?.expect("B1 still there").text, "B1");
    Ok(())
}

/// Tab / Shift+Tab cell navigation: `cell_step` walks cells in reading order (across a row, then to
/// the next row), and stops at the table's edges + outside any table.
#[test]
fn cell_step_walks_cells_in_reading_order() -> Result<()> {
    // Flat layout: 0=Intro, 1=A1, 2=B1, 3=A2, 4=B2, 5=Outro (2x2 table between two body paragraphs).
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    // Forward across the row, then wrap to the next row's first cell.
    assert_eq!(doc.cell_step(1, true), Some(2), "A1 -> B1");
    assert_eq!(doc.cell_step(2, true), Some(3), "B1 -> A2 (next row)");
    assert_eq!(doc.cell_step(4, true), None, "B2 is the last cell");
    // Backward mirrors it.
    assert_eq!(doc.cell_step(2, false), Some(1), "B1 -> A1");
    assert_eq!(doc.cell_step(3, false), Some(2), "A2 -> B1 (previous row)");
    assert_eq!(doc.cell_step(1, false), None, "A1 is the first cell");
    // A body paragraph outside the table has no cell step.
    assert_eq!(doc.cell_step(0, true), None, "Intro isn't in a cell");
    Ok(())
}

/// Insert/delete row + column ops keep the flat paragraph list, the table structure, and the grid
/// consistent, and survive an export round-trip.
#[test]
fn table_row_and_column_ops() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let dims = |d: &CollabDoc, p: usize| d.table_context(p).map(|t| (t.2, t.3));
    assert_eq!(doc.paragraphs()?.len(), 6);
    assert_eq!(doc.table_context(1), Some((0, 0, 2, 2))); // A1: row0 col0, 2x2

    // Insert a row below row 0 -> 3x2, two new empty cells.
    let c = doc.insert_table_row(1, true, "ir")?.expect("in table");
    assert_eq!(doc.paragraphs()?.len(), 8);
    assert!(doc.paragraphs()?[c].runs.is_empty(), "new cell starts empty");
    assert_eq!(dims(&doc, c), Some((3, 2)));

    // Insert a column to the right of the caret cell -> 3x3, one new cell per row.
    let c = doc.insert_table_column(c, true, "ic")?.expect("in table");
    assert_eq!(doc.paragraphs()?.len(), 11);
    assert_eq!(dims(&doc, c), Some((3, 3)));

    // Delete that column -> 3x2.
    let c = doc.delete_table_column(c, "dc")?.expect("in table");
    assert_eq!(doc.paragraphs()?.len(), 8);
    assert_eq!(dims(&doc, c), Some((3, 2)));

    // Delete the inserted row -> back to the original 2x2.
    let c = doc.delete_table_row(c, "dr")?.expect("in table");
    assert_eq!(doc.paragraphs()?.len(), 6);
    assert_eq!(dims(&doc, c), Some((2, 2)));

    // The original cell text is intact and the table round-trips through export.
    let texts: Vec<String> = doc
        .paragraphs()?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert_eq!(texts, ["Intro", "A1", "B1", "A2", "B2", "Outro"]);
    let doc2 = CollabDoc::from_document_xml(doc.to_document_xml()?.as_bytes())?;
    assert_eq!(doc2.paragraphs()?.len(), 6);
    let body = doc2.body();
    let tbl = body
        .iter()
        .find_map(|i| if let BodyItem::Table(t) = i { Some(t) } else { None })
        .expect("table survived");
    assert_eq!(tbl.rows.len(), 2);
    assert_eq!(tbl.col_widths.len(), 2);
    assert!(tbl.rows.iter().all(|r| r.cells.len() == 2));
    Ok(())
}

/// A tracked insertion imported by one peer survives a concurrent edit + merge from another.
#[test]
fn tracked_changes_survive_merge() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:ins w:id="3" w:author="Agent" w:date="2026-06-17T00:00:00Z"><w:r><w:t xml:space="preserve">suggested</w:t></w:r></w:ins></w:p>
</w:body></w:document>"#;

    let a = CollabDoc::from_document_xml(xml)?;
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;

    // Concurrent edit on B; A stays put.
    b.append_paragraph(&[plain("review note")], None)?;
    a.merge(&b.snapshot()?)?;

    let para = &a.paragraphs()?[0];
    let track = para.runs[0].track.clone().expect("tracked insertion lost across merge");
    assert_eq!(track.kind, TrackKind::Ins);
    assert_eq!(track.author, "Agent");
    assert_eq!(track.id, 3);
    Ok(())
}

/// Tracked table-structure revisions: a tracked row / column delete marks (`w:trPr/del`,
/// `w:tcPr/cellDel`) without removing; a tracked row / column insert adds + marks (`.../ins`,
/// `cellIns`); all round-trip through document.xml; accept applies + reject reverts on the body.
#[test]
fn tracked_table_structure_resolves_and_round_trips() -> Result<()> {
    const XML: &[u8] = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
</w:body></w:document>"#;
    let texts = |d: &CollabDoc| -> Vec<String> {
        d.paragraphs()
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };

    // Row delete: marked + non-destructive; round-trips; reject keeps the row.
    let doc = CollabDoc::from_document_xml(XML)?;
    doc.suggest_delete_table_row(2, "Alice", "D", "del row")?; // caret in A2 (the second row)
    assert_eq!(texts(&doc), ["A1", "B1", "A2", "B2"], "row retained until accepted");
    let tc = doc.table_changes();
    assert_eq!(tc.len(), 1);
    assert!(tc[0].is_row && tc[0].kind == TrackKind::Del);
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:trPr><w:del "), "row deletion marker emitted: {out}");
    assert_eq!(
        CollabDoc::from_document_xml(out.as_bytes())?.table_changes().len(),
        1,
        "row revision survives round-trip"
    );
    doc.reject_revision(tc[0].id, "reject")?;
    assert!(doc.table_changes().is_empty(), "reject dropped the row mark");
    assert_eq!(texts(&doc), ["A1", "B1", "A2", "B2"]);

    // Row delete accepted -> the row + its cells are removed.
    let doc = CollabDoc::from_document_xml(XML)?;
    doc.suggest_delete_table_row(2, "Alice", "D", "del row")?;
    let id = doc.table_changes()[0].id;
    doc.accept_revision(id, "accept")?;
    assert!(doc.table_changes().is_empty());
    assert_eq!(texts(&doc), ["A1", "B1"], "accept removed the second row");

    // Row insert (tracked): a new row is added + marked; reject removes it.
    let doc = CollabDoc::from_document_xml(XML)?;
    doc.suggest_insert_table_row(0, true, "Alice", "D", "ins row")?; // below the first row
    assert_eq!(doc.paragraphs()?.len(), 6, "two empty cells added for the new row");
    let tc = doc.table_changes();
    assert!(tc.len() == 1 && tc[0].is_row && tc[0].kind == TrackKind::Ins);
    assert!(doc.to_document_xml()?.contains("<w:trPr><w:ins "), "row insertion marker emitted");
    doc.reject_revision(tc[0].id, "reject")?;
    assert_eq!(texts(&doc), ["A1", "B1", "A2", "B2"], "reject removed the inserted row");

    // Column delete (tracked): every cell of the column is marked under one id; accept removes it.
    let doc = CollabDoc::from_document_xml(XML)?;
    doc.suggest_delete_table_column(1, "Alice", "D", "del col")?; // caret in B1 (the second column)
    assert_eq!(texts(&doc), ["A1", "B1", "A2", "B2"], "cells retained until accepted");
    let tc = doc.table_changes();
    assert_eq!(tc.len(), 1, "a column is one revision (its cells share an id)");
    assert!(!tc[0].is_row && tc[0].kind == TrackKind::Del);
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:cellDel "), "cell deletion marker emitted: {out}");
    assert_eq!(
        CollabDoc::from_document_xml(out.as_bytes())?.table_changes().len(),
        1,
        "column revision survives round-trip"
    );
    doc.accept_revision(tc[0].id, "accept")?;
    assert!(doc.table_changes().is_empty());
    assert_eq!(texts(&doc), ["A1", "A2"], "accept removed the B column");

    Ok(())
}

/// Tracked table-PROPERTY revisions (`w:tcPrChange` / `w:trPrChange` / `w:tblPrChange`): a tracked
/// cell-shading / row-height / table-border change records the old props, lists as one change,
/// round-trips through document.xml, and accept keeps the new props while reject restores the old.
#[test]
fn tracked_table_property_changes_resolve_and_round_trip() -> Result<()> {
    const XML: &[u8] = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
</w:body></w:document>"#;
    let first_table = |d: &CollabDoc| -> Table {
        d.body()
            .into_iter()
            .find_map(|it| match it {
                BodyItem::Table(t) => Some(*t),
                _ => None,
            })
            .expect("a table")
    };

    // ── Cell shading (w:tcPrChange) ──
    let doc = CollabDoc::from_document_xml(XML)?;
    assert!(doc.suggest_cell_shading(0, Some("FFFF00".into()), "Alice", "D", "shade A1")?);
    assert_eq!(doc.cell_shading(0).as_deref(), Some("FFFF00"), "new shading applied");
    let tc = doc.table_changes();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].prop_level, Some(TablePropLevel::Cell));
    match &first_table(&doc).rows[0].cells[0].prop_change.as_ref().unwrap().old {
        TablePropSnapshot::Cell { shading, .. } => assert_eq!(shading.as_deref(), None),
        _ => panic!("expected a cell snapshot"),
    }
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:tcPrChange"), "tcPrChange emitted: {out}");
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    assert_eq!(doc2.cell_shading(0).as_deref(), Some("FFFF00"), "new shading survives round-trip");
    assert_eq!(doc2.table_changes().len(), 1, "the cell-property revision survives round-trip");
    let id = doc2.table_changes()[0].id;
    assert!(doc2.reject_revision(id, "reject")?);
    assert_eq!(doc2.cell_shading(0), None, "reject restored the old (no) shading");
    assert!(doc2.table_changes().is_empty());
    let doc3 = CollabDoc::from_document_xml(out.as_bytes())?;
    let id3 = doc3.table_changes()[0].id;
    assert!(doc3.accept_revision(id3, "accept")?);
    assert_eq!(doc3.cell_shading(0).as_deref(), Some("FFFF00"), "accept kept the new shading");
    assert!(doc3.table_changes().is_empty());

    // ── Row height (w:trPrChange) ──
    let doc = CollabDoc::from_document_xml(XML)?;
    assert!(doc.suggest_row_height(0, Some(600), true, "Alice", "D", "row h")?);
    assert_eq!(first_table(&doc).rows[0].height, Some(600), "new height applied");
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:trPrChange"), "trPrChange emitted: {out}");
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    assert_eq!(first_table(&doc2).rows[0].height, Some(600), "height survives round-trip");
    let id = doc2.table_changes()[0].id;
    assert!(doc2.reject_revision(id, "reject")?);
    assert_eq!(first_table(&doc2).rows[0].height, None, "reject restored the old (no) height");

    // ── Table borders (w:tblPrChange) ──
    let doc = CollabDoc::from_document_xml(XML)?;
    let border = Border { size_eighths: 8, color: "FF0000".into() };
    assert!(doc.suggest_table_borders(0, Some(border), "Alice", "D", "borders")?);
    assert!(first_table(&doc).borders.top.is_some(), "new border applied");
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:tblPrChange"), "tblPrChange emitted: {out}");
    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    assert!(first_table(&doc2).borders.top.is_some(), "borders survive round-trip");
    let id = doc2.table_changes()[0].id;
    assert!(doc2.reject_revision(id, "reject")?);
    assert!(first_table(&doc2).borders.top.is_none(), "reject restored the old (no) borders");

    Ok(())
}

/// `append_table` builds a `<w:tbl>` whose header row is bold and whose every cell text survives a
/// full `.docx` save + reopen: the cell paragraphs interleave into the flat `paragraphs()` list, so
/// the exported document carries "A","B","1","2" in row-major order, and the header runs are bold.
#[test]
fn append_table_round_trips() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_table(
        &[
            vec!["A".into(), "B".into()],
            vec!["1".into(), "2".into()],
        ],
        "test",
    )?;

    let bytes = doc.to_docx_bytes()?;
    let reopened = CollabDoc::from_docx_bytes(&bytes)?;
    let paras = reopened.paragraphs()?;
    // The four cell paragraphs interleave into the flat list, row-major.
    let texts: Vec<String> =
        paras.iter().map(|p| p.runs.iter().map(|r| r.text.as_str()).collect()).collect();
    for want in ["A", "B", "1", "2"] {
        assert!(texts.iter().any(|t| t == want), "cell text {want:?} present after round-trip: {texts:?}");
    }
    // The header row's cells are bold; the body row's are not.
    let bold_of = |cell: &str| -> bool {
        paras
            .iter()
            .find(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>() == cell)
            .map(|p| p.runs.iter().all(|r| r.bold))
            .unwrap_or(false)
    };
    assert!(bold_of("A") && bold_of("B"), "header cells are bold");
    assert!(!bold_of("1") && !bold_of("2"), "body cells are not bold");
    Ok(())
}
