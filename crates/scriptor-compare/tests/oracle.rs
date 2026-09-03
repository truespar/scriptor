//! The correctness oracle: for any pair (A, B), `compare(A, B)` then accept-all must reproduce B and
//! reject-all must reproduce A, text-stable. This is the property the whole engine is built to
//! satisfy - a mechanical completeness guarantee no hand-tuned redline can offer. Every case below
//! is a distinct structural shape (inline edit, whole-paragraph insert/delete at each boundary, a
//! dissimilar replacement, and mixtures).

use scriptor_compare::{compare, CompareOptions};
use scriptor_crdt::{CollabDoc, Run};

/// Build a `.docx` from a list of paragraph texts (one plain run each).
fn docx(paras: &[&str]) -> Vec<u8> {
    let doc = CollabDoc::new();
    for p in paras {
        doc.append_paragraph(&[Run::plain(*p)], None).unwrap();
    }
    doc.to_docx_bytes().unwrap()
}

/// A `.docx` from raw body XML - for structures `docx()` can't express (e.g. a block `<w:sdt>`).
fn docx_body(body: &str) -> Vec<u8> {
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}</w:body></w:document>"
    );
    CollabDoc::from_document_xml(xml.as_bytes()).unwrap().to_docx_bytes().unwrap()
}

/// Assert the oracle holds for A -> B, and return the change count (so cases can also assert the
/// diff was non-trivial where expected).
fn oracle(a: &[&str], b: &[&str]) -> usize {
    let opts = CompareOptions::default();
    let rep = scriptor_compare::check(&docx(a), &docx(b), &opts).unwrap();
    assert!(
        rep.ok(),
        "oracle failed for {a:?} -> {b:?}\n  accept_ok={} reject_ok={}\n  accept_mismatch={:?}\n  reject_mismatch={:?}",
        rep.accept_ok,
        rep.reject_ok,
        rep.accept_mismatch,
        rep.reject_mismatch,
    );
    rep.changes
}

#[test]
fn identical_documents_produce_no_changes() {
    let d = ["Alpha paragraph.", "Beta paragraph.", "Gamma paragraph."];
    assert_eq!(oracle(&d, &d), 0);
}

/// The oracle still holds when the documents carry a block-level `<w:sdt>` content control: the inner
/// paragraph is compared like any other (the wrapper rides along on each doc and re-emits), so an edit
/// inside the control redlines and resolves cleanly. Regression guard for the editability-preserving
/// wrapper model (see `docs/passthrough.md`).
#[test]
fn oracle_holds_with_a_block_sdt_content_control() {
    let doc = |name: &str| {
        format!(
            "<w:p><w:r><w:t>Recital.</w:t></w:r></w:p>\
<w:sdt><w:sdtPr><w:tag w:val=\"party\"/></w:sdtPr><w:sdtContent>\
<w:p><w:r><w:t>{name}</w:t></w:r></w:p></w:sdtContent></w:sdt>\
<w:p><w:r><w:t>Signatures.</w:t></w:r></w:p>"
        )
    };
    let a = docx_body(&doc("Acme Corporation"));
    let b = docx_body(&doc("Beta Limited"));
    let rep = scriptor_compare::check(&a, &b, &CompareOptions::default()).unwrap();
    assert!(rep.ok(), "oracle failed with an sdt present: accept_ok={} reject_ok={}", rep.accept_ok, rep.reject_ok);
    assert!(rep.changes >= 1, "the party-name edit inside the control is detected");
}

