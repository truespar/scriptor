//! The byte-stability invariant, exercised end-to-end: parse -> serialize -> parse must leave
//! every part's decompressed bytes identical. `roundtrip_bytes` is the in-memory corpus-gate
//! path (`scriptor roundtrip <dir>`); these tests pin its contract.

use scriptor_ooxml::{Part, roundtrip_bytes, write_parts_bytes};

#[test]
fn synthetic_package_roundtrips_byte_stable() {
    let parts = vec![
        Part {
            name: "[Content_Types].xml".into(),
            data: b"<?xml version=\"1.0\"?><Types/>".to_vec(),
        },
        Part {
            name: "word/document.xml".into(),
            data: b"<w:document><w:body><w:p/></w:body></w:document>".to_vec(),
        },
        // A binary part: byte-stability must hold for non-XML content too.
        Part { name: "word/media/image1.png".into(), data: vec![0x89, 0x50, 0x4e, 0x47, 0x00, 0xff] },
    ];
    let bytes = write_parts_bytes(&parts).expect("serialize package");
    let r = roundtrip_bytes(&bytes).expect("roundtrip");
    assert!(r.stable, "first diff: {:?}", r.first_diff);
    assert_eq!(r.parts, 3);
}

#[test]
fn real_fixture_roundtrips_byte_stable() {
    // The Word-produced fixture shared with scriptor-crdt's through-CRDT round-trip test.
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scriptor-crdt/tests/fixtures/sample.docx");
    let bytes = std::fs::read(&fixture).expect("reading the fixture .docx");
    let r = roundtrip_bytes(&bytes).expect("roundtrip");
    assert!(r.stable, "first diff: {:?}", r.first_diff);
    assert!(r.parts > 0);
}

#[test]
fn non_docx_bytes_error_cleanly() {
    assert!(roundtrip_bytes(b"not a zip archive").is_err());
}
