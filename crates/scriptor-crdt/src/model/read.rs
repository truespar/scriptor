//! Materializing the model out of the CRDT.
//! 
//! Walks the block tree and turns it back into [`Paragraph`] values with their runs,
//! marks and properties resolved - the read side every caller sees, and the input to
//! both layout and export.

use super::*;

// ── read (materialize the model) ─────────────────────────────────────────────

/// Materialize every paragraph in document order.
pub fn read_paragraphs(doc: &LoroDoc) -> Result<Vec<Paragraph>> {
    Ok(block_seq(doc).iter().filter_map(|r| block_ref_paragraph(doc, r)).collect())
}

/// Read a full [`Paragraph`] (style + props + tracked prop/mark changes + run text) from a paragraph
/// map `pm`: the meta map of a top-level block node, OR a grid cell's block-list entry - both have the
/// same `{style?, text, <para props>, <pPrChange>, <mark change>}` shape. Inverse of
/// [`write_paragraph_into_map`]. Centralizing this is what lets a table-cell paragraph carry the same
/// fidelity (alignment, spacing, numbering, tracked pPrChange / paragraph-mark) as a body paragraph.
pub(crate) fn read_paragraph_from_map(pm: &LoroMap) -> Paragraph {
    let style = meta_string(pm, "style");
    let props = read_para_props(pm);
    let runs = match meta_text(pm, "text") {
        Some(text) => runs_from_delta(&text.get_richtext_value().to_json_value()),
        None => Vec::new(),
    };
    let prop_change = read_para_prop_change(pm);
    let mark_change = read_para_mark(pm);
    Paragraph { style, props, runs, prop_change, mark_change }
}

/// Write a full [`Paragraph`] into a paragraph map `pm` (a fresh top-level block meta or grid cell
/// block-list entry). Writes the style, the run text (into a `text` container), the paragraph props,
/// and any tracked pPrChange / paragraph-mark revision. Inverse of [`read_paragraph_from_map`];
/// assumes a fresh `text` container (the caller rebuilds the block). Caller commits.
pub(crate) fn write_paragraph_into_map(pm: &LoroMap, p: &Paragraph) -> Result<()> {
    if let Some(s) = &p.style {
        pm.insert("style", s.as_str())?;
    }
    let text: LoroText = pm.get_or_create_container("text", LoroText::new())?;
    write_runs(&text, &p.runs)?;
    if p.props != ParaProps::default() {
        write_para_props(pm, &p.props)?;
    }
    if let Some(c) = &p.prop_change {
        write_para_prop_change(pm, c)?;
    }
    if let Some(m) = &p.mark_change {
        write_para_mark(pm, m)?;
    }
    Ok(())
}

fn meta_i64(meta: &LoroMap, key: &str) -> Option<i64> {
    match meta.get(key) {
        Some(ValueOrContainer::Value(LoroValue::I64(n))) => Some(n),
        _ => None,
    }
}

pub(crate) fn meta_bool(meta: &LoroMap, key: &str) -> Option<bool> {
    match meta.get(key) {
        Some(ValueOrContainer::Value(LoroValue::Bool(b))) => Some(b),
        _ => None,
    }
}

/// Read paragraph properties off a block's meta map.
pub(crate) fn read_para_props(meta: &LoroMap) -> ParaProps {
    ParaProps {
        align: meta_string(meta, "align").as_deref().and_then(Align::parse),
        line_spacing: meta_i64(meta, "lineSpacing").map(|n| n as u16),
        line_rule: meta_string(meta, "lineRule").as_deref().and_then(LineRule::from_ooxml),
        space_before: meta_i64(meta, "spBefore").map(|n| n as u32),
        space_after: meta_i64(meta, "spAfter").map(|n| n as u32),
        indent_left: meta_i64(meta, "indL").map(|n| n as i32),
        indent_right: meta_i64(meta, "indR").map(|n| n as i32),
        indent_first: meta_i64(meta, "indFirst").map(|n| n as i32),
        num_id: meta_i64(meta, "numId").map(|n| n as i32),
        num_ilvl: meta_i64(meta, "ilvl").map(|n| n as i32),
        shading: meta_string(meta, "shd"),
        // Stored as `pos:kind` per stop (kind omitted == 0/left), comma-joined. Plain `pos`
        // tokens (the pre-alignment format) parse as left tabs.
        tab_stops: meta_string(meta, "tabs")
            .map(|s| {
                s.split(',')
                    .filter(|t| !t.is_empty())
                    .filter_map(|t| t.split(':').next().unwrap_or(t).parse().ok())
                    .collect()
            })
            .unwrap_or_default(),
        tab_kinds: meta_string(meta, "tabs")
            .map(|s| {
                s.split(',')
                    .filter(|t| !t.is_empty())
                    .map(|t| t.split(':').nth(1).and_then(|k| k.parse().ok()).unwrap_or(0u8))
                    .collect()
            })
            .unwrap_or_default(),
        keep_next: meta_bool(meta, "keepNext"),
        contextual_spacing: meta_bool(meta, "ctxSp"),
        page_break_before: meta_bool(meta, "pgBrkBef").unwrap_or(false),
        page_break_after: meta_bool(meta, "pgBrkAft").unwrap_or(false),
        section_end: meta_bool(meta, "sectEnd").unwrap_or(false),
        continuous_break: meta_bool(meta, "contSect").unwrap_or(false),
        column_break_after: meta_bool(meta, "colBrkAft").unwrap_or(false),
        frame: meta_string(meta, "frame"),
        border: meta_string(meta, "pbdr"),
        mark_size: meta_i64(meta, "markSz").map(|n| n as u32),
        sect_pr: meta_string(meta, "sectPr"),
    }
}

