//! The loro containers: block tree, side maps and Peritext marks.
//! 
//! Container names and mark-key formats, the id-keyed maps holding comments, fields,
//! bookmarks, hyperlinks, images, media and style overrides, and the routines that
//! place and clear marks over a paragraph's text. Append lives here too, since it is
//! the primitive that builds the tree.

use super::*;

// ── append (used by tests + the agent peer) ──────────────────────────────────

/// Append a paragraph built from `runs` (with an optional paragraph style) to the block tree.
/// Caller commits.
pub fn append_paragraph(doc: &LoroDoc, runs: &[Run], style: Option<&str>) -> Result<TreeID> {
    let tree = doc.get_tree(BLOCKS);
    let id = tree.create(None)?;
    let meta = tree.get_meta(id)?;
    meta.insert("type", "p")?;
    if let Some(s) = style {
        meta.insert("style", s)?;
    }
    let text: LoroText = meta.insert_container("text", LoroText::new())?;
    write_runs(&text, runs)?;
    block_cache_invalidate(); // a block was appended to the body
    Ok(id)
}

/// Insert `runs` into a (fresh) paragraph text container, applying formatting + track marks.
pub(crate) fn write_runs(text: &LoroText, runs: &[Run]) -> Result<()> {
    append_runs(text, 0, runs)
}

/// Insert `runs` into a paragraph text container starting at codepoint `start`, applying formatting
/// + track marks. Used to write a fresh paragraph (`start == 0`) and to append one paragraph's runs
///   onto another when joining (`start == prev length`).
///
/// All text is inserted first, then marks are applied by range. Marking as we insert would let an
/// `After`-expanding mark (bold, ins) grow over the next run inserted at its right boundary; with
/// the text already complete, no boundary insert can perturb a mark.
pub(crate) fn append_runs(text: &LoroText, start: usize, runs: &[Run]) -> Result<()> {
    let mut ranges = Vec::with_capacity(runs.len());
    let mut pos = start;
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        text.insert(pos, &run.text)?;
        let n = run.text.chars().count();
        ranges.push((pos..pos + n, run));
        pos += n;
    }
    for (range, run) in ranges {
        if run.bold {
            text.mark(range.clone(), "b", true)?;
        }
        if run.italic {
            text.mark(range.clone(), "i", true)?;
        }
        if run.underline {
            text.mark(range.clone(), "u", true)?;
        }
        if run.strike {
            text.mark(range.clone(), "strike", true)?;
        }
        if let Some(sz) = run.size {
            text.mark(range.clone(), "sz", sz as i64)?;
        }
        if let Some(c) = &run.color {
            text.mark(range.clone(), "color", c.as_str())?;
        }
        if let Some(f) = &run.font {
            text.mark(range.clone(), "font", f.as_str())?;
        }
        if let Some(h) = &run.highlight {
            text.mark(range.clone(), "hl", h.as_str())?;
        }
        if let Some(v) = &run.vert_align {
            text.mark(range.clone(), "va", v.as_str())?;
        }
        if let Some(l) = &run.lang {
            text.mark(range.clone(), "lang", l.as_str())?;
        }
        if let Some(cs) = &run.char_style {
            text.mark(range.clone(), "rstyle", cs.as_str())?;
        }
        if let Some(s) = &run.shading {
            text.mark(range.clone(), "rshd", s.as_str())?;
        }
        if let Some(t) = &run.track {
            mark_track(text, range.clone(), t)?;
        }
        if let Some(fc) = &run.fmt_change {
            mark_fmt_change(text, range.clone(), fc)?;
        }
        for id in &run.comments {
            text.mark(range.clone(), &comment_mark_key(*id), true)?;
        }
        if let Some(id) = run.image {
            text.mark(range.clone(), &image_mark_key(id), true)?;
        }
    }
    Ok(())
}

/// The Peritext mark key for a comment's anchored range (`cmt~{id}`). One key per comment so
/// overlapping / nested comment anchors don't collide (a single shared key would clobber). Configured
/// `ExpandType::None` (an anchor is a fixed range - typing at its boundary must not extend it).
pub fn comment_mark_key(id: u64) -> String {
    format!("cmt~{id}")
}

/// The Peritext mark key for an OOXML field's cached-result range (`fld~{id}`). One key per field
/// (configured `ExpandType::None` - a field result is a fixed range), mirroring [`comment_mark_key`].
pub fn field_mark_key(id: u64) -> String {
    format!("fld~{id}")
}

/// The loro map container holding field *instructions* (`TOC \o "1-3" \h ...`), keyed by field id (as
/// a string). A field's result *range* is a `fld~{id}` Peritext mark on the body text (see
/// [`Run::field`]); the instruction lives here, document-level, so it syncs + merges like any edit.
pub const FIELDS: &str = "fields";

/// Read every field's instruction from the `fields` map: id -> instruction string.
pub fn read_fields(doc: &LoroDoc) -> std::collections::HashMap<u64, String> {
    read_string_map(doc, FIELDS)
}

/// Store field `id`'s instruction in the `fields` map. Caller commits.
pub fn write_field(doc: &LoroDoc, id: u64, instr: &str) -> Result<()> {
    doc.get_map(FIELDS).insert(&id.to_string(), instr)?;
    Ok(())
}

/// Remove field `id`'s instruction from the `fields` map (its result-range marks are cleared by
/// deleting the marked paragraphs). Used when regenerating a TOC. Caller commits.
pub fn delete_field(doc: &LoroDoc, id: u64) -> Result<()> {
    doc.get_map(FIELDS).delete(&id.to_string())?;
    Ok(())
}

/// The Peritext mark key for a bookmark's range (`bkm~{id}`, expand `None`), mirroring
/// [`comment_mark_key`].
pub fn bookmark_mark_key(id: u64) -> String {
    format!("bkm~{id}")
}

