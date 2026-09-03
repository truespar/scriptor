//! Move detection: a paragraph relocated unchanged is redlined as a native `w:moveFrom` /
//! `w:moveTo` pair (sharing one revision id) rather than a delete + an insert. Oracle-neutral - a
//! move resolves exactly like delete+insert - but a far cleaner redline for a reordered clause.

use scriptor_compare::{check, compare, ChangeKind, CompareOptions};
use scriptor_crdt::{CollabDoc, Run, TrackKind};

fn docx(paras: &[&str]) -> Vec<u8> {
    let doc = CollabDoc::new();
    for p in paras {
        doc.append_paragraph(&[Run::plain(*p)], None).unwrap();
    }
    doc.to_docx_bytes().unwrap()
}

fn oracle(a: &[u8], b: &[u8]) -> scriptor_compare::OracleReport {
    check(a, b, &CompareOptions::default()).unwrap()
}

/// (moveFrom run count, moveTo run count) in the redline.
fn move_run_counts(redline: &[u8]) -> (usize, usize) {
    let doc = CollabDoc::from_docx_bytes(redline).unwrap();
    let mut from = 0;
    let mut to = 0;
    for p in doc.paragraphs().unwrap() {
        for r in &p.runs {
            match r.track.as_ref().map(|t| t.kind) {
                Some(TrackKind::MoveFrom) => from += 1,
                Some(TrackKind::MoveTo) => to += 1,
                _ => {}
            }
        }
    }
    (from, to)
}

#[test]
fn paragraph_moved_down_is_a_move() {
    let a = docx(&["Intro.", "The movable clause.", "Middle.", "End."]);
    let b = docx(&["Intro.", "Middle.", "End.", "The movable clause."]);
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);

    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    assert!(result.manifest.changes.iter().any(|c| c.kind == ChangeKind::Move), "a move should be recorded");
    // The relocated clause must NOT show as a plain delete + insert.
    assert!(!result.manifest.changes.iter().any(|c| matches!(c.kind, ChangeKind::ParaDelete | ChangeKind::ParaInsert)));
    let (from, to) = move_run_counts(&result.redline);
    assert!(from >= 1 && to >= 1, "native w:moveFrom/w:moveTo expected, got from={from} to={to}");
}

#[test]
fn paragraph_moved_up_is_a_move() {
    let a = docx(&["Intro.", "Middle.", "End.", "The movable clause."]);
    let b = docx(&["Intro.", "The movable clause.", "Middle.", "End."]);
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    assert!(result.manifest.changes.iter().any(|c| c.kind == ChangeKind::Move));
}

#[test]
fn move_alongside_an_unrelated_edit() {
    let a = docx(&["Intro paragraph.", "The movable clause.", "A clause to revise here.", "End."]);
    let b = docx(&["Intro paragraph.", "A clause to revise now.", "End.", "The movable clause."]);
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    assert!(result.manifest.changes.iter().any(|c| c.kind == ChangeKind::Move), "the relocation is a move");
}

#[test]
fn unrelated_delete_and_insert_is_not_a_move() {
    // A deleted paragraph and a *different* inserted paragraph must stay delete + insert.
    let a = docx(&["Keep.", "This whole clause is being removed entirely.", "Keep two."]);
    let b = docx(&["Keep.", "An entirely different new provision.", "Keep two."]);
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    assert!(!result.manifest.changes.iter().any(|c| c.kind == ChangeKind::Move), "not a move");
}

#[test]
fn two_paragraphs_swapped_are_two_moves() {
    let a = docx(&["Header.", "Clause alpha here.", "Clause beta here.", "Footer."]);
    let b = docx(&["Header.", "Clause beta here.", "Clause alpha here.", "Footer."]);
    let r = oracle(&a, &b);
    assert!(r.ok(), "accept={:?} reject={:?}", r.accept_mismatch, r.reject_mismatch);
}
