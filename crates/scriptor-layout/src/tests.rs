use super::*;

#[test]
fn renders_text_to_a_white_page_with_dark_ink() {
    let mut r = Renderer::new();
    let (w, h) = (320u32, 80u32);
    let px = r.render_rgba("Hello world", w, h, 24.0);
    assert_eq!(px.len(), (w * h * 4) as usize);
    // The page starts opaque white; rendered ink must darken at least some pixels.
    let dark = px.as_chunks::<4>().0.iter().filter(|p| p[0] < 128 && p[3] == 255).count();
    assert!(dark > 0, "expected dark ink pixels from rasterized text");
}

/// `w:background w:color` fills the page sheet instead of white when set (the top-4 worst docs
/// of the pixel-diff baseline were an unpainted page colour - ~100% per-page difference).
#[test]
fn page_background_fills_the_sheet() {
    let mut r = Renderer::new();
    let blocks = [Block { line_mult: 1.0, ..Default::default() }];
    let content = vec![Content::Para(0)];
    let layout = r.layout_doc(&blocks, &content, 40, 40, 4.0, 4.0, 4.0, 4.0, 0, 0.0);
    let white = r.paint_page(&blocks, &layout, 0, &[], &[], 0.0, 0.0, &[], &[], &[], 0.0, 0.0, 0.0);
    assert_eq!(&white[0..4], &[255, 255, 255, 255], "no background -> a white sheet");
    r.set_page_background(Some([0x92, 0xD0, 0x50]));
    let green = r.paint_page(&blocks, &layout, 0, &[], &[], 0.0, 0.0, &[], &[], &[], 0.0, 0.0, 0.0);
    assert_eq!(&green[0..4], &[0x92, 0xD0, 0x50, 255], "w:background fills the sheet");
}

