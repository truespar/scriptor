//! OPC relationship parts (`_rels/*.rels`).
//! 
//! Resolves the main document part, which OOXML does not require to be named
//! `word/document.xml`, and the id-to-target map used to follow image and hyperlink
//! relationships.

use super::*;

/// The package-root-relative path of the main document part, read from `_rels/.rels` (the
/// relationship whose Type ends in `/officeDocument`). OOXML does not require it to be
/// `word/document.xml` - some authoring tools name it differently (e.g. `word/trial.xml`), and a doc
/// that hardcoded the name failed to open. Returns `None` if the rels has no officeDocument entry.
pub fn main_document_part(rels_xml: &[u8]) -> Option<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(rels_xml);
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                if attr(&e, b"Type").is_some_and(|t| t.ends_with("/officeDocument"))
                    && let Some(target) = attr(&e, b"Target")
                {
                    return Some(target.trim_start_matches('/').to_string());
                }
            }
            _ => {}
        }
        buf.clear();
    }
    None
}

/// Resolve `word/_rels/document.xml.rels` into a map of relationship id -> target part name
/// (relative to `word/`, e.g. `header1.xml`).
pub fn resolve_rels(rels_xml: &[u8]) -> std::collections::HashMap<String, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(rels_xml);
    let mut buf = Vec::new();
    let mut map = std::collections::HashMap::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    map.insert(id, target);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    map
}
