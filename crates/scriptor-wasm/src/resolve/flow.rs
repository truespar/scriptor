//! Block flow assembly: interleaving paragraphs and tables into one stream.
//! 
//! The second half of a relayout. Walks the body in document order, builds the table
//! content blocks, maintains list counters across the document, renders list markers,
//! and pulls text-frame paragraphs out of the inline flow.

use crate::*;

/// Header/footer stories render flattened (table-cell paragraphs as plain stacked blocks), so a
/// table centred via `w:jc` - the logo-in-a-centred-table footer pattern - would lose its
/// alignment. Inherit the table's alignment into its cell paragraphs' blocks, only where the
/// paragraph sets none of its own.
pub(crate) fn inherit_hf_table_align(
    doc: Option<&scriptor_crdt::CollabDoc>,
    paras: &[scriptor_crdt::Paragraph],
    blocks: &mut [scriptor_layout::Block],
) {
    let Some(d) = doc else { return };
    let mut flat = 0usize;
    for item in d.body() {
        match item {
            scriptor_crdt::BodyItem::Paragraph => flat += 1,
            scriptor_crdt::BodyItem::Table(t) => {
                let count: usize =
                    t.rows.iter().flat_map(|r| r.cells.iter()).map(|c| c.para_count).sum();
                let jc = t
                    .justify
                    .as_deref()
                    .or_else(|| t.rows.first().and_then(|r| r.justify.as_deref()));
                let align = match jc {
                    Some("center") => Some(scriptor_layout::BlockAlign::Center),
                    Some("right") | Some("end") => Some(scriptor_layout::BlockAlign::Right),
                    _ => None,
                };
                if let Some(a) = align {
                    for i in flat..(flat + count).min(blocks.len()) {
                        if paras.get(i).is_some_and(|p| p.props.align.is_none()) {
                            blocks[i].align = a;
                        }
                    }
                }
                flat += count;
            }
        }
    }
}

/// Whether a table's `w:tblLook` enables first-row conditional formatting: the explicit
/// `w:firstRow` attribute wins, else bit `0x0020` of `w:val`; a table with no tblLook at all gets
/// Word's legacy default (conditional formats on).
pub(crate) fn look_first_row(look: Option<&str>) -> bool {
    let Some(look) = look else { return true };
    if let Some(i) = look.find("w:firstRow=\"") {
        let v = &look[i + 12..];
        return v.starts_with('1') || v.starts_with("true") || v.starts_with("on");
    }
    if let Some(i) = look.find("w:val=\"") {
        let v: String = look[i + 7..].chars().take_while(|c| *c != '"').collect();
        if let Ok(bits) = u32::from_str_radix(&v, 16) {
            return bits & 0x0020 != 0;
        }
    }
    true
}

/// Counters for in-flight list numbering, keyed by abstract-list id (each is per-level).
pub(crate) type ListCounters = std::collections::HashMap<i32, Vec<i32>>;