/// Set a block's paragraph-property meta to *exactly* `p`: each field is written when set and deleted
/// when unset. Unlike [`write_para_props`] (which only writes set fields), this clears stale keys -
/// needed to restore the before-state when a tracked paragraph-property change is rejected.
pub(crate) fn set_para_props_exact(meta: &LoroMap, p: &ParaProps) -> Result<()> {
    match p.align {
        Some(a) => meta.insert("align", a.as_str())?,
        None => meta.delete("align")?,
    }
    set_or_del_i64(meta, "lineSpacing", p.line_spacing.map(|v| v as i64))?;
    match p.line_rule {
        Some(r) => meta.insert("lineRule", r.as_str())?,
        None => meta.delete("lineRule")?,
    }
    set_or_del_i64(meta, "spBefore", p.space_before.map(|v| v as i64))?;
    set_or_del_i64(meta, "spAfter", p.space_after.map(|v| v as i64))?;
    set_or_del_i64(meta, "indL", p.indent_left.map(|v| v as i64))?;
    set_or_del_i64(meta, "indR", p.indent_right.map(|v| v as i64))?;
    set_or_del_i64(meta, "indFirst", p.indent_first.map(|v| v as i64))?;
    set_or_del_i64(meta, "numId", p.num_id.map(|v| v as i64))?;
    set_or_del_i64(meta, "ilvl", p.num_ilvl.map(|v| v as i64))?;
    match &p.shading {
        Some(s) => meta.insert("shd", s.as_str())?,
        None => meta.delete("shd")?,
    }
    if p.tab_stops.is_empty() {
        meta.delete("tabs")?;
    } else {
        let s = p
            .tab_stops
            .iter()
            .enumerate()
            .map(|(i, t)| match p.tab_kinds.get(i).copied().unwrap_or(0) {
                0 => t.to_string(),
                k => format!("{t}:{k}"),
            })
            .collect::<Vec<_>>()
            .join(",");
        meta.insert("tabs", s.as_str())?;
    }
    match p.keep_next {
        Some(k) => meta.insert("keepNext", k)?,
        None => meta.delete("keepNext")?,
    }
    match p.contextual_spacing {
        Some(c) => meta.insert("ctxSp", c)?,
        None => meta.delete("ctxSp")?,
    }
    if p.page_break_before { meta.insert("pgBrkBef", true)?; } else { meta.delete("pgBrkBef")?; }
    if p.page_break_after { meta.insert("pgBrkAft", true)?; } else { meta.delete("pgBrkAft")?; }
    if p.column_break_after { meta.insert("colBrkAft", true)?; } else { meta.delete("colBrkAft")?; }
    if p.section_end { meta.insert("sectEnd", true)?; } else { meta.delete("sectEnd")?; }
    if p.continuous_break { meta.insert("contSect", true)?; } else { meta.delete("contSect")?; }
    match &p.frame {
        Some(s) => meta.insert("frame", s.as_str())?,
        None => meta.delete("frame")?,
    }
    match &p.border {
        Some(s) => meta.insert("pbdr", s.as_str())?,
        None => meta.delete("pbdr")?,
    }
    set_or_del_i64(meta, "markSz", p.mark_size.map(|v| v as i64))?;
    match &p.sect_pr {
        Some(s) => meta.insert("sectPr", s.as_str())?,
        None => meta.delete("sectPr")?,
    }
    Ok(())
}

pub(crate) fn set_or_del_i64(meta: &LoroMap, key: &str, v: Option<i64>) -> Result<()> {
    match v {
        Some(n) => meta.insert(key, n)?,
        None => meta.delete(key)?,
    }
    Ok(())
}

