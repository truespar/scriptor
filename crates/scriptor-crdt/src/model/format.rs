//! Formatting commands and the selection-format query.
//! 
//! What the toolbar drives: apply or clear run formatting over a range, set paragraph
//! properties, and report what a selection currently resolves to so the buttons can
//! show their state.

use super::*;

// ── run formatting commands + selection-format query (the toolbar layer) ─────

/// A run-formatting command: each set field is applied over a range; `None` leaves that attribute
/// untouched. Booleans are explicit on/off (`Some(false)` clears the mark) so a toolbar can toggle.
/// (Clearing size / color / font - vs setting - is not supported; the toolbar sets them.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFormat {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub size: Option<u16>,
    pub color: Option<String>,
    pub font: Option<String>,
    /// Highlight color name, or `Some("")` to clear it. `None` leaves it unchanged.
    pub highlight: Option<String>,
    /// Vertical alignment ("superscript" / "subscript"), or `Some("")` to clear (back to baseline).
    /// `None` leaves it unchanged.
    pub vert_align: Option<String>,
}

impl RunFormat {
    pub fn bold(on: bool) -> Self {
        Self { bold: Some(on), ..Default::default() }
    }
    pub fn italic(on: bool) -> Self {
        Self { italic: Some(on), ..Default::default() }
    }
    pub fn underline(on: bool) -> Self {
        Self { underline: Some(on), ..Default::default() }
    }
    pub fn strike(on: bool) -> Self {
        Self { strike: Some(on), ..Default::default() }
    }
    pub fn size(half_points: u16) -> Self {
        Self { size: Some(half_points), ..Default::default() }
    }
    pub fn color(hex: impl Into<String>) -> Self {
        Self { color: Some(hex.into()), ..Default::default() }
    }
    pub fn font(family: impl Into<String>) -> Self {
        Self { font: Some(family.into()), ..Default::default() }
    }
    /// A highlight color (e.g. "yellow"); pass `""` to remove the highlight.
    pub fn highlight(value: impl Into<String>) -> Self {
        Self { highlight: Some(value.into()), ..Default::default() }
    }
    /// A vertical alignment ("superscript" / "subscript"); pass `""` to clear (back to baseline).
    pub fn vert_align(value: impl Into<String>) -> Self {
        Self { vert_align: Some(value.into()), ..Default::default() }
    }
}

fn mark_bool(text: &LoroText, range: &Range<usize>, key: &str, v: Option<bool>) -> Result<()> {
    match v {
        Some(true) => text.mark(range.clone(), key, true)?,
        Some(false) => text.unmark(range.clone(), key)?,
        None => {}
    }
    Ok(())
}

/// Apply a [`RunFormat`] over codepoint `range` in paragraph `idx` (the toolbar's run commands).
/// Caller commits.
pub fn apply_run_format(doc: &LoroDoc, idx: usize, range: Range<usize>, fmt: &RunFormat) -> Result<()> {
    let text = nth_block_text(doc, idx)?;
    mark_bool(&text, &range, "b", fmt.bold)?;
    mark_bool(&text, &range, "i", fmt.italic)?;
    mark_bool(&text, &range, "u", fmt.underline)?;
    mark_bool(&text, &range, "strike", fmt.strike)?;
    if let Some(sz) = fmt.size {
        text.mark(range.clone(), "sz", sz as i64)?;
    }
    if let Some(c) = &fmt.color {
        text.mark(range.clone(), "color", c.as_str())?;
    }
    if let Some(f) = &fmt.font {
        text.mark(range.clone(), "font", f.as_str())?;
    }
    // Highlight / vertAlign are toggles: a non-empty value marks, an empty value clears the mark.
    if let Some(h) = &fmt.highlight {
        if h.is_empty() {
            text.unmark(range.clone(), "hl")?;
        } else {
            text.mark(range.clone(), "hl", h.as_str())?;
        }
    }
    if let Some(v) = &fmt.vert_align {
        if v.is_empty() {
            text.unmark(range.clone(), "va")?;
        } else {
            text.mark(range.clone(), "va", v.as_str())?;
        }
    }
    Ok(())
}

