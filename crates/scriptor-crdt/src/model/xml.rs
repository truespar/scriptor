//! quick-xml reading helpers shared by every part reader.
//!
//! Attribute lookup, the OOXML on/off attribute conventions, entity resolution, and the revision
//! attributes that appear on any tracked element. These are the leaves the rest of the model sits on.

use super::*;
/// Resolve a quick-xml `GeneralRef` (entity / character reference) back to its literal text.
/// quick-xml 0.38+ reports references separately from `Event::Text`; resolving them here keeps
/// document content intact across the round-trip (export re-escapes via `xml_escape`). Numeric
/// refs (`&#NN;` / `&#xNN;`) go through quick-xml; the five XML predefined entities are mapped
/// directly; any unknown named entity (Word does not emit these) is preserved verbatim.
pub(crate) fn resolve_reference(r: &quick_xml::events::BytesRef) -> Result<String> {
    if r.is_char_ref()
        && let Some(ch) = r.resolve_char_ref()? {
            return Ok(ch.to_string());
        }
    let name = r.decode()?;
    Ok(match name.as_ref() {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        other => format!("&{other};"),
    })
}

/// `w:u` (underline) is on unless its `w:val` is `none` (the value names the underline *style* -
/// "single", "double", ... - which we collapse to a boolean for now).
pub(crate) fn u_on(e: &quick_xml::events::BytesStart) -> bool {
    !matches!(attr(e, b"w:val").as_deref(), Some("none") | Some("false") | Some("0"))
}

/// `true` unless the element carries `w:val="false|0|none|off"` (Word's "explicitly off").
pub(crate) fn toggle_on(e: &quick_xml::events::BytesStart) -> bool {
    !matches!(attr(e, b"w:val").as_deref(), Some("false") | Some("0") | Some("none") | Some("off"))
}

/// Build a [`Track`] from a `w:ins`/`w:del` start tag's attributes.
pub(crate) fn revision_track(e: &quick_xml::events::BytesStart, kind: TrackKind) -> Option<Track> {
    Some(Track {
        kind,
        author: attr(e, b"w:author").unwrap_or_default(),
        date: attr(e, b"w:date").unwrap_or_default(),
        id: attr(e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

/// Read one (unescaped) attribute value by qualified name.
pub(crate) fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return a
                .normalized_value(quick_xml::XmlVersion::Explicit1_0)
                .ok()
                .map(|c| c.into_owned());
        }
    }
    None
}

pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Strip a leading UTF-8 BOM. quick-xml skips a BOM without counting it in `buffer_position`,
/// so a parser that byte-slices its input against reader positions (`parse_passthrough`,
/// `parse_block_wraps`) captured spans shifted 3 bytes left on BOM-prefixed parts - truncating
/// the closing `</w:r>` to `</w` and producing non-well-formed output that Word rejects
/// (the dashed_line_custdash_* corpus docs; LibreOffice writes BOMs on its XML parts).
pub(crate) fn strip_bom(xml: &[u8]) -> &[u8] {
    xml.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(xml)
}

/// ` w:date="…"` (with the leading space) when the revision carries a date, empty otherwise.
/// `w:date` is optional on every tracked-change element (`CT_TrackChange`), and an EMPTY value
/// is a schema violation - a source revision without a date must round-trip without the
/// attribute, not with `w:date=""` (193 corpus hits before this).
pub(crate) fn date_attr(date: &str) -> String {
    if date.is_empty() { String::new() } else { format!(" w:date=\"{}\"", xml_escape(date)) }
}
