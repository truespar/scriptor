//! Unit tests for `CollabDoc`, grouped the same way the API is.
//! 
//! Shared fixtures live here; each submodule covers one construct.

use super::*;

mod tables;
mod tracked;
mod comments;
mod fields;
mod headers_footers;
mod images;
mod styles;
mod anchors;
mod collab;
mod export;
mod editing;

fn plain(text: &str) -> Run {
    Run::plain(text)
}

/// Parse `xml` tracking the in-scope `xmlns:` declarations and assert every element and
/// attribute prefix is bound. Substring assertions cannot catch an undeclared prefix - the
/// part parses fine as bytes but Word rejects it as "unreadable content" - so resolve them
/// for real.
fn assert_ns_well_formed(xml: &str) {
    use quick_xml::events::{BytesStart, Event};
    fn prefix_of(qname: &[u8]) -> Option<String> {
        let s = String::from_utf8_lossy(qname);
        s.split_once(':').map(|(p, _)| p.to_string())
    }
    fn check(e: &BytesStart, scopes: &mut Vec<Vec<String>>, keep_scope: bool) {
        // Declarations on this element are in scope for the element itself.
        let mut here = Vec::new();
        for a in e.attributes().with_checks(false).flatten() {
            if let Some(rest) = a.key.as_ref().strip_prefix(b"xmlns:") {
                here.push(String::from_utf8_lossy(rest).into_owned());
            }
        }
        scopes.push(here);
        let bound = |scopes: &[Vec<String>], p: &str| {
            p == "xml" || scopes.iter().any(|s| s.iter().any(|d| d == p))
        };
        if let Some(p) = prefix_of(e.name().as_ref()) {
            assert!(
                bound(scopes, &p),
                "undeclared element prefix `{p}`: <{}>",
                String::from_utf8_lossy(e.name().as_ref())
            );
        }
        for a in e.attributes().with_checks(false).flatten() {
            let key = a.key.as_ref();
            if key == b"xmlns" || key.starts_with(b"xmlns:") {
                continue;
            }
            if let Some(p) = prefix_of(key) {
                assert!(
                    bound(scopes, &p),
                    "undeclared attribute prefix `{p}`: {}",
                    String::from_utf8_lossy(key)
                );
            }
        }
        if !keep_scope {
            scopes.pop();
        }
    }
    let mut reader = quick_xml::Reader::from_reader(xml.as_bytes());
    let mut buf = Vec::new();
    let mut scopes: Vec<Vec<String>> = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).expect("well-formed XML") {
            Event::Start(e) => check(&e, &mut scopes, true),
            Event::Empty(e) => check(&e, &mut scopes, false),
            Event::End(_) => {
                scopes.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
}

fn table_2x2_doc() -> &'static [u8] {
    br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#
}

fn first_table(doc: &CollabDoc) -> model::Table {
    doc.body()
        .into_iter()
        .find_map(|i| if let BodyItem::Table(t) = i { Some(*t) } else { None })
        .expect("a table")
}

/// A bookmark sitting between a block wrapper's opening and its first paragraph (Word marks
/// it `displacedByCustomXml`) is modeled ONCE - the verbatim sdt prefix must not capture it
/// too, or export emits the same bookmark id twice (tdf154478/tdf154481).
#[test]
fn wrapper_prefix_excludes_range_markers() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:sdt><w:sdtPr><w:id w:val="7"/></w:sdtPr><w:sdtContent><w:bookmarkStart w:id="2" w:name="_Toc1" w:displacedByCustomXml="prev"/><w:p><w:r><w:t>Inside</w:t></w:r></w:p><w:bookmarkEnd w:id="2"/></w:sdtContent></w:sdt>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let out = doc.to_document_xml()?;
    assert!(out.matches("<w:bookmarkStart").count() <= 1, "no double emission: {out}");
    assert!(out.contains("<w:sdt>"), "wrapper kept: {out}");
    assert!(out.contains(">Inside<"), "content kept: {out}");
    Ok(())
}

/// An explicit `w:highlight w:val="none"` is kept (not folded to unset), so it cancels an inherited
/// highlight and round-trips - the mirror of `color="auto"`.
#[test]
fn highlight_none_is_preserved_not_dropped() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:rPr><w:highlight w:val="none"/></w:rPr><w:t>off</w:t></w:r>
     <w:r><w:rPr><w:highlight w:val="cyan"/></w:rPr><w:t>cyan</w:t></w:r>
     <w:r><w:t>plain</w:t></w:r></w:p>
</w:body></w:document>"#;
    let doc = CollabDoc::from_document_xml(xml)?;
    let runs = doc.paragraphs()?[0].runs.clone();
    let by = |t: &str| runs.iter().find(|r| r.text == t).unwrap().highlight.clone();
    assert_eq!(by("off").as_deref(), Some("none"), "explicit none kept (cancels inheritance)");
    assert_eq!(by("cyan").as_deref(), Some("cyan"), "a real highlight is kept");
    assert_eq!(by("plain"), None, "no highlight stays unset (inherits)");
    assert!(doc.to_document_xml()?.contains(r#"<w:highlight w:val="none"/>"#), "none survives export");
    Ok(())
}