/// The Peritext mark key anchoring a **collapsed** bookmark (`bkp~{id}`, expand `None`): the
/// bookmark sits immediately *before* the marked codepoint. See [`mark_bookmark_point`].
pub fn point_bookmark_mark_key(id: u64) -> String {
    format!("bkp~{id}")
}

/// The Peritext mark key anchoring a collapsed bookmark that sits *after* the marked codepoint
/// (`bkpe~{id}`, expand `None`) - used when it falls past the paragraph's last codepoint, so there is
/// nothing left to sit before. See [`mark_bookmark_point`].
pub fn end_point_bookmark_mark_key(id: u64) -> String {
    format!("bkpe~{id}")
}

/// The Peritext mark key for a hyperlink's range (`lnk~{id}`, expand `None`).
pub fn link_mark_key(id: u64) -> String {
    format!("lnk~{id}")
}

/// The loro map of runtime-synthesized list definitions, keyed by `numId` (as a string); the value is
/// the list's level-0 `w:numFmt` token (e.g. `"bullet"` / `"decimal"`). A list created at runtime (the
/// editor's Bullets / Numbering buttons, or the agent's `add_list`) is synthesized rather than imported,
/// so it is NOT in any `numbering.xml` part. Persisting its identity here makes it a loro citizen: it
/// survives a reopen (op-log replay), syncs to collaborators, and rebuilds live - not just on export.
/// Only the KIND is stored; [`build_list_levels`] regenerates the full level table from it. Imported
/// `numbering.xml` definitions never land here (they ride the source-parts re-attach path) so their ids
/// can't collide - and synthesized ids use a high base ([`SYNTH_NUM_BASE`]) for belt-and-suspenders.
pub const NUM_SYNTH: &str = "num_synth";

/// The floor for a runtime-synthesized list's `numId`, chosen high enough that it cannot collide with an
/// imported `numbering.xml`'s ids (Word documents number their lists from 1 upward; nothing reaches
/// this). A fresh synth id is `max(SYNTH_NUM_BASE + count, max_existing_synth_id + 1)`.
pub const SYNTH_NUM_BASE: i32 = 900_000;

/// The loro map of bookmark *names* (`w:bookmarkStart w:name`), keyed by bookmark id (as a string).
pub const BOOKMARKS: &str = "bookmarks";
/// The loro map of hyperlink *targets*, keyed by hyperlink id (as a string). A target is either an
/// internal anchor (`#bookmarkName`) or an external URL (`https://...`).
pub const HYPERLINKS: &str = "hyperlinks";

/// The loro map of runtime **style-definition edits** (Word's Modify-Style), keyed by style id. Each
/// value is a nested map of only the *changed* [`StyleProps`] fields (per-field override) - so two
/// peers editing different fields of one style merge cleanly, and the rest still inherit from the
/// import-parsed style / `basedOn` chain. Styles are otherwise in-memory (parsed read-only from
/// `styles.xml`); this map is their one CRDT-backed home, so an edit persists in the op-log, syncs
/// over `merge`, and participates in undo. The effective [`StyleTable`] is rebuilt from the parsed
/// base + this map on read (see `CollabDoc::styles` / [`StyleTable::apply_overrides`]). Mirrors the
/// [`NUM_SYNTH`] numbering pattern.
pub const STYLE_OVERRIDES: &str = "style_overrides";

/// The loro map of runtime-**added** paragraph styles (Word's New-Style / Save-Selection-as-a-Style),
/// keyed by the new style id. Each value is a nested map of the style's *identity* - `name` (the human
/// label) + `basedOn` (the parent style id, optional). The style's *formatting* rides in
/// [`STYLE_OVERRIDES`] under the same id (so an added style reconciles through the same path as an
/// edit). Reconciled into the effective table on read (see [`StyleTable::apply_added_styles`]). Type
/// is always `paragraph`.
pub const STYLE_ADDED: &str = "style_added";

/// Read the `bookmarks` map: id -> name.
pub fn read_bookmarks(doc: &LoroDoc) -> std::collections::HashMap<u64, String> {
    read_string_map(doc, BOOKMARKS)
}

/// Read the `hyperlinks` map: id -> target (`#anchor` or URL).
pub fn read_hyperlinks(doc: &LoroDoc) -> std::collections::HashMap<u64, String> {
    read_string_map(doc, HYPERLINKS)
}

/// Read a loro string-map (`key.parse::<u64>() -> String`), shared by the field / bookmark / hyperlink
/// maps.
fn read_string_map(doc: &LoroDoc, name: &str) -> std::collections::HashMap<u64, String> {
    let mut out = std::collections::HashMap::new();
    if let LoroValue::Map(m) = doc.get_map(name).get_value() {
        for (k, v) in m.iter() {
            if let (Ok(id), LoroValue::String(s)) = (k.parse::<u64>(), v) {
                out.insert(id, s.to_string());
            }
        }
    }
    out
}

/// Read the runtime-synthesized list definitions from the [`NUM_SYNTH`] loro map: `numId -> level-0
/// numFmt token`. The map is the persistent + synced source of truth for lists created at runtime; the
/// in-memory [`Numbering`] is rebuilt from it on load (see [`Numbering::reconcile_synth`]).
pub fn read_num_synth(doc: &LoroDoc) -> std::collections::HashMap<i32, String> {
    let mut out = std::collections::HashMap::new();
    if let LoroValue::Map(m) = doc.get_map(NUM_SYNTH).get_value() {
        for (k, v) in m.iter() {
            if let (Ok(id), LoroValue::String(s)) = (k.parse::<i32>(), v) {
                out.insert(id, s.to_string());
            }
        }
    }
    out
}

/// Record a runtime-synthesized list (`numId -> level-0 numFmt token`) in the [`NUM_SYNTH`] loro map, so
/// it persists in the op-log + syncs to peers. Caller commits.
pub fn write_num_synth(doc: &LoroDoc, num_id: i32, numfmt: &str) -> Result<()> {
    doc.get_map(NUM_SYNTH).insert(&num_id.to_string(), numfmt)?;
    Ok(())
}

