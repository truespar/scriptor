//! OPC package plumbing for save.
//! 
//! A `.docx` is a zip of parts wired together by relationship and content-type files.
//! Writing one back means more than regenerating `document.xml`: a new image needs a
//! relationship, a media part and possibly a content-type default, and every part the
//! model does not understand has to survive untouched. These are the small surgical
//! edits that keep the package coherent.

/// Replace (or add) a part's bytes by name.
pub(crate) fn set_part(parts: &mut Vec<scriptor_ooxml::Part>, name: &str, data: Vec<u8>) {
    if let Some(p) = parts.iter_mut().find(|p| p.name == name) {
        p.data = data;
    } else {
        parts.push(scriptor_ooxml::Part { name: name.to_string(), data });
    }
}

/// Register the comments + commentsExtended parts in `[Content_Types].xml` and `document.xml.rels`
/// (idempotent - a no-op for a document that already shipped with comments).
pub(crate) fn ensure_comment_parts_registered(parts: &mut Vec<scriptor_ooxml::Part>) {
    patch_content_types(
        parts,
        "/word/comments.xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
    );
    patch_content_types(
        parts,
        "/word/commentsExtended.xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml",
    );
    patch_doc_rels(
        parts,
        "comments.xml",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments",
    );
    patch_doc_rels(
        parts,
        "commentsExtended.xml",
        "http://schemas.microsoft.com/office/2011/relationships/commentsExtended",
    );
}

/// Add an `<Override>` for `part_name` to `[Content_Types].xml` if absent.
pub(crate) fn patch_content_types(parts: &mut [scriptor_ooxml::Part], part_name: &str, content_type: &str) {
    let Some(ct) = parts.iter_mut().find(|p| p.name == "[Content_Types].xml") else { return };
    let mut s = String::from_utf8_lossy(&ct.data).into_owned();
    if s.contains(&format!("PartName=\"{part_name}\"")) {
        return;
    }
    let ins = format!("<Override PartName=\"{part_name}\" ContentType=\"{content_type}\"/>");
    if let Some(pos) = s.rfind("</Types>") {
        s.insert_str(pos, &ins);
        ct.data = s.into_bytes();
    }
}

/// Add a `<Relationship>` from `document.xml` to `word/{target}` if no relationship targets it yet
/// (creating the rels part if missing). The new id is `rId{max existing + 1}`.
pub(crate) fn patch_doc_rels(parts: &mut Vec<scriptor_ooxml::Part>, target: &str, rel_type: &str) {
    let name = "word/_rels/document.xml.rels";
    if !parts.iter().any(|p| p.name == name) {
        set_part(parts, name, DOC_RELS_MIN.to_vec());
    }
    let Some(rels) = parts.iter_mut().find(|p| p.name == name) else { return };
    let mut s = String::from_utf8_lossy(&rels.data).into_owned();
    if s.contains(&format!("Target=\"{target}\"")) {
        return;
    }
    let next = max_rid(&s) + 1;
    let ins = format!("<Relationship Id=\"rId{next}\" Type=\"{rel_type}\" Target=\"{target}\"/>");
    if let Some(pos) = s.rfind("</Relationships>") {
        s.insert_str(pos, &ins);
        rels.data = s.into_bytes();
    }
}

/// Add a `<Relationship>` with a caller-supplied id from `document.xml` to `word/{target}` (creating
/// the rels part if missing), unless one already targets it. Used when the id was pre-allocated so the
/// matching `sectPr` reference in `document.xml` can carry the same id (header/footer materialization).
pub(crate) fn patch_doc_rels_with_id(
    parts: &mut Vec<scriptor_ooxml::Part>,
    r_id: &str,
    target: &str,
    rel_type: &str,
) {
    let name = "word/_rels/document.xml.rels";
    if !parts.iter().any(|p| p.name == name) {
        set_part(parts, name, DOC_RELS_MIN.to_vec());
    }
    let Some(rels) = parts.iter_mut().find(|p| p.name == name) else { return };
    let mut s = String::from_utf8_lossy(&rels.data).into_owned();
    if s.contains(&format!("Target=\"{target}\"")) {
        return;
    }
    let ins = format!("<Relationship Id=\"{r_id}\" Type=\"{rel_type}\" Target=\"{target}\"/>");
    if let Some(pos) = s.rfind("</Relationships>") {
        s.insert_str(pos, &ins);
        rels.data = s.into_bytes();
    }
}

