//! Plain-text extraction: docx bytes → the FINAL reading of the document.
//!
//! Built for machine consumers (paddock's model-attachment path, any headless
//! caller that wants "what the document says"): tracked insertions and move
//! destinations are kept, tracked deletions and move sources dropped - the
//! text a reader accepting every change would see. Core properties
//! (docProps/core.xml) ride along for metadata injection.

use crate::{CollabDoc, TrackKind};
use anyhow::Result;

/// The extracted document text plus what resolving it involved.
#[derive(Debug)]
pub struct DocxText {
    /// Paragraphs joined by blank lines; table-cell paragraphs appear in flow
    /// order. Tracked deletions are absent, insertions present (final view).
    pub text: String,
    /// Non-empty paragraphs emitted.
    pub paragraphs: usize,
    /// Tracked revisions the final view resolved (insertions kept + deletions
    /// dropped). A caller can surface "this document carries N tracked
    /// changes" so redlines are never silently flattened.
    pub revisions: usize,
}

/// Extract the final-view text of a `.docx`.
pub fn extract_text(docx: &[u8]) -> Result<DocxText> {
    let doc = CollabDoc::from_docx_bytes(docx)?;
    let paras = doc.paragraphs()?;
    let mut out = String::new();
    let mut emitted = 0usize;
    let mut revisions = 0usize;
    // A tracked paragraph-mark deletion is a suggested JOIN: in the final view
    // this paragraph and the next run together, so no break is emitted.
    let mut join_next = false;
    for p in &paras {
        let mut line = String::new();
        for r in &p.runs {
            match r.track.as_ref().map(|t| &t.kind) {
                Some(TrackKind::Del | TrackKind::MoveFrom) => revisions += 1,
                Some(TrackKind::Ins | TrackKind::MoveTo) => {
                    revisions += 1;
                    line.push_str(&r.text);
                }
                _ => line.push_str(&r.text),
            }
        }
        let line = line.trim_end();
        if line.is_empty() {
            join_next = false;
            continue;
        }
        if join_next {
            out.push(' ');
        } else {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            emitted += 1;
        }
        out.push_str(line);
        join_next = matches!(p.mark_change.as_ref().map(|t| &t.kind), Some(TrackKind::Del));
        if join_next {
            revisions += 1;
        }
    }
    Ok(DocxText { text: out, paragraphs: emitted, revisions })
}

/// Core document properties from `docProps/core.xml` - the Word equivalent of
/// a PDF's Info dict. All fields optional; an absent part yields all-`None`.
#[derive(Debug, Default, Clone)]
pub struct CoreProperties {
    pub title: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub keywords: Option<String>,
    pub last_modified_by: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
}

