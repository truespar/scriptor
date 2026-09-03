//! Table comparison (structured path). This increment covers *content* editing - cell values
//! changing in an otherwise same-shaped table (the common legal case: a schedule whose figures were
//! revised) plus paragraph edits around a table. Intra-table structural changes (rows / columns /
//! whole tables added or removed) are the next increment; here we assert only that they never
//! corrupt the document (reject still reproduces the original).

use scriptor_compare::{check, compare, CompareOptions};
use scriptor_crdt::{CollabDoc, Run};

/// A doc: an intro paragraph, a table (grid of cell strings), a closing paragraph.
fn doc(intro: &str, rows: &[Vec<&str>], closing: &str) -> Vec<u8> {
    let d = CollabDoc::new();
    d.append_paragraph(&[Run::plain(intro)], None).unwrap();
    let grid: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect();
    d.append_table(&grid, "build").unwrap();
    d.append_paragraph(&[Run::plain(closing)], None).unwrap();
    d.to_docx_bytes().unwrap()
}

fn oracle(a: &[u8], b: &[u8]) -> scriptor_compare::OracleReport {
    check(a, b, &CompareOptions::default()).unwrap()
}

#[test]
fn table_cell_edit_round_trips() {
    let a = doc("Schedule of fees.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "End.");
    let b = doc("Schedule of fees.", &[vec!["Item", "Price"], vec!["Widget", "12"]], "End.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    assert!(r.changes >= 1, "the changed cell should be redlined");
}

#[test]
fn multiple_cell_edits_across_rows() {
    let a = doc("T.", &[vec!["A", "1"], vec!["B", "2"], vec!["C", "3"]], "E.");
    let b = doc("T.", &[vec!["A", "1"], vec!["B", "20"], vec!["C", "30"]], "E.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    assert!(r.changes >= 2);
}

#[test]
fn paragraph_edits_around_a_table() {
    // Editing the body paragraphs while a table is present must still work (and not touch the grid).
    let a = doc("The original intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "The original close.");
    let b = doc("The revised intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "The revised close.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    assert!(r.changes >= 2);
}

#[test]
fn cell_and_paragraph_edits_together() {
    let a = doc("Intro one.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close one.");
    let b = doc("Intro two.", &[vec!["Item", "Price"], vec!["Gizmo", "10"]], "Close one.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
}

#[test]
fn identical_table_doc_has_no_changes() {
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close.");
    let r = oracle(&a, &a);
    assert!(r.ok());
    assert_eq!(r.changes, 0);
}

#[test]
fn row_appended() {
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close.");
    let b = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"], vec!["Gadget", "20"]], "Close.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    assert!(r.changes >= 1);
}

#[test]
fn row_inserted_in_the_middle() {
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"], vec!["Bolt", "5"]], "Close.");
    let b = doc(
        "Intro.",
        &[vec!["Item", "Price"], vec!["Widget", "10"], vec!["Gadget", "20"], vec!["Bolt", "5"]],
        "Close.",
    );
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
}

#[test]
fn multiple_rows_appended() {
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close.");
    let b = doc(
        "Intro.",
        &[vec!["Item", "Price"], vec!["Widget", "10"], vec!["Gadget", "20"], vec!["Sprocket", "30"]],
        "Close.",
    );
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
}

#[test]
fn row_removed() {
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"], vec!["Gadget", "20"]], "Close.");
    let b = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    assert!(r.changes >= 1);
}

#[test]
fn row_edit_and_row_add_together() {
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close.");
    let b = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "12"], vec!["Gadget", "20"]], "Close.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
}

#[test]
fn top_of_table_row_inserted() {
    // A row added before the first row (e.g. a header) - anchored above the first row.
    let a = doc("Intro.", &[vec!["Widget", "10"]], "Close.");
    let b = doc("Intro.", &[vec!["Header", "Col"], vec!["Widget", "10"]], "Close.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
}

#[test]
fn column_removed() {
    let a = doc("T.", &[vec!["Item", "Price", "Qty"], vec!["Widget", "10", "5"]], "E.");
    let b = doc("T.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "E.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    assert!(r.changes >= 1);
}

#[test]
fn column_removed_with_a_cell_edit() {
    let a = doc("T.", &[vec!["Item", "Price", "Qty"], vec!["Widget", "10", "5"]], "E.");
    let b = doc("T.", &[vec!["Item", "Price"], vec!["Widget", "12"]], "E.");
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
}

#[test]
fn whole_table_removed() {
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close.");
    let d = CollabDoc::new();
    d.append_paragraph(&[Run::plain("Intro.")], None).unwrap();
    d.append_paragraph(&[Run::plain("Close.")], None).unwrap();
    let b = d.to_docx_bytes().unwrap();
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    assert!(r.changes >= 1);
}

#[test]
fn staged_structural_change_never_corrupts() {
    // A column *added* (staged: it would shift every row's flat indices) is not yet redlined, but
    // comparison must not crash and reject-all must still reproduce the original.
    let a = doc("Intro.", &[vec!["Item", "Price"], vec!["Widget", "10"]], "Close.");
    let b = doc("Intro.", &[vec!["Item", "Price", "Qty"], vec!["Widget", "10", "5"]], "Close.");
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    assert!(!result.redline.is_empty());
    let r = oracle(&a, &b);
    assert!(r.reject_ok, "reject must reproduce the original: {:?}", r.reject_mismatch);
}
