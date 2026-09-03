//! `document.xml` -> CRDT.
//! 
//! One SAX pass over the body, building the block tree as it goes: paragraphs and
//! runs, tables, section properties, tracked changes, fields, bookmarks, hyperlinks,
//! images and unmodeled passthrough, all interleaved in document order. Positions are
//! recorded as character offsets during the walk and converted to Peritext marks in a
//! second pass, because a mark cannot be placed until the text it covers exists.

use super::*;

// ── import (document.xml -> CRDT) ────────────────────────────────────────────

/// A comment's anchored range, captured during import: the comment id + the codepoint range
/// `(start_para, start_off)..(end_para, end_off)` over the flat paragraph list. Applied as `cmt~{id}`
/// Peritext marks after the tree is built (see [`apply_comment_anchors`]).
#[derive(Debug, Clone, Copy)]
pub struct CommentAnchor {
    pub id: u64,
    pub start_para: usize,
    pub start_off: usize,
    pub end_para: usize,
    pub end_off: usize,
}

/// Build the block tree from `word/document.xml` bytes. Returns counts, the body item order, and the
/// captured comment anchors (`w:commentRangeStart`/`End`); caller commits + applies the anchors.
pub fn import_document_xml(
    doc: &LoroDoc,
    xml: &[u8],
) -> Result<(ImportStats, ImportAnchors)> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut stats = ImportStats::default();

    // Per-document state.
    let mut table_depth = 0usize;
    // Byte offset of an open nested <w:tbl> (table depth 2), captured verbatim at its close
    // because the model cannot represent a table inside a cell. See NestedBlock.
    let mut nested_start: Option<usize> = None;

    // Tables are loro citizens (tables-crdt T2.7): a parsed `<w:tbl>` builds a table NODE hosting a grid,
    // not an in-memory `Vec<BodyItem>`. `cell_paras` accumulates the current (outer) table's cell
    // paragraphs row-major, lifted into the grid at `</w:tbl>`. Top-level paragraph content stays in the
    // loro flow. The flat paragraph index every anchor records (`appended`) advances per paragraph in
    // parse order (top-level + cell) - which equals `block_seq` order - so cell-anchored comment / field /
    // bookmark / hyperlink ranges still resolve after the flip (they descend into the cell's text).
    let mut cell_paras: Vec<Paragraph> = Vec::new();
    let mut cur_table: Option<Table> = None;
    let mut cur_row: Option<TableRow> = None;
    let mut cur_cell: Option<TableCell> = None;
    let mut in_grid = false;
    // Which border / margin container the top/left/bottom/right edge elements currently belong to
    // (those element names are reused for borders AND margins, so they need context).
    let mut in_tbl_borders = false;
    let mut in_tc_borders = false;
    let mut in_tbl_cellmar = false;
    let mut in_tc_mar = false;
    // Inside a paragraph's `w:pBdr`: the top/left/bottom/right edges here are the paragraph box,
    // accumulated into `pbdr_edges` and committed to `para_props.border` at `</w:pBdr>`.
    let mut in_para_pbdr = false;
    let mut pbdr_edges: Vec<String> = Vec::new();
    let mut in_tabs = false; // inside pPr/w:tabs (distinguishes a tab STOP from a tab CHARACTER)
    // Section breaks. A `<w:sectPr>` in a paragraph's pPr ends that section; the body-level final one
    // closes the last section. A break to a new section starts a new page UNLESS the *following*
    // section's `w:type` is `continuous` / `nextColumn`. Since the type lives on the section AFTER the
    // break, we defer: on each `</w:sectPr>` we know its type, and apply it to the PREVIOUS section's
    // last paragraph (remembered in `last_section_para`) as a page-break-after - reusing the same
    // forced-break machinery as manual page breaks.
    let mut in_sectpr = false;
    let mut cur_sect_type: Option<String> = None;
    let mut cur_para_has_sectpr = false; // the paragraph currently being built ends a section
    let mut last_section_para: Option<TreeID> = None; // top-level block id of the last section-ender
    // Tracked table-structure revisions: a `w:ins`/`w:del` inside `w:trPr` marks the row; a
    // `w:cellIns`/`w:cellDel` inside `w:tcPr` marks the cell. `in_trpr` routes the (otherwise run-level)
    // `w:ins`/`w:del` names to the row when they appear in row properties.
    let mut in_trpr = false;
    let mut row_change: Option<Track> = None;
    let mut cell_change: Option<Track> = None;
    // Tracked table-PROPERTY revisions (`w:tblPrChange` / `w:trPrChange` / `w:tcPrChange`). Same
    // snapshot/reset/restore trick as rPrChange: on the change element's start, bank the NEW props
    // (already parsed) into `*prc_saved`, reset the live table/row/cell props so the nested old
    // `w:tblPr`/`w:trPr`/`w:tcPr` fills them with the OLD values; on its end, snapshot the live (OLD)
    // props into the change record and restore the NEW backup. Attached at the w:tbl / w:tr / w:tc end.
    let mut tbl_prop_change: Option<TablePropChange> = None;
    let mut row_prop_change: Option<TablePropChange> = None;
    let mut cell_prop_change: Option<TablePropChange> = None;
    let mut tblprc_saved: Option<TablePropSnapshot> = None;
    let mut trprc_saved: Option<TablePropSnapshot> = None;
    let mut tcprc_saved: Option<TablePropSnapshot> = None;
    let mut tprc_author = String::new();
    let mut tprc_date = String::new();
    let mut tprc_id: u64 = 0;

    // Per-paragraph state.
    let mut in_para = false;
    let mut style: Option<String> = None;
    let mut para_props = ParaProps::default();
    let mut segs: Vec<Run> = Vec::new();

    // Tracked paragraph-property change (`w:pPrChange`) state - same snapshot/restore trick as
    // rPrChange (the nested `<w:pPr>` holds the OLD style + props).
    let mut para_prop_change: Option<ParaPropChange> = None;
    let mut pprc_author = String::new();
    let mut pprc_date = String::new();
    let mut pprc_id: u64 = 0;
    let mut pprc_saved: Option<(Option<String>, ParaProps)> = None;

    // Tracked paragraph-mark revision (`w:pPr/w:rPr/w:ins|w:del`): the rPr inside pPr is the
    // paragraph mark's props, distinct from a run's rPr (so route by `in_ppr_rpr`).
    let mut para_mark: Option<Track> = None;
    let mut in_ppr_rpr = false;

    // Per-run state.
    let mut in_run = false;
    let mut in_rpr = false;
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut strike = false;
    let mut run_size: Option<u16> = None;
    let mut run_color: Option<String> = None;
    let mut run_font: Option<String> = None;
    let mut run_highlight: Option<String> = None;
    let mut run_char_style: Option<String> = None;
    let mut run_shading: Option<String> = None;
    let mut run_vert_align: Option<String> = None;
    let mut run_lang: Option<String> = None;

    // Tracked-change + text-capture state.
    let mut track: Option<Track> = None;
    let mut text_kind: Option<TrackKind> = None; // Some(Del) when inside w:delText, Some(Ins) for w:t

    // Move pairing: `moveFromRangeStart` / `moveToRangeStart` carry a shared `w:name` linking the two
    // halves of a move. We map each name to one canonical revision id (the first range id seen for it)
    // so both halves of `Run::track` share an id and resolve together - even though Word stamps the
    // run wrappers + range markers with distinct ids. `cur_move_*` is the active range's canonical id.
    let mut move_names: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut cur_move_from: Option<u64> = None;
    let mut cur_move_to: Option<u64> = None;
    let mut capturing = false;
    let mut cur_text = String::new(); // accumulates one w:t / w:delText body (text + resolved refs)

    // Tracked run-property change (`w:rPrChange`) state. Its nested `<w:rPr>` holds the OLD props, so
    // on entering we snapshot the run's current props, let the nested rPr overwrite the same vars
    // (now read as "old"), then restore the snapshot on exit - keeping the per-element prop arms as-is.
    let mut run_fmt_change: Option<FormatChange> = None;
    let mut rprc_author = String::new();
    let mut rprc_date = String::new();
    let mut rprc_id: u64 = 0;
    let mut rprc_saved: Option<RunProps> = None;

    // Field state: a PAGE / NUMPAGES field's cached result is replaced with a placeholder char
    // (computed per page at paint). `field_active` spans begin..end; `field_in_result` is the part
    // after `separate` (the cached value); `field_ph` is the placeholder if it is a computed field.
    let mut field_active = false;
    let mut field_in_result = false;
    let mut field_ph: Option<char> = None;
    let mut field_result_idx = 0usize;
    let mut capturing_instr = false;
    let mut instr_buf = String::new();

    // Comment anchors: `appended` is the flat index of the paragraph currently being built (the next
    // `append_paragraph`). A `w:commentRangeStart`/`End` records `(id, appended, current codepoint)`;
    // the offset is the chars flushed into `segs` so far (markers sit between runs, so `segs` is
    // up to date). Paired into [`CommentAnchor`]s at the end.
    let mut appended = 0usize;
    let mut comment_starts: Vec<(u64, usize, usize)> = Vec::new();
    let mut comment_ends: Vec<(u64, usize, usize)> = Vec::new();
    let segs_chars = |segs: &[Run]| -> usize { segs.iter().map(|r| r.text.chars().count()).sum() };
    // The `(para, offset)` a milestone (bookmark range marker) sits at. `segs` only describes the
    // paragraph currently in hand: index `appended` while inside one, but once `</w:p>` has fired
    // `appended` already points at the NEXT paragraph and `segs` still holds the just-closed one
    // (it is not cleared until the next `<w:p>`). A block-level marker between paragraphs must
    // therefore anchor to the closed paragraph (`appended - 1`) at its end - not to the next
    // paragraph with a stale offset (which would mark past its length and panic loro). Comment /
    // field / hyperlink markers can't reach here: they're guarded by `in_para` / `in_run`.
    let milestone_pos = |in_para: bool, appended: usize, segs: &[Run]| -> (usize, usize) {
        if in_para || appended == 0 {
            (appended, segs_chars(segs))
        } else {
            (appended - 1, segs_chars(segs))
        }
    };

    // Generic field preservation: the OUTERMOST non-PAGE/NUMPAGES field (e.g. a TOC) is captured as a
    // `FieldAnchor` - its instruction + the codepoint range of its cached result - then re-wrapped on
    // export. `field_depth` tracks nesting (so an inner PAGEREF inside a TOC doesn't spawn its own
    // anchor; it flattens to text). `gen_field` holds the open outer field's (id, instr, start_para,
    // start_off) between its `separate` and `end`.
    let mut field_depth = 0usize;
    let mut next_field_id = 0u64;
    let mut gen_field: Option<(u64, String, usize, usize)> = None;
    let mut field_anchors: Vec<FieldAnchor> = Vec::new();

    // Bookmarks (paired by `w:id`, like comments) + hyperlinks (a `w:hyperlink` element wrapping runs;
    // its target is an internal `#anchor` or an external `rid:{r:id}` resolved to a URL after import).
    let mut bookmark_starts: Vec<(u64, String, usize, usize)> = Vec::new(); // (id, name, para, off)
    let mut bookmark_ends: Vec<(u64, usize, usize)> = Vec::new(); // (id, para, off)
    let mut next_link_id = 0u64;
    let mut cur_link: Option<(u64, String, usize, usize)> = None; // (id, target, start_para, start_off)
    let mut hyperlink_anchors: Vec<HyperlinkAnchor> = Vec::new();

    loop {
        // Position before the event, so a captured span starts at its opening `<`.
        let ev_start = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) => {
                let name = e.name();
                match name.as_ref() {
                    // A shape / VML text box's content (`<w:txbxContent>`): its paragraphs are NOT body
                    // text - they belong to a floating object that Word positions independently and that
                    // doesn't drive body pagination. Skip the whole subtree (both the DrawingML choice
                    // and the VML fallback) so 248 chart/textbox labels don't become 5 pages of body
                    // (FDO74774). Matches `parse_images`, which already skips it.
                    b"w:txbxContent" => {
                        let mut skip = Vec::new();
                        reader.read_to_end_into(name, &mut skip)?;
                    }
                    b"w:tbl" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            cur_table = Some(Table::default());
                            cell_paras.clear();
                        } else if table_depth == 2 {
                            // A table inside a cell. The model cannot hold one (a cell owns a slice
                            // of the flat paragraph list), and its paragraphs used to be skipped
                            // silently - losing every word in it. Remember where it starts so the
                            // whole element can be captured verbatim at its `</w:tbl>`.
                            nested_start = Some(ev_start);
                        }
                    }
                    b"w:tblGrid" if table_depth == 1 => in_grid = true,
                    // A hyperlink wraps runs: capture its target (internal `w:anchor` -> `#name`,
                    // external `r:id` -> `rid:{id}` resolved to a URL after import) + its start
                    // position; the end is recorded on the matching End event.
                    b"w:hyperlink" => {
                        let target = if let Some(anchor) = attr(&e, b"w:anchor") {
                            format!("#{anchor}")
                        } else if let Some(rid) = attr(&e, b"r:id") {
                            format!("rid:{rid}")
                        } else {
                            String::new()
                        };
                        if !target.is_empty() {
                            let id = next_link_id;
                            next_link_id += 1;
                            cur_link = Some((id, target, appended, segs_chars(&segs)));
                        }
                    }
                    b"w:tabs" if in_para && !in_run => in_tabs = true,
                    // A section's properties: embedded in a paragraph's pPr (ends that section) or the
                    // body-level final one. Its `w:type` decides whether the break starting it makes a
                    // new page; applied to the previous section's last paragraph on `</w:sectPr>`.
                    b"w:sectPr" => {
                        in_sectpr = true;
                        cur_sect_type = None;
                        if in_para {
                            cur_para_has_sectpr = true;
                        }
                    }
                    b"w:tblBorders" if table_depth == 1 => in_tbl_borders = true,
                    b"w:tblCellMar" if table_depth == 1 => in_tbl_cellmar = true,
                    // A paragraph's own border box (`w:pPr/w:pBdr`); its edges follow, collected below.
                    b"w:pBdr" if in_para && !in_run => {
                        in_para_pbdr = true;
                        pbdr_edges.clear();
                    }
                    b"w:tr" if table_depth == 1 => {
                        cur_row = Some(TableRow::default());
                        row_change = None;
                    }
                    b"w:tc" if table_depth == 1 => {
                        cur_cell = Some(TableCell { grid_span: 1, ..Default::default() });
                        cell_change = None;
                    }
                    // Row properties: a `w:ins`/`w:del` here is a tracked row revision, not a run one.
                    b"w:trPr" if cur_row.is_some() && !in_para => in_trpr = true,
                    b"w:tcBorders" if cur_cell.is_some() => in_tc_borders = true,
                    b"w:tcMar" if cur_cell.is_some() => in_tc_mar = true,
                    // A tracked table-property change: bank the NEW props, reset the live props, and
                    // capture who/when/id so the nested old `w:tblPr` fills the OLD values (see the End
                    // arm, which snapshots them + restores the NEW backup).
                    b"w:tblPrChange" if table_depth == 1 && cur_table.is_some() && cur_row.is_none() => {
                        if let Some(t) = cur_table.as_mut() {
                            tblprc_saved = Some(table_prop_snapshot(t));
                            t.style = None;
                            t.borders = EdgeBorders::default();
                            t.cell_margins = None;
                        }
                        tprc_author = attr(&e, b"w:author").unwrap_or_default();
                        tprc_date = attr(&e, b"w:date").unwrap_or_default();
                        tprc_id = attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                    b"w:trPrChange" if table_depth == 1 && cur_row.is_some() && cur_cell.is_none() => {
                        if let Some(r) = cur_row.as_mut() {
                            trprc_saved = Some(row_prop_snapshot(r));
                            r.height = None;
                            r.height_exact = false;
                        }
                        tprc_author = attr(&e, b"w:author").unwrap_or_default();
                        tprc_date = attr(&e, b"w:date").unwrap_or_default();
                        tprc_id = attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                    b"w:tcPrChange" if table_depth == 1 && cur_cell.is_some() => {
                        if let Some(c) = cur_cell.as_mut() {
                            tcprc_saved = Some(cell_prop_snapshot(c));
                            c.width = None;
                            c.grid_span = 1;
                            c.vmerge = VMerge::None;
                            c.borders = EdgeBorders::default();
                            c.margins = None;
                            c.shading = None;
                        }
                        tprc_author = attr(&e, b"w:author").unwrap_or_default();
                        tprc_date = attr(&e, b"w:date").unwrap_or_default();
                        tprc_id = attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                    // Top-level paragraphs go to the loro flow; cell paragraphs (depth 1, in a cell)
                    // are captured into the table. Both share the run/format/track parsing below.
                    b"w:p" if table_depth == 0 || (table_depth == 1 && cur_cell.is_some()) => {
                        in_para = true;
                        style = None;
                        para_props = ParaProps::default();
                        segs.clear();
                        para_prop_change = None;
                        para_mark = None;
                    }
                    // The paragraph mark's own rPr (inside pPr, not a run) - its w:ins/w:del records a
                    // tracked paragraph-mark revision.
                    b"w:rPr" if in_para && !in_run => in_ppr_rpr = true,
                    // A tracked paragraph-property change: snapshot the current style + props, capture
                    // who/when/id, reset to defaults, and let the nested pPr fill in the OLD values.
                    b"w:pPrChange" if in_para && !in_run => {
                        pprc_saved = Some((style.clone(), para_props.clone()));
                        pprc_author = attr(&e, b"w:author").unwrap_or_default();
                        pprc_date = attr(&e, b"w:date").unwrap_or_default();
                        pprc_id = attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0);
                        style = None;
                        para_props = ParaProps::default();
                    }
                    b"w:r" if in_para => {
                        in_run = true;
                        bold = false;
                        italic = false;
                        underline = false;
                        strike = false;
                        run_size = None;
                        run_color = None;
                        run_font = None;
                        run_highlight = None;
                        run_char_style = None;
                        run_shading = None;
                        run_vert_align = None;
                        run_lang = None;
                        run_fmt_change = None;
                    }
                    b"w:rPr" if in_run => in_rpr = true,
                    // A tracked run-property change: snapshot the current props, capture who/when/id,
                    // then let the nested rPr overwrite the prop vars with the OLD values.
                    b"w:rPrChange" if in_rpr => {
                        rprc_saved = Some(RunProps {
                            bold,
                            italic,
                            underline,
                            strike,
                            size: run_size,
                            color: run_color.clone(),
                            font: run_font.clone(),
                            highlight: run_highlight.clone(),
                            vert_align: run_vert_align.clone(),
                            lang: run_lang.clone(),
                        });
                        rprc_author = attr(&e, b"w:author").unwrap_or_default();
                        rprc_date = attr(&e, b"w:date").unwrap_or_default();
                        rprc_id = attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0);
                        // Reset to defaults so a prop ABSENT from the old rPr reads as off/unset (the
                        // nested rPr then sets only the props that were present in the before-state).
                        bold = false;
                        italic = false;
                        underline = false;
                        strike = false;
                        run_size = None;
                        run_color = None;
                        run_font = None;
                        run_highlight = None;
                        run_char_style = None;
                        run_shading = None;
                        run_vert_align = None;
                        run_lang = None;
                    }
                    b"w:b" if in_rpr => bold = toggle_on(&e),
                    b"w:i" if in_rpr => italic = toggle_on(&e),
                    b"w:u" if in_rpr => underline = u_on(&e),
                    b"w:strike" if in_rpr => strike = toggle_on(&e),
                    b"w:rFonts" if in_rpr => {
                        if let Some(f) = attr(&e, b"w:ascii") {
                            run_font = Some(f);
                        }
                    }
                    b"w:ins" if in_para => track = revision_track(&e, TrackKind::Ins),
                    b"w:del" if in_para => track = revision_track(&e, TrackKind::Del),
                    // Move run wrappers: take the canonical pair id from the enclosing range marker
                    // (so both halves share an id), keeping the wrapper's author/date.
                    b"w:moveFrom" if in_para => {
                        let mut t = revision_track(&e, TrackKind::MoveFrom);
                        if let (Some(t), Some(cid)) = (t.as_mut(), cur_move_from) {
                            t.id = cid;
                        }
                        track = t;
                    }
                    b"w:moveTo" if in_para => {
                        let mut t = revision_track(&e, TrackKind::MoveTo);
                        if let (Some(t), Some(cid)) = (t.as_mut(), cur_move_to) {
                            t.id = cid;
                        }
                        track = t;
                    }
                    b"w:t" if in_run => {
                        capturing = true;
                        text_kind = Some(TrackKind::Ins);
                    }
                    b"w:delText" if in_run => {
                        capturing = true;
                        text_kind = Some(TrackKind::Del);
                    }
                    // `w:delInstrText` is the instruction of a tracked-DELETED field - same
                    // capture as the live form (unparsed, a deleted field re-exported with an
                    // EMPTY instruction, which Word treats as a broken field).
                    b"w:instrText" | b"w:delInstrText" if in_run => capturing_instr = true,
                    b"w:fldSimple" if in_para => {
                        field_active = true;
                        field_in_result = true;
                        field_ph = attr(&e, b"w:instr").and_then(|i| field_placeholder(i.trim()));
                        field_result_idx = 0;
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:gridCol" if in_grid => {
                        if let (Some(t), Some(w)) =
                            (cur_table.as_mut(), attr(&e, b"w:w").and_then(|s| s.parse().ok()))
                        {
                            t.col_widths.push(w);
                        }
                    }
                    // The section type (`<w:type w:val="continuous"|"nextPage"|...>`) inside a sectPr.
                    b"w:type" if in_sectpr => cur_sect_type = attr(&e, b"w:val"),
                    b"w:gridSpan" => {
                        if let Some(c) = cur_cell.as_mut() {
                            c.grid_span =
                                attr(&e, b"w:val").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
                        }
                    }
                    b"w:tblStyle" if table_depth == 1 => {
                        if let Some(t) = cur_table.as_mut() {
                            t.style = attr(&e, b"w:val");
                        }
                    }
                    b"w:tblLook" if table_depth == 1 => {
                        if let Some(t) = cur_table.as_mut() {
                            t.look = raw_attrs(&e);
                        }
                    }
                    // Row alignment (trPr/jc) vs table alignment (tblPr/jc): a paragraph's jc has
                    // in_para set; tblPr comes before any row opens.
                    b"w:jc" if in_trpr => {
                        if let Some(r) = cur_row.as_mut() {
                            r.justify = attr(&e, b"w:val");
                        }
                    }
                    b"w:jc" if table_depth == 1 && !in_para && !in_trpr && cur_row.is_none() => {
                        if let Some(t) = cur_table.as_mut() {
                            t.justify = attr(&e, b"w:val");
                        }
                    }
                    b"w:vMerge" => {
                        if let Some(c) = cur_cell.as_mut() {
                            c.vmerge = match attr(&e, b"w:val").as_deref() {
                                Some("restart") => VMerge::Restart,
                                _ => VMerge::Continue, // bare <w:vMerge/> = continue
                            };
                        }
                    }
                    b"w:tcW" => {
                        if let Some(c) = cur_cell.as_mut() {
                            c.width = attr(&e, b"w:w").and_then(|s| s.parse().ok());
                        }
                    }
                    b"w:trHeight" => {
                        if let Some(r) = cur_row.as_mut() {
                            r.height = attr(&e, b"w:val").and_then(|s| s.parse().ok());
                            r.height_exact = attr(&e, b"w:hRule").as_deref() == Some("exact");
                        }
                    }
                    // "Allow row to break across pages" OFF: the row paginates whole. A bare
                    // <w:cantSplit/> is ON; w:val="false"/"0" explicitly re-allows splitting.
                    b"w:cantSplit" => {
                        if let Some(r) = cur_row.as_mut() {
                            r.cant_split =
                                !matches!(attr(&e, b"w:val").as_deref(), Some("false") | Some("0"));
                        }
                    }
                    // A tab-stop definition (inside pPr/w:tabs) - record position + alignment.
                    b"w:tab" if in_tabs => {
                        if attr(&e, b"w:val").as_deref() != Some("clear")
                            && let Some(p) = attr(&e, b"w:pos").and_then(|s| s.parse().ok()) {
                                // 0=left, 1=center, 2=right, 3=decimal; 255 = bar (a vertical
                                // rule, not an alignment stop) - skip so it doesn't consume a tab.
                                let kind = match attr(&e, b"w:val").as_deref() {
                                    Some("center") => 1u8,
                                    Some("right") | Some("end") => 2,
                                    Some("decimal") => 3,
                                    Some("bar") => 255,
                                    _ => 0, // left / start / num / default
                                };
                                if kind != 255 {
                                    para_props.tab_stops.push(p);
                                    para_props.tab_kinds.push(kind);
                                }
                            }
                    }
                    // A literal tab character in a run - emit a "\t" run carrying the run's formatting.
                    b"w:tab" if in_run => {
                        segs.push(Run {
                            text: "\t".into(),
                            bold,
                            italic,
                            underline,
                            strike,
                            size: run_size,
                            color: run_color.clone(),
                            font: run_font.clone(),
                            highlight: run_highlight.clone(),
                            vert_align: run_vert_align.clone(),
                            lang: run_lang.clone(),
                            char_style: run_char_style.clone(),
                            shading: run_shading.clone(),
                            track: track.clone(),
                            fmt_change: run_fmt_change.clone(),
                            comments: Vec::new(),
                            field: None,
                            bookmarks: Vec::new(),
                        point_bookmarks: Vec::new(),
                        end_point_bookmarks: Vec::new(),
                            link: None,
                            image: None,
                            raw: None,
                        });
                    }
                    // Shading: run-level (rPr) painted behind the run's glyphs; else paragraph (pPr)
                    // or cell (tcPr) background.
                    b"w:shd" => {
                        let fill = attr(&e, b"w:fill").filter(|c| c != "auto" && c != "none");
                        if in_rpr {
                            run_shading = fill;
                        } else if in_para {
                            para_props.shading = fill;
                        } else if let Some(c) = cur_cell.as_mut() {
                            c.shading = fill;
                        }
                    }
                    // A paragraph-border edge (inside `w:pBdr`): keep weight / spacing / colour for the
                    // box the layout paints. A `w:val="none"/"nil"` (or absent) edge has no line.
                    b"w:top" | b"w:left" | b"w:bottom" | b"w:right" if in_para_pbdr => {
                        let val = attr(&e, b"w:val").unwrap_or_default();
                        if !val.is_empty() && val != "none" && val != "nil" {
                            let sz = attr(&e, b"w:sz").unwrap_or_else(|| "4".into());
                            let space = attr(&e, b"w:space").unwrap_or_else(|| "0".into());
                            let color = attr(&e, b"w:color").unwrap_or_else(|| "auto".into());
                            let edge = match name.as_ref() {
                                b"w:top" => "t",
                                b"w:left" => "l",
                                b"w:bottom" => "b",
                                _ => "r",
                            };
                            pbdr_edges.push(format!("{edge}={val},{sz},{space},{color}"));
                        }
                    }
                    // Border edges (within tblBorders / tcBorders) and margin edges (within
                    // tblCellMar / tcMar) share element names, so route by the active container.
                    b"w:top" | b"w:left" | b"w:bottom" | b"w:right" | b"w:insideH" | b"w:insideV"
                        if in_tbl_borders || in_tc_borders =>
                    {
                        let border = parse_border(&e);
                        let edges = if in_tc_borders {
                            cur_cell.as_mut().map(|c| &mut c.borders)
                        } else {
                            cur_table.as_mut().map(|t| &mut t.borders)
                        };
                        if let Some(edges) = edges {
                            match name.as_ref() {
                                b"w:top" => edges.top = border,
                                b"w:left" => edges.left = border,
                                b"w:bottom" => edges.bottom = border,
                                b"w:right" => edges.right = border,
                                b"w:insideH" => edges.inside_h = border,
                                b"w:insideV" => edges.inside_v = border,
                                _ => {}
                            }
                        }
                    }
                    b"w:top" | b"w:left" | b"w:bottom" | b"w:right"
                        if in_tbl_cellmar || in_tc_mar =>
                    {
                        let w: u32 = attr(&e, b"w:w").and_then(|s| s.parse().ok()).unwrap_or(0);
                        let m = if in_tc_mar {
                            cur_cell.as_mut().map(|c| c.margins.get_or_insert(CellMargins::default()))
                        } else {
                            cur_table.as_mut().map(|t| t.cell_margins.get_or_insert(CellMargins::default()))
                        };
                        if let Some(m) = m {
                            match name.as_ref() {
                                b"w:top" => m.top = Some(w),
                                b"w:left" => m.left = Some(w),
                                b"w:bottom" => m.bottom = Some(w),
                                b"w:right" => m.right = Some(w),
                                _ => {}
                            }
                        }
                    }
                    b"w:pStyle" if in_para && !in_run => {
                        style = attr(&e, b"w:val");
                    }
                    b"w:jc" if in_para && !in_run => {
                        para_props.align = attr(&e, b"w:val").as_deref().and_then(Align::from_ooxml);
                    }
                    b"w:ilvl" if in_para && !in_run => {
                        para_props.num_ilvl = attr(&e, b"w:val").and_then(|s| s.parse().ok());
                    }
                    b"w:numId" if in_para && !in_run => {
                        para_props.num_id = attr(&e, b"w:val").and_then(|s| s.parse().ok());
                    }
                    b"w:spacing" if in_para && !in_run => {
                        para_props.line_spacing = attr(&e, b"w:line").and_then(|s| s.parse().ok());
                        para_props.line_rule = attr(&e, b"w:lineRule").and_then(|s| LineRule::from_ooxml(&s));
                        para_props.space_before = attr(&e, b"w:before").and_then(|s| s.parse().ok());
                        para_props.space_after = attr(&e, b"w:after").and_then(|s| s.parse().ok());
                    }
                    b"w:ind" if in_para && !in_run => {
                        para_props.indent_left = attr(&e, b"w:left").and_then(|s| s.parse().ok());
                        para_props.indent_right = attr(&e, b"w:right").and_then(|s| s.parse().ok());
                        // OOXML: firstLine (positive) and hanging (subtracted) are mutually exclusive.
                        if let Some(fl) = attr(&e, b"w:firstLine").and_then(|s| s.parse::<i32>().ok()) {
                            para_props.indent_first = Some(fl);
                        } else if let Some(h) = attr(&e, b"w:hanging").and_then(|s| s.parse::<i32>().ok()) {
                            para_props.indent_first = Some(-h);
                        }
                    }
                    b"w:keepNext" if in_para && !in_run => para_props.keep_next = Some(toggle_on(&e)),
                    b"w:contextualSpacing" if in_para && !in_run => {
                        para_props.contextual_spacing = Some(toggle_on(&e))
                    }
                    // `w:pageBreakBefore` (pPr): force this paragraph to a new page.
                    b"w:pageBreakBefore" if in_para && !in_run => {
                        para_props.page_break_before = toggle_on(&e)
                    }
                    // A text frame (`w:framePr`): this paragraph is a positioned floating box. Capture
                    // its attributes verbatim (escaped, export-ready) for the round-trip; the layout
                    // parses position / size / wrap on demand.
                    b"w:framePr" if in_para && !in_run => {
                        let attrs: Vec<String> = e
                            .attributes()
                            .flatten()
                            .map(|a| {
                                let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
                                let v = a
                                    .normalized_value(quick_xml::XmlVersion::Explicit1_0)
                                    .ok()
                                    .map(|c| c.into_owned())
                                    .unwrap_or_default();
                                format!("{k}=\"{}\"", xml_escape(&v))
                            })
                            .collect();
                        if !attrs.is_empty() {
                            para_props.frame = Some(attrs.join(" "));
                        }
                    }
                    // A manual page break (`<w:br w:type="page"/>`) inside a run: the content after it
                    // continues on a new page. Recorded on the paragraph; layout breaks after it.
                    b"w:br" if in_run && attr(&e, b"w:type").as_deref() == Some("page") => {
                        para_props.page_break_after = true
                    }
                    // A manual column break (`<w:br w:type="column"/>`): in a single-column document it
                    // is a page break (the layer maps it). Recorded for round-trip; layout decides.
                    b"w:br" if in_run && attr(&e, b"w:type").as_deref() == Some("column") => {
                        para_props.column_break_after = true
                    }
                    b"w:b" if in_rpr => bold = toggle_on(&e),
                    b"w:i" if in_rpr => italic = toggle_on(&e),
                    b"w:u" if in_rpr => underline = u_on(&e),
                    b"w:strike" if in_rpr => strike = toggle_on(&e),
                    b"w:rFonts" if in_rpr => {
                        if let Some(f) = attr(&e, b"w:ascii") {
                            run_font = Some(f);
                        }
                    }
                    // Character style reference: kept for the round-trip and resolved for its highlight.
                    b"w:rStyle" if in_rpr => run_char_style = attr(&e, b"w:val"),
                    b"w:sz" if in_rpr => run_size = attr(&e, b"w:val").and_then(|s| s.parse().ok()),
                    // The paragraph MARK's size (pPr/rPr/sz) - the height of an empty paragraph's
                    // line in Word (tiny-mark spacer paragraphs).
                    b"w:sz" if in_ppr_rpr => {
                        para_props.mark_size = attr(&e, b"w:val").and_then(|s| s.parse().ok());
                    }
                    b"w:color" if in_rpr => {
                        // Keep an explicit `w:val="auto"` (don't fold it to "unset"): automatic colour
                        // means render-black AND override any inherited colour - so a `color="auto"`
                        // run inside a styled table stays black instead of picking up the table
                        // style's rPr colour. `parse_hex("auto")` already yields the near-black
                        // default, and exporting it back round-trips. Only a truly empty value drops.
                        run_color = attr(&e, b"w:val").filter(|c| !c.is_empty())
                    }
                    b"w:highlight" if in_rpr => {
                        // Keep an explicit `w:val="none"` (don't fold it to "unset"): it CANCELS an
                        // inherited highlight (a run/char-style "none" overrides the paragraph or
                        // table style's highlight), and `highlight_rgb("none")` paints nothing. Only a
                        // truly empty value drops. Same idea as `color="auto"`.
                        run_highlight = attr(&e, b"w:val").filter(|c| !c.is_empty())
                    }
                    b"w:vertAlign" if in_rpr => {
                        run_vert_align = attr(&e, b"w:val")
                            .filter(|v| v == "superscript" || v == "subscript")
                    }
                    b"w:lang" if in_rpr => {
                        run_lang = attr(&e, b"w:val").filter(|v| !v.is_empty())
                    }
                    // A tracked paragraph-mark revision (empty `<w:ins/>` / `<w:del/>` inside pPr/rPr) -
                    // including a move's ¶ (`<w:moveFrom/>` / `<w:moveTo/>`), so a pending multi-paragraph
                    // move round-trips through docx.
                    b"w:ins" if in_ppr_rpr => para_mark = revision_track(&e, TrackKind::Ins),
                    b"w:del" if in_ppr_rpr => para_mark = revision_track(&e, TrackKind::Del),
                    b"w:moveFrom" if in_ppr_rpr => para_mark = revision_track(&e, TrackKind::MoveFrom),
                    b"w:moveTo" if in_ppr_rpr => para_mark = revision_track(&e, TrackKind::MoveTo),
                    // A tracked row revision (empty `<w:ins/>` / `<w:del/>` inside `w:trPr`).
                    b"w:ins" if in_trpr => row_change = revision_track(&e, TrackKind::Ins),
                    b"w:del" if in_trpr => row_change = revision_track(&e, TrackKind::Del),
                    // A tracked cell revision (empty `<w:cellIns/>` / `<w:cellDel/>` inside `w:tcPr`).
                    b"w:cellIns" if cur_cell.is_some() => {
                        cell_change = revision_track(&e, TrackKind::Ins)
                    }
                    b"w:cellDel" if cur_cell.is_some() => {
                        cell_change = revision_track(&e, TrackKind::Del)
                    }
                    // Comment anchor range boundaries (empty elements between runs in a paragraph).
                    b"w:commentRangeStart" if in_para => {
                        if let Some(id) = attr(&e, b"w:id").and_then(|s| s.parse().ok()) {
                            comment_starts.push((id, appended, segs_chars(&segs)));
                        }
                    }
                    b"w:commentRangeEnd" if in_para => {
                        if let Some(id) = attr(&e, b"w:id").and_then(|s| s.parse().ok()) {
                            comment_ends.push((id, appended, segs_chars(&segs)));
                        }
                    }
                    // Move range boundaries (milestone elements): map the shared `w:name` to one
                    // canonical id so the source + destination halves pair. Not guarded by `in_para`
                    // so a block-level moved-paragraph range is captured too.
                    b"w:moveFromRangeStart" => {
                        let name = attr(&e, b"w:name").unwrap_or_default();
                        let rid = attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0);
                        cur_move_from = Some(*move_names.entry(name).or_insert(rid));
                    }
                    b"w:moveFromRangeEnd" => cur_move_from = None,
                    b"w:moveToRangeStart" => {
                        let name = attr(&e, b"w:name").unwrap_or_default();
                        let rid = attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0);
                        cur_move_to = Some(*move_names.entry(name).or_insert(rid));
                    }
                    b"w:moveToRangeEnd" => cur_move_to = None,
                    // Bookmark range boundaries (milestone elements). Not guarded by in_para (a bookmark
                    // can be block-level / span paragraphs). Word's reserved `_GoBack` is skipped.
                    b"w:bookmarkStart" => {
                        let name = attr(&e, b"w:name").unwrap_or_default();
                        if name != "_GoBack"
                            && let Some(id) = attr(&e, b"w:id").and_then(|s| s.parse().ok()) {
                                let (bp, bo) = milestone_pos(in_para, appended, &segs);
                                bookmark_starts.push((id, name, bp, bo));
                            }
                    }
                    b"w:bookmarkEnd" => {
                        if let Some(id) = attr(&e, b"w:id").and_then(|s| s.parse().ok()) {
                            let (bp, bo) = milestone_pos(in_para, appended, &segs);
                            bookmark_ends.push((id, bp, bo));
                        }
                    }
                    b"w:fldChar" if in_run => match attr(&e, b"w:fldCharType").as_deref() {
                        Some("begin") => {
                            field_depth += 1;
                            // Only the outermost field drives the PAGE/NUMPAGES placeholder state +
                            // generic capture; nested fields (inside a TOC result) leave it untouched
                            // so their result text stays plain and the outer field keeps spanning.
                            if field_depth == 1 {
                                field_active = true;
                                field_in_result = false;
                                field_ph = None;
                                field_result_idx = 0;
                                instr_buf.clear();
                            }
                        }
                        Some("separate") if field_depth == 1 => {
                            field_in_result = true;
                            field_ph = field_placeholder(instr_buf.trim());
                            // A non-computed outer field (TOC, REF, ...) starts a generic capture: its
                            // result range will be marked + re-wrapped on export. PAGE/NUMPAGES keep
                            // the placeholder path (field_ph is Some).
                            if field_ph.is_none() {
                                let fid = next_field_id;
                                next_field_id += 1;
                                gen_field = Some((fid, instr_buf.clone(), appended, segs_chars(&segs)));
                            }
                        }
                        Some("end") => {
                            if field_depth == 1 {
                                if let Some((fid, instr, sp, so)) = gen_field.take() {
                                    field_anchors.push(FieldAnchor {
                                        id: fid,
                                        instr,
                                        start_para: sp,
                                        start_off: so,
                                        end_para: appended,
                                        end_off: segs_chars(&segs),
                                    });
                                }
                                field_active = false;
                                field_in_result = false;
                                field_ph = None;
                            }
                            field_depth = field_depth.saturating_sub(1);
                        }
                        _ => {}
                    },
                    // A self-closing `<w:p/>` is an empty paragraph (no runs, default props). quick-xml
                    // reports it as `Empty` - no Start/End pair - so the `</w:p>` append handler never
                    // fires and the paragraph would vanish, collapsing blank lines and under-counting
                    // page height (Word lays every empty paragraph out as a full line, so dropping them
                    // mis-paginates - e.g. firstheadernofooter.docx folded 2 pages into 1). Mirror the
                    // Start+End path with empty/default state: a self-closing element carries no pPr or
                    // runs, and `appended` still advances by one so later anchors stay index-aligned.
                    b"w:p" if table_depth == 0 => {
                        append_paragraph(doc, &[], None)?;
                        in_para = false;
                        appended += 1;
                        stats.paragraphs += 1;
                    }
                    b"w:p" if table_depth == 1 && cur_cell.is_some() => {
                        if let Some(cell) = cur_cell.as_mut() {
                            cell_paras.push(Paragraph {
                                style: None,
                                props: ParaProps::default(),
                                runs: Vec::new(),
                                prop_change: None,
                                mark_change: None,
                            });
                            cell.para_count += 1;
                            appended += 1;
                        }
                        stats.table_paragraphs += 1;
                    }
                    _ => {}
                }
            }
            // quick-xml 0.38+ no longer unescapes Text: entity / character references arrive as
            // separate GeneralRef events. Accumulate decoded text + resolved refs into one buffer,
            // then flush a single Run when the w:t / w:delText element ends (see the End arm) - so a
            // w:t body like "AT&amp;T" survives as one run (the export path re-escapes via xml_escape).
            Event::Text(t) if capturing_instr => {
                instr_buf.push_str(&t.decode()?);
            }
            Event::Text(t) if capturing && in_para => {
                cur_text.push_str(&t.decode()?);
            }
            Event::GeneralRef(r) if capturing && in_para => {
                cur_text.push_str(&resolve_reference(&r)?);
            }
            Event::End(e) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:tbl" => {
                        table_depth = table_depth.saturating_sub(1);
                        // Closing a nested table: capture it verbatim onto the enclosing cell, at the
                        // position its paragraphs occupied.
                        if table_depth == 1
                            && let Some(start) = nested_start.take()
                            && let Some(cell) = cur_cell.as_mut()
                            && let Ok(s) = std::str::from_utf8(&xml[start..reader.buffer_position() as usize])
                        {
                            cell.nested.push(NestedBlock {
                                after_para: cell.para_count,
                                xml: s.to_string(),
                            });
                        }
                        if table_depth == 0
                            && let Some(mut t) = cur_table.take() {
                                t.prop_change = tbl_prop_change.take();
                                // Lift the parsed table + its row-major cell paragraphs into a loro table
                                // NODE + grid (tables-crdt T2.7). Created at the current end of the body,
                                // so it interleaves with the top-level paragraphs in document order.
                                let node = create_table_node(doc)?;
                                populate_grid_from_table(&open_table_grid(doc, node)?, &t, &cell_paras)?;
                                cell_paras.clear();
                            }
                    }
                    b"w:tblGrid" => in_grid = false,
                    b"w:tabs" => in_tabs = false,
                    b"w:tblBorders" => in_tbl_borders = false,
                    b"w:tcBorders" => in_tc_borders = false,
                    b"w:tblCellMar" => in_tbl_cellmar = false,
                    b"w:tcMar" => in_tc_mar = false,
                    // Commit the collected paragraph-box edges (if any) onto the current paragraph.
                    b"w:pBdr" => {
                        in_para_pbdr = false;
                        if !pbdr_edges.is_empty() {
                            para_props.border = Some(pbdr_edges.join("|"));
                        }
                    }
                    b"w:trPr" => in_trpr = false,
                    // End of a tracked table-property change: the live props now hold the OLD state;
                    // bank them into the change record, then restore the NEW backup (mirrors pPrChange).
                    b"w:tblPrChange" if tblprc_saved.is_some() => {
                        if let Some(t) = cur_table.as_mut() {
                            tbl_prop_change = Some(TablePropChange {
                                author: std::mem::take(&mut tprc_author),
                                date: std::mem::take(&mut tprc_date),
                                id: tprc_id,
                                old: table_prop_snapshot(t),
                            });
                            if let Some(s) = tblprc_saved.take() {
                                apply_table_snapshot(t, &s);
                            }
                        }
                    }
                    b"w:trPrChange" if trprc_saved.is_some() => {
                        if let Some(r) = cur_row.as_mut() {
                            row_prop_change = Some(TablePropChange {
                                author: std::mem::take(&mut tprc_author),
                                date: std::mem::take(&mut tprc_date),
                                id: tprc_id,
                                old: row_prop_snapshot(r),
                            });
                            if let Some(s) = trprc_saved.take() {
                                apply_row_snapshot(r, &s);
                            }
                        }
                    }
                    b"w:tcPrChange" if tcprc_saved.is_some() => {
                        if let Some(c) = cur_cell.as_mut() {
                            cell_prop_change = Some(TablePropChange {
                                author: std::mem::take(&mut tprc_author),
                                date: std::mem::take(&mut tprc_date),
                                id: tprc_id,
                                old: cell_prop_snapshot(c),
                            });
                            if let Some(s) = tcprc_saved.take() {
                                apply_cell_snapshot(c, &s);
                            }
                        }
                    }
                    b"w:tc" if table_depth == 1 => {
                        if let (Some(row), Some(mut cell)) = (cur_row.as_mut(), cur_cell.take()) {
                            cell.change = cell_change.take();
                            cell.prop_change = cell_prop_change.take();
                            row.cells.push(cell);
                        }
                    }
                    b"w:tr" if table_depth == 1 => {
                        if let (Some(t), Some(mut r)) = (cur_table.as_mut(), cur_row.take()) {
                            r.change = row_change.take();
                            r.prop_change = row_prop_change.take();
                            t.rows.push(r);
                        }
                    }
                    b"w:t" | b"w:delText" => {
                        if !cur_text.is_empty() {
                            // A computed field's cached result becomes one placeholder char (later
                            // result runs of the same field are dropped); otherwise take the text.
                            let is_field = field_active && field_in_result && field_ph.is_some();
                            let text = if is_field {
                                cur_text.clear();
                                field_result_idx += 1;
                                if field_result_idx == 1 {
                                    field_ph.unwrap().to_string()
                                } else {
                                    String::new()
                                }
                            } else {
                                std::mem::take(&mut cur_text)
                            };
                            if !text.is_empty() {
                                // A run inside w:del carries a Del track even if the wrapper attrs
                                // were sparse; prefer the wrapper's track, else synthesize.
                                let run_track = track.clone().or_else(|| {
                                    text_kind.filter(|k| matches!(k, TrackKind::Del)).map(|kind| {
                                        Track { kind, author: String::new(), date: String::new(), id: 0 }
                                    })
                                });
                                segs.push(Run {
                                    text,
                                    bold,
                                    italic,
                                    underline,
                                    strike,
                                    size: run_size,
                                    color: run_color.clone(),
                                    font: run_font.clone(),
                                    highlight: run_highlight.clone(),
                                    vert_align: run_vert_align.clone(),
                                    lang: run_lang.clone(),
                                    char_style: run_char_style.clone(),
                                    shading: run_shading.clone(),
                                    track: run_track,
                                    fmt_change: run_fmt_change.clone(),
                                    comments: Vec::new(),
                                    field: None,
                                    bookmarks: Vec::new(),
                        point_bookmarks: Vec::new(),
                        end_point_bookmarks: Vec::new(),
                                    link: None,
                                    image: None,
                                    raw: None,
                                });
                            }
                        }
                        capturing = false;
                        text_kind = None;
                    }
                    b"w:instrText" | b"w:delInstrText" => capturing_instr = false,
                    b"w:hyperlink" => {
                        if let Some((id, target, sp, so)) = cur_link.take() {
                            hyperlink_anchors.push(HyperlinkAnchor {
                                id,
                                target,
                                start_para: sp,
                                start_off: so,
                                end_para: appended,
                                end_off: segs_chars(&segs),
                            });
                        }
                    }
                    b"w:fldSimple" => {
                        field_active = false;
                        field_in_result = false;
                        field_ph = None;
                    }
                    // End of a tracked run-property change: the prop vars now hold the OLD props; bank
                    // them as the run's `fmt_change`, then restore the snapshot (the current props).
                    b"w:rPrChange" => {
                        let old = RunProps {
                            bold,
                            italic,
                            underline,
                            strike,
                            size: run_size,
                            color: run_color.clone(),
                            font: run_font.clone(),
                            highlight: run_highlight.clone(),
                            vert_align: run_vert_align.clone(),
                            lang: run_lang.clone(),
                        };
                        run_fmt_change = Some(FormatChange {
                            author: std::mem::take(&mut rprc_author),
                            date: std::mem::take(&mut rprc_date),
                            id: rprc_id,
                            old,
                        });
                        if let Some(s) = rprc_saved.take() {
                            bold = s.bold;
                            italic = s.italic;
                            underline = s.underline;
                            strike = s.strike;
                            run_size = s.size;
                            run_color = s.color;
                            run_font = s.font;
                            run_highlight = s.highlight;
                            run_vert_align = s.vert_align;
                            run_lang = s.lang;
                        }
                    }
                    b"w:rPr" => {
                        in_rpr = false;
                        in_ppr_rpr = false;
                    }
                    b"w:r" => {
                        in_run = false;
                        bold = false;
                        italic = false;
                    }
                    // A section ends: the break that STARTS this section makes a new page unless it's
                    // `continuous` (same page, e.g. a column-layout change) or `nextColumn`. Apply it
                    // as a page-break-after the PREVIOUS section's last paragraph (recorded at its
                    // `</w:p>`); the first section has no predecessor, so nothing breaks before it.
                    b"w:sectPr" => {
                        in_sectpr = false;
                        let page_creating =
                            !matches!(cur_sect_type.as_deref(), Some("continuous") | Some("nextColumn"));
                        if let Some(prev) = last_section_para {
                            let meta = doc.get_tree(BLOCKS).get_meta(prev)?;
                            if page_creating {
                                meta.insert("pgBrkAft", true)?;
                                // Mark it a section terminator (distinct from a manual `<w:br>`): an
                                // empty one mustn't spill to its own page, and a table after it starts
                                // a new page.
                                meta.insert("sectEnd", true)?;
                            } else {
                                // A continuous (or nextColumn) break: no page is created. If the carrier
                                // is empty, Word consolidates it away - the layout drops its line + after
                                // (tdf169986 + the `*bottomSpacing` continuous-break fixtures).
                                meta.insert("contSect", true)?;
                            }
                        }
                    }
                    b"w:ins" | b"w:del" | b"w:moveFrom" | b"w:moveTo" => track = None,
                    // End of a tracked paragraph-property change: the style/props vars now hold the
                    // OLD state; bank them, then restore the snapshot (the current state).
                    b"w:pPrChange" => {
                        para_prop_change = Some(ParaPropChange {
                            author: std::mem::take(&mut pprc_author),
                            date: std::mem::take(&mut pprc_date),
                            id: pprc_id,
                            old_style: style.take(),
                            old: std::mem::take(&mut para_props),
                        });
                        if let Some((s, p)) = pprc_saved.take() {
                            style = s;
                            para_props = p;
                        }
                    }
                    b"w:p" if in_para && table_depth == 0 => {
                        // Top-level paragraph: into the editable loro flow (a flat root node).
                        let id = append_paragraph(doc, &segs, style.as_deref())?;
                        let meta = doc.get_tree(BLOCKS).get_meta(id)?;
                        if para_props != ParaProps::default() {
                            write_para_props(&meta, &para_props)?;
                        }
                        if let Some(c) = &para_prop_change {
                            write_para_prop_change(&meta, c)?;
                        }
                        if let Some(m) = &para_mark {
                            write_para_mark(&meta, m)?;
                        }
                        // This paragraph ended a section: remember it so the NEXT section's break type
                        // (known at the next `</w:sectPr>`) can set its page-break-after.
                        if cur_para_has_sectpr {
                            last_section_para = Some(id);
                            cur_para_has_sectpr = false;
                        }
                        in_para = false;
                        appended += 1;
                        stats.paragraphs += 1;
                    }
                    b"w:p" if in_para => {
                        // Cell paragraph (inside a cell): captured into the table's grid (lifted at
                        // `</w:tbl>`), NOT the flat flow. `appended` still advances by one so an anchor's
                        // flat index stays aligned with `block_seq` (which descends into cells); the cell
                        // records how many paragraphs it owns, so the grid projection slices them right.
                        if let Some(cell) = cur_cell.as_mut() {
                            cell_paras.push(Paragraph {
                                style: style.clone(),
                                props: para_props.clone(),
                                runs: segs.clone(),
                                prop_change: para_prop_change.clone(),
                                mark_change: para_mark.clone(),
                            });
                            cell.para_count += 1;
                            appended += 1;
                        }
                        in_para = false;
                        stats.table_paragraphs += 1;
                    }
                    // A paragraph in a nested table (depth >= 2) - not modeled; skipped silently.
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }

    // Pair each comment id's start with its end into an anchor range (skipping unmatched halves).
    let mut anchors: Vec<CommentAnchor> = Vec::new();
    for (id, sp, so) in &comment_starts {
        if let Some((_, ep, eo)) = comment_ends.iter().find(|(eid, ..)| eid == id) {
            anchors.push(CommentAnchor {
                id: *id,
                start_para: *sp,
                start_off: *so,
                end_para: *ep,
                end_off: *eo,
            });
        }
    }
    // Pair each bookmark id's start with its end (skipping unmatched halves).
    let mut bookmarks: Vec<BookmarkAnchor> = Vec::new();
    for (id, name, sp, so) in &bookmark_starts {
        if let Some((_, ep, eo)) = bookmark_ends.iter().find(|(eid, ..)| eid == id) {
            bookmarks.push(BookmarkAnchor {
                id: *id,
                name: name.clone(),
                start_para: *sp,
                start_off: *so,
                end_para: *ep,
                end_off: *eo,
            });
        }
    }
    let import_anchors = ImportAnchors { comments: anchors, fields: field_anchors, bookmarks, hyperlinks: hyperlink_anchors };
    Ok((stats, import_anchors))
}