/// Record a style-definition edit in the [`STYLE_OVERRIDES`] map: merge `props`' *set* fields into
/// style `id`'s per-field override (a nested map). Only `Some(_)` fields are written, so a later edit
/// of a different field doesn't clobber this one and unset fields keep inheriting from the parsed
/// base. `Some(false)` is an explicit "off" (e.g. not-bold) and is recorded; `None` means "inherit"
/// and is left absent. Caller commits.
pub fn write_style_override(doc: &LoroDoc, id: &str, props: &StyleProps) -> Result<()> {
    let m: LoroMap = doc.get_map(STYLE_OVERRIDES).get_or_create_container(id, LoroMap::new())?;
    if let Some(v) = props.size {
        m.insert("sz", v as i64)?;
    }
    if let Some(v) = props.bold {
        m.insert("b", v)?;
    }
    if let Some(v) = props.italic {
        m.insert("i", v)?;
    }
    if let Some(v) = &props.color {
        m.insert("color", v.as_str())?;
    }
    if let Some(v) = &props.highlight {
        m.insert("hl", v.as_str())?;
    }
    if let Some(v) = &props.font {
        m.insert("font", v.as_str())?;
    }
    if let Some(v) = props.line_spacing {
        m.insert("line", v as i64)?;
        // The rule travels with its value (matching StyleProps::overlay); "auto" when unset.
        m.insert("lrule", props.line_rule.map(|r| r.as_str()).unwrap_or("auto"))?;
    }
    if let Some(v) = props.space_before {
        m.insert("before", v as i64)?;
    }
    if let Some(v) = props.space_after {
        m.insert("after", v as i64)?;
    }
    if let Some(v) = props.num_id {
        m.insert("numid", v as i64)?;
    }
    if let Some(v) = props.num_ilvl {
        m.insert("ilvl", v as i64)?;
    }
    if let Some(v) = props.keep_next {
        m.insert("keepnext", v)?;
    }
    if let Some(v) = props.contextual_spacing {
        m.insert("ctxsp", v)?;
    }
    if let Some(v) = props.align {
        m.insert("align", v.as_str())?;
    }
    if let Some(v) = props.page_break_before {
        m.insert("pbb", v)?;
    }
    Ok(())
}

/// Read every style-definition edit from the [`STYLE_OVERRIDES`] map: style id -> the [`StyleProps`]
/// of its overridden fields (unset fields stay `None` = inherit). Each value is a nested map opened
/// via the container API, mirroring [`read_images`].
pub fn read_style_overrides(doc: &LoroDoc) -> std::collections::HashMap<String, StyleProps> {
    let map = doc.get_map(STYLE_OVERRIDES);
    let mut out = std::collections::HashMap::new();
    let LoroValue::Map(snapshot) = map.get_value() else { return out };
    let s = |m: &LoroMap, k: &str| match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::String(v))) => Some(v.to_string()),
        _ => None,
    };
    let n = |m: &LoroMap, k: &str| match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::I64(v))) => Some(v),
        _ => None,
    };
    let b = |m: &LoroMap, k: &str| match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::Bool(v))) => Some(v),
        _ => None,
    };
    for k in snapshot.keys() {
        let Some(ValueOrContainer::Container(Container::Map(om))) = map.get(k) else { continue };
        out.insert(
            k.to_string(),
            StyleProps {
                size: n(&om, "sz").map(|v| v as u16),
                bold: b(&om, "b"),
                italic: b(&om, "i"),
                color: s(&om, "color"),
                highlight: s(&om, "hl"),
                font: s(&om, "font"),
                line_spacing: n(&om, "line").map(|v| v as u16),
                line_rule: s(&om, "lrule").and_then(|r| LineRule::from_ooxml(&r)),
                space_before: n(&om, "before").map(|v| v as u32),
                space_after: n(&om, "after").map(|v| v as u32),
                num_id: n(&om, "numid").map(|v| v as i32),
                num_ilvl: n(&om, "ilvl").map(|v| v as i32),
                border: None, // runtime style edits don't touch the border box
                indent_left: None,
                indent_right: None,
                tab_stops: Vec::new(),
                tab_kinds: Vec::new(),
                tab_clears: Vec::new(),
                keep_next: b(&om, "keepnext"),
                contextual_spacing: b(&om, "ctxsp"),
                align: s(&om, "align").as_deref().and_then(Align::parse),
                page_break_before: b(&om, "pbb"),
            },
        );
    }
    out
}

/// The identity of a runtime-added paragraph style: its human `name` and optional `based_on` parent.
/// Its formatting lives in [`STYLE_OVERRIDES`] under the same id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedStyle {
    pub name: String,
    pub based_on: Option<String>,
}

/// Record a runtime-added paragraph style's identity (`name` + optional `based_on`) in the
/// [`STYLE_ADDED`] map. The formatting is written separately via [`write_style_override`]. Caller
/// commits.
pub fn write_added_style(doc: &LoroDoc, id: &str, name: &str, based_on: Option<&str>) -> Result<()> {
    let m: LoroMap = doc.get_map(STYLE_ADDED).get_or_create_container(id, LoroMap::new())?;
    m.insert("name", name)?;
    if let Some(b) = based_on {
        m.insert("basedOn", b)?;
    }
    Ok(())
}