/// Add an external relationship (`TargetMode="External"` - e.g. a hyperlink URL) to
/// `document.xml.rels`, keyed by `r_id` (idempotent on the id; the URL is escaped for an attribute).
pub(crate) fn patch_external_rel(parts: &mut Vec<scriptor_ooxml::Part>, r_id: &str, url: &str, rel_type: &str) {
    let name = "word/_rels/document.xml.rels";
    if !parts.iter().any(|p| p.name == name) {
        set_part(parts, name, DOC_RELS_MIN.to_vec());
    }
    let Some(rels) = parts.iter_mut().find(|p| p.name == name) else { return };
    let mut s = String::from_utf8_lossy(&rels.data).into_owned();
    if s.contains(&format!("Id=\"{r_id}\"")) {
        return;
    }
    let esc = url
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let ins =
        format!("<Relationship Id=\"{r_id}\" Type=\"{rel_type}\" Target=\"{esc}\" TargetMode=\"External\"/>");
    if let Some(pos) = s.rfind("</Relationships>") {
        s.insert_str(pos, &ins);
        rels.data = s.into_bytes();
    }
}

/// Add an **internal** `<Relationship>` (no `TargetMode`) keyed by `r_id` to `document.xml.rels` -
/// idempotent on the **id** (so several `rIdImg{id}` rels can target the same media part). Used to wire
/// each picture's blip (`r:embed="rIdImg{id}"`) to its `word/media` part on save.
pub(crate) fn patch_internal_rel(parts: &mut Vec<scriptor_ooxml::Part>, r_id: &str, target: &str, rel_type: &str) {
    patch_internal_rel_in(parts, "word/_rels/document.xml.rels", r_id, target, rel_type);
}

/// Add `r_id -> target` (an internal relationship of `rel_type`) to the named rels part, creating an
/// empty rels part if absent. Idempotent on `r_id`. Used to wire a picture's `rIdImg{id}` blip to its
/// `word/media` part - in `document.xml.rels` for the body, or a header/footer part's own rels.
pub(crate) fn patch_internal_rel_in(
    parts: &mut Vec<scriptor_ooxml::Part>,
    rels_name: &str,
    r_id: &str,
    target: &str,
    rel_type: &str,
) {
    if !parts.iter().any(|p| p.name == rels_name) {
        set_part(parts, rels_name, DOC_RELS_MIN.to_vec());
    }
    let Some(rels) = parts.iter_mut().find(|p| p.name == rels_name) else { return };
    let mut s = String::from_utf8_lossy(&rels.data).into_owned();
    if s.contains(&format!("Id=\"{r_id}\"")) {
        return;
    }
    let ins = format!("<Relationship Id=\"{r_id}\" Type=\"{rel_type}\" Target=\"{target}\"/>");
    if let Some(pos) = s.rfind("</Relationships>") {
        s.insert_str(pos, &ins);
        rels.data = s.into_bytes();
    }
}

/// Ensure `[Content_Types].xml` declares a `<Default Extension=.. ContentType=..>` for `ext` (idempotent).
/// Inserted right after the `<Types>` open so Defaults precede Overrides (the CT_Types schema order).
pub(crate) fn patch_content_type_default(parts: &mut [scriptor_ooxml::Part], ext: &str, content_type: &str) {
    let Some(ct) = parts.iter_mut().find(|p| p.name == "[Content_Types].xml") else { return };
    let mut s = String::from_utf8_lossy(&ct.data).into_owned();
    if s.contains(&format!("Extension=\"{ext}\"")) {
        return;
    }
    let ins = format!("<Default Extension=\"{ext}\" ContentType=\"{content_type}\"/>");
    if let Some(start) = s.find("<Types")
        && let Some(gt) = s[start..].find('>') {
            s.insert_str(start + gt + 1, &ins);
            ct.data = s.into_bytes();
        }
}

/// The OOXML content type for a media file extension (lower-cased), or `None` for an unknown one.
pub(crate) fn image_content_type(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "emf" => "image/x-emf",
        "wmf" => "image/x-wmf",
        _ => return None,
    })
}

/// The media file extension for a MIME type (for an inserted picture's `word/media/imageN.{ext}` key).
/// Falls back to `png` for an unrecognised type.
pub(crate) fn ext_for_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tif",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}

/// The largest `rId{N}` number in a rels XML string (0 if none).
pub(crate) fn max_rid(s: &str) -> u32 {
    s.split("rId")
        .skip(1)
        .filter_map(|tail| {
            let num: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            num.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// Relationship type for a picture's blip (`r:embed -> word/media/...`). Used for the body
/// (`patch_link_and_image_rels`) and the header/footer parts alike.
pub(crate) const IMAGE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

// Minimal OPC parts for saving a from-scratch document (Word opens these as a valid .docx).
pub(crate) const CONTENT_TYPES_MIN: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
pub(crate) const ROOT_RELS_MIN: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
pub(crate) const DOC_RELS_MIN: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