/// Clear every inline run-format mark over `range` in paragraph `idx` (the Home tab's Clear Formatting
/// eraser): bold / italic / underline / strike / size / color / font / highlight / vertAlign. Annotation
/// marks (comments, fields, bookmarks, links) and tracked-change marks are left untouched. Caller commits.
pub fn clear_run_format(doc: &LoroDoc, idx: usize, range: Range<usize>) -> Result<()> {
    let text = nth_block_text(doc, idx)?;
    for key in ["b", "i", "u", "strike", "sz", "color", "font", "hl", "va", "rstyle", "rshd"] {
        text.unmark(range.clone(), key)?;
    }
    Ok(())
}

/// The resolved formatting of a selection: a value where every covered run agrees, else `None`
/// ("mixed"). Drives toolbar button states + the font / size dropdown values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionFormat {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub size: Option<u16>,
    pub color: Option<String>,
    pub font: Option<String>,
    /// Highlight color the selection agrees on (`None` = none or mixed).
    pub highlight: Option<String>,
    /// Vertical alignment the selection agrees on ("superscript" / "subscript"; `None` = baseline/mixed).
    pub vert_align: Option<String>,
}

/// Compute the [`SelectionFormat`] over codepoint `[start, end)` in paragraph `idx`. A collapsed
/// selection (`start == end`) reports the format of the run at the caret (the char before it, like
/// Word). Runs not overlapping the range are ignored. `styles` resolves the **effective** font / size
/// (a run override, else the paragraph style, else the doc default) so the toolbar shows what's actually
/// rendered - normal body text inherits its font / size from the style, not an inline `w:rFonts`/`w:sz`.
pub fn selection_format(
    doc: &LoroDoc,
    styles: &StyleTable,
    idx: usize,
    start: usize,
    end: usize,
) -> Result<SelectionFormat> {
    // Word's defaults when neither the run, the style, nor docDefaults specify (mirrors `resolve_blocks`).
    const DEFAULT_HALFPT: u16 = 22; // 11pt
    let paras = read_paragraphs(doc)?;
    let Some(para) = paras.get(idx) else { return Ok(SelectionFormat::default()) };
    let base = styles.resolve(para.style.as_deref());

    // Collect the runs overlapping the selection (or the run at the caret if collapsed).
    let mut covered: Vec<&Run> = Vec::new();
    let mut pos = 0usize;
    for run in &para.runs {
        let n = run.text.chars().count();
        let (rs, re) = (pos, pos + n);
        let overlaps = if start == end {
            // caret: the run ending at or containing the caret (prefer the char before)
            start > rs && start <= re
        } else {
            rs < end && re > start
        };
        if overlaps {
            covered.push(run);
        }
        pos = re;
    }
    if covered.is_empty() {
        // Collapsed caret at offset 0 (or empty paragraph): fall back to the first run if any.
        if let Some(first) = para.runs.first() {
            covered.push(first);
        } else {
            // A truly empty paragraph (no runs) - report the effective font / size from its style +
            // doc default (what you'd type), so the toolbar shows that instead of going blank.
            return Ok(SelectionFormat {
                bold: Some(base.bold.unwrap_or(false)),
                italic: Some(base.italic.unwrap_or(false)),
                underline: Some(false),
                strike: Some(false),
                size: Some(base.size.unwrap_or(DEFAULT_HALFPT)),
                color: base.color.clone(),
                font: Some(base.font.clone().unwrap_or_else(|| "Calibri".to_string())),
                highlight: None,
                vert_align: None,
            });
        }
    }

    let agree_bool = |f: fn(&Run) -> bool| -> Option<bool> {
        let first = f(covered[0]);
        covered.iter().all(|r| f(r) == first).then_some(first)
    };
    let agree_opt_str = |f: fn(&Run) -> Option<String>| -> Option<String> {
        let first = f(covered[0]);
        if covered.iter().all(|r| f(r) == first) { first } else { None }
    };
    // Effective font / size per run: the run's own override, else the paragraph style's, else the doc
    // default. All covered runs must agree (else `None` = "mixed", a blank box). Resolving here is what
    // makes the toolbar reflect inherited (un-overridden) formatting, the common case for body text.
    let eff_size = |r: &Run| r.size.or(base.size).unwrap_or(DEFAULT_HALFPT);
    let eff_font = |r: &Run| {
        r.font.clone().or_else(|| base.font.clone()).unwrap_or_else(|| "Calibri".to_string())
    };
    let first_size = eff_size(covered[0]);
    let size = covered.iter().all(|r| eff_size(r) == first_size).then_some(first_size);
    let first_font = eff_font(covered[0]);
    let font = covered.iter().all(|r| eff_font(r) == first_font).then_some(first_font);

    Ok(SelectionFormat {
        bold: agree_bool(|r| r.bold),
        italic: agree_bool(|r| r.italic),
        underline: agree_bool(|r| r.underline),
        strike: agree_bool(|r| r.strike),
        size,
        color: agree_opt_str(|r| r.color.clone()),
        font,
        highlight: agree_opt_str(|r| r.highlight.clone()),
        vert_align: agree_opt_str(|r| r.vert_align.clone()),
    })
}