/// Apply captured comment anchors as `cmt~{id}` Peritext marks over the (now-built) paragraphs. A
/// single-paragraph anchor marks `[start_off, end_off)`; a multi-paragraph anchor marks the start
/// paragraph's tail, every fully-spanned paragraph, and the end paragraph's head. The `cmt~{id}` mark
/// keys must already be configured. Caller commits.
pub fn apply_comment_anchors(doc: &LoroDoc, anchors: &[CommentAnchor]) -> Result<()> {
    let lengths: Vec<usize> = read_paragraphs(doc)?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.chars().count()).sum())
        .collect();
    for a in anchors {
        if a.start_para == a.end_para {
            mark_comment_range(doc, a.id, a.start_para, a.start_off, a.end_off)?;
        } else {
            let start_len = lengths.get(a.start_para).copied().unwrap_or(0);
            mark_comment_range(doc, a.id, a.start_para, a.start_off, start_len)?;
            for pi in (a.start_para + 1)..a.end_para {
                let len = lengths.get(pi).copied().unwrap_or(0);
                mark_comment_range(doc, a.id, pi, 0, len)?;
            }
            mark_comment_range(doc, a.id, a.end_para, 0, a.end_off)?;
        }
    }
    Ok(())
}

/// A field's captured cached-result range during import (paired `w:fldChar` separate..end, or a
/// `w:fldSimple`) plus its instruction - applied after the paragraphs are built, mirroring
/// [`CommentAnchor`].
#[derive(Debug, Clone)]
pub struct FieldAnchor {
    pub id: u64,
    pub instr: String,
    pub start_para: usize,
    pub start_off: usize,
    pub end_para: usize,
    pub end_off: usize,
}