/// Compute + attach the list marker (`1.`, `a.`, `•`) for one numbered paragraph, advancing the
/// shared `counters` (deeper levels reset when a higher level advances). Shared by the body flow and
/// table cells so a single counter runs across the whole document in order.
pub(crate) fn mark_block(
    block: &mut scriptor_layout::Block,
    p: &scriptor_crdt::Paragraph,
    numbering: &scriptor_crdt::Numbering,
    styles: &scriptor_crdt::StyleTable,
    counters: &mut ListCounters,
    scale: f32,
) {
    if numbering.is_empty() {
        return;
    }
    // Effective list membership: a direct `w:numPr` on the paragraph wins; otherwise inherit it from
    // the paragraph's style chain (Word's outline headings - Rubrik1/Heading1 etc. - carry their
    // numbering on the style, not the paragraph, so "1." / "1.1" come from here).
    let (num, ilvl_opt) = match p.props.num_id {
        Some(n) => (n, p.props.num_ilvl),
        None => {
            let sp = styles.resolve(p.style.as_deref());
            match sp.num_id {
                Some(n) => (n, sp.num_ilvl),
                None => return,
            }
        }
    };
    if num == 0 {
        return; // numId 0 = explicitly no list
    }
    let ilvl = ilvl_opt.unwrap_or(0).max(0);
    let Some(aid) = numbering.abstract_id(num) else { return };
    let Some(level) = numbering.level(num, ilvl) else { return };
    let (fmt, lvl_text, start, ind_left, ind_hanging) =
        (level.fmt.clone(), level.text.clone(), level.start, level.ind_left, level.ind_hanging);

    let vec = counters.entry(aid).or_default();
    let l = ilvl as usize;
    if vec.len() <= l {
        vec.resize(l + 1, i32::MIN);
    }
    vec[l] = if vec[l] == i32::MIN { start } else { vec[l] + 1 };
    for slot in vec.iter_mut().skip(l + 1) {
        *slot = i32::MIN;
    }
    let snapshot = vec.clone();

    let marker = if fmt == "bullet" {
        // Honour the level's own glyph (our synthesized lists cycle `• o ▪` by depth). Imported lists
        // often encode bullets as Symbol/Wingdings private-use codepoints (U+E000-U+F8FF) that aren't
        // in our fonts - fall back to a real bullet for those (and for an empty template).
        let renderable =
            !lvl_text.is_empty() && lvl_text.chars().all(|c| !('\u{E000}'..='\u{F8FF}').contains(&c));
        if renderable { lvl_text.clone() } else { "\u{2022}".to_string() }
    } else {
        render_marker(numbering, num, &lvl_text, &snapshot)
    };

    // Word's hanging indent: the marker hangs in the gap and the text aligns at a fixed left edge `L`
    // (so "1." and "10." start their text at the SAME x), continuation lines too. Effective `L` /
    // hanging `H`: a direct `w:ind` on the paragraph wins, else the level's. Place the block's left at
    // `L - H` (where the marker hangs) and tell the layout the hang distance so it shifts the text to
    // `L`. A zero hang falls back to the inline marker (marker + em space).
    let twip_px = |t: i32| (t.max(0) as f32 / 20.0) * (96.0 / 72.0) * scale;
    let l_twip = p.props.indent_left.unwrap_or(ind_left);
    let h_twip = match p.props.indent_first {
        Some(fl) if fl < 0 => -fl, // a hanging indent is a negative first-line indent
        _ => ind_hanging,
    };
    if h_twip > 0 {
        block.marker = marker;
        block.indent_left_px = twip_px(l_twip - h_twip);
        block.hang_px = twip_px(h_twip);
    } else {
        block.marker = format!("{marker}\u{2003}"); // no hang: marker + em space, inline
        if p.props.indent_left.is_none() {
            block.indent_left_px = twip_px(l_twip);
        }
    }
}