/// Apply paragraph-level formatting ([`ParaProps`]) to paragraph `idx` - each set field is written
/// to the block's meta. Caller commits. (The Home tab's Paragraph group.)
pub fn apply_paragraph_format(doc: &LoroDoc, idx: usize, props: &ParaProps) -> Result<()> {
    write_para_props(&block_meta_at(doc, idx)?, props)
}

/// Set / clear paragraph `idx`'s named style id (`w:pStyle`) on its meta map. `style = None` clears it
/// (the paragraph falls back to Normal / docDefaults). Caller commits. (The Home tab's Styles gallery.)
pub fn set_paragraph_style(doc: &LoroDoc, idx: usize, style: Option<&str>) -> Result<()> {
    let meta = block_meta_at(doc, idx)?;
    match style {
        Some(s) => meta.insert("style", s)?,
        None => meta.delete("style")?,
    }
    Ok(())
}

/// Apply a style change to paragraph `idx` as a tracked paragraph-property change (`w:pPrChange`): the
/// new style is set; the old style + props are recorded for reject (unless the paragraph already carries
/// a pPrChange, in which case the original before-state is kept - a style change reuses the pPrChange
/// machinery, like a numbering change). Caller allocates `id` and commits.
pub fn suggest_paragraph_style(
    doc: &LoroDoc,
    idx: usize,
    style: Option<&str>,
    author: &str,
    date: &str,
    id: u64,
) -> Result<()> {
    let meta = block_meta_at(doc, idx)?;
    let already = meta_string(&meta, "ppc").is_some();
    let old = read_para_props(&meta);
    let old_style = meta_string(&meta, "style");
    match style {
        Some(s) => meta.insert("style", s)?,
        None => meta.delete("style")?,
    }
    if !already {
        write_para_prop_change(
            &meta,
            &ParaPropChange { author: author.into(), date: date.into(), id, old_style, old },
        )?;
    }
    Ok(())
}

/// Apply [`ParaProps`] to paragraph `idx` as a tracked paragraph-property change (`w:pPrChange`):
/// the new props are written; the old style + props are recorded (unless the paragraph already
/// carries a pPrChange, in which case the original before-state is kept) for reject. Caller allocates
/// `id` (see [`max_revision_id`]) and commits.
pub fn suggest_paragraph_format(
    doc: &LoroDoc,
    idx: usize,
    props: &ParaProps,
    author: &str,
    date: &str,
    id: u64,
) -> Result<()> {
    let meta = block_meta_at(doc, idx)?;
    let already = meta_string(&meta, "ppc").is_some();
    let old = read_para_props(&meta);
    let old_style = meta_string(&meta, "style");
    write_para_props(&meta, props)?;
    if !already {
        write_para_prop_change(
            &meta,
            &ParaPropChange { author: author.into(), date: date.into(), id, old_style, old },
        )?;
    }
    Ok(())
}