/// Apply captured field anchors: store each instruction in the `fields` map and mark its cached-result
/// range with `fld~{id}` (single- or multi-paragraph, like [`apply_comment_anchors`]). The `fld~{id}`
/// mark keys must already be configured. Caller commits.
pub fn apply_field_anchors(doc: &LoroDoc, anchors: &[FieldAnchor]) -> Result<()> {
    let lengths: Vec<usize> = read_paragraphs(doc)?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.chars().count()).sum())
        .collect();
    for a in anchors {
        write_field(doc, a.id, &a.instr)?;
        if a.start_para == a.end_para {
            mark_field_range(doc, a.id, a.start_para, a.start_off, a.end_off)?;
        } else {
            let start_len = lengths.get(a.start_para).copied().unwrap_or(0);
            mark_field_range(doc, a.id, a.start_para, a.start_off, start_len)?;
            for pi in (a.start_para + 1)..a.end_para {
                let len = lengths.get(pi).copied().unwrap_or(0);
                mark_field_range(doc, a.id, pi, 0, len)?;
            }
            mark_field_range(doc, a.id, a.end_para, 0, a.end_off)?;
        }
    }
    Ok(())
}

/// A bookmark's captured range during import + its name, applied after the paragraphs are built.
#[derive(Debug, Clone)]
pub struct BookmarkAnchor {
    pub id: u64,
    pub name: String,
    pub start_para: usize,
    pub start_off: usize,
    pub end_para: usize,
    pub end_off: usize,
}