/// Read the core properties of a `.docx`. Never fails on a missing or odd
/// `core.xml` - metadata is a garnish, not a gate.
pub fn core_properties(docx: &[u8]) -> CoreProperties {
    let Ok(parts) = scriptor_ooxml::read_parts_bytes(docx) else {
        return CoreProperties::default();
    };
    let Some(core) = parts.iter().find(|p| p.name == "docProps/core.xml") else {
        return CoreProperties::default();
    };
    let mut props = CoreProperties::default();
    let mut reader = quick_xml::Reader::from_reader(core.data.as_slice());
    let mut buf = Vec::new();
    let mut current: Option<&'static str> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                // local-name match: prefixes vary (dc:, cp:, dcterms:)
                let name = e.local_name();
                current = match name.as_ref() {
                    b"title" => Some("title"),
                    b"subject" => Some("subject"),
                    b"creator" => Some("creator"),
                    b"keywords" => Some("keywords"),
                    b"lastModifiedBy" => Some("lastModifiedBy"),
                    b"created" => Some("created"),
                    b"modified" => Some("modified"),
                    _ => None,
                };
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if let Some(field) = current {
                    let v = t.decode().unwrap_or_default().trim().to_string();
                    if !v.is_empty() {
                        match field {
                            "title" => props.title = Some(v),
                            "subject" => props.subject = Some(v),
                            "creator" => props.creator = Some(v),
                            "keywords" => props.keywords = Some(v),
                            "lastModifiedBy" => props.last_modified_by = Some(v),
                            "created" => props.created = Some(v),
                            "modified" => props.modified = Some(v),
                            _ => {}
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::End(_)) => current = None,
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    props
}

/// Extended + custom document properties: `docProps/app.xml` (the Word/Excel
/// statistics panel - pages, word count, company) and `docProps/custom.xml`
/// (the interesting one for provenance: DMS stamps like client/matter
/// numbers, compare-tool markers). Works on any OPC package - docx, xlsx,
/// xlsb share the parts; absent parts yield the empty default. Same
/// garnish-not-gate contract as [`core_properties`].
#[derive(Debug, Default, Clone)]
pub struct ExtendedProperties {
    pub pages: Option<String>,
    pub words: Option<String>,
    pub company: Option<String>,
    pub manager: Option<String>,
    /// `custom.xml` `property` entries as (name, value) in file order, every
    /// variant type stringified (lpwstr, filetime, numerics, bool). Empty
    /// names/values are dropped.
    pub custom: Vec<(String, String)>,
}

pub fn extended_properties(bytes: &[u8]) -> ExtendedProperties {
    let Ok(parts) = scriptor_ooxml::read_parts_bytes(bytes) else {
        return ExtendedProperties::default();
    };
    let mut out = ExtendedProperties::default();

    if let Some(app) = parts.iter().find(|p| p.name == "docProps/app.xml") {
        let mut reader = quick_xml::Reader::from_reader(app.data.as_slice());
        let mut buf = Vec::new();
        let mut current: Option<&'static str> = None;
        let mut depth = 0usize;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(e)) => {
                    depth += 1;
                    // top-level fields only: HeadingPairs/TitlesOfParts nest
                    // vt:vector children whose text must not leak in
                    current = if depth == 2 {
                        match e.local_name().as_ref() {
                            b"Pages" => Some("pages"),
                            b"Words" => Some("words"),
                            b"Company" => Some("company"),
                            b"Manager" => Some("manager"),
                            _ => None,
                        }
                    } else {
                        None
                    };
                }
                Ok(quick_xml::events::Event::Text(t)) => {
                    if let Some(field) = current {
                        let v = t.decode().unwrap_or_default().trim().to_string();
                        if !v.is_empty() {
                            match field {
                                "pages" => out.pages = Some(v),
                                "words" => out.words = Some(v),
                                "company" => out.company = Some(v),
                                "manager" => out.manager = Some(v),
                                _ => {}
                            }
                        }
                    }
                }
                Ok(quick_xml::events::Event::End(_)) => {
                    depth = depth.saturating_sub(1);
                    current = None;
                }
                Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }

    if let Some(custom) = parts.iter().find(|p| p.name == "docProps/custom.xml") {
        let mut reader = quick_xml::Reader::from_reader(custom.data.as_slice());
        let mut buf = Vec::new();
        let mut name: Option<String> = None;
        let mut value = String::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(e)) => {
                    if e.local_name().as_ref() == b"property" {
                        // attribute decoded by hand (see tables.rs::raw_attrs:
                        // unescape_value vanishes under quick-xml's `encoding`
                        // feature, which any consumer crate may enable)
                        name = e.attributes().flatten().find_map(|a| {
                            if a.key.local_name().as_ref() != b"name" {
                                return None;
                            }
                            let raw = std::str::from_utf8(&a.value).ok()?;
                            let v = quick_xml::escape::unescape(raw).ok()?.trim().to_string();
                            (!v.is_empty()).then_some(v)
                        });
                        value.clear();
                    }
                }
                Ok(quick_xml::events::Event::Text(t)) => {
                    if name.is_some() {
                        value.push_str(t.decode().unwrap_or_default().trim());
                    }
                }
                Ok(quick_xml::events::Event::End(e)) => {
                    if e.local_name().as_ref() == b"property"
                        && let Some(n) = name.take()
                        && !value.is_empty()
                    {
                        out.custom.push((n, std::mem::take(&mut value)));
                    }
                }
                Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal in-memory docx: hand-written document.xml with tracked
    /// changes + core.xml, zipped through scriptor-ooxml itself.
    fn tracked_docx() -> Vec<u8> {
        let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:ins w:id="1" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">brave </w:t></w:r></w:ins><w:r><w:t>world.</w:t></w:r></w:p>
<w:p><w:del w:id="2" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>Entirely deleted.</w:delText></w:r></w:del></w:p>
<w:p><w:r><w:t>End.</w:t></w:r></w:p>
</w:body></w:document>"#;
        let core = r#"<?xml version="1.0"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/">
<dc:title>Test Agreement</dc:title><dc:creator>Jane Author</dc:creator>
<dcterms:modified>2026-02-03T04:05:06Z</dcterms:modified>
</cp:coreProperties>"#;
        let parts = vec![
            scriptor_ooxml::Part { name: "word/document.xml".into(), data: document.as_bytes().to_vec() },
            scriptor_ooxml::Part { name: "docProps/core.xml".into(), data: core.as_bytes().to_vec() },
        ];
        scriptor_ooxml::write_parts_bytes(&parts).expect("zip")
    }

    #[test]
    fn final_view_keeps_insertions_drops_deletions() {
        let out = extract_text(&tracked_docx()).expect("extract");
        assert_eq!(out.text, "Hello brave world.\n\nEnd.");
        assert_eq!(out.paragraphs, 2);
        assert!(out.revisions >= 2, "ins + del both counted, got {}", out.revisions);
    }

    #[test]
    fn extended_and_custom_properties_read() {
        let app = r#"<?xml version="1.0"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">
<Template>Normal</Template><Pages>4</Pages><Words>1055</Words><Company>Truespar AB</Company>
<HeadingPairs><vt:vector size="2" baseType="variant"><vt:variant><vt:lpstr>Rubrik</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs>
</Properties>"#;
        let custom = r#"<?xml version="1.0"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">
<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="Matter &amp; Client"><vt:lpwstr>ACME-0042</vt:lpwstr></property>
<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3" name="LastSaved"><vt:filetime>2021-10-15T00:00:00Z</vt:filetime></property>
<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="4" name="Empty"><vt:lpwstr></vt:lpwstr></property>
</Properties>"#;
        let parts = vec![
            scriptor_ooxml::Part { name: "docProps/app.xml".into(), data: app.as_bytes().to_vec() },
            scriptor_ooxml::Part { name: "docProps/custom.xml".into(), data: custom.as_bytes().to_vec() },
        ];
        let zip = scriptor_ooxml::write_parts_bytes(&parts).expect("zip");
        let p = extended_properties(&zip);
        assert_eq!(p.pages.as_deref(), Some("4"));
        assert_eq!(p.words.as_deref(), Some("1055"));
        assert_eq!(p.company.as_deref(), Some("Truespar AB"));
        assert!(p.manager.is_none());
        // vt:vector text inside HeadingPairs must not leak into any field
        assert_ne!(p.pages.as_deref(), Some("Rubrik"));
        assert_eq!(
            p.custom,
            vec![
                ("Matter & Client".to_string(), "ACME-0042".to_string()),
                ("LastSaved".to_string(), "2021-10-15T00:00:00Z".to_string()),
            ],
            "entities unescape, empty values drop"
        );
        // garnish, not a gate
        assert!(extended_properties(b"junk").custom.is_empty());
    }

    #[test]
    fn core_properties_read() {
        let props = core_properties(&tracked_docx());
        assert_eq!(props.title.as_deref(), Some("Test Agreement"));
        assert_eq!(props.creator.as_deref(), Some("Jane Author"));
        assert_eq!(props.modified.as_deref(), Some("2026-02-03T04:05:06Z"));
        // garnish, not a gate: junk bytes yield defaults, never an error
        let junk = core_properties(b"not a docx");
        assert!(junk.title.is_none());
    }
}