#[test]
fn inline_word_replacement() {
    let a = ["The Supplier shall indemnify the Buyer."];
    let b = ["The Supplier may indemnify the Buyer."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn inline_insertion_and_deletion() {
    oracle(&["Party A shall pay the sum."], &["Party A shall pay the full sum promptly."]);
    oracle(&["Party A shall pay the full sum promptly."], &["Party A shall pay the sum."]);
}

#[test]
fn edited_paragraph_among_unchanged() {
    let a = ["Recitals.", "The term is five years.", "Signatures."];
    let b = ["Recitals.", "The term is ten years.", "Signatures."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn whole_paragraph_insert_middle() {
    let a = ["First.", "Third."];
    let b = ["First.", "Second, newly added.", "Third."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn whole_paragraph_insert_top() {
    let a = ["Body one.", "Body two."];
    let b = ["A brand new heading.", "Body one.", "Body two."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn whole_paragraph_insert_bottom() {
    let a = ["Body one.", "Body two."];
    let b = ["Body one.", "Body two.", "A new closing paragraph."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn multiple_paragraph_insert() {
    let a = ["Open.", "Close."];
    let b = ["Open.", "New clause one.", "New clause two.", "New clause three.", "Close."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn whole_paragraph_delete_middle() {
    let a = ["Keep this.", "Remove this entirely.", "Keep this too."];
    let b = ["Keep this.", "Keep this too."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn whole_paragraph_delete_bottom() {
    let a = ["Keep one.", "Keep two.", "Delete the trailing paragraph."];
    let b = ["Keep one.", "Keep two."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn whole_paragraph_delete_top() {
    let a = ["Delete the leading paragraph.", "Keep one.", "Keep two."];
    let b = ["Keep one.", "Keep two."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn dissimilar_paragraph_replacement() {
    // Too different to pair -> a whole-paragraph delete + insert in one gap (the ¶-ownership case).
    let a = ["Header.", "The quick brown fox jumps over the lazy dog.", "Footer."];
    let b = ["Header.", "Completely unrelated replacement clause text.", "Footer."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn mixed_insert_delete_edit() {
    let a = ["Intro.", "Clause A original.", "Clause B to be removed.", "Outro."];
    let b = ["Intro.", "Clause A revised text.", "A fresh clause C.", "Outro."];
    assert!(oracle(&a, &b) >= 1);
}

#[test]
fn everything_deleted_then_everything_new() {
    // Nothing in common: full replacement of the body.
    let a = ["Old one.", "Old two.", "Old three."];
    let b = ["New one.", "New two."];
    oracle(&a, &b);
}

#[test]
fn real_docx_fixture_round_trips_through_the_oracle() {
    // Exercise the engine on genuine Word-authored content (styles, a bold run) rather than the
    // synthetic append-only docs above. The fixture ships with pre-existing tracked changes, so
    // accept-all first to get a clean base A; then edit it directly to produce B.
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../scriptor-crdt/tests/fixtures/sample.docx");
    let base = CollabDoc::import_docx(&fixture).unwrap();
    base.accept_all("clean base").unwrap();
    let a_bytes = base.to_docx_bytes().unwrap();

    // Build B: edit the first paragraph, delete a word from the second, and append a paragraph.
    let b = CollabDoc::from_docx_bytes(&a_bytes).unwrap();
    b.insert_text(0, 0, "Revised: ", "edit").unwrap();
    let p1_len: usize =
        b.paragraphs().unwrap()[1].runs.iter().map(|r| r.text.chars().count()).sum();
    if p1_len > 4 {
        b.delete_text(1, 0..3, "edit").unwrap();
    }
    b.append_paragraph(&[Run::plain("An appended closing paragraph.")], None).unwrap();
    let b_bytes = b.to_docx_bytes().unwrap();

    let rep = scriptor_compare::check(&a_bytes, &b_bytes, &CompareOptions::default()).unwrap();
    assert!(
        rep.ok(),
        "fixture oracle failed: accept_ok={} reject_ok={} accept={:?} reject={:?}",
        rep.accept_ok,
        rep.reject_ok,
        rep.accept_mismatch,
        rep.reject_mismatch,
    );
    assert!(rep.changes >= 1);
}

#[test]
fn manifest_ids_are_populated() {
    let a = ["The term is five years."];
    let b = ["The term is ten years."];
    let result = compare(&docx(&a), &docx(&b), &CompareOptions::default()).unwrap();
    assert!(!result.manifest.changes.is_empty());
    assert!(result.manifest.changes.iter().all(|c| c.id > 0), "every change carries a revision id");
}

/// The alignment surfaced for the side-by-side view: one entry per aligned block, in document order,
/// with the right kinds (equal / edited / delete / insert) and index correspondence.
#[test]
fn alignment_maps_original_to_revised() {
    use scriptor_compare::AlignKind;
    // A: [p0, p1, p2]  ->  B: [p0, p1-edited, x-inserted, p2]
    let a = docx(&["Alpha unchanged", "Party A shall pay the sum", "Tail unchanged"]);
    let b = docx(&["Alpha unchanged", "Party A shall pay the amount", "A brand new clause", "Tail unchanged"]);
    let m = compare(&a, &b, &CompareOptions::default()).unwrap().manifest;
    let al = m.alignment;

    // Anchors (equal/edited) carry both indices; delete only `a`, insert only `b`.
    assert!(al.iter().all(|e| match e.kind {
        AlignKind::Equal | AlignKind::Edited => e.a.is_some() && e.b.is_some(),
        AlignKind::Delete => e.a.is_some() && e.b.is_none(),
        AlignKind::Insert => e.a.is_none() && e.b.is_some(),
    }), "{al:?}");

    let equal = al.iter().find(|e| e.a == Some(0)).unwrap();
    assert_eq!(equal.kind, AlignKind::Equal); // p0 identical
    let edited = al.iter().find(|e| e.a == Some(1)).unwrap();
    assert_eq!(edited.kind, AlignKind::Edited); // p1 reworded
    assert_eq!(edited.b, Some(1));
    assert!(al.iter().any(|e| e.kind == AlignKind::Insert && e.b == Some(2)), "{al:?}"); // new clause
    // The last paragraph re-pairs across the insertion (A[2] <-> B[3]).
    let tail = al.iter().find(|e| e.a == Some(2)).unwrap();
    assert_eq!((tail.kind, tail.b), (AlignKind::Equal, Some(3)));
}