/// A hyperlink's captured range during import + its target. Internal links store `#{anchor}`; external
/// links store `rid:{r:id}` during the parse, resolved to a URL via the document rels afterwards.
#[derive(Debug, Clone)]
pub struct HyperlinkAnchor {
    pub id: u64,
    pub target: String,
    pub start_para: usize,
    pub start_off: usize,
    pub end_para: usize,
    pub end_off: usize,
}

/// Everything captured during a `document.xml` parse that's applied after the paragraphs are built:
/// comment + field + bookmark + hyperlink anchor ranges (each carries the data + the codepoint range).
#[derive(Debug, Default)]
pub struct ImportAnchors {
    pub comments: Vec<CommentAnchor>,
    pub fields: Vec<FieldAnchor>,
    pub bookmarks: Vec<BookmarkAnchor>,
    pub hyperlinks: Vec<HyperlinkAnchor>,
}

/// Apply a (possibly multi-paragraph) annotation range with `mark` (a `mark_*_range` fn): one call for
/// a single-paragraph range, else the start tail + every spanned paragraph + the end head.
#[allow(clippy::too_many_arguments)]
fn apply_range(
    doc: &LoroDoc,
    lengths: &[usize],
    id: u64,
    sp: usize,
    so: usize,
    ep: usize,
    eo: usize,
    mark: fn(&LoroDoc, u64, usize, usize, usize) -> Result<()>,
) -> Result<()> {
    // Clamp every offset to its paragraph's actual length. Captured anchor offsets come from the
    // run-segment cursor during parse, which can legitimately overshoot the stored text (a block-level
    // milestone between paragraphs, a field/deleted-text run dropped from the flow). Marking past the
    // text length is a hard loro panic, so a malformed or surprising anchor must degrade gracefully -
    // the bookmark lands a few chars off, the document still opens - never crash the whole import.
    let so = so.min(lengths.get(sp).copied().unwrap_or(0));
    let eo = eo.min(lengths.get(ep).copied().unwrap_or(0));
    if sp == ep {
        mark(doc, id, sp, so, eo)?;
    } else {
        mark(doc, id, sp, so, lengths.get(sp).copied().unwrap_or(0))?;
        for pi in (sp + 1)..ep {
            mark(doc, id, pi, 0, lengths.get(pi).copied().unwrap_or(0))?;
        }
        mark(doc, id, ep, 0, eo)?;
    }
    Ok(())
}