/// JSON encoding of paragraph properties (only set fields emitted) for a `w:pPrChange`'s old props.
fn para_props_json(p: &ParaProps) -> Json {
    let mut m = serde_json::Map::new();
    if let Some(a) = p.align {
        m.insert("align".into(), Json::from(a.as_str()));
    }
    if let Some(v) = p.line_spacing {
        m.insert("ls".into(), Json::from(v));
    }
    if let Some(r) = p.line_rule {
        m.insert("lr".into(), Json::from(r.as_str()));
    }
    if let Some(v) = p.space_before {
        m.insert("sb".into(), Json::from(v));
    }
    if let Some(v) = p.space_after {
        m.insert("sa".into(), Json::from(v));
    }
    if let Some(v) = p.indent_left {
        m.insert("il".into(), Json::from(v));
    }
    if let Some(v) = p.indent_right {
        m.insert("ir".into(), Json::from(v));
    }
    if let Some(v) = p.indent_first {
        m.insert("ifst".into(), Json::from(v));
    }
    if let Some(v) = p.num_id {
        m.insert("nid".into(), Json::from(v));
    }
    if let Some(v) = p.num_ilvl {
        m.insert("ilvl".into(), Json::from(v));
    }
    if let Some(s) = &p.shading {
        m.insert("shd".into(), Json::from(s.as_str()));
    }
    if !p.tab_stops.is_empty() {
        m.insert("tabs".into(), Json::from(p.tab_stops.clone()));
        if p.tab_kinds.iter().any(|&k| k != 0) {
            m.insert("tabKinds".into(), Json::from(p.tab_kinds.clone()));
        }
    }
    if let Some(k) = p.keep_next {
        m.insert("kn".into(), Json::from(k));
    }
    if let Some(c) = p.contextual_spacing {
        m.insert("cs".into(), Json::from(c));
    }
    if p.page_break_before {
        m.insert("pbb".into(), Json::from(true));
    }
    if p.page_break_after {
        m.insert("pba".into(), Json::from(true));
    }
    if p.column_break_after {
        m.insert("colb".into(), Json::from(true));
    }
    if p.section_end {
        m.insert("se".into(), Json::from(true));
    }
    if p.continuous_break {
        m.insert("cse".into(), Json::from(true));
    }
    if let Some(s) = &p.frame {
        m.insert("frame".into(), Json::from(s.as_str()));
    }
    if let Some(s) = &p.border {
        m.insert("pbdr".into(), Json::from(s.as_str()));
    }
    if let Some(v) = p.mark_size {
        m.insert("mksz".into(), Json::from(v));
    }
    if let Some(s) = &p.sect_pr {
        m.insert("sectPr".into(), Json::from(s.as_str()));
    }
    Json::Object(m)
}

/// Parse paragraph properties from the JSON written by [`para_props_json`].
fn para_props_from_json(j: &Json) -> ParaProps {
    ParaProps {
        align: j.get("align").and_then(|v| v.as_str()).and_then(Align::parse),
        line_spacing: j.get("ls").and_then(|v| v.as_u64()).map(|n| n as u16),
        line_rule: j.get("lr").and_then(|v| v.as_str()).and_then(LineRule::from_ooxml),
        space_before: j.get("sb").and_then(|v| v.as_u64()).map(|n| n as u32),
        space_after: j.get("sa").and_then(|v| v.as_u64()).map(|n| n as u32),
        indent_left: j.get("il").and_then(|v| v.as_i64()).map(|n| n as i32),
        indent_right: j.get("ir").and_then(|v| v.as_i64()).map(|n| n as i32),
        indent_first: j.get("ifst").and_then(|v| v.as_i64()).map(|n| n as i32),
        num_id: j.get("nid").and_then(|v| v.as_i64()).map(|n| n as i32),
        num_ilvl: j.get("ilvl").and_then(|v| v.as_i64()).map(|n| n as i32),
        shading: j.get("shd").and_then(|v| v.as_str()).map(String::from),
        tab_stops: j
            .get("tabs")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect())
            .unwrap_or_default(),
        tab_kinds: j
            .get("tabKinds")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect())
            .unwrap_or_default(),
        keep_next: j.get("kn").and_then(|v| v.as_bool()),
        contextual_spacing: j.get("cs").and_then(|v| v.as_bool()),
        page_break_before: j.get("pbb").and_then(|v| v.as_bool()).unwrap_or(false),
        page_break_after: j.get("pba").and_then(|v| v.as_bool()).unwrap_or(false),
        section_end: j.get("se").and_then(|v| v.as_bool()).unwrap_or(false),
        continuous_break: j.get("cse").and_then(|v| v.as_bool()).unwrap_or(false),
        column_break_after: j.get("colb").and_then(|v| v.as_bool()).unwrap_or(false),
        frame: j.get("frame").and_then(|v| v.as_str()).map(String::from),
        border: j.get("pbdr").and_then(|v| v.as_str()).map(String::from),
        mark_size: j.get("mksz").and_then(|v| v.as_u64()).map(|n| n as u32),
        sect_pr: j.get("sectPr").and_then(|v| v.as_str()).map(String::from),
    }
}