/// Read every runtime-added paragraph style's identity from the [`STYLE_ADDED`] map: id -> name +
/// optional `basedOn`.
pub fn read_added_styles(doc: &LoroDoc) -> std::collections::HashMap<String, AddedStyle> {
    let map = doc.get_map(STYLE_ADDED);
    let mut out = std::collections::HashMap::new();
    let LoroValue::Map(snapshot) = map.get_value() else { return out };
    let s = |m: &LoroMap, k: &str| match m.get(k) {
        Some(ValueOrContainer::Value(LoroValue::String(v))) => Some(v.to_string()),
        _ => None,
    };
    for k in snapshot.keys() {
        let Some(ValueOrContainer::Container(Container::Map(am))) = map.get(k) else { continue };
        let Some(name) = s(&am, "name") else { continue };
        out.insert(k.to_string(), AddedStyle { name, based_on: s(&am, "basedOn") });
    }
    out
}

/// Store bookmark `id`'s name in the `bookmarks` map. Caller commits.
pub fn write_bookmark(doc: &LoroDoc, id: u64, name: &str) -> Result<()> {
    doc.get_map(BOOKMARKS).insert(&id.to_string(), name)?;
    Ok(())
}

/// Store hyperlink `id`'s target (`#anchor` or URL) in the `hyperlinks` map. Caller commits.
pub fn write_hyperlink(doc: &LoroDoc, id: u64, target: &str) -> Result<()> {
    doc.get_map(HYPERLINKS).insert(&id.to_string(), target)?;
    Ok(())
}

// ── comments (document-level annotations, stored in a loro map) ───────────────

/// The loro map container holding comment bodies + thread state, keyed by comment id (as a string).
/// Comments live here (not per-paragraph) because their *anchor* is a Peritext mark on the body text
/// while their *body* is document-level; keeping them in a CRDT map means they sync + merge like any
/// other edit (so an agent can comment too).
pub const COMMENTS: &str = "comments";

/// A deterministic `w14:paraId` for line `line` of a comment's body (8 hex digits). Derived from the
/// id rather than random so export is reproducible (the engine never invents randomness) and
/// round-trips stably. `line == 0` is the comment's canonical paraId (referenced by `commentsExtended`
/// for threading); up to 16 body lines per comment stay collision-free.
pub(crate) fn comment_para_id(id: u64, line: u64) -> String {
    format!("{:08X}", 0x1000_0000u64.wrapping_add(id.wrapping_mul(16)).wrapping_add(line))
}

/// Read every comment from the `comments` map, sorted by id.
pub fn read_comments(doc: &LoroDoc) -> Vec<Comment> {
    let map = doc.get_map(COMMENTS);
    let mut out = Vec::new();
    if let LoroValue::Map(m) = map.get_value() {
        for v in m.values() {
            if let LoroValue::String(s) = v
                && let Some(c) = comment_from_json(s) {
                    out.push(c);
                }
        }
    }
    out.sort_by_key(|c| c.id);
    out
}

/// Every comment id present in the document (for configuring the `cmt~{id}` mark keys).
pub fn comment_ids(doc: &LoroDoc) -> Vec<u64> {
    read_comments(doc).into_iter().map(|c| c.id).collect()
}

/// Write (insert or overwrite) one comment into the `comments` map. Caller commits.
pub fn write_comment(doc: &LoroDoc, c: &Comment) -> Result<()> {
    let map = doc.get_map(COMMENTS);
    map.insert(c.id.to_string().as_str(), comment_to_json(c))?;
    Ok(())
}

/// Remove a comment's body from the `comments` map (its anchor marks are cleared separately). Caller
/// commits.
pub fn delete_comment_entry(doc: &LoroDoc, id: u64) -> Result<()> {
    doc.get_map(COMMENTS).delete(id.to_string().as_str())?;
    Ok(())
}

/// JSON encoding of a comment's body + thread state (the value stored in the `comments` map).
fn comment_to_json(c: &Comment) -> String {
    serde_json::json!({
        "id": c.id,
        "author": c.author,
        "initials": c.initials,
        "date": c.date,
        "parent": c.parent,
        "resolved": c.resolved,
        "text": c.text,
    })
    .to_string()
}

/// Parse a comment from its stored JSON value.
fn comment_from_json(raw: &str) -> Option<Comment> {
    let j: Json = serde_json::from_str(raw).ok()?;
    Some(Comment {
        id: j.get("id").and_then(|v| v.as_u64())?,
        author: j.get("author").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        initials: j.get("initials").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        date: j.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        parent: j.get("parent").and_then(|v| v.as_u64()),
        resolved: j.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false),
        text: j.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    })
}

/// Mark codepoint `[start, end)` in paragraph `para` as anchored to comment `id` (a `cmt~{id}` mark).
/// The key must already be configured (see `configure_text_styles`). Caller commits.
pub fn mark_comment_range(doc: &LoroDoc, id: u64, para: usize, start: usize, end: usize) -> Result<()> {
    if end > start {
        nth_block_text(doc, para)?.mark(start..end, &comment_mark_key(id), true)?;
    }
    Ok(())
}

/// Mark codepoint `[start, end)` in paragraph `para` as part of field `id`'s cached result (a
/// `fld~{id}` mark). The key must already be configured. Caller commits.
pub fn mark_field_range(doc: &LoroDoc, id: u64, para: usize, start: usize, end: usize) -> Result<()> {
    if end > start {
        nth_block_text(doc, para)?.mark(start..end, &field_mark_key(id), true)?;
    }
    Ok(())
}

/// Mark codepoint `[start, end)` in paragraph `para` as image `id` (an `img~{id}` mark). The key must
/// already be configured. Caller commits. Used by the insert-image op (the placeholder is one char).
pub fn mark_image_range(doc: &LoroDoc, id: u64, para: usize, start: usize, end: usize) -> Result<()> {
    if end > start {
        nth_block_text(doc, para)?.mark(start..end, &image_mark_key(id), true)?;
    }
    Ok(())
}