/// The codepoint length of each paragraph (for multi-paragraph anchor application).
fn paragraph_lengths(doc: &LoroDoc) -> Result<Vec<usize>> {
    Ok(read_paragraphs(doc)?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.chars().count()).sum())
        .collect())
}

/// Apply captured bookmark anchors: store each name in the `bookmarks` map + mark its range with
/// `bkm~{id}`. The mark keys must already be configured. Caller commits.
pub fn apply_bookmark_anchors(doc: &LoroDoc, anchors: &[BookmarkAnchor]) -> Result<()> {
    let lengths = paragraph_lengths(doc)?;
    for a in anchors {
        write_bookmark(doc, a.id, &a.name)?;
        // A collapsed bookmark (start == end) covers no codepoints, so a range mark would silently
        // drop it - which is exactly how `_Ref…` cross-reference targets used to disappear. Anchor it
        // to the codepoint it sits before instead.
        if a.start_para == a.end_para && a.start_off == a.end_off {
            let len = lengths.get(a.start_para).copied().unwrap_or(0);
            mark_bookmark_point(doc, a.id, a.start_para, a.start_off, len)?;
            continue;
        }
        apply_range(doc, &lengths, a.id, a.start_para, a.start_off, a.end_para, a.end_off, mark_bookmark_range)?;
    }
    Ok(())
}

/// Apply captured hyperlink anchors: store each target in the `hyperlinks` map + mark its range with
/// `lnk~{id}`. The mark keys must already be configured. Caller commits.
pub fn apply_hyperlink_anchors(doc: &LoroDoc, anchors: &[HyperlinkAnchor]) -> Result<()> {
    let lengths = paragraph_lengths(doc)?;
    for a in anchors {
        write_hyperlink(doc, a.id, &a.target)?;
        apply_range(doc, &lengths, a.id, a.start_para, a.start_off, a.end_para, a.end_off, mark_link_range)?;
    }
    Ok(())
}

/// A comment parsed from `word/comments.xml`, plus the `w14:paraId`s of its body paragraphs (used to
/// resolve threading / resolved-state from `word/commentsExtended.xml`, which links by paraId).
pub struct ParsedComment {
    pub comment: Comment,
    pub para_ids: Vec<String>,
}