/// Read a tracked paragraph-property change (`ppc` meta) off a block.
fn read_para_prop_change(meta: &LoroMap) -> Option<ParaPropChange> {
    let raw = meta_string(meta, "ppc")?;
    let j: Json = serde_json::from_str(&raw).ok()?;
    Some(ParaPropChange {
        author: j.get("author").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        date: j.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        id: j.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
        old_style: j.get("style").and_then(|v| v.as_str()).map(String::from),
        old: para_props_from_json(j.get("old").unwrap_or(&Json::Null)),
    })
}

/// Write a tracked paragraph-property change to the `ppc` meta key.
pub(crate) fn write_para_prop_change(meta: &LoroMap, c: &ParaPropChange) -> Result<()> {
    let mut o = serde_json::Map::new();
    o.insert("author".into(), Json::from(c.author.as_str()));
    o.insert("date".into(), Json::from(c.date.as_str()));
    o.insert("id".into(), Json::from(c.id));
    if let Some(s) = &c.old_style {
        o.insert("style".into(), Json::from(s.as_str()));
    }
    o.insert("old".into(), para_props_json(&c.old));
    meta.insert("ppc", Json::Object(o).to_string().as_str())?;
    Ok(())
}

/// Read a tracked paragraph-mark revision (`pmark` meta) off a block.
fn read_para_mark(meta: &LoroMap) -> Option<Track> {
    let raw = meta_string(meta, "pmark")?;
    let j: Json = serde_json::from_str(&raw).ok()?;
    let kind = match j.get("kind").and_then(|v| v.as_str())? {
        "ins" => TrackKind::Ins,
        "del" => TrackKind::Del,
        "movefrom" => TrackKind::MoveFrom,
        "moveto" => TrackKind::MoveTo,
        _ => return None,
    };
    Some(Track {
        kind,
        author: j.get("author").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        date: j.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        id: j.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

/// Write a paragraph-mark revision to a block's `pmark` meta.
pub(crate) fn write_para_mark(meta: &LoroMap, m: &Track) -> Result<()> {
    let kind = match m.kind {
        TrackKind::Ins => "ins",
        TrackKind::Del => "del",
        // Move ¶s: the destination break a move created (moveTo) / a ¶ inside the moved-away source
        // (moveFrom). They resolve like ins/del (moveTo merges on reject, moveFrom on accept).
        TrackKind::MoveFrom => "movefrom",
        TrackKind::MoveTo => "moveto",
        TrackKind::Fmt => return Err(anyhow!("a paragraph mark cannot be a format change")),
    };
    let o = serde_json::json!({ "kind": kind, "author": m.author, "date": m.date, "id": m.id });
    meta.insert("pmark", o.to_string().as_str())?;
    Ok(())
}

/// Set the `idx`-th block's paragraph-mark revision. Caller commits.
pub fn set_para_mark(doc: &LoroDoc, idx: usize, m: &Track) -> Result<()> {
    let tree = doc.get_tree(BLOCKS);
    let id = paragraph_roots(doc)
        .get(idx)
        .copied()
        .ok_or_else(|| anyhow!("no block at index {idx}"))?;
    write_para_mark(&tree.get_meta(id)?, m)
}

/// Clear a block's paragraph-mark revision. Caller commits.
pub fn clear_para_mark(doc: &LoroDoc, idx: usize) -> Result<()> {
    let tree = doc.get_tree(BLOCKS);
    let id = paragraph_roots(doc)
        .get(idx)
        .copied()
        .ok_or_else(|| anyhow!("no block at index {idx}"))?;
    tree.get_meta(id)?.delete("pmark")?;
    Ok(())
}

/// Convert loro's rich-text Delta (`[{insert, attributes?}]`) into runs.
pub(crate) fn runs_from_delta(delta: &Json) -> Vec<Run> {
    let mut runs = Vec::new();
    let Some(arr) = delta.as_array() else { return runs };
    for seg in arr {
        let Some(text) = seg.get("insert").and_then(|v| v.as_str()) else { continue };
        if text.is_empty() {
            continue;
        }
        let attrs = seg.get("attributes").unwrap_or(&Json::Null);
        runs.push(Run {
            text: text.to_string(),
            bold: attrs.get("b").and_then(|v| v.as_bool()).unwrap_or(false),
            italic: attrs.get("i").and_then(|v| v.as_bool()).unwrap_or(false),
            underline: attrs.get("u").and_then(|v| v.as_bool()).unwrap_or(false),
            strike: attrs.get("strike").and_then(|v| v.as_bool()).unwrap_or(false),
            size: attrs.get("sz").and_then(|v| v.as_i64()).map(|n| n as u16),
            color: attrs.get("color").and_then(|v| v.as_str()).map(String::from),
            font: attrs.get("font").and_then(|v| v.as_str()).map(String::from),
            highlight: attrs.get("hl").and_then(|v| v.as_str()).map(String::from),
            vert_align: attrs.get("va").and_then(|v| v.as_str()).map(String::from),
            lang: attrs.get("lang").and_then(|v| v.as_str()).map(String::from),
            char_style: attrs.get("rstyle").and_then(|v| v.as_str()).map(String::from),
            shading: attrs.get("rshd").and_then(|v| v.as_str()).map(String::from),
            track: track_from_attrs(attrs),
            fmt_change: fmt_change_from_attrs(attrs),
            comments: comment_ids_from_attrs(attrs),
            field: field_id_from_attrs(attrs),
            bookmarks: bookmark_ids_from_attrs(attrs),
            point_bookmarks: point_bookmark_ids_from_attrs(attrs),
            end_point_bookmarks: end_point_bookmark_ids_from_attrs(attrs),
            link: link_id_from_attrs(attrs),
            image: img_id_from_attrs(attrs),
            raw: raw_id_from_attrs(attrs),
        });
    }
    runs
}

/// The ids of every present `{prefix}{id}` mark on a run's attributes, sorted ascending. Shared by
/// the multi-id annotations (comments, bookmarks) that can overlap on one run.
fn prefixed_ids_from_attrs(attrs: &Json, prefix: &str) -> Vec<u64> {
    let Some(obj) = attrs.as_object() else { return Vec::new() };
    let mut ids: Vec<u64> = obj
        .iter()
        .filter_map(|(k, v)| {
            let id = k.strip_prefix(prefix)?.parse::<u64>().ok()?;
            // The mark is present (truthy).
            (!matches!(v, Json::Null) && v.as_bool() != Some(false)).then_some(id)
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// The comment ids anchored over a run, read from its `cmt~{id}` marks (sorted ascending).
fn comment_ids_from_attrs(attrs: &Json) -> Vec<u64> {
    prefixed_ids_from_attrs(attrs, "cmt~")
}

/// The lowest id of a present `{prefix}{id}` mark on a run's attributes, or `None`. Shared by the
/// single-id annotations (field / bookmark / hyperlink), which don't nest in the model.
fn id_from_attrs(attrs: &Json, prefix: &str) -> Option<u64> {
    let obj = attrs.as_object()?;
    obj.iter()
        .filter_map(|(k, v)| {
            let id = k.strip_prefix(prefix)?.parse::<u64>().ok()?;
            (!matches!(v, Json::Null) && v.as_bool() != Some(false)).then_some(id)
        })
        .min()
}

/// The field id whose cached-result range covers a run (`fld~{id}`), or `None`.
fn field_id_from_attrs(attrs: &Json) -> Option<u64> {
    id_from_attrs(attrs, "fld~")
}

/// The bookmark ids whose ranges cover a run (`bkm~{id}` marks, ascending) - like
/// [`comment_ids_from_attrs`], since several bookmarks can overlap on one run.
fn bookmark_ids_from_attrs(attrs: &Json) -> Vec<u64> {
    prefixed_ids_from_attrs(attrs, "bkm~")
}

/// The ids of **collapsed** bookmarks anchored immediately before a run (`bkp~{id}` marks,
/// ascending). Several can stack at one position - a heading commonly carries a run of `_Ref…`
/// cross-reference targets pointing at the same spot.
fn point_bookmark_ids_from_attrs(attrs: &Json) -> Vec<u64> {
    prefixed_ids_from_attrs(attrs, "bkp~")
}

/// The ids of collapsed bookmarks anchored immediately *after* a run (`bkpe~{id}` marks, ascending) -
/// the ones that fell past the paragraph's last codepoint, so there was nothing left to sit before.
fn end_point_bookmark_ids_from_attrs(attrs: &Json) -> Vec<u64> {
    prefixed_ids_from_attrs(attrs, "bkpe~")
}

/// The image id this run carries (`img~{id}`), or `None`.
fn img_id_from_attrs(attrs: &Json) -> Option<u64> {
    id_from_attrs(attrs, "img~")
}

/// The passthrough id whose `raw~{id}` mark covers a run, or `None`.
fn raw_id_from_attrs(attrs: &Json) -> Option<u64> {
    id_from_attrs(attrs, "raw~")
}

/// The hyperlink id whose range covers a run (`lnk~{id}`), or `None`.
fn link_id_from_attrs(attrs: &Json) -> Option<u64> {
    id_from_attrs(attrs, "lnk~")
}

/// Read a tracked run-property-change mark (`rfmt`) off run attributes.
fn fmt_change_from_attrs(attrs: &Json) -> Option<FormatChange> {
    let raw = attrs.get("rfmt").and_then(|v| v.as_str())?;
    let j: Json = serde_json::from_str(raw).ok()?;
    let old = j.get("old").unwrap_or(&Json::Null);
    Some(FormatChange {
        author: j.get("author").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        date: j.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        id: j.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
        old: RunProps {
            bold: old.get("b").and_then(|v| v.as_bool()).unwrap_or(false),
            italic: old.get("i").and_then(|v| v.as_bool()).unwrap_or(false),
            underline: old.get("u").and_then(|v| v.as_bool()).unwrap_or(false),
            strike: old.get("strike").and_then(|v| v.as_bool()).unwrap_or(false),
            size: old.get("sz").and_then(|v| v.as_i64()).map(|n| n as u16),
            color: old.get("color").and_then(|v| v.as_str()).map(String::from),
            font: old.get("font").and_then(|v| v.as_str()).map(String::from),
            highlight: old.get("hl").and_then(|v| v.as_str()).map(String::from),
            vert_align: old.get("va").and_then(|v| v.as_str()).map(String::from),
            lang: old.get("lang").and_then(|v| v.as_str()).map(String::from),
        },
    })
}

/// Read a tracked-change mark (`ins`/`del`/`mvf`/`mvt`, value = JSON `{author,date,id}`) off run
/// attributes.
fn track_from_attrs(attrs: &Json) -> Option<Track> {
    for kind in [TrackKind::Ins, TrackKind::Del, TrackKind::MoveFrom, TrackKind::MoveTo] {
        if let Some(raw) = attrs.get(kind.mark_key()).and_then(|v| v.as_str()) {
            let j: Json = serde_json::from_str(raw).ok()?;
            return Some(Track {
                kind,
                author: j.get("author").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                date: j.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                id: j.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
            });
        }
    }
    None
}

/// Root block ids in document order (by their index under the tree root).
pub(crate) fn ordered_roots(doc: &LoroDoc) -> Vec<TreeID> {
    let tree = doc.get_tree(BLOCKS);
    let mut roots: Vec<_> = tree
        .get_nodes(false)
        .into_iter()
        .filter(|n| matches!(n.parent, TreeParentId::Root))
        .collect();
    roots.sort_by_key(|n| n.index);
    roots.into_iter().map(|n| n.id).collect()
}

/// The root nodes that are **paragraphs** (`type != "table"`), in document order - the flat *paragraph*
/// index that anchors + edit ops address. A table is a root node of `type "table"` hosting its own grid
/// containers (tables-crdt); it is excluded here so the paragraph index counts only paragraphs. For a
/// table-free document (and the legacy flat-flow tables, whose cells are ordinary paragraph nodes) this
/// equals [`ordered_roots`], so all existing paragraph addressing is unchanged.
fn paragraph_roots(doc: &LoroDoc) -> Vec<TreeID> {
    let tree = doc.get_tree(BLOCKS);
    ordered_roots(doc)
        .into_iter()
        .filter(|id| {
            tree.get_meta(*id).ok().and_then(|m| meta_string(&m, "type")).as_deref() != Some("table")
        })
        .collect()
}

/// The raw tree-root position of `id` (the index `create_at` uses, counting *all* root children incl.
/// table nodes), or the end if absent.
pub(crate) fn raw_root_pos(doc: &LoroDoc, id: TreeID) -> usize {
    let roots = ordered_roots(doc);
    roots.iter().position(|t| *t == id).unwrap_or(roots.len())
}

/// One item of the document body in order: a paragraph block, or a table (a `type "table"` root node
/// hosting its own [`TableGrid`](crate::table_crdt::TableGrid) - tables-crdt). Read with [`body_nodes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyNode {
    Paragraph(TreeID),
    Table(TreeID),
}

/// The document body in order: paragraphs interleaved with tables (each a tree node). This is the
/// body-order projection the renderer / export walk once tables are loro citizens; `read_paragraphs`
/// gives the paragraph-only flat list, and a `Table(id)` node's grid is opened with [`open_table_grid`].
pub fn body_nodes(doc: &LoroDoc) -> Vec<BodyNode> {
    let tree = doc.get_tree(BLOCKS);
    ordered_roots(doc)
        .into_iter()
        .map(|id| {
            let is_table = tree
                .get_meta(id)
                .ok()
                .and_then(|m| meta_string(&m, "type"))
                .as_deref()
                == Some("table");
            if is_table { BodyNode::Table(id) } else { BodyNode::Paragraph(id) }
        })
        .collect()
}

/// The meta (property) map of a body node ([`BodyNode`]) - a top-level paragraph or table root. Used to
/// attach block-wrapper anchors ([`set_block_wrap_anchors`]) to the node without exposing the tree.
pub fn block_node_meta(doc: &LoroDoc, node: &BodyNode) -> Result<LoroMap> {
    let id = match node {
        BodyNode::Paragraph(id) | BodyNode::Table(id) => *id,
    };
    Ok(doc.get_tree(BLOCKS).get_meta(id)?)
}

/// Create an empty table as a `type "table"` root node, appended at the end of the body. Open its grid
/// with [`open_table_grid`] to populate rows/columns/cells. Caller commits.
pub fn create_table_node(doc: &LoroDoc) -> Result<TreeID> {
    let tree = doc.get_tree(BLOCKS);
    let id = tree.create_at(TreeParentId::Root, ordered_roots(doc).len())?;
    tree.get_meta(id)?.insert("type", "table")?;
    block_cache_invalidate(); // a table node was added to the body
    Ok(id)
}

/// Open the [`TableGrid`](crate::table_crdt::TableGrid) hosted on table node `node` (its meta map holds
/// the grid containers).
pub fn open_table_grid(doc: &LoroDoc, node: TreeID) -> Result<crate::table_crdt::TableGrid> {
    crate::table_crdt::TableGrid::open(doc.get_tree(BLOCKS).get_meta(node)?)
}

/// A reference to one paragraph in the flat document-order block sequence ([`block_seq`]): a top-level
/// block node, or a paragraph inside a table cell. The flat paragraph *index* every edit op / anchor /
/// caret / renderer uses is a position in `block_seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockRef {
    /// A top-level block paragraph node.
    Top(TreeID),
    /// The `idx`-th block paragraph of cell `(row, col)` in the table hosted on node `node`.
    Cell { node: TreeID, row: String, col: String, idx: usize },
}

thread_local! {
    /// A **batch-scoped** memo of [`block_seq`], enabled only between [`block_cache_begin`] and
    /// [`block_cache_end`]. It exists for bulk emission (document comparison replays thousands of
    /// `suggest_*` ops, each resolving a flat paragraph index to its container - an O(N) walk), on a
    /// doc that the batch alone mutates on this thread. The cache is invalidated at every structural
    /// change to the block sequence (paragraph create / delete / split / join, table row / column /
    /// cell-block change - see [`block_cache_invalidate`]). Inactive by default, so interactive and
    /// server editing recompute `block_seq` fresh on every call, exactly as before this existed.
    static BLOCK_CACHE: std::cell::RefCell<Option<Vec<BlockRef>>> = const { std::cell::RefCell::new(None) };
    static BLOCK_CACHE_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Begin a bulk-emission batch on this thread: [`block_seq`] memoizes until [`block_cache_end`]. Safe
/// only for a *synchronous* burst of edits to a single document (no other document is touched on this
/// thread meanwhile) - which is exactly how comparison emits. Idempotent; always pair with an end.
pub(crate) fn block_cache_begin() {
    BLOCK_CACHE_ON.with(|f| f.set(true));
    BLOCK_CACHE.with(|c| *c.borrow_mut() = None);
}

/// End the batch and drop the memo. After this, `block_seq` is uncached again.
pub(crate) fn block_cache_end() {
    BLOCK_CACHE_ON.with(|f| f.set(false));
    BLOCK_CACHE.with(|c| *c.borrow_mut() = None);
}

/// Drop the memoized sequence because a structural mutation changed it (a no-op when no batch is
/// active). Every function that adds / removes / reorders a block must call this after mutating.
pub(crate) fn block_cache_invalidate() {
    BLOCK_CACHE.with(|c| {
        if c.borrow().is_some() {
            *c.borrow_mut() = None;
        }
    });
}

/// The document's paragraphs in flat document order: top-level block paragraphs interleaved with table
/// cell paragraphs (each table's cells row-major - `row_order` x `col_order`, each cell's blocks in
/// order). This is **the** flat paragraph index the edit ops, anchors, caret, and renderer address.
///
/// For a table-free document (and the legacy flat-flow tables, whose cells are ordinary paragraph root
/// nodes) there are no `type "table"` nodes to descend into, so this equals [`paragraph_roots`] mapped
/// to `Top` - every existing paragraph-addressing call is unchanged. Once a table is a loro citizen
/// (cells live in its grid), the descent makes those cell paragraphs part of the same flat index.
///
/// O(N): walks every body node and descends each table's grid. Memoized within a
/// [`block_cache_begin`]/[`block_cache_end`] batch; otherwise recomputed on every call.
pub(crate) fn block_seq(doc: &LoroDoc) -> Vec<BlockRef> {
    if BLOCK_CACHE_ON.with(|f| f.get()) {
        if let Some(seq) = BLOCK_CACHE.with(|c| c.borrow().clone()) {
            // Safety net (native debug / `cargo test` only): prove the memo still matches a fresh walk,
            // so any structural mutation that forgot to invalidate trips here (and the compare oracle
            // tests) at once. Excluded from wasm builds so a `wasm-pack --dev` compare stays fast.
            #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
            assert_eq!(
                seq,
                block_seq_uncached(doc),
                "block_seq cache stale - a structural mutation missed block_cache_invalidate()"
            );
            return seq;
        }
        let seq = block_seq_uncached(doc);
        BLOCK_CACHE.with(|c| *c.borrow_mut() = Some(seq.clone()));
        return seq;
    }
    block_seq_uncached(doc)
}

/// One element of [`block_seq`] by flat index, without cloning the whole sequence on a cache hit - the
/// hot path for `nth_block_text` / `block_meta_at`, which resolve a single paragraph per op.
pub(crate) fn block_ref_at(doc: &LoroDoc, idx: usize) -> Option<BlockRef> {
    if BLOCK_CACHE_ON.with(|f| f.get()) {
        return BLOCK_CACHE.with(|c| {
            if c.borrow().is_none() {
                *c.borrow_mut() = Some(block_seq_uncached(doc));
            }
            c.borrow().as_ref().and_then(|seq| seq.get(idx).cloned())
        });
    }
    block_seq_uncached(doc).into_iter().nth(idx)
}

fn block_seq_uncached(doc: &LoroDoc) -> Vec<BlockRef> {
    let mut out = Vec::new();
    for node in body_nodes(doc) {
        match node {
            BodyNode::Paragraph(id) => out.push(BlockRef::Top(id)),
            BodyNode::Table(id) => {
                let Ok(grid) = open_table_grid(doc, id) else { continue };
                let (Ok(rows), Ok(cols)) = (grid.row_ids(), grid.col_ids()) else { continue };
                for r in &rows {
                    for c in &cols {
                        let n = grid.cell_block_count(r, c).unwrap_or(0);
                        for idx in 0..n {
                            out.push(BlockRef::Cell { node: id, row: r.clone(), col: c.clone(), idx });
                        }
                    }
                }
            }
        }
    }
    out
}

/// The editable `text` container of one [`BlockRef`] (top-level node meta text, or a grid cell's block
/// paragraph text). Its container id is the block's durable identity (used for a cell paragraph's node id).
pub(crate) fn block_ref_text(doc: &LoroDoc, r: &BlockRef) -> Option<LoroText> {
    match r {
        BlockRef::Top(id) => doc.get_tree(BLOCKS).get_meta(*id).ok().and_then(|m| meta_text(&m, "text")),
        BlockRef::Cell { node, row, col, idx } => {
            open_table_grid(doc, *node).ok()?.cell_block_text(row, col, *idx).ok().flatten()
        }
    }
}

/// Read the full [`Paragraph`] a [`BlockRef`] points at.
fn block_ref_paragraph(doc: &LoroDoc, r: &BlockRef) -> Option<Paragraph> {
    match r {
        BlockRef::Top(id) => doc.get_tree(BLOCKS).get_meta(*id).ok().map(|m| read_paragraph_from_map(&m)),
        BlockRef::Cell { node, row, col, idx } => {
            open_table_grid(doc, *node).ok()?.cell_block_map(row, col, *idx).ok().flatten().map(|m| read_paragraph_from_map(&m))
        }
    }
}

/// The paragraph meta / property map for the **flat** paragraph index `idx` ([`block_seq`] order):
/// the meta map of a top-level block node, OR a table cell's block-list paragraph map. Both carry the
/// identical `{style?, text, <para props>, <pPrChange>, <mark change>}` shape, so a paragraph-property
/// op (alignment / numbering / style / tracked pPrChange) writes the same keys whichever it resolves to.
/// This is **the** index the agent, editor caret, anchors, edit ops, and renderer all address - the same
/// resolution [`block_ref_text`] / [`block_ref_paragraph`] use, but returning the writable property map.
pub(crate) fn block_meta_at(doc: &LoroDoc, idx: usize) -> Result<LoroMap> {
    match block_ref_at(doc, idx) {
        Some(BlockRef::Top(id)) => Ok(doc.get_tree(BLOCKS).get_meta(id)?),
        Some(BlockRef::Cell { node, row, col, idx: bi }) => open_table_grid(doc, node)?
            .cell_block_map(&row, &col, bi)?
            .ok_or_else(|| anyhow!("no cell block at index {idx}")),
        None => Err(anyhow!("no block at index {idx}")),
    }
}

pub(crate) fn meta_string(meta: &LoroMap, key: &str) -> Option<String> {
    match meta.get(key) {
        Some(ValueOrContainer::Value(LoroValue::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

fn meta_text(meta: &LoroMap, key: &str) -> Option<LoroText> {
    match meta.get(key) {
        Some(ValueOrContainer::Container(Container::Text(t))) => Some(t),
        _ => None,
    }
}