/// Insert an image placeholder (`U+FFFC`) at the end of paragraph `para` and mark it `img~{id}` - the
/// anchor for an imported `<w:drawing>`. The `img~{id}` style must already be configured. Caller commits.
pub fn insert_image_placeholder(doc: &LoroDoc, id: u64, para: usize) -> Result<()> {
    let text = nth_block_text(doc, para)?;
    let at = text.to_string().chars().count();
    text.insert(at, &IMAGE_PLACEHOLDER.to_string())?;
    text.mark(at..at + 1, &image_mark_key(id), true)?;
    Ok(())
}

/// Like [`insert_image_placeholder`] but the placeholder run also carries a tracked-change mark
/// (`w:ins`/`w:del`), so an imported picture inside a `w:ins`/`w:del` keeps its redline. The track's
/// key must already be configured (it always is - the four track keys are fixed). Caller commits.
pub fn insert_image_placeholder_tracked(doc: &LoroDoc, id: u64, para: usize, track: &Track) -> Result<()> {
    let text = nth_block_text(doc, para)?;
    let at = text.to_string().chars().count();
    text.insert(at, &IMAGE_PLACEHOLDER.to_string())?;
    text.mark(at..at + 1, &image_mark_key(id), true)?;
    mark_track(&text, at..at + 1, track)
}

/// Mark codepoint `[start, end)` in paragraph `para` as bookmark `id` (a `bkm~{id}` mark). Caller commits.
///
/// A **collapsed** bookmark (`end == start`) covers no codepoints, so a range mark cannot hold it -
/// which is how `_Ref…` cross-reference targets, `_MON_…` object anchors and named form-field
/// bookmarks used to vanish on save. Those go to [`mark_bookmark_point`] instead.
pub fn mark_bookmark_range(doc: &LoroDoc, id: u64, para: usize, start: usize, end: usize) -> Result<()> {
    if end > start {
        nth_block_text(doc, para)?.mark(start..end, &bookmark_mark_key(id), true)?;
    }
    Ok(())
}

/// Anchor a **collapsed** bookmark (`<w:bookmarkStart/><w:bookmarkEnd/>` with nothing between) at
/// codepoint `off` in paragraph `para`.
///
/// There is no zero-width mark, so the anchor is a `bkp~{id}` mark on the codepoint the bookmark sits
/// *before*. Export re-emits the start and end adjacent ahead of that run, reproducing the collapsed
/// pair exactly rather than widening it to span the character - and because it is a mark, the anchor
/// travels with the text through edits and merges like any other.
///
/// Past the last codepoint there is nothing to sit *before*, so the bookmark anchors to the final
/// codepoint with [`end_point_bookmark_mark_key`] and export emits it *after* that run instead. This
/// is the common shape when the bookmark precedes something the model does not represent as text -
/// an OMML formula, say - because nothing modeled follows it in the paragraph.
///
/// Returns `false` only for an empty paragraph, where there is no codepoint of any kind to hold the
/// anchor.
pub fn mark_bookmark_point(doc: &LoroDoc, id: u64, para: usize, off: usize, len: usize) -> Result<bool> {
    if len == 0 {
        return Ok(false);
    }
    let text = nth_block_text(doc, para)?;
    if off >= len {
        text.mark(len - 1..len, &end_point_bookmark_mark_key(id), true)?;
    } else {
        text.mark(off..off + 1, &point_bookmark_mark_key(id), true)?;
    }
    Ok(true)
}

/// Mark codepoint `[start, end)` in paragraph `para` as hyperlink `id` (a `lnk~{id}` mark). Caller commits.
pub fn mark_link_range(doc: &LoroDoc, id: u64, para: usize, start: usize, end: usize) -> Result<()> {
    if end > start {
        nth_block_text(doc, para)?.mark(start..end, &link_mark_key(id), true)?;
    }
    Ok(())
}

/// Clear every `lnk~{id}` mark for hyperlink `id` across the document (used when removing it). Caller commits.
pub fn clear_link_marks(doc: &LoroDoc, id: u64) -> Result<()> {
    let key = link_mark_key(id);
    for (pi, para) in read_paragraphs(doc)?.iter().enumerate() {
        if para.runs.iter().any(|r| r.link == Some(id)) {
            let len: usize = para.runs.iter().map(|r| r.text.chars().count()).sum();
            nth_block_text(doc, pi)?.unmark(0..len, &key)?;
        }
    }
    Ok(())
}

/// Remove hyperlink `id`'s target from the `hyperlinks` map. Caller commits.
pub fn delete_hyperlink(doc: &LoroDoc, id: u64) -> Result<()> {
    doc.get_map(HYPERLINKS).delete(&id.to_string())?;
    Ok(())
}

// ── images (editable: w:drawing as a run-anchored picture) ───────────────────────────────────────

/// The Peritext mark key for a picture's anchor run (`img~{id}`, expand `None`), mirroring
/// [`link_mark_key`]. The run is a single placeholder char; its position is the picture's anchor point.
pub fn image_mark_key(id: u64) -> String {
    format!("img~{id}")
}

/// The single placeholder character a picture's anchor run carries (`U+FFFC` OBJECT REPLACEMENT
/// CHARACTER) - one codepoint that stands in for the drawing in the text flow.
pub const IMAGE_PLACEHOLDER: char = '\u{FFFC}';

/// The Peritext mark key for a verbatim-passthrough run (`raw~{id}`, expand `None`), mirroring
/// [`image_mark_key`]. The run is a single [`IMAGE_PLACEHOLDER`] char standing in for the embedded
/// object; the captured XML lives in the [`RAWXML`] map keyed by `id`. See `docs/passthrough.md`.
pub fn raw_mark_key(id: u64) -> String {
    format!("raw~{id}")
}

/// The loro map of **verbatim passthrough** XML (id -> the captured `<w:r>...</w:r>` string), keyed by
/// id as a string. Its anchor is a `raw~{id}` mark on a placeholder run; the bytes are re-emitted
/// unchanged on export so an unmodeled embedded object round-trips. Document-level so it syncs / merges.
/// Loro root map holding document-level section state that is not a paragraph property: key
/// `"final"` -> the verbatim inner XML of the body-final `<w:sectPr>` (the last section's
/// properties). In-paragraph section breaks live on their carrier paragraph ([`ParaProps::sect_pr`]).
pub const SECTPR: &str = "sectpr";