/// Build the document flow ([`Content`]) from the body structure, applying list markers across the
/// whole document (body + table cells) with one shared counter. Body paragraphs index into `blocks`
/// (and get their markers applied in place); table cells resolve + mark their own blocks. When the
/// document has no tables, the flow is just every paragraph in order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_flow(
    body: &[scriptor_crdt::BodyItem],
    paras: &[scriptor_crdt::Paragraph],
    blocks: &mut [scriptor_layout::Block],
    numbering: &scriptor_crdt::Numbering,
    styles: &scriptor_crdt::StyleTable,
    scale: f32,
    mode: TrackDisplay,
    hidden: &std::collections::HashSet<String>,
    frames: &mut Vec<FrameSpec>,
) -> Vec<scriptor_layout::Content> {
    let mut counters: ListCounters = std::collections::HashMap::new();
    let mut content = Vec::new();

    if body.is_empty() {
        for (i, p) in paras.iter().enumerate() {
            mark_block(&mut blocks[i], p, numbering, styles, &mut counters, scale);
            content.push(scriptor_layout::Content::Para(i));
        }
        return content;
    }

    let mut cursor = 0usize;
    // An open text-frame group (raw framePr, its block indices, its anchor body block), flushed when
    // the next item isn't a same-framePr frame paragraph.
    let mut cur_frame: Option<(String, Vec<usize>, usize, bool)> = None;
    let mut last_body = 0usize;
    let mut last_body_break = false; // the last in-flow body paragraph ended with a page break (any kind)
    // The last body paragraph forces a FOLLOWING TABLE onto a new page: a true section terminator, or
    // an empty manual-break paragraph (a deliberate "page break, then table"). A non-empty paragraph
    // that merely ends with a manual break does NOT - Word keeps the table with the break's tail.
    let mut last_body_table_break = false;
    // A table whose cell carried a manual page break pushes the content AFTER the table to a new page -
    // but only when that content is non-empty (Word adds no trailing blank page for a dangling break).
    let mut pending_incell_break = false;
    macro_rules! flush_frame {
        () => {
            if let Some((raw, idxs, anchor, after_break)) = cur_frame.take() {
                frames.push(FrameSpec { blocks: idxs, raw, anchor, after_break });
            }
        };
    }
    for item in body {
        match item {
            scriptor_crdt::BodyItem::Paragraph => {
                if let (Some(b), Some(p)) = (blocks.get_mut(cursor), paras.get(cursor)) {
                    mark_block(b, p, numbering, styles, &mut counters, scale);
                    match &p.props.frame {
                        // A framed paragraph extends the current frame group - consecutive framePr
                        // paragraphs are ONE frame (Word's multi-paragraph text box), kept OUT of the
                        // inline flow. The group's geometry uses the most-specified framePr (the
                        // longest string - continuation paragraphs often carry only a bare `wrap`).
                        Some(raw) => match &mut cur_frame {
                            Some((best, idxs, _, _)) => {
                                idxs.push(cursor);
                                if raw.len() > best.len() {
                                    *best = raw.clone();
                                }
                            }
                            None => {
                                cur_frame = Some((raw.clone(), vec![cursor], last_body, last_body_break))
                            }
                        },
                        None => {
                            flush_frame!();
                            let is_empty = b.spans.iter().all(|s| s.text.is_empty());
                            // The previous in-flow body block carried a break (section / manual page
                            // break): start this paragraph on a new page.
                            if last_body_break {
                                b.page_break_before = true;
                            }
                            // A manual page break inside a preceding table's cell pushes the next
                            // content to a new page - but only if it's non-empty (no trailing blank).
                            if pending_incell_break {
                                if !is_empty {
                                    b.page_break_before = true;
                                }
                                pending_incell_break = false;
                            }
                            content.push(scriptor_layout::Content::Para(cursor));
                            last_body = cursor;
                            last_body_break = p.props.page_break_after;
                            // A table after this paragraph breaks only for a real section terminator or
                            // a deliberate empty page-break paragraph.
                            last_body_table_break =
                                p.props.section_end || (is_empty && p.props.page_break_after);
                        }
                    }
                }
                cursor += 1;
            }
            scriptor_crdt::BodyItem::Table(t) => {
                flush_frame!();
                // A section terminator (or deliberate empty page-break paragraph) before the table
                // starts it on a new page. A table carries no break of its own, so the per-paragraph
                // flags clear here; a manual break inside a cell is handled after the rows are built.
                let tbl_break = last_body_table_break;
                last_body_break = false;
                last_body_table_break = false;
                let mut cell_has_break = false;
                // twips -> px, and an OOXML border (eighths-of-a-point) -> a resolved px border.
                let to_px = |tw: u32| (tw as f32 / 20.0) * (96.0 / 72.0) * scale;
                let border_px = |cell_edge: &Option<scriptor_crdt::Border>,
                                 tbl_edge: &Option<scriptor_crdt::Border>|
                 -> Option<scriptor_layout::CellBorder> {
                    cell_edge.clone().or_else(|| tbl_edge.clone()).map(|b| {
                        scriptor_layout::CellBorder {
                            width: ((b.size_eighths as f32 / 8.0) * (96.0 / 72.0) * scale).max(1.0),
                            color: parse_hex(&b.color),
                        }
                    })
                };
                // Cell paragraphs are flow paragraphs (already resolved into `blocks`); pull each
                // cell's contiguous slice by `cursor` (document order), marking lists in place + the
                // shared counter, and record the flat index of each block for the caret geometry.
                // Effective table borders: an edge the table doesn't set directly falls back to its
                // `w:tblStyle`'s borders (the TableGrid grid lines live only in the style). Without
                // this, every style-bordered table - Word's default Insert-Table look - renders
                // borderless. Direct `w:tblBorders` still win per edge.
                let style_b = styles.resolve_table_borders(t.style.as_deref());
                let tb_top = t.borders.top.clone().or_else(|| style_b.top.clone());
                let tb_left = t.borders.left.clone().or_else(|| style_b.left.clone());
                let tb_bottom = t.borders.bottom.clone().or_else(|| style_b.bottom.clone());
                let tb_right = t.borders.right.clone().or_else(|| style_b.right.clone());
                // A side the table's direct tblCellMar doesn't set falls back to the table style
                // chain (TableNormal's 108-twip left/right is where Word's default cell padding
                // lives), then to Word's built-in default.
                let style_m = styles.resolve_table_cell_margins(t.style.as_deref());
                // The table style's first-row (header) shading, if the table's tblLook enables it.
                let first_row_shd = if look_first_row(t.look.as_deref()) {
                    styles.resolve_table_first_row_shd(t.style.as_deref())
                } else {
                    None
                };
                let mut rows = Vec::with_capacity(t.rows.len());
                let row_count = t.rows.len();
                for (ri, row) in t.rows.iter().enumerate() {
                    let mut cells = Vec::with_capacity(row.cells.len());
                    let cell_count = row.cells.len();
                    for (ci, cell) in row.cells.iter().enumerate() {
                        // A tracked row revision marks every cell; otherwise the cell's own (column)
                        // revision, if any. Decorate the cell's blocks (struck/underlined + recoloured
                        // in All-Markup) and light the change-bar in the markup views.
                        let change = row.change.as_ref().or(cell.change.as_ref());
                        let mut cb = Vec::with_capacity(cell.para_count);
                        let mut para_ids = Vec::with_capacity(cell.para_count);
                        for _ in 0..cell.para_count {
                            if let (Some(p), Some(b)) = (paras.get(cursor), blocks.get_mut(cursor)) {
                                mark_block(b, p, numbering, styles, &mut counters, scale);
                                // A manual page break in the table's FINAL cell is a trailing break that
                                // pushes the content after the table onto a new page (Word). A break in
                                // an earlier (inter-column / inter-row) cell splits the table internally
                                // and does NOT move the following content - so only the last cell counts.
                                if p.props.page_break_after
                                    && ri + 1 == row_count
                                    && ci + 1 == cell_count
                                {
                                    cell_has_break = true;
                                }
                                // Re-resolve vertical spacing through the table style (docDefaults <
                                // table style < paragraph style < direct). A TableGrid-styled table's
                                // pPr sets after=0/line=240, so its cells are single-spaced with no
                                // space-after; without this layer the cells keep docDefaults' body
                                // spacing (8pt after, 1.08x line), inflating every row and
                                // over-paginating dense tables (1-table-1-page, calendar3/4/5).
                                if t.style.is_some() {
                                    let tb = styles.resolve_in_table(p.style.as_deref(), t.style.as_deref());
                                    b.space_before_px = p.props.space_before.or(tb.space_before).map(&to_px).unwrap_or(0.0);
                                    b.space_after_px = p.props.space_after.or(tb.space_after).map(&to_px).unwrap_or(0.0);
                                    // Re-resolve line spacing through the table style, honoring the rule.
                                    let (lv, lr) = if let Some(v) = p.props.line_spacing {
                                        (Some(v), p.props.line_rule)
                                    } else {
                                        (tb.line_spacing, tb.line_rule)
                                    };
                                    let (lm, lex, lmin) = match (lv, lr) {
                                        (Some(v), Some(scriptor_crdt::LineRule::Exact)) if v > 0 => (1.0, to_px(v as u32), 0.0),
                                        (Some(v), _) if v > 0 => (v as f32 / 240.0, 0.0, 0.0),
                                        _ => (1.0, 0.0, 0.0),
                                    };
                                    b.line_mult = lm;
                                    b.line_exact_px = lex;
                                    b.line_min_px = lmin;
                                }
                                if let Some(c) = change.filter(|c| !hidden.contains(&c.author)) {
                                    mark_table_block(b, c, mode);
                                }
                                cb.push(b.clone());
                                para_ids.push(cursor);
                            }
                            cursor += 1;
                        }
                        // Cell margins, per side: tcMar, else the direct tblCellMar, else the table
                        // style chain, else Word's built-in default (108 twips left/right, 0
                        // top/bottom - the padding every Word table shows).
                        let cm = cell.margins.unwrap_or_default();
                        let tm = t.cell_margins.unwrap_or_default();
                        let side = |c: Option<u32>, t_: Option<u32>, s: Option<u32>, d: u32| {
                            c.or(t_).or(s).unwrap_or(d)
                        };
                        // A direct cell shd wins; otherwise row 0 takes the style's header band.
                        let shading = cell.shading.as_deref().map(parse_hex).or_else(|| {
                            (ri == 0).then(|| first_row_shd.as_deref().map(parse_hex)).flatten()
                        });
                        // Word's auto-colour rule: text with no explicit colour renders WHITE on a
                        // dark fill (a black header band keeps its heading readable). Only the
                        // default auto ink flips - an explicit run/style colour is kept.
                        if let Some(bg) = shading {
                            let lum =
                                0.299 * bg[0] as f32 + 0.587 * bg[1] as f32 + 0.114 * bg[2] as f32;
                            if lum < 128.0 {
                                for b in &mut cb {
                                    for s in &mut b.spans {
                                        if s.color == [0x1a, 0x1a, 0x1a] {
                                            s.color = [0xFF, 0xFF, 0xFF];
                                        }
                                    }
                                }
                            }
                        }
                        cells.push(scriptor_layout::CellData {
                            blocks: cb,
                            para_ids,
                            grid_span: cell.grid_span.max(1),
                            vmerge_restart: cell.vmerge == scriptor_crdt::VMerge::Restart,
                            vmerge_continue: cell.vmerge == scriptor_crdt::VMerge::Continue,
                            margins: [
                                to_px(side(cm.top, tm.top, style_m.top, 0)),
                                to_px(side(cm.left, tm.left, style_m.left, 108)),
                                to_px(side(cm.bottom, tm.bottom, style_m.bottom, 0)),
                                to_px(side(cm.right, tm.right, style_m.right, 108)),
                            ],
                            borders: scriptor_layout::CellEdges {
                                top: border_px(&cell.borders.top, &tb_top),
                                left: border_px(&cell.borders.left, &tb_left),
                                bottom: border_px(&cell.borders.bottom, &tb_bottom),
                                right: border_px(&cell.borders.right, &tb_right),
                            },
                            shading,
                        });
                    }
                    rows.push(scriptor_layout::RowData {
                        cells,
                        min_height: row.height.map(to_px).unwrap_or(0.0),
                        exact: row.height_exact,
                        cant_split: row.cant_split,
                    });
                }
                content.push(scriptor_layout::Content::Table(scriptor_layout::TableData {
                    col_widths: t.col_widths.iter().map(|w| to_px(*w)).collect(),
                    // tblPr/jc, else the first row's trPr/jc (Word's per-row alignment, uniform in
                    // practice) - positions the absolute grid within the text column.
                    justify: match t
                        .justify
                        .as_deref()
                        .or_else(|| t.rows.first().and_then(|r| r.justify.as_deref()))
                    {
                        Some("center") => 1,
                        Some("right") | Some("end") => 2,
                        _ => 0,
                    },
                    rows,
                    page_break_before: tbl_break,
                }));
                // A cell's manual page break breaks the content after the table (gated to non-empty
                // content at the consuming paragraph).
                pending_incell_break = cell_has_break;
            }
        }
    }
    flush_frame!();
    content
}

