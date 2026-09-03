//! Detection controls (the "comparison profile" knobs): ignore formatting / whitespace / case, and
//! whether to detect moves. Each keeps the redline focused on the changes a reviewer asked for.

use scriptor_compare::{compare, ChangeKind, CompareOptions};
use scriptor_crdt::{CollabDoc, Run};

fn docx_runs(paras: &[Vec<Run>]) -> Vec<u8> {
    let d = CollabDoc::new();
    for runs in paras {
        d.append_paragraph(runs, None).unwrap();
    }
    d.to_docx_bytes().unwrap()
}
fn docx(paras: &[&str]) -> Vec<u8> {
    let built: Vec<Vec<Run>> = paras.iter().map(|p| vec![Run::plain(*p)]).collect();
    docx_runs(&built)
}
fn count(a: &[u8], b: &[u8], opts: &CompareOptions) -> usize {
    compare(a, b, opts).unwrap().manifest.changes.len()
}

#[test]
fn ignore_formatting_suppresses_a_format_only_change() {
    let a = docx_runs(&[vec![Run::plain("The Supplier shall pay the fee.")]]);
    let b = docx_runs(&[vec![Run { bold: true, ..Run::plain("The Supplier shall pay the fee.") }]]);
    // Same text, bold added: a formatting change by default, none when formatting is ignored.
    let base = compare(&a, &b, &CompareOptions::default()).unwrap().manifest;
    assert!(
        base.changes.iter().any(|c| c.kind == ChangeKind::Format),
        "expected a format change by default, got {:?}",
        base.changes
    );
    assert_eq!(count(&a, &b, &CompareOptions { detect_formatting: false, ..Default::default() }), 0);
}

#[test]
fn ignore_case_suppresses_a_case_only_change() {
    let a = docx(&["Party A shall pay the Sum within Thirty days."]);
    let b = docx(&["party a shall pay the sum within thirty days."]);
    assert!(count(&a, &b, &CompareOptions::default()) >= 1);
    assert_eq!(count(&a, &b, &CompareOptions { ignore_case: true, ..Default::default() }), 0);
}

#[test]
fn ignore_whitespace_suppresses_a_spacing_only_change() {
    let a = docx(&["hello   world  and   more"]);
    let b = docx(&["hello world and more"]);
    assert!(count(&a, &b, &CompareOptions::default()) >= 1);
    assert_eq!(count(&a, &b, &CompareOptions { ignore_whitespace: true, ..Default::default() }), 0);
}

#[test]
fn ignore_case_still_reports_a_real_edit() {
    // A genuine word change must survive ignore-case (it's not a case-only diff).
    let a = docx(&["Party shall indemnify the Buyer."]);
    let b = docx(&["Party may indemnify the Buyer."]);
    assert!(count(&a, &b, &CompareOptions { ignore_case: true, ..Default::default() }) >= 1);
}

#[test]
fn detect_moves_off_falls_back_to_delete_insert() {
    let a = docx(&["Intro.", "The movable clause.", "Middle.", "End."]);
    let b = docx(&["Intro.", "Middle.", "End.", "The movable clause."]);
    let on = compare(&a, &b, &CompareOptions::default()).unwrap().manifest;
    assert!(on.changes.iter().any(|c| c.kind == ChangeKind::Move), "{:?}", on.changes);

    let off = compare(&a, &b, &CompareOptions { detect_moves: false, ..Default::default() }).unwrap();
    assert!(!off.manifest.changes.iter().any(|c| c.kind == ChangeKind::Move), "{:?}", off.manifest.changes);
    // Still a valid redline (the clause is now a delete + insert).
    assert!(!off.redline.is_empty());
}
