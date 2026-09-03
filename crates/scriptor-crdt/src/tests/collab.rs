//! CRDT convergence: concurrent merge, snapshots and deltas.

use super::*;

/// Two peers branch from a shared base, each append a paragraph concurrently, then exchange
/// snapshots and converge to identical state with both paragraphs preserved (the kernel
/// convergence property, over the block-tree model).
#[test]
fn concurrent_paragraph_appends_converge() -> Result<()> {
    let a = CollabDoc::new();
    a.append_paragraph(&[plain("Base.")], None)?;

    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;
    assert_eq!(a.paragraphs()?, b.paragraphs()?);

    a.append_paragraph(&[plain("From A.")], None)?;
    b.append_paragraph(&[plain("From B.")], None)?;

    let (sa, sb) = (a.snapshot()?, b.snapshot()?);
    a.merge(&sb)?;
    b.merge(&sa)?;

    assert_eq!(a.paragraphs()?, b.paragraphs()?, "peers did not converge");
    let texts: Vec<String> = a
        .paragraphs()?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert!(texts.iter().any(|t| t == "Base."));
    assert!(texts.iter().any(|t| t == "From A."), "lost peer A's paragraph");
    assert!(texts.iter().any(|t| t == "From B."), "lost peer B's paragraph");
    Ok(())
}

/// `version()` + `export_updates_since()` produce an incremental delta that
/// brings a peer at the captured version up to date, is smaller than a full
/// snapshot, and is a harmless no-op when nothing changed.
#[test]
fn export_updates_since_is_an_incremental_delta() -> Result<()> {
    let a = CollabDoc::new();
    a.append_paragraph(&[plain("base")], None)?;

    // A peer at the base version.
    let b = CollabDoc::new();
    b.merge(&a.snapshot()?)?;

    // Capture the version, mutate, then export only the new ops. Round-trip
    // the version through encode/decode (the browser holds it as bytes).
    let v0 = DocVersion::decode(&a.version().encode())?;
    a.append_paragraph(&[plain("added later")], None)?;
    let delta = a.export_updates_since(&v0)?;

    // The delta brings the peer up to date.
    b.merge(&delta)?;
    let texts: Vec<String> = b
        .paragraphs()?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert!(texts.iter().any(|t| t == "added later"), "delta did not carry the new paragraph");
    assert!(delta.len() < a.snapshot()?.len(), "a delta should be smaller than a full snapshot");

    // No-op interval: the delta merges harmlessly and changes nothing.
    let v1 = a.version();
    let before = b.paragraphs()?;
    b.merge(&a.export_updates_since(&v1)?)?;
    assert_eq!(b.paragraphs()?, before, "empty-interval delta changed state");
    Ok(())
}

#[test]
fn swedish_diacritics_round_trip_through_append_snapshot_and_docx() -> Result<()> {
    // Regression: å ä ö rendered as a/o. Prove the *data* path preserves the bytes
    // through every stage the authoring tools + canvas touch: append -> read,
    // snapshot -> reload, docx export -> reimport.
    const S: &str = "Hörnågård Åäö Öberg æ ø é ü ñ €";
    let read_last = |d: &CollabDoc| -> String {
        d.paragraphs().unwrap().last().unwrap().runs.iter().map(|r| r.text.as_str()).collect()
    };

    let doc = CollabDoc::new();
    doc.append_paragraph(&[plain(S)], None)?;
    assert_eq!(read_last(&doc), S, "append -> paragraphs corrupted the diacritics");

    let reloaded = CollabDoc::new();
    reloaded.merge(&doc.snapshot()?)?;
    assert_eq!(read_last(&reloaded), S, "snapshot round-trip corrupted the diacritics");

    let docx = doc.to_docx_bytes()?;
    let from_docx = CollabDoc::from_docx_bytes(&docx)?;
    let all: String = from_docx
        .paragraphs()?
        .iter()
        .flat_map(|p| p.runs.iter())
        .map(|r| r.text.as_str())
        .collect();
    assert!(all.contains("Hörnågård") && all.contains("Öberg") && all.contains('€'),
        "docx export/reimport corrupted the diacritics: {all:?}");
    Ok(())
}
