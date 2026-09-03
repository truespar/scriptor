//! Stable addressing: cursors, durable node ids, outline and search.

use super::*;

/// An anchor tracks its logical position through a concurrent insertion *before* it: the integer
/// offset shifts, but the anchor still resolves to the same character. This is the property raw
/// offsets lack - and the reason the agent addresses by anchor, not by `(para, off)`.
#[test]
fn anchor_tracks_through_insertion_before_it() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let a = doc.anchor(0, 4, Side::Right)?; // the 'c' of "cat"
    doc.insert_text(0, 0, "big ", "test")?; // "big The cat sat." - shifts everything +4
    assert_eq!(doc.resolve(&a), Resolved::Live { para: 0, off: 8 });
    Ok(())
}

/// An anchor tracks back through a deletion before it.
#[test]
fn anchor_tracks_through_deletion_before_it() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The big cat sat.")], None)?;
    let a = doc.anchor(0, 8, Side::Right)?; // the 'c' of "cat"
    doc.delete_text(0, 0..4, "test")?; // drop "The " -> "big cat sat.", 'c' now at 4
    assert_eq!(doc.resolve(&a), Resolved::Live { para: 0, off: 4 });
    Ok(())
}

/// When the anchored block is removed (here joined into its predecessor), resolving reports the
/// explicit `Deleted` signal rather than silently pointing somewhere wrong.
#[test]
fn anchor_reports_deleted_when_block_removed() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("First.")], None)?;
    doc.append_paragraph(&[plain("Second.")], None)?;
    let a = doc.anchor(1, 0, Side::Right)?; // in the second paragraph
    doc.join_paragraph(1, "test")?; // merge para 1 into para 0; para-1 container is deleted
    assert_eq!(doc.resolve(&a), Resolved::Deleted);
    Ok(())
}

/// An anchor survives serialization to bytes and back (the wire path for an out-of-process
/// agent), and the decoded anchor still tracks edits made after decoding.
#[test]
fn anchor_round_trips_through_bytes() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let a = doc.anchor(0, 4, Side::Right)?;
    let back = Anchor::from_bytes(&a.to_bytes())?;
    assert_eq!(back, a);
    doc.insert_text(0, 0, "big ", "test")?;
    assert_eq!(doc.resolve(&back), Resolved::Live { para: 0, off: 8 });
    Ok(())
}

/// An anchor range keeps its span (here "cat") across an insertion before it.
#[test]
fn anchor_range_tracks_through_edit() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let r = doc.anchor_range(0, 4, 7)?; // "cat"
    doc.insert_text(0, 0, "big ", "test")?; // -> "big The cat sat."
    assert_eq!(doc.resolve_range(&r), Some((0, 8, 11)));
    Ok(())
}

/// An AnchorRange round-trips through bytes (the inline select->ask wire form)
/// and still tracks its span after a concurrent insertion before it.
#[test]
fn anchor_range_round_trips_through_bytes() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let r = doc.anchor_range(0, 4, 7)?; // "cat"
    let back = AnchorRange::from_bytes(&r.to_bytes())?;
    assert_eq!(back, r);
    doc.insert_text(0, 0, "big ", "test")?; // -> "big The cat sat."
    assert_eq!(doc.resolve_range_multi(&back), Some((0, 8, 0, 11)));
    Ok(())
}

/// `outline` windows a large body: a capped/paged request returns just that slice + the true total.
#[test]
fn outline_pages_a_large_body() -> Result<()> {
    let doc = CollabDoc::new();
    for i in 0..5 {
        doc.append_paragraph(&[plain(&format!("Paragraph {i}."))], None)?;
    }
    let snap = doc.outline(40, 1, 2)?; // window [1, 3)
    assert_eq!(snap.total, 5, "true total reported even when capped");
    assert_eq!(snap.offset, 1);
    assert_eq!(snap.nodes.len(), 2, "only the windowed nodes");
    assert_eq!(snap.nodes[0].para, 1);
    assert_eq!(snap.nodes[0].preview, "Paragraph 1.");
    Ok(())
}

/// When the exact anchored character is deleted (directly), `resolve` signals `Shifted` (re-pinned
/// to a neighbour) rather than pretending the position is `Live`.
#[test]
fn resolve_signals_a_repin_when_the_char_is_deleted() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("The cat sat.")], None)?;
    let a = doc.anchor(0, 4, Side::Right)?; // the 'c' of "cat"
    doc.delete_text(0, 4..7, "drop cat")?; // direct (non-tracked) removal of "cat"
    match doc.resolve(&a) {
        Resolved::Shifted { .. } | Resolved::Deleted => {}
        Resolved::Live { .. } => panic!("anchor over a deleted char must not report Live"),
    }
    Ok(())
}

/// find_text still locates text that is a tracked deletion (so quote-addressing works) but flags the
/// hit with `in_deletion`, while a live match is not flagged.
#[test]
fn find_text_flags_a_match_inside_a_deletion() -> Result<()> {
    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain("Keep this secret word.")], None)?;
    // Tracked-delete "secret" (codepoints 10..16).
    doc.suggest_deletion_multi(0, 10, 0, 16, "Agent", "2026-06-24T00:00:00Z", "drop")?;

    let hits = doc.find_text("secret", false)?;
    assert_eq!(hits.len(), 1, "deleted text is still searchable");
    assert!(hits[0].in_deletion, "the match is inside tracked-deleted text");

    let live = doc.find_text("Keep", false)?;
    assert!(!live[0].in_deletion, "a live match is not flagged");
    Ok(())
}