/// Set or clear a paragraph's numbering (`w:numPr`) on its meta map: `num_id = Some` writes the list
/// id + level (defaulting the level to 0); `num_id = None` removes the paragraph from any list. Unlike
/// [`write_para_props`] this can *clear* (a numbering change includes turning a list item back into a
/// plain paragraph), so it set-or-deletes both keys.
fn set_numbering_meta(meta: &LoroMap, num_id: Option<i32>, ilvl: Option<i32>) -> Result<()> {
    set_or_del_i64(meta, "numId", num_id.map(|v| v as i64))?;
    // The level only means anything inside a list - clear it when leaving one.
    let level = num_id.and(Some(ilvl.unwrap_or(0).max(0) as i64));
    set_or_del_i64(meta, "ilvl", level)?;
    Ok(())
}

/// Apply a numbering change (`w:numPr`) to paragraph `idx` **directly** (no revision). Caller commits.
pub fn set_numbering(doc: &LoroDoc, idx: usize, num_id: Option<i32>, ilvl: Option<i32>) -> Result<()> {
    set_numbering_meta(&block_meta_at(doc, idx)?, num_id, ilvl)
}

/// Apply a numbering change to paragraph `idx` as a tracked paragraph-property change (`w:pPrChange`):
/// set / clear the list (`w:numPr`) and record the old style + props for reject - unless the paragraph
/// already carries a pPrChange, in which case the original before-state is kept (a numbering change is
/// just another paragraph-property change, so it reuses the pPrChange resolution / export / import).
/// Caller allocates `id` (see [`max_revision_id`]) and commits.
pub fn suggest_numbering(
    doc: &LoroDoc,
    idx: usize,
    num_id: Option<i32>,
    ilvl: Option<i32>,
    author: &str,
    date: &str,
    id: u64,
) -> Result<()> {
    let meta = block_meta_at(doc, idx)?;
    let already = meta_string(&meta, "ppc").is_some();
    let old = read_para_props(&meta);
    let old_style = meta_string(&meta, "style");
    set_numbering_meta(&meta, num_id, ilvl)?;
    if !already {
        write_para_prop_change(
            &meta,
            &ParaPropChange { author: author.into(), date: date.into(), id, old_style, old },
        )?;
    }
    Ok(())
}

/// Write each set [`ParaProps`] field to a block's meta map.
pub(crate) fn write_para_props(meta: &LoroMap, props: &ParaProps) -> Result<()> {
    if let Some(a) = props.align {
        meta.insert("align", a.as_str())?;
    }
    if let Some(ls) = props.line_spacing {
        meta.insert("lineSpacing", ls as i64)?;
    }
    if let Some(r) = props.line_rule {
        meta.insert("lineRule", r.as_str())?;
    }
    if let Some(sb) = props.space_before {
        meta.insert("spBefore", sb as i64)?;
    }
    if let Some(sa) = props.space_after {
        meta.insert("spAfter", sa as i64)?;
    }
    if let Some(l) = props.indent_left {
        meta.insert("indL", l as i64)?;
    }
    if let Some(r) = props.indent_right {
        meta.insert("indR", r as i64)?;
    }
    if let Some(f) = props.indent_first {
        meta.insert("indFirst", f as i64)?;
    }
    if let Some(n) = props.num_id {
        meta.insert("numId", n as i64)?;
    }
    if let Some(l) = props.num_ilvl {
        meta.insert("ilvl", l as i64)?;
    }
    if let Some(s) = &props.shading {
        meta.insert("shd", s.as_str())?;
    }
    if !props.tab_stops.is_empty() {
        let s = props.tab_stops.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
        meta.insert("tabs", s.as_str())?;
    }
    if let Some(k) = props.keep_next {
        meta.insert("keepNext", k)?;
    }
    if let Some(c) = props.contextual_spacing {
        meta.insert("ctxSp", c)?;
    }
    if props.page_break_before {
        meta.insert("pgBrkBef", true)?;
    }
    if props.page_break_after {
        meta.insert("pgBrkAft", true)?;
    }
    if props.section_end {
        meta.insert("sectEnd", true)?;
    }
    if props.continuous_break {
        meta.insert("contSect", true)?;
    }
    if props.column_break_after {
        meta.insert("colBrkAft", true)?;
    }
    if let Some(f) = &props.frame {
        meta.insert("frame", f.as_str())?;
    }
    if let Some(b) = &props.border {
        meta.insert("pbdr", b.as_str())?;
    }
    if let Some(sz) = props.mark_size {
        meta.insert("markSz", sz as i64)?;
    }
    if let Some(s) = &props.sect_pr {
        meta.insert("sectPr", s.as_str())?;
    }
    Ok(())
}