/// Decorate a table cell's block for a tracked row / column revision: in the markup views light the
/// change-bar; in All-Markup recolour the text to the author's hue and strike (deletion) / underline
/// (insertion) it, mirroring run-level redline. Other modes render the cell plain (v1 doesn't hide a
/// deleted row / inserted row in the Final / Original views - the structure stays visible).
pub(crate) fn mark_table_block(b: &mut scriptor_layout::Block, change: &scriptor_crdt::Track, mode: TrackDisplay) {
    if !matches!(mode, TrackDisplay::AllMarkup | TrackDisplay::SimpleMarkup) {
        return;
    }
    b.has_change = true;
    if mode == TrackDisplay::AllMarkup {
        let color = track_colour(&change.author, change.kind);
        for s in &mut b.spans {
            s.color = color;
            match change.kind {
                TrackKind::Del => s.strike = true,
                _ => s.underline = true,
            }
        }
    }
}

/// Expand a level text template (`%1.%2`) into a marker, formatting each `%N` with level N-1's
/// current counter + number format.
pub(crate) fn render_marker(
    numbering: &scriptor_crdt::Numbering,
    num: i32,
    text: &str,
    counters: &[i32],
) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
            let lvl = (chars[i + 1] as u8 - b'1') as i32; // %1 -> level 0
            let val = counters
                .get(lvl.max(0) as usize)
                .copied()
                .filter(|v| *v != i32::MIN)
                .or_else(|| numbering.level(num, lvl).map(|l| l.start))
                .unwrap_or(1);
            let fmt = numbering.level(num, lvl).map(|l| l.fmt.clone()).unwrap_or_else(|| "decimal".into());
            out.push_str(&format_num(val, &fmt));
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

pub(crate) fn format_num(n: i32, fmt: &str) -> String {
    match fmt {
        "lowerLetter" => alpha(n, false),
        "upperLetter" => alpha(n, true),
        "lowerRoman" => roman(n).to_lowercase(),
        "upperRoman" => roman(n),
        _ => n.to_string(),
    }
}

/// Spreadsheet-style alphabetic numbering: 1->a, 26->z, 27->aa.
pub(crate) fn alpha(n: i32, upper: bool) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let mut n = n;
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    let r: String = s.into_iter().rev().collect();
    if upper { r.to_uppercase() } else { r }
}

pub(crate) fn roman(mut n: i32) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let table = [
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"),
        (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
    ];
    let mut s = String::new();
    for (v, sym) in table {
        while n >= v {
            s.push_str(sym);
            n -= v;
        }
    }
    s
}