pub const RAWXML: &str = "rawxml";

/// Store a passthrough run's captured XML under `id`. Caller commits.
pub fn write_raw(doc: &LoroDoc, id: u64, xml: &str) -> Result<()> {
    doc.get_map(RAWXML).insert(&id.to_string(), xml)?;
    Ok(())
}

/// Read every passthrough entry: id -> captured `<w:r>...</w:r>` XML.
pub fn read_raw(doc: &LoroDoc) -> std::collections::HashMap<u64, String> {
    let map = doc.get_map(RAWXML);
    let mut out = std::collections::HashMap::new();
    let LoroValue::Map(snapshot) = map.get_value() else { return out };
    for k in snapshot.keys() {
        let Ok(id) = k.parse::<u64>() else { continue };
        if let Some(ValueOrContainer::Value(LoroValue::String(v))) = map.get(k) {
            out.insert(id, v.to_string());
        }
    }
    out
}

/// Insert a passthrough placeholder run (a single [`IMAGE_PLACEHOLDER`] char carrying `raw~{id}`) at
/// codepoint `at` in paragraph `para` - the anchor for a captured embedded object.
///
/// `at` is where the captured run actually sat, not the end of the paragraph. Appending instead
/// *moved* the object: `BEFORE | object | AFTER` came back as `BEFOREAFTER | object`, with the text
/// either side merged across the gap. That went unnoticed because the objects the capture whitelist
/// catches - OLE, charts, shapes, content controls - nearly always occupy a paragraph of their own,
/// where the two positions coincide.
///
/// Callers insert in ascending `at` order per paragraph and offset by the placeholders already
/// placed there, since each insertion shifts the ones after it. The mark key must already be
/// configured. Caller commits.
pub fn insert_raw_placeholder(doc: &LoroDoc, id: u64, para: usize, at: usize) -> Result<()> {
    let text = nth_block_text(doc, para)?;
    let at = at.min(text.to_string().chars().count());
    text.insert(at, &IMAGE_PLACEHOLDER.to_string())?;
    text.mark(at..at + 1, &raw_mark_key(id), true)?;
    Ok(())
}

/// Like [`insert_raw_placeholder`] but the placeholder run also carries a tracked-change mark
/// (`w:ins`/`w:del`), so an imported embedded object inside a revision keeps its redline and resolves
/// through accept/reject like any tracked run. The track's key is always configured. Caller commits.
pub fn insert_raw_placeholder_tracked(
    doc: &LoroDoc,
    id: u64,
    para: usize,
    at: usize,
    track: &Track,
) -> Result<()> {
    let text = nth_block_text(doc, para)?;
    let at = at.min(text.to_string().chars().count());
    text.insert(at, &IMAGE_PLACEHOLDER.to_string())?;
    text.mark(at..at + 1, &raw_mark_key(id), true)?;
    mark_track(&text, at..at + 1, track)
}

/// The loro map of **block-level wrapper** openings (id -> the captured verbatim prefix XML - a
/// `<w:sdt>…<w:sdtContent>` or `<w:customXml…>` opening). The wrapper's *anchor* is a `wrapopen`/
/// `wrapclose` id list on the enclosed block nodes' meta ([`set_block_wrap_anchors`]); the prefix lives
/// here, document-level, so a `<w:sdt>` content control round-trips while its content stays editable.
/// The closing tag is derived from the prefix (custom-XML vs sdt). See `docs/passthrough.md`.
pub const BLOCKWRAP: &str = "blockwrap";

/// Store a block wrapper's captured opening XML under `id`. Caller commits.
pub fn write_block_wrap(doc: &LoroDoc, id: u64, prefix: &str) -> Result<()> {
    doc.get_map(BLOCKWRAP).insert(&id.to_string(), prefix)?;
    Ok(())
}

/// Read every block wrapper: id -> captured opening XML.
pub fn read_block_wraps(doc: &LoroDoc) -> std::collections::HashMap<u64, String> {
    let map = doc.get_map(BLOCKWRAP);
    let mut out = std::collections::HashMap::new();
    let LoroValue::Map(snapshot) = map.get_value() else { return out };
    for k in snapshot.keys() {
        let Ok(id) = k.parse::<u64>() else { continue };
        if let Some(ValueOrContainer::Value(LoroValue::String(v))) = map.get(k) {
            out.insert(id, v.to_string());
        }
    }
    out
}

/// The closing tag for a block wrapper, derived from its captured opening (a `<w:customXml>` closes
/// with `</w:customXml>`; an `<w:sdt>` with `</w:sdtContent></w:sdt>`).
pub fn block_wrap_suffix(prefix: &str) -> &'static str {
    if prefix.trim_start().starts_with("<w:customXml") {
        "</w:customXml>"
    } else {
        "</w:sdtContent></w:sdt>"
    }
}

/// Record, on a block node's meta map, the wrapper ids that **open** at this node (in outer-first order)
/// and **close** at it (inner-first order), as comma-separated id strings. Read back by
/// [`block_wrap_ids`] during export. Empty lists write nothing (the common no-wrapper case).
pub fn set_block_wrap_anchors(meta: &LoroMap, opens: &[u64], closes: &[u64]) -> Result<()> {
    let join = |ids: &[u64]| ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    if !opens.is_empty() {
        meta.insert("wrapopen", join(opens))?;
    }
    if !closes.is_empty() {
        meta.insert("wrapclose", join(closes))?;
    }
    Ok(())
}

