//! Threaded comments and their anchors.

use super::*;

/// A comment added on the live model survives a full `.docx` save + reopen: body, author,
/// initials, resolved state, threading (reply -> parent), and the anchored range (as
/// `Run.comments`) all round-trip via comments.xml / commentsExtended.xml / the document.xml
/// commentRange markers. Delete removes the whole thread + scrubs the anchor.
#[test]
fn comments_round_trip_through_docx() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The quick brown fox.")], None)?; // 20 chars
    // Comment on "quick" (chars 4..9).
    let id =
        doc.add_comment(0, 4, 0, 9, "is it?", "Alice Author", "2026-06-21T00:00:00Z", "comment")?;
    let rid = doc.reply_comment(id, "yes", "Reviewer", "2026-06-21T01:00:00Z", "reply")?;
    assert_ne!(id, rid, "the reply takes a fresh id from the shared pool");
    doc.set_comment_resolved(id, true, "resolve")?;

    let bytes = doc.to_docx_bytes()?;
    let reopened = CollabDoc::from_docx_bytes(&bytes)?;

    let comments = reopened.comments();
    assert_eq!(comments.len(), 2, "comment + reply round-trip");
    let c = comments.iter().find(|c| c.id == id).expect("the comment");
    assert_eq!(c.text, "is it?");
    assert_eq!(c.author, "Alice Author");
    assert_eq!(c.initials, "AA");
    assert!(c.resolved, "resolved state round-trips");
    let r = comments.iter().find(|c| c.id == rid).expect("the reply");
    assert_eq!(r.parent, Some(id), "threading round-trips");
    assert_eq!(r.text, "yes");

    // The anchored range survives: the run covering "quick" carries the comment id.
    let anchored: String = reopened.paragraphs()?[0]
        .runs
        .iter()
        .filter(|r| r.comments.contains(&id))
        .map(|r| r.text.clone())
        .collect();
    assert_eq!(anchored, "quick", "the anchored range round-trips");
    assert!(reopened.comments_at(0, 5)?.contains(&id), "comments_at finds it under the caret");

    // Delete removes the whole thread + scrubs the anchor marks.
    assert_eq!(reopened.delete_comment(id, "del")?, 2);
    assert!(reopened.comments().is_empty());
    assert!(
        reopened.paragraphs()?[0].runs.iter().all(|r| r.comments.is_empty()),
        "anchor marks cleared on delete"
    );
    Ok(())
}

/// Importing document.xml with comment range markers anchors the right runs, export re-emits the
/// markers, and a re-import preserves the anchor (semantic round-trip of the anchor alone).
#[test]
fn comment_anchor_imports_and_round_trips_in_document_xml() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:commentRangeStart w:id="5"/><w:r><w:t xml:space="preserve">world</w:t></w:r><w:commentRangeEnd w:id="5"/><w:r><w:commentReference w:id="5"/></w:r><w:r><w:t xml:space="preserve">!</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let anchored: String = doc.paragraphs()?[0]
        .runs
        .iter()
        .filter(|r| r.comments.contains(&5))
        .map(|r| r.text.clone())
        .collect();
    assert_eq!(anchored, "world");
    let out = doc.to_document_xml()?;
    assert!(out.contains("<w:commentRangeStart w:id=\"5\"/>"), "export re-emits the start marker");
    assert!(out.contains("<w:commentReference w:id=\"5\"/>"), "export re-emits the reference");

    let doc2 = CollabDoc::from_document_xml(out.as_bytes())?;
    let anchored2: String = doc2.paragraphs()?[0]
        .runs
        .iter()
        .filter(|r| r.comments.contains(&5))
        .map(|r| r.text.clone())
        .collect();
    assert_eq!(anchored2, "world", "anchor survives the document.xml round-trip");
    Ok(())
}

/// comment_locations reports each comment's anchored codepoint span (here a single-paragraph range).
#[test]
fn comment_locations_report_anchor_spans() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat on the mat.")], None)?;
    let id = doc.add_comment(0, 4, 0, 7, "which cat?", "Alice", "2026-06-24T00:00:00Z", "c")?; // "cat"

    let locs = doc.comment_locations()?;
    let loc = locs.iter().find(|l| l.id == id).expect("comment located");
    assert_eq!((loc.start_para, loc.start_off, loc.end_para, loc.end_off), (0, 4, 0, 7));
    Ok(())
}