/// The paragraph-level formatting of paragraph `idx` (for the toolbar's Paragraph group state).
pub fn paragraph_format(doc: &LoroDoc, idx: usize) -> Result<ParaProps> {
    let Ok(meta) = block_meta_at(doc, idx) else {
        return Ok(ParaProps::default());
    };
    Ok(read_para_props(&meta))
}

/// Paragraph `idx`'s named style id (`w:pStyle`), or `None` for the default (Normal).
pub fn paragraph_style(doc: &LoroDoc, idx: usize) -> Option<String> {
    meta_string(&block_meta_at(doc, idx).ok()?, "style")
}

/// The runs of paragraph `runs` from codepoint `pos` onward (the run straddling `pos` is split).
fn runs_from(runs: &[Run], pos: usize) -> Vec<Run> {
    let mut tail = Vec::new();
    let mut acc = 0usize;
    for run in runs {
        let n = run.text.chars().count();
        if acc >= pos {
            tail.push(run.clone());
        } else if acc + n > pos {
            let byte = run
                .text
                .char_indices()
                .nth(pos - acc)
                .map(|(b, _)| b)
                .unwrap_or(run.text.len());
            tail.push(Run { text: run.text[byte..].to_string(), ..run.clone() });
        }
        acc += n;
    }
    tail
}

/// Split paragraph `idx` at codepoint `pos`: text from `pos` onward (with its run formatting) moves
/// into a new paragraph inserted immediately after `idx`, which inherits the original's style. The
/// original keeps `[0, pos)`. Caller commits.
pub fn split_paragraph(doc: &LoroDoc, idx: usize, pos: usize) -> Result<()> {
    let paras = read_paragraphs(doc)?;
    let para = paras.get(idx).ok_or_else(|| anyhow!("no block at index {idx}"))?;
    let style = para.style.clone();
    let tail = runs_from(&para.runs, pos);
    let total: usize = para.runs.iter().map(|r| r.text.chars().count()).sum();

    // Truncate the original to [0, pos).
    if pos < total {
        delete_text(doc, idx, pos..total)?;
    }

    // The tail becomes a new paragraph immediately after `idx`. A top-level paragraph gets a new sibling
    // root node (its raw tree position + 1); a cell paragraph gets a new block inserted into the same
    // cell at the next position.
    match block_seq(doc).into_iter().nth(idx) {
        Some(BlockRef::Top(id)) => {
            let tree = doc.get_tree(BLOCKS);
            let new_id = tree.create_at(TreeParentId::Root, raw_root_pos(doc, id) + 1)?;
            let meta = tree.get_meta(new_id)?;
            meta.insert("type", "p")?;
            if let Some(s) = &style {
                meta.insert("style", s.as_str())?;
            }
            let text: LoroText = meta.insert_container("text", LoroText::new())?;
            write_runs(&text, &tail)?;
        }
        Some(BlockRef::Cell { node, row, col, idx: bidx }) => {
            let new = Paragraph {
                style,
                props: ParaProps::default(),
                runs: tail,
                prop_change: None,
                mark_change: None,
            };
            open_table_grid(doc, node)?.insert_cell_block(&row, &col, bidx + 1, &new)?;
        }
        None => return Err(anyhow!("no block at index {idx}")),
    }
    block_cache_invalidate(); // a block was added
    Ok(())
}