/// The wrapper ids listed under `key` (`wrapopen` / `wrapclose`) on a block node's meta, in stored order.
pub fn block_wrap_ids(meta: &LoroMap, key: &str) -> Vec<u64> {
    meta_string(meta, key)
        .map(|s| s.split(',').filter_map(|p| p.parse::<u64>().ok()).collect())
        .unwrap_or_default()
}

/// The loro map of picture placements ([`ImagePlacement`]), keyed by image id (as a string). The
/// picture's *anchor* is an `img~{id}` mark on a placeholder run; its geometry / crop / placement live
/// here, document-level, so they sync + merge (LWW per field - a concurrent resize + crop converge).
pub const IMAGES: &str = "images";

/// A picture's media reference + size + crop + placement - the editable record behind a
/// `w:drawing`. Both inline (`wp:inline`) and
/// floating (`wp:anchor`) pictures use this; `floating` distinguishes them and the position / wrap
/// fields apply only when floating. Sizes + offsets are EMU (914400 / inch); crop is thousandths of a
/// percent (`<a:srcRect>` l/t/r/b, 0..100000).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImagePlacement {
    /// The media part key (e.g. `image1.png`), resolved to `word/media/{media}` on save.
    pub media: String,
    pub w_emu: i64,
    pub h_emu: i64,
    pub crop_l: i64,
    pub crop_t: i64,
    pub crop_r: i64,
    pub crop_b: i64,
    /// `false` = inline (`wp:inline`, in the text flow); `true` = floating (`wp:anchor`, positioned).
    pub floating: bool,
    /// `behindDoc` - the picture paints under the text (floating only).
    pub behind: bool,
    /// `wp:positionH/V relativeFrom` (column / page / margin / ...), floating only.
    pub h_from: String,
    pub v_from: String,
    /// `wp:posOffset` (EMU), floating only - used when there's no `wp:align`.
    pub x_emu: i64,
    pub y_emu: i64,
    /// `wp:align` (left/center/right or top/bottom/center), floating only.
    pub h_align: String,
    pub v_align: String,
    /// Wrap type: `square` / `tight` / `topAndBottom` / `through` / `none` (floating only).
    pub wrap: String,
}

/// Store picture `id`'s placement in the `images` map (a nested map, one key per field, so a concurrent
/// resize + crop converge). Caller commits.
pub fn write_image(doc: &LoroDoc, id: u64, p: &ImagePlacement) -> Result<()> {
    let m: LoroMap = doc.get_map(IMAGES).get_or_create_container(&id.to_string(), LoroMap::new())?;
    m.insert("media", p.media.as_str())?;
    m.insert("w", p.w_emu)?;
    m.insert("h", p.h_emu)?;
    m.insert("cl", p.crop_l)?;
    m.insert("ct", p.crop_t)?;
    m.insert("cr", p.crop_r)?;
    m.insert("cb", p.crop_b)?;
    m.insert("float", p.floating)?;
    m.insert("behind", p.behind)?;
    m.insert("hfrom", p.h_from.as_str())?;
    m.insert("vfrom", p.v_from.as_str())?;
    m.insert("x", p.x_emu)?;
    m.insert("y", p.y_emu)?;
    m.insert("halign", p.h_align.as_str())?;
    m.insert("valign", p.v_align.as_str())?;
    m.insert("wrap", p.wrap.as_str())?;
    Ok(())
}

/// Read every picture placement from the `images` map: id -> [`ImagePlacement`]. Each value is a nested
/// map container, opened via the container API (the value snapshot materializes nested containers
/// shallowly).
pub fn read_images(doc: &LoroDoc) -> std::collections::HashMap<u64, ImagePlacement> {
    let map = doc.get_map(IMAGES);
    let mut out = std::collections::HashMap::new();
    let LoroValue::Map(snapshot) = map.get_value() else { return out };
    let s = |im: &LoroMap, k: &str| match im.get(k) {
        Some(ValueOrContainer::Value(LoroValue::String(v))) => v.to_string(),
        _ => String::new(),
    };
    let n = |im: &LoroMap, k: &str| match im.get(k) {
        Some(ValueOrContainer::Value(LoroValue::I64(v))) => v,
        _ => 0,
    };
    let b = |im: &LoroMap, k: &str| matches!(im.get(k), Some(ValueOrContainer::Value(LoroValue::Bool(true))));
    for k in snapshot.keys() {
        let Ok(id) = k.parse::<u64>() else { continue };
        let Some(ValueOrContainer::Container(Container::Map(im))) = map.get(k) else { continue };
        out.insert(
            id,
            ImagePlacement {
                media: s(&im, "media"),
                w_emu: n(&im, "w"),
                h_emu: n(&im, "h"),
                crop_l: n(&im, "cl"),
                crop_t: n(&im, "ct"),
                crop_r: n(&im, "cr"),
                crop_b: n(&im, "cb"),
                floating: b(&im, "float"),
                behind: b(&im, "behind"),
                h_from: s(&im, "hfrom"),
                v_from: s(&im, "vfrom"),
                x_emu: n(&im, "x"),
                y_emu: n(&im, "y"),
                h_align: s(&im, "halign"),
                v_align: s(&im, "valign"),
                wrap: s(&im, "wrap"),
            },
        );
    }
    out
}

/// One picture placement by id, or `None`.
pub fn read_image(doc: &LoroDoc, id: u64) -> Option<ImagePlacement> {
    read_images(doc).remove(&id)
}

/// Remove picture `id`'s placement from the `images` map (its anchor mark is cleared by deleting the
/// placeholder run). Caller commits.
pub fn delete_image(doc: &LoroDoc, id: u64) -> Result<()> {
    doc.get_map(IMAGES).delete(&id.to_string())?;
    Ok(())
}

/// The `media` map: inserted-picture bytes keyed by their media part name (e.g.
/// `word/media/image3.png`). Unlike `pending_media` (in-memory, this-session only), this lives in the
/// CRDT, so the bytes snapshot, replicate to peers, and survive a reopen-from-op-log. Imported pictures
/// keep their bytes in `source_parts`; only newly-inserted media goes here.
pub const MEDIA: &str = "media";

