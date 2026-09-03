//! File-IO integration test for the OOXML <-> loro mapping: import a real `.docx`, export it back through
//! the CRDT, and confirm the modeled content survives a disk round-trip. The unit tests in `lib.rs`
//! cover the in-memory XML path; this exercises `import_docx` / `export_docx` (the zip layer).

use std::path::PathBuf;

use scriptor_crdt::{CollabDoc, TrackKind};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.docx")
}

#[test]
fn docx_import_export_disk_roundtrip() -> anyhow::Result<()> {
    let doc = CollabDoc::import_docx(&fixture())?;
    let before = doc.paragraphs()?;

    // Two paragraphs: a styled heading and a body paragraph with a bold run + ins + del.
    assert_eq!(before.len(), 2);
    assert_eq!(before[0].style.as_deref(), Some("Heading1"));
    assert!(before[1].runs.iter().any(|r| r.bold));
    assert!(before[1]
        .runs
        .iter()
        .any(|r| r.track.as_ref().is_some_and(|t| t.kind == TrackKind::Ins)));
    assert!(before[1]
        .runs
        .iter()
        .any(|r| r.track.as_ref().is_some_and(|t| t.kind == TrackKind::Del)));

    // Export to a temp .docx (reusing the fixture as the template), then re-import.
    let out = std::env::temp_dir().join("scriptor-a31-roundtrip.docx");
    doc.export_docx(&fixture(), &out)?;
    let after = CollabDoc::import_docx(&out)?.paragraphs()?;

    assert_eq!(before, after, "disk round-trip changed the modeled content");
    let _ = std::fs::remove_file(&out);
    Ok(())
}