/// Join paragraph `idx` into the previous paragraph (`idx - 1`): `idx`'s runs (with formatting) are
/// appended to the previous paragraph, and `idx` is removed from the tree. Errors if `idx == 0`.
/// Returns the codepoint length of the previous paragraph *before* the join - the caret position
/// after merging. Caller commits. (The Backspace-at-paragraph-start / Delete-at-end key.)
pub fn join_paragraph(doc: &LoroDoc, idx: usize) -> Result<usize> {
    if idx == 0 {
        return Err(anyhow!("cannot join the first paragraph into a previous one"));
    }
    let paras = read_paragraphs(doc)?;
    let prev = paras.get(idx - 1).ok_or_else(|| anyhow!("no block at index {}", idx - 1))?;
    let cur = paras.get(idx).ok_or_else(|| anyhow!("no block at index {idx}"))?;
    let prev_len: usize = prev.runs.iter().map(|r| r.text.chars().count()).sum();
    let tail = cur.runs.clone();

    // A join only ever merges within one container: two top-level paragraphs, or two paragraphs of the
    // same cell. Refuse across a table-cell boundary (a cell's first paragraph can't swallow the
    // previous cell / body paragraph). For a table-free / flat-flow document every block is `Top`, so
    // this never triggers - identical to the old behavior.
    let seq = block_seq(doc);
    let same_container = match (seq.get(idx - 1), seq.get(idx)) {
        (Some(BlockRef::Top(_)), Some(BlockRef::Top(_))) => true,
        (
            Some(BlockRef::Cell { node: n1, row: r1, col: c1, .. }),
            Some(BlockRef::Cell { node: n2, row: r2, col: c2, .. }),
        ) => n1 == n2 && r1 == r2 && c1 == c2,
        _ => false,
    };
    if !same_container {
        return Err(anyhow!("cannot join across a table-cell boundary"));
    }

    // Append idx's runs onto the previous paragraph's text, then drop idx's block.
    let prev_text = nth_block_text(doc, idx - 1)?;
    append_runs(&prev_text, prev_len, &tail)?;
    match seq.into_iter().nth(idx) {
        Some(BlockRef::Top(id)) => {
            doc.get_tree(BLOCKS).delete(id)?;
        }
        Some(BlockRef::Cell { node, row, col, idx: bidx }) => {
            open_table_grid(doc, node)?.remove_cell_block(&row, &col, bidx)?;
        }
        None => return Err(anyhow!("no block at index {idx}")),
    }
    block_cache_invalidate(); // a block was removed
    Ok(prev_len)
}

/// Insert an empty paragraph as the `idx`-th flow paragraph (document order). Used by table
/// row/column inserts to materialize a new cell's required paragraph. Caller commits.
pub fn insert_empty_paragraph(doc: &LoroDoc, idx: usize) -> Result<()> {
    let new_top = |raw: usize| -> Result<()> {
        let tree = doc.get_tree(BLOCKS);
        let id = tree.create_at(TreeParentId::Root, raw)?;
        let meta = tree.get_meta(id)?;
        meta.insert("type", "p")?;
        let _: LoroText = meta.insert_container("text", LoroText::new())?;
        Ok(())
    };
    let r = match block_ref_at(doc, idx) {
        Some(BlockRef::Top(id)) => new_top(raw_root_pos(doc, id)),
        Some(BlockRef::Cell { node, row, col, idx: bidx }) => {
            let empty = Paragraph {
                style: None,
                props: ParaProps::default(),
                runs: Vec::new(),
                prop_change: None,
                mark_change: None,
            };
            open_table_grid(doc, node)?.insert_cell_block(&row, &col, bidx, &empty)
        }
        None => new_top(ordered_roots(doc).len()), // past the end -> append a top-level paragraph
    };
    block_cache_invalidate(); // a block was added
    r
}

/// Delete the `idx`-th flow paragraph outright (used by table row/column deletes). Caller commits.
pub fn delete_paragraph(doc: &LoroDoc, idx: usize) -> Result<()> {
    let r = match block_ref_at(doc, idx) {
        Some(BlockRef::Top(id)) => {
            doc.get_tree(BLOCKS).delete(id)?;
            Ok(())
        }
        Some(BlockRef::Cell { node, row, col, idx: bidx }) => {
            open_table_grid(doc, node)?.remove_cell_block(&row, &col, bidx)
        }
        None => Err(anyhow!("no block at index {idx}")),
    };
    block_cache_invalidate(); // a block was removed
    r
}