/// Two-sided frame wrap: a paragraph with a centre `WrapHole` flows its text through the left
/// column (x <= hole.x0), then the right column (x >= hole.x1), then full width below - so the
/// body reads around the frame on both sides instead of running through it.
#[test]
fn two_sided_wrap_flows_text_around_a_centre_hole() {
    let mut r = Renderer::new();
    let span = |text: &str| Span {
        text: text.into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let words = "lorem ipsum dolor sit amet ".repeat(20);
    let block = Block {
        spans: vec![span(&words)],
        line_mult: 1.0,
        // 600px box; a 100px-wide frame centred at 300, over the first ~80px of vertical band.
        wrap_holes: vec![WrapHole { x0: 250.0, x1: 350.0, top: 0.0, bot: 80.0 }],
        ..Default::default()
    };
    let (_, lines) = r.shape_block_lines(&block, 600.0, 0.0);
    // A line confined to the left column (every stop left of the hole, but real text).
    let left_col = lines
        .iter()
        .any(|(_, _, s)| s.iter().all(|c| c.x <= 250.5) && s.iter().any(|c| c.x > 100.0));
    // A line confined to the right column (every stop right of the hole).
    let right_col = lines.iter().any(|(_, _, s)| s.iter().all(|c| c.x >= 349.5));
    assert!(left_col, "text wraps in the left column beside the frame");
    assert!(right_col, "text wraps in the right column beside the frame");
    assert!(lines.len() > 6, "two-sided wrap produced {} lines", lines.len());
}

/// A tab landing in a sliver of the content's right edge (a right-aligned stop rendered as a left
/// tab) must let the following text overflow on one line, not wrap into the sliver - the pathology
/// that exploded FDO77715 from 46 to 639 pages.
#[test]
fn tab_segment_avoids_sliver_wrap() {
    // 4px of room left of a 500px line: overflow at full width instead of wrapping into 4px.
    assert_eq!(tab_segment_width(4.0, 500.0), 500.0);
    // A tab with ample room keeps its available width (normal left-tab behaviour).
    assert_eq!(tab_segment_width(300.0, 500.0), 300.0);
    // Right at the threshold (25%) stays on the available side.
    assert_eq!(tab_segment_width(125.0, 500.0), 125.0);
}

#[test]
fn tab_align_offset_aligns_on_the_stop() {
    // Right/decimal: shift left by the full segment width so it ends at the stop.
    assert_eq!(tab_align_offset(2, 30.0, 100.0, 0.0), -30.0);
    assert_eq!(tab_align_offset(3, 30.0, 100.0, 0.0), -30.0);
    // Centre: shift left by half the width (straddle the stop).
    assert_eq!(tab_align_offset(1, 30.0, 100.0, 0.0), -15.0);
    // Left (and any unknown kind): no shift.
    assert_eq!(tab_align_offset(0, 30.0, 100.0, 0.0), 0.0);
    // Clamp: a segment wider than the room left of the stop can't slide past the content-box
    // left edge - the offset is capped at `left - pen`, not the full -segw.
    assert_eq!(tab_align_offset(2, 200.0, 100.0, 0.0), -100.0);
}

#[test]
fn right_and_center_tabs_align_the_segment_on_the_stop() {
    let mut r = Renderer::new();
    let span = |text: &str| Span {
        text: text.into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    // "A", a tab to the 200px stop, then "Wide". The stop's alignment decides where "Wide" lands.
    let mk = |kind: u8| Block {
        spans: vec![span("A\tWide")],
        line_mult: 1.0,
        tab_stops_px: vec![200.0],
        tab_kinds: vec![kind],
        ..Default::default()
    };
    let max_x = |r: &mut Renderer, b: &Block| {
        r.shape_block_lines(b, 600.0, 0.0)
            .1
            .iter()
            .flat_map(|(_, _, s)| s.iter())
            .map(|s| s.x)
            .fold(0.0_f32, f32::max)
    };
    let left = max_x(&mut r, &mk(0)); // left tab: "Wide" starts at the stop, extends right
    let right = max_x(&mut r, &mk(2)); // right tab: "Wide" ends at the stop
    let center = max_x(&mut r, &mk(1)); // centre tab: straddles the stop

    assert!(left > 205.0, "left tab: text starts at the stop and runs right (max_x={left})");
    assert!((right - 200.0).abs() < 1.5, "right tab: text ends at the stop (max_x={right})");
    // Centre's right edge sits half the segment width past the stop - between right and left.
    assert!(right < center && center < left, "centre between right/left (c={center} r={right} l={left})");
    assert!(
        (center - (200.0 + (left - 200.0) / 2.0)).abs() < 1.5,
        "centre straddles the stop (c={center})"
    );
}

#[test]
fn shapes_a_line_into_glyphs() {
    let mut fs = FontSystem::new();
    let layout = layout_text(&mut fs, "Hello world", 16.0, None);
    assert!(layout.line_count() >= 1, "expected at least one line");
    assert!(layout.glyph_count() >= 8, "expected glyphs for the text");
    // glyph x offsets advance left-to-right within a line (LTR)
    let first = &layout.lines[0];
    assert!(first.windows(2).all(|w| w[1].x >= w[0].x), "glyph x must be non-decreasing");
}

#[test]
fn swedish_diacritics_keep_distinct_glyphs() {
    // Regression: å ä ö rendered as a/o. Shape through the REAL bundled FontSystem
    // (en-US locale + Advanced shaping, exactly the canvas path) and assert the
    // diacritic codepoints resolve to glyphs distinct from their base letters.
    let mut r = Renderer::new();
    let glyph_of = |fs: &mut FontSystem, ch: &str| -> Option<u16> {
        let mut b = Buffer::new(fs, Metrics::new(16.0, 20.0));
        b.set_text(ch, &Attrs::new().family(Family::Name("Carlito")), Shaping::Advanced, None);
        b.shape_until_scroll(fs, false);
        for run in b.layout_runs() {
            if let Some(g) = run.glyphs.first() {
                return Some(g.glyph_id);
            }
        }
        None
    };
    for (lo, base) in [("å", "a"), ("ä", "a"), ("ö", "o"), ("Å", "A"), ("Ä", "A"), ("Ö", "O")] {
        let g_lo = glyph_of(&mut r.font_system, lo);
        let g_base = glyph_of(&mut r.font_system, base);
        assert!(g_lo.is_some(), "{lo} produced no glyph");
        assert_ne!(g_lo, g_base, "{lo} shaped to the same glyph as {base} (diacritic lost)");
    }
}

#[test]
fn hit_test_disambiguates_side_by_side_lines_by_x() {
    // Two lines sharing a vertical band (a table row's two cells). Hit-testing must pick the line
    // whose horizontal extent is nearest x, not just the first one in the band.
    let layout = DocLayout {
        lines: vec![
            LineBox {
                para: 10,
                y: 0.0,
                height: 20.0,
                stops: vec![CaretStop { byte: 0, x: 0.0 }, CaretStop { byte: 2, x: 40.0 }],
            },
            LineBox {
                para: 11,
                y: 0.0,
                height: 20.0,
                stops: vec![CaretStop { byte: 0, x: 200.0 }, CaretStop { byte: 2, x: 240.0 }],
            },
        ],
        ..Default::default()
    };
    assert_eq!(layout.hit_test(220.0, 10.0).0, 11, "click in the right cell hits its paragraph");
    assert_eq!(layout.hit_test(10.0, 10.0).0, 10, "click in the left cell hits its paragraph");
}

#[test]
fn balloon_band_narrows_the_content_width() {
    let mut r = Renderer::new();
    let blk = Block { line_mult: 1.0, ..Default::default() };
    let args = (816u32, 1056u32, 96.0_f32, 96.0_f32, 96.0_f32, 96.0_f32, 24u32);
    let no_band =
        r.layout_doc(std::slice::from_ref(&blk), &[Content::Para(0)], args.0, args.1, args.2, args.3, args.4, args.5, args.6, 0.0);
    let with_band =
        r.layout_doc(&[blk], &[Content::Para(0)], args.0, args.1, args.2, args.3, args.4, args.5, args.6, 180.0);
    assert_eq!(no_band.balloon_band, 0.0);
    assert_eq!(with_band.balloon_band, 180.0);
    // The band is reserved by narrowing the body (it shifts left), not by widening the page.
    assert!((with_band.content_width - (no_band.content_width - 180.0)).abs() < 0.01);
}

#[test]
fn caret_follows_the_hinted_page_for_a_repeated_header_paragraph() {
    // One header paragraph (para 5) painted on two pages: page 0 at y=10, page 1 at y=1010
    // (page_height 1000 + gap 0). A repeated story's caret must resolve to the page the user is on.
    let layout = DocLayout {
        lines: vec![
            LineBox {
                para: 5,
                y: 10.0,
                height: 20.0,
                stops: vec![CaretStop { byte: 0, x: 0.0 }, CaretStop { byte: 3, x: 30.0 }],
            },
            LineBox {
                para: 5,
                y: 1010.0,
                height: 20.0,
                stops: vec![CaretStop { byte: 0, x: 0.0 }, CaretStop { byte: 3, x: 30.0 }],
            },
        ],
        page_height: 1000,
        gap: 0,
        ..Default::default()
    };
    // No hint -> the first instance (page 0), the old behaviour.
    assert_eq!(layout.caret_rect(5, 1, None).1, 10.0);
    // A page hint picks the matching instance.
    assert_eq!(layout.caret_rect(5, 1, Some(0)).1, 10.0, "hint page 0");
    assert_eq!(layout.caret_rect(5, 1, Some(1)).1, 1010.0, "the caret follows the hinted page");
}

#[test]
fn keep_next_moves_a_heading_to_stay_with_its_body() {
    // Three empty paragraphs on a 100px page (page_bottom = 100, one empty line = 16*1.15 = 18.4px).
    // The middle one (the "heading") is pushed to y=75 by its space-before, so it fits alone
    // (75 + 18.4 = 93.4 < 100) but not together with the first line of the next paragraph
    // (75 + 18.4 + 18.4 = 111.8 > 100). keepNext must then move the heading to the next page.
    let mut r = Renderer::new();
    let content = vec![Content::Para(0), Content::Para(1), Content::Para(2)];
    let empty = || Block { line_mult: 1.0, ..Default::default() };
    let heading = |keep: bool| Block { line_mult: 1.0, space_before_px: 56.6, keep_next: keep, ..Default::default() };

    // Without keepNext the heading sits at the page foot (page 0); the body flows to page 1.
    let plain = [empty(), heading(false), empty()];
    let lay = r.layout_doc(&plain, &content, 200, 100, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(lay.placements[1].page, 0, "without keepNext the heading stays at the page foot");
    assert_eq!(lay.placements[2].page, 1, "and its body falls to the next page");

    // With keepNext the heading + the start of its body don't both fit, so the heading moves down.
    let kept = [empty(), heading(true), empty()];
    let lay2 = r.layout_doc(&kept, &content, 200, 100, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(lay2.placements[1].page, 1, "keepNext moves the heading onto its body's page");
    assert_eq!(lay2.placements[2].page, 1, "heading + body share page 1");
}

#[test]
fn page_break_before_a_table_starts_a_new_page() {
    // A short paragraph then a one-row table. Both fit on page 0; flagging the table
    // page_break_before (a section / manual break preceded it) must push it to page 1.
    let mut r = Renderer::new();
    let blocks = [Block { line_mult: 1.0, ..Default::default() }];
    let mk_table = |brk: bool| TableData {
        col_widths: vec![5000.0],
        justify: 0,
        rows: vec![RowData {
            cant_split: false,
            cells: vec![CellData {
                blocks: vec![],
                para_ids: vec![],
                grid_span: 1,
                vmerge_restart: false,
                vmerge_continue: false,
                margins: [0.0; 4],
                borders: CellEdges::default(),
                shading: None,
            }],
            min_height: 30.0,
            exact: true,
        }],
        page_break_before: brk,
    };

    let no_break = vec![Content::Para(0), Content::Table(mk_table(false))];
    let a = r.layout_doc(&blocks, &no_break, 400, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(a.pages.len(), 1, "without a break the table shares the paragraph's page");

    let with_break = vec![Content::Para(0), Content::Table(mk_table(true))];
    let b = r.layout_doc(&blocks, &with_break, 400, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(b.pages.len(), 2, "page_break_before sends the table to a fresh page");
    assert!(b.cells.iter().all(|c| c.page == 1), "the table's cells land on page 1");
}

#[test]
fn line_step_crosses_paragraph_spacing() {
    // ArrowUp/Down must reach the neighbouring paragraph across its spacing gap - the old 1px
    // hit-test probe landed inside the gap and snapped back to the caret's own line, so
    // vertical arrows worked within a paragraph but never between paragraphs.
    let mut r = Renderer::new();
    let span = |t: &str| Span {
        text: t.into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let blocks = [
        Block { spans: vec![span("first paragraph")], line_mult: 1.0, space_after_px: 40.0, ..Default::default() },
        Block { spans: vec![span("second paragraph")], line_mult: 1.0, ..Default::default() },
    ];
    let content = vec![Content::Para(0), Content::Para(1)];
    let l = r.layout_doc(&blocks, &content, 400, 1000, 10.0, 10.0, 10.0, 10.0, 0, 0.0);
    let down = l.line_step(0, 3, 40.0, true, None).expect("a line below exists");
    assert_eq!(down.0, 1, "ArrowDown crosses the 40px spacing into paragraph 1");
    let up = l.line_step(1, 3, 40.0, false, None).expect("a line above exists");
    assert_eq!(up.0, 0, "ArrowUp crosses back into paragraph 0");
    assert!(l.line_step(0, 3, 40.0, false, None).is_none(), "no line above the first");
    assert!(l.line_step(1, 3, 40.0, true, None).is_none(), "no line below the last");
}

#[test]
fn shape_cache_is_transparent_and_invalidates_on_edit() {
    // The shaped-lines memo must be invisible: a second layout of identical content returns
    // byte-identical geometry, and an edited paragraph re-shapes while its neighbours hit.
    let mut r = Renderer::new();
    let span = |t: &str| Span {
        text: t.into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let mk = |t: &str| {
        vec![
            Block { spans: vec![span(t)], line_mult: 1.0, ..Default::default() },
            Block { spans: vec![span("a stable second paragraph")], line_mult: 1.0, ..Default::default() },
        ]
    };
    let content = vec![Content::Para(0), Content::Para(1)];
    let text = "hello world this wraps across a couple of lines at this width";
    let a1 = r.layout_doc(&mk(text), &content, 220, 1000, 10.0, 10.0, 10.0, 10.0, 0, 0.0);
    let a2 = r.layout_doc(&mk(text), &content, 220, 1000, 10.0, 10.0, 10.0, 10.0, 0, 0.0);
    assert_eq!(a1.lines.len(), a2.lines.len(), "cache-hit pass has the same line count");
    for (l1, l2) in a1.lines.iter().zip(&a2.lines) {
        assert_eq!((l1.para, l1.y, l1.height), (l2.para, l2.y, l2.height), "identical line boxes");
        assert_eq!(l1.stops.len(), l2.stops.len(), "identical caret stops");
        for (s1, s2) in l1.stops.iter().zip(&l2.stops) {
            assert_eq!((s1.byte, s1.x), (s2.byte, s2.x), "identical stop geometry");
        }
    }
    // Edit paragraph 0: its shaped geometry must change (no stale cache hit).
    let b = r.layout_doc(
        &mk("now a much longer paragraph zero with quite a bit more text so it wraps to more lines"),
        &content, 220, 1000, 10.0, 10.0, 10.0, 10.0, 0, 0.0,
    );
    let p0_lines = |l: &DocLayout| l.lines.iter().filter(|x| x.para == 0).count();
    assert!(
        p0_lines(&b) > p0_lines(&a1),
        "the edited paragraph re-shaped ({} -> {} lines)",
        p0_lines(&a1),
        p0_lines(&b)
    );
}

#[test]
fn cell_page_break_before_starts_its_row_on_a_new_page() {
    // A `w:pageBreakBefore` paragraph inside a cell forces its row to a fresh page (tdf89377:
    // a break-before style on each table's first cell paragraph). Suppressed at the page top.
    let mut r = Renderer::new();
    let mk_table = |brk: bool| TableData {
        col_widths: vec![5000.0],
        justify: 0,
        rows: vec![RowData {
            cant_split: false,
            cells: vec![CellData {
                blocks: vec![Block { line_mult: 1.0, page_break_before: brk, ..Default::default() }],
                para_ids: vec![1],
                grid_span: 1,
                vmerge_restart: false,
                vmerge_continue: false,
                margins: [0.0; 4],
                borders: CellEdges::default(),
                shading: None,
            }],
            min_height: 30.0,
            exact: true,
        }],
        page_break_before: false,
    };
    let body = [Block { line_mult: 1.0, ..Default::default() }];
    let plain = vec![Content::Para(0), Content::Table(mk_table(false))];
    let a = r.layout_doc(&body, &plain, 400, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(a.pages.len(), 1, "without the flag the table shares the page");

    let with_break = vec![Content::Para(0), Content::Table(mk_table(true))];
    let b = r.layout_doc(&body, &with_break, 400, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(b.pages.len(), 2, "a break-before cell paragraph sends its row to page 1");
    assert!(b.cells.iter().all(|c| c.page == 1), "the row's cells land on page 1");

    // At the very top of the document the break is suppressed (no leading blank page).
    let first = vec![Content::Table(mk_table(true))];
    let c = r.layout_doc(&[], &first, 400, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(c.pages.len(), 1, "no blank page before a break-row at the document start");
}

#[test]
fn cell_paragraph_indent_shifts_its_text_column() {
    // A cell paragraph's `w:ind` indents within the cell's text column, like the body pass
    // (Word) - the NOBA price rows indent left=300tw so their floating checkbox sits in front
    // of the text rather than under it.
    let mut r = Renderer::new();
    let span = || Span {
        text: "Row".into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let table = |indent: f32| TableData {
        col_widths: vec![5000.0],
        justify: 0,
        rows: vec![RowData {
            cant_split: false,
            cells: vec![CellData {
                blocks: vec![Block {
                    spans: vec![span()],
                    line_mult: 1.0,
                    indent_left_px: indent,
                    ..Default::default()
                }],
                para_ids: vec![1],
                grid_span: 1,
                vmerge_restart: false,
                vmerge_continue: false,
                margins: [2.0, 5.0, 2.0, 5.0],
                borders: CellEdges::default(),
                shading: None,
            }],
            min_height: 0.0,
            exact: false,
        }],
        page_break_before: false,
    };
    let mut first_stop = |indent: f32| {
        let l = r.layout_doc(&[], &[Content::Table(table(indent))], 400, 400, 10.0, 10.0, 10.0, 10.0, 0, 0.0);
        l.lines.iter().find(|l| l.para == 1).expect("the cell line exists").stops[0].x
    };
    let x0 = first_stop(0.0);
    let x1 = first_stop(20.0);
    assert!(
        (x1 - x0 - 20.0).abs() < 0.5,
        "indented cell text starts 20px further right (x0={x0}, x1={x1})"
    );
}

#[test]
fn empty_section_terminator_does_not_spill_to_a_new_page() {
    // An empty paragraph at the top (page 0), then an empty paragraph pushed by a big space-before
    // to just past the page foot (page_bottom = 100). A plain empty para spills to page 1; one that
    // carries page_break_after (a bare w:sectPr terminator) must stay at the foot of page 0 - Word
    // lets that mark sit there and starts the next section on the new page anyway.
    let mut r = Renderer::new();
    let top = Block { line_mult: 1.0, ..Default::default() };
    let term = |sect: bool| Block { line_mult: 1.0, space_before_px: 90.0, section_terminator: sect, ..Default::default() };
    let content = vec![Content::Para(0), Content::Para(1)];

    let spills = [top.clone(), term(false)];
    let a = r.layout_doc(&spills, &content, 200, 100, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(a.placements[1].page, 1, "a plain empty para at the foot spills to the next page");

    let kept = [top, term(true)];
    let b = r.layout_doc(&kept, &content, 200, 100, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(b.placements[1].page, 0, "an empty section terminator stays at the foot");
}

#[test]
fn empty_continuous_break_carrier_is_consolidated_away() {
    // An empty paragraph that only carries a CONTINUOUS section break is consolidated away by
    // Word: it occupies no line and contributes no space-after, so a following paragraph rides up
    // over it rather than being pushed down (and onto a new page) by the carrier's large `w:after`
    // (tdf169986 + the `*bottomSpacing` continuous-break fixtures).
    let mut r = Renderer::new();
    let para = || Block { line_mult: 1.0, ..Default::default() };
    let carrier = |cont: bool| Block {
        line_mult: 1.0,
        space_after_px: 200.0,
        continuous_break: cont,
        ..Default::default()
    };
    let content = vec![Content::Para(0), Content::Para(1), Content::Para(2)];

    // A normal empty paragraph with a 200px space-after pushes the tail past the page foot -> page 1.
    let plain = [para(), carrier(false), para()];
    let a = r.layout_doc(&plain, &content, 200, 100, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(a.placements[2].page, 1, "a normal empty para's space-after spills the tail to page 1");

    // The same carrier flagged continuous: consolidated away, so the tail rides up and stays page 0.
    let cont = [para(), carrier(true), para()];
    let b = r.layout_doc(&cont, &content, 200, 100, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    assert_eq!(b.placements[2].page, 0, "an empty continuous-break carrier consolidates - tail stays on page 0");
    assert!(b.lines.iter().any(|l| l.para == 1), "the consolidated carrier keeps its caret line (still selectable)");
}

#[test]
fn adjacent_paragraph_spacing_collapses_to_the_max() {
    // Word: the gap between two paragraphs is the MAX of the first one's space-after and the
    // second one's space-before, not the sum (tdf169986: 20pt-after meeting 20pt-before renders
    // a ~20pt gap in Word). Either side may win.
    let mut r = Renderer::new();
    let content = vec![Content::Para(0), Content::Para(1)];
    let mk = |after: f32, before: f32| {
        [
            Block { line_mult: 1.0, space_after_px: after, ..Default::default() },
            Block { line_mult: 1.0, space_before_px: before, ..Default::default() },
        ]
    };
    let mut gap = |after: f32, before: f32| {
        let blocks = mk(after, before);
        let l = r.layout_doc(&blocks, &content, 200, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
        l.placements[1].y - (l.placements[0].y + empty_line_height(&blocks[0]))
    };
    let g = gap(30.0, 20.0);
    assert!((g - 30.0).abs() < 0.5, "after wins: max(30, 20) = 30 (got {g})");
    let g = gap(20.0, 50.0);
    assert!((g - 50.0).abs() < 0.5, "before wins: max(20, 50) = 50 (got {g})");
}

#[test]
fn legacy_spacing_sums_adjacent_spacing() {
    // A legacy document (`w:doNotUseHTMLParagraphAutoSpacing` / compatibilityMode <= 11) keeps
    // Word's old behavior: adjacent space-after + space-before SUM (tdf145716's own body text
    // documents this), so 30 + 20 is a 50px gap, not max = 30.
    let mut r = Renderer::new();
    let content = vec![Content::Para(0), Content::Para(1)];
    let blocks = [
        Block { line_mult: 1.0, space_after_px: 30.0, legacy_spacing: true, ..Default::default() },
        Block { line_mult: 1.0, space_before_px: 20.0, legacy_spacing: true, ..Default::default() },
    ];
    let l = r.layout_doc(&blocks, &content, 200, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    let g = l.placements[1].y - (l.placements[0].y + empty_line_height(&blocks[0]));
    assert!((g - 50.0).abs() < 0.5, "legacy mode sums: 30 + 20 = 50 (got {g})");
}

#[test]
fn page_break_before_is_suppressed_on_an_empty_page() {
    // A pageBreakBefore paragraph whose CONTENT would be the first thing on the page must not
    // break - even when its space-before pushes `top` below the margin (tdf95495: a
    // pageBreakBefore style on the document's first paragraph renders on page 1, at
    // margin + space-before, with no leading blank page).
    let mut r = Renderer::new();
    let blocks = [Block {
        line_mult: 1.0,
        space_before_px: 8.0,
        page_break_before: true,
        ..Default::default()
    }];
    let l = r.layout_doc(&blocks, &[Content::Para(0)], 200, 1000, 0.0, 0.0, 10.0, 0.0, 0, 0.0);
    assert_eq!(l.placements[0].page, 0, "no leading blank page");
    assert!(
        (l.placements[0].y - 18.0).abs() < 0.5,
        "margin + space-before, break suppressed (got y={})",
        l.placements[0].y
    );
}

#[test]
fn space_before_is_honored_at_the_start_of_the_document() {
    // Word drops space-before at the top of pages 2+ (tdf170119) but HONORS it on the first
    // paragraph of the document (tdf160049: `w:before=1134` on the doc's first paragraph
    // renders below the top margin in Word).
    let mut r = Renderer::new();
    let blocks = [Block { line_mult: 1.0, space_before_px: 40.0, ..Default::default() }];
    let l = r.layout_doc(&blocks, &[Content::Para(0)], 200, 1000, 0.0, 0.0, 10.0, 0.0, 0, 0.0);
    assert!(
        (l.placements[0].y - 50.0).abs() < 0.5,
        "doc-start space-before is applied: 10 margin + 40 before (got y={})",
        l.placements[0].y
    );
}

#[test]
fn spacing_collapses_across_a_consolidated_carrier() {
    // The empty continuous-break carrier is transparent to the spacing collapse: the paragraph
    // after it consolidates against the carrier's PREDECESSOR - 20pt-after + carrier + 20pt-before
    // is one 20pt gap in Word, not 40 (tdf169986's residual after the carrier itself landed).
    let mut r = Renderer::new();
    let content = vec![Content::Para(0), Content::Para(1), Content::Para(2)];
    let blocks = [
        Block { line_mult: 1.0, space_after_px: 20.0, ..Default::default() },
        Block { line_mult: 1.0, space_after_px: 200.0, continuous_break: true, ..Default::default() },
        Block { line_mult: 1.0, space_before_px: 20.0, ..Default::default() },
    ];
    let l = r.layout_doc(&blocks, &content, 200, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    let g = l.placements[2].y - (l.placements[0].y + empty_line_height(&blocks[0]));
    assert!((g - 20.0).abs() < 0.5, "the gap across the carrier is max(20, 20) = 20 (got {g})");
}

#[test]
fn space_before_is_dropped_at_the_top_of_a_page() {
    // tdf170119: a hard page break, then a consolidated continuous-break carrier, then a paragraph
    // with a large space-before. Word starts that paragraph at the margin exactly - space-before is
    // not applied at the top of a page.
    let mut r = Renderer::new();
    let content = vec![Content::Para(0), Content::Para(1), Content::Para(2)];
    let blocks = [
        Block { line_mult: 1.0, ..Default::default() },
        Block {
            line_mult: 1.0,
            page_break_before: true,
            space_after_px: 200.0,
            continuous_break: true,
            ..Default::default()
        },
        Block { line_mult: 1.0, space_before_px: 150.0, ..Default::default() },
    ];
    let l = r.layout_doc(&blocks, &content, 200, 300, 0.0, 0.0, 10.0, 0.0, 0, 0.0);
    assert_eq!(l.placements[2].page, 1, "the paragraph follows the break onto page 1");
    assert!(
        (l.placements[2].y - 10.0).abs() < 0.5,
        "space-before is dropped at the page top (got y={})",
        l.placements[2].y
    );
}

#[test]
fn exact_line_rule_pins_the_line_height() {
    // `lineRule="exact"` fixes the line box regardless of font size; `auto` uses the font-natural
    // line. A 40px font with exact 24px lines should be SHORTER than its ~49px natural line.
    let mut r = Renderer::new();
    let span = || Span {
        text: "Hi".into(),
        size_px: 40.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let content = vec![Content::Para(0)];
    let exact = [Block { spans: vec![span()], line_mult: 1.0, line_exact_px: 24.0, ..Default::default() }];
    let auto = [Block { spans: vec![span()], line_mult: 1.0, ..Default::default() }];
    let le = r.layout_doc(&exact, &content, 400, 4000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    let la = r.layout_doc(&auto, &content, 400, 4000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    let he = le.lines[0].height;
    let ha = la.lines[0].height;
    assert!((he - 24.0).abs() < 1.5, "exact pins the line to 24px (got {he})");
    assert!(ha > 40.0, "auto uses the natural ~49px line (got {ha})");
}

#[test]
fn contextual_spacing_collapses_same_style_gap() {
    // Two adjacent paragraphs, the first with 10pt space-after. When both opt into
    // contextualSpacing AND share a style group, that 10pt gap collapses; a different style keeps it.
    let mut r = Renderer::new();
    let content = vec![Content::Para(0), Content::Para(1)];
    let mk = |ctx: bool, group: u64, after: f32| Block {
        line_mult: 1.0, contextual_spacing: ctx, style_group: group, space_after_px: after,
        ..Default::default()
    };
    let collapsed = [mk(true, 7, 10.0), mk(true, 7, 0.0)];
    let a = r.layout_doc(&collapsed, &content, 200, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    let gap_collapsed = a.placements[1].y - a.placements[0].y; // ~one line, no inter-para space

    let kept = [mk(true, 7, 10.0), mk(true, 9, 0.0)]; // different style group -> no collapse
    let b = r.layout_doc(&kept, &content, 200, 1000, 0.0, 0.0, 0.0, 0.0, 0, 0.0);
    let gap_kept = b.placements[1].y - b.placements[0].y; // ~one line + 10pt

    assert!(
        (gap_kept - gap_collapsed - 10.0).abs() < 0.5,
        "contextualSpacing removes the 10pt gap (kept={gap_kept}, collapsed={gap_collapsed})"
    );
}

#[test]
fn highlight_hugs_the_font_cell_not_the_spaced_line() {
    // A highlighted span on a double-spaced line: the fill must track the glyph cell
    // (size * line-height factor), not the doubled line box (Word highlights text, not leading).
    let mut r = Renderer::new();
    let span = Span {
        text: "Hi".into(),
        size_px: 40.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: Some([255, 255, 0]),
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let block = Block { spans: vec![span], line_mult: 2.0, ..Default::default() };
    let (w, h) = (200u32, 300u32);
    let mut px = vec![0xFFu8; (w * h) as usize * 4];
    r.raster_block(&block, w as f32, 4.0, 20.0, w, h, &mut px);
    // Count rows containing any yellow (highlight) pixel.
    let mut rows = 0;
    for y in 0..h as usize {
        let mut hit = false;
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            if px[i] > 200 && px[i + 1] > 200 && px[i + 2] < 80 {
                hit = true;
                break;
            }
        }
        if hit {
            rows += 1;
        }
    }
    let cell = 40.0 * line_height_factor(DEFAULT_FAMILY); // ~49px
    let line_box = cell * 2.0; // the doubled line the OLD code filled (~98px)
    assert!(
        (rows as f32) < cell + 8.0 && (rows as f32) > cell - 12.0,
        "highlight ~the font cell ({cell:.0}px), got {rows} rows"
    );
    assert!(
        (rows as f32) < line_box * 0.7,
        "highlight must be well under the doubled line box ({line_box:.0}px), got {rows}"
    );
}

#[test]
fn trailing_pilcrow_is_excluded_from_caret_geometry() {
    let mut r = Renderer::new();
    let span = Span {
        text: "Hello".into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let mut block = Block { spans: vec![span], line_mult: 1.0, ..Default::default() };
    let max_byte = |lines: &[(f32, f32, Vec<CaretStop>)]| {
        lines.iter().flat_map(|(_, _, s)| s.iter()).map(|s| s.byte).max().unwrap()
    };

    // Without a trailing ¶: the last caret stop is the end of "Hello" (byte 5).
    let (_h, lines) = r.shape_block_lines(&block, 400.0, 0.0);
    assert_eq!(max_byte(&lines), 5);

    // With a trailing ¶ (a tracked paragraph mark): the caret geometry is unchanged - the ¶ is
    // painted but is NOT a caret stop, so the caret can't land past the editable text.
    block.trailing = "¶".into();
    block.trailing_color = [0xC0, 0x30, 0x2E];
    block.trailing_strike = true;
    let (_h2, lines2) = r.shape_block_lines(&block, 400.0, 0.0);
    assert_eq!(max_byte(&lines2), 5, "the trailing ¶ must not add a caret stop past the text");
}

#[test]
fn line_spacing_multiplier_scales_line_height() {
    let mut r = Renderer::new();
    let span = || Span {
        text: "Hello".into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    // The same single-line block at single vs "multiple 1.5" spacing (w:spacing w:line="360").
    // cosmic-text takes the line height from the per-span Attrs metrics, so line_mult must reach
    // them - a regression here renders 1.5x spacing as single (the bug these docs exposed).
    let single = Block { spans: vec![span()], line_mult: 1.0, ..Default::default() };
    let onehalf = Block { spans: vec![span()], line_mult: 1.5, ..Default::default() };
    let (h1, _) = r.shape_block_lines(&single, 400.0, 0.0);
    let (h15, _) = r.shape_block_lines(&onehalf, 400.0, 0.0);
    let ratio = h15 / h1;
    assert!(
        (ratio - 1.5).abs() < 0.02,
        "1.5x line spacing must make the line ~1.5x taller, got {h15}/{h1} = {ratio}"
    );
}

#[test]
fn hanging_indent_aligns_text_regardless_of_marker_width() {
    let mut r = Renderer::new();
    let span = || Span {
        text: "Item text".into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    // Two list items with the SAME hanging indent but different-width markers ("1." vs "10.").
    // Word's hanging indent puts the text at a fixed left edge (x_left + hang) for both - the
    // marker hangs in the gap. The first text caret (byte 0) marks where text begins.
    let mk = |marker: &str| Block {
        spans: vec![span()],
        marker: marker.into(),
        hang_px: 24.0,
        line_mult: 1.0,
        ..Default::default()
    };
    let x_left = 40.0;
    let (_h1, l1) = r.shape_block_lines(&mk("1."), 400.0, x_left);
    let (_h2, l2) = r.shape_block_lines(&mk("10."), 400.0, x_left);
    let text_x = |lines: &[(f32, f32, Vec<CaretStop>)]| {
        lines[0].2.iter().map(|c| c.x).fold(f32::MAX, f32::min)
    };
    let (tx1, tx2) = (text_x(&l1), text_x(&l2));
    assert!((tx1 - (x_left + 24.0)).abs() < 0.5, "text starts at the hanging stop, got {tx1}");
    assert!((tx1 - tx2).abs() < 0.5, "marker width must not shift the text ({tx1} vs {tx2})");
}

#[test]
fn inline_image_reserves_a_line_of_its_height() {
    let mut r = Renderer::new();
    let blk = Block {
        line_mult: 1.0,
        inline_images: vec![InlineImage {
            id: 0, byte: 0, w: 120.0, h: 200.0, key: "k".into(), crop: [0; 4],
        }],
        ..Default::default()
    };
    // 816x1056 px (US Letter @96dpi), 96px margins, 24px page gap, no balloon band.
    let dl = r.layout_doc(
        std::slice::from_ref(&blk), &[Content::Para(0)], 816, 1056, 96.0, 96.0, 96.0, 96.0, 24, 0.0,
    );
    assert_eq!(dl.inline_images.len(), 1, "one inline picture placed");
    let im = &dl.inline_images[0];
    assert_eq!((im.w, im.h, im.page), (120.0, 200.0, 0));
    assert!((im.y - 96.0).abs() < 0.5, "placed at the top margin (page-local y), got {}", im.y);
    // A picture-only paragraph reserves exactly its image line - no extra blank text line.
    assert_eq!(dl.lines.len(), 1, "only the image line");
    assert!((dl.lines[0].height - 200.0).abs() < 0.5, "the line is the image's height");
}

#[test]
fn passthrough_placeholder_reserves_a_line_and_places_a_box() {
    let mut r = Renderer::new();
    let blk = Block {
        line_mult: 1.0,
        placeholders: vec![Placeholder {
            id: 0, byte: 0, w: 180.0, h: 120.0, label: "Embedded Object".into(),
        }],
        ..Default::default()
    };
    let dl = r.layout_doc(
        std::slice::from_ref(&blk), &[Content::Para(0)], 816, 1056, 96.0, 96.0, 96.0, 96.0, 24, 0.0,
    );
    assert_eq!(dl.placeholders.len(), 1, "one placeholder box placed");
    let ph = &dl.placeholders[0];
    assert_eq!((ph.w, ph.h, ph.page), (180.0, 120.0, 0));
    assert_eq!(ph.label, "Embedded Object");
    assert!((ph.y - 96.0).abs() < 0.5, "placed at the top margin (page-local y), got {}", ph.y);
    // An object-only paragraph reserves exactly its object line - no blank text line above it.
    assert_eq!(dl.lines.len(), 1, "only the placeholder line");
    assert!((dl.lines[0].height - 120.0).abs() < 0.5, "the line is the box's height");
    // The box paints without panicking (fill + border + caption) into a page buffer.
    let px = r.paint_page(std::slice::from_ref(&blk), &dl, 0, &[], &[], 0.0, 0.0, &[], &[], &[], 0.0, 0.0, 0.0);
    assert_eq!(px.len(), 816 * 1056 * 4);
}

#[test]
fn crop_drops_the_cut_region_before_scaling() {
    // A 100x100 source: left half pure red, right half pure green.
    let mut src = image::RgbaImage::new(100, 100);
    for (x, _y, p) in src.enumerate_pixels_mut() {
        *p = if x < 50 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 255, 0, 255]) };
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(src)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let mut r = Renderer::new();
    r.register_image("k", &png);
    let img = |crop: [i64; 4]| PageImage {
        key: "k".into(), x: 0.0, y: 0.0, w: 100.0, h: 100.0, behind: false, crop, page: 0, id: None, dim: 0.0,
    };
    let red = |px: &[u8]| px.as_chunks::<4>().0.iter().filter(|p| p[0] > 200 && p[1] < 80).count();

    let mut whole = vec![0xFFu8; 100 * 100 * 4];
    r.composite_image(&img([0; 4]), 100, 100, &mut whole);
    let mut cropped = vec![0xFFu8; 100 * 100 * 4];
    // Cut the left 50% (the red half); the kept green half scales to fill the box.
    r.composite_image(&img([50_000, 0, 0, 0]), 100, 100, &mut cropped);

    assert!(red(&whole) > 3000, "uncropped box keeps the red half ({})", red(&whole));
    assert!(red(&cropped) < 200, "left-cropped box drops the red half ({})", red(&cropped));
}

/// A negative `srcRect` extends the box with blank padding (Word keeps a logo's aspect this way):
/// a -100% bottom crop maps the image into the top half of the box, leaving the bottom half blank.
#[test]
fn negative_crop_pads_the_display_box() {
    let mut src = image::RgbaImage::new(40, 40);
    for (_x, _y, p) in src.enumerate_pixels_mut() {
        *p = image::Rgba([255, 0, 0, 255]);
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(src)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let mut r = Renderer::new();
    r.register_image("k", &png);
    let img = PageImage {
        key: "k".into(), x: 0.0, y: 0.0, w: 100.0, h: 100.0, behind: false,
        crop: [0, 0, 0, -100_000], page: 0, id: None, dim: 0.0,
    };
    let mut buf = vec![0xFFu8; 100 * 100 * 4];
    r.composite_image(&img, 100, 100, &mut buf);
    let red_in = |y0: usize, y1: usize| -> usize {
        (y0..y1)
            .flat_map(|y| (0..100usize).map(move |x| (y * 100 + x) * 4))
            .filter(|&i| buf[i] > 200 && buf[i + 1] < 80)
            .count()
    };
    assert!(red_in(0, 50) > 4000, "top half filled by the image ({})", red_in(0, 50));
    assert!(red_in(50, 100) < 200, "bottom half stays blank padding ({})", red_in(50, 100));
}

/// An absurdly-sized draw-rect (a mis-parsed VML shape once asked for a 171k x 171k resize) must
/// not scale the FULL image - that target buffer overflows usize on wasm32 (a panic kills the
/// paint) and stalls natively. Past the cap only the page-visible window is scaled, so the paint
/// completes and the on-page area still gets its pixels.
#[test]
fn oversized_image_rect_composites_only_the_visible_window() {
    let mut src = image::RgbaImage::new(27, 27);
    for (_x, _y, p) in src.enumerate_pixels_mut() {
        *p = image::Rgba([255, 0, 0, 255]);
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(src)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let mut r = Renderer::new();
    r.register_image("k", &png);
    let img = PageImage {
        key: "k".into(), x: 0.0, y: 0.0, w: 171_449.0, h: 171_449.0, behind: false,
        crop: [0; 4], page: 0, id: None, dim: 0.0,
    };
    let mut buf = vec![0xFFu8; 100 * 100 * 4];
    r.composite_image(&img, 100, 100, &mut buf);
    let red = buf.as_chunks::<4>().0.iter().filter(|p| p[0] > 200 && p[1] < 80).count();
    assert_eq!(red, 100 * 100, "the visible window is fully painted");
}

#[test]
fn change_bar_emitted_only_for_changed_paragraphs() {
    let mut r = Renderer::new();
    let span = |text: &str| Span {
        text: text.into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    let plain = Block { spans: vec![span("clean line")], line_mult: 1.0, ..Default::default() };
    // "edited line" is 11 bytes; mark the whole run changed (one visual line).
    let changed = Block {
        spans: vec![span("edited line")],
        line_mult: 1.0,
        has_change: true,
        change_ranges: vec![(0, 11)],
        ..Default::default()
    };
    let blocks = vec![plain, changed];
    let content = vec![Content::Para(0), Content::Para(1)];
    // A big page so both paragraphs land on page 0; 96px margins.
    let layout = r.layout_doc(&blocks, &content, 816, 1056, 96.0, 96.0, 96.0, 96.0, 24, 0.0);

    // Exactly one bar (for paragraph 1, one line), on page 0, aligned to that paragraph's placement.
    assert_eq!(layout.change_bars.len(), 1, "only the changed paragraph gets a bar");
    let bar = layout.change_bars[0];
    assert_eq!(bar.page, 0);
    let p1 = layout.placements.iter().find(|p| p.block == 1).unwrap();
    assert!((bar.y - p1.y).abs() < 0.5, "bar aligns with the changed paragraph's top");
    assert!(bar.height > 0.0);
    // The bar sits inside the left margin (between the page edge and the text), and is visible.
    assert!(layout.change_bar_w >= 1.0);
    assert!(layout.change_bar_x > 0.0 && layout.change_bar_x < layout.margin_left);
}

/// Per-line change-bar: a paragraph that wraps to several lines but is changed on only one line
/// gets a single bar, beside that line - not the whole paragraph (Word bars lines, not paragraphs).
#[test]
fn change_bar_is_per_visual_line() {
    let mut r = Renderer::new();
    let span = |text: &str| Span {
        text: text.into(),
        size_px: 16.0,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: [0, 0, 0],
        highlight: None,
        baseline_shift: 0.0,
        family: DEFAULT_FAMILY.to_string(),
    };
    // One long paragraph that wraps; mark only the first word ("the", bytes 0..3) as changed - it
    // lands on the first visual line only.
    let text = "the quick brown fox jumps over the lazy dog and then keeps running onward";
    let block = Block {
        spans: vec![span(text)],
        line_mult: 1.0,
        has_change: true,
        change_ranges: vec![(0, 3)],
        ..Default::default()
    };
    // A narrow content box forces several wrapped lines (816 - 2*360 margins ~= 96px wide).
    let layout = r.layout_doc(&[block], &[Content::Para(0)], 816, 1056, 360.0, 360.0, 96.0, 96.0, 24, 0.0);
    let para_lines: Vec<_> = layout.lines.iter().filter(|l| l.para == 0).collect();
    assert!(para_lines.len() > 1, "the paragraph wrapped to multiple lines (got {})", para_lines.len());
    assert_eq!(layout.change_bars.len(), 1, "only the one changed line gets a bar, not every line");
    // The bar is one line tall - not the whole (much taller) paragraph: this is the per-line win.
    let para_height: f32 = para_lines.iter().map(|l| l.height).sum();
    let one_line = para_lines[0].height;
    let bar = layout.change_bars[0];
    assert!(bar.height < para_height - 1.0, "the bar spans one line, not the whole paragraph");
    assert!((bar.height - one_line).abs() < 1.0, "the bar is one line tall");
}

#[test]
fn narrow_width_wraps_to_multiple_lines() {
    let mut fs = FontSystem::new();
    let one = layout_text(&mut fs, "the quick brown fox jumps over the lazy dog", 16.0, None);
    let wrapped = layout_text(&mut fs, "the quick brown fox jumps over the lazy dog", 16.0, Some(60.0));
    assert!(wrapped.line_count() > one.line_count(), "narrow width should wrap to more lines");
}