/// Store inserted picture `part`'s `bytes` in the `media` map. Caller commits.
pub fn write_media(doc: &LoroDoc, part: &str, bytes: &[u8]) -> Result<()> {
    doc.get_map(MEDIA).insert(part, LoroValue::Binary(bytes.to_vec().into()))?;
    Ok(())
}

/// Read inserted picture `part`'s bytes from the `media` map, if present.
pub fn read_media(doc: &LoroDoc, part: &str) -> Option<Vec<u8>> {
    match doc.get_map(MEDIA).get(part) {
        Some(ValueOrContainer::Value(LoroValue::Binary(b))) => Some(b.to_vec()),
        _ => None,
    }
}

/// Every inserted media part -> bytes (e.g. to enumerate used keys after a reopen).
pub fn read_all_media(doc: &LoroDoc) -> std::collections::HashMap<String, Vec<u8>> {
    let map = doc.get_map(MEDIA);
    let mut out = std::collections::HashMap::new();
    let LoroValue::Map(snapshot) = map.get_value() else { return out };
    for k in snapshot.keys() {
        if let Some(ValueOrContainer::Value(LoroValue::Binary(b))) = map.get(k) {
            out.insert(k.clone(), b.to_vec());
        }
    }
    out
}

/// Clear every `bkm~{id}` mark for bookmark `id` across the document (used when regenerating a TOC's
/// `_Toc*` anchors). Caller commits.
pub fn clear_bookmark_marks(doc: &LoroDoc, id: u64) -> Result<()> {
    let key = bookmark_mark_key(id);
    for (pi, para) in read_paragraphs(doc)?.iter().enumerate() {
        if para.runs.iter().any(|r| r.bookmarks.contains(&id)) {
            let len: usize = para.runs.iter().map(|r| r.text.chars().count()).sum();
            nth_block_text(doc, pi)?.unmark(0..len, &key)?;
        }
    }
    Ok(())
}

/// Remove bookmark `id`'s name from the `bookmarks` map (its range marks are cleared separately via
/// [`clear_bookmark_marks`]). Caller commits.
pub fn delete_bookmark(doc: &LoroDoc, id: u64) -> Result<()> {
    doc.get_map(BOOKMARKS).delete(&id.to_string())?;
    Ok(())
}

/// Clear every `cmt~{id}` anchor mark for comment `id` across the document (used when deleting it).
/// Caller commits.
pub fn clear_comment_marks(doc: &LoroDoc, id: u64) -> Result<()> {
    let key = comment_mark_key(id);
    for (pi, para) in read_paragraphs(doc)?.iter().enumerate() {
        if para.runs.iter().any(|r| r.comments.contains(&id)) {
            let len: usize = para.runs.iter().map(|r| r.text.chars().count()).sum();
            nth_block_text(doc, pi)?.unmark(0..len, &key)?;
        }
    }
    Ok(())
}

/// The comment ids anchored at codepoint `off` in paragraph `idx` (the run under the caret, or one
/// ending exactly at it). Drives "what comments are here" for the popover. Sorted ascending.
pub fn comments_at(doc: &LoroDoc, idx: usize, off: usize) -> Result<Vec<u64>> {
    let paras = read_paragraphs(doc)?;
    let Some(para) = paras.get(idx) else { return Ok(Vec::new()) };
    let mut pos = 0usize;
    let mut hit: Vec<u64> = Vec::new();
    for run in &para.runs {
        let n = run.text.chars().count();
        let (rs, re) = (pos, pos + n);
        if (off >= rs && off < re) || (off == re && n > 0) {
            for id in &run.comments {
                if !hit.contains(id) {
                    hit.push(*id);
                }
            }
        }
        pos = re;
    }
    hit.sort_unstable();
    Ok(hit)
}

/// Apply a tracked-change mark over `range`, encoding `{author,date,id}` as the mark value.
pub(crate) fn mark_track(text: &LoroText, range: Range<usize>, track: &Track) -> Result<()> {
    let val =
        serde_json::json!({ "author": track.author, "date": track.date, "id": track.id })
            .to_string();
    text.mark(range, track.kind.mark_key(), val)?;
    Ok(())
}

/// The JSON encoding of a run-property snapshot (only set fields are emitted; absence = inherit).
fn props_json(p: &RunProps) -> Json {
    let mut m = serde_json::Map::new();
    m.insert("b".into(), Json::Bool(p.bold));
    m.insert("i".into(), Json::Bool(p.italic));
    m.insert("u".into(), Json::Bool(p.underline));
    m.insert("strike".into(), Json::Bool(p.strike));
    if let Some(s) = p.size {
        m.insert("sz".into(), Json::from(s));
    }
    if let Some(c) = &p.color {
        m.insert("color".into(), Json::from(c.as_str()));
    }
    if let Some(f) = &p.font {
        m.insert("font".into(), Json::from(f.as_str()));
    }
    if let Some(h) = &p.highlight {
        m.insert("hl".into(), Json::from(h.as_str()));
    }
    if let Some(v) = &p.vert_align {
        m.insert("va".into(), Json::from(v.as_str()));
    }
    if let Some(l) = &p.lang {
        m.insert("lang".into(), Json::from(l.as_str()));
    }
    Json::Object(m)
}

/// Apply a tracked run-property-change mark (`rfmt`) over `range`, encoding author/date/id + the old
/// props as the value (the CRDT form of `w:rPrChange`).
pub(crate) fn mark_fmt_change(text: &LoroText, range: Range<usize>, fc: &FormatChange) -> Result<()> {
    let val = serde_json::json!({
        "author": fc.author, "date": fc.date, "id": fc.id, "old": props_json(&fc.old),
    })
    .to_string();
    text.mark(range, "rfmt", val)?;
    Ok(())
}
