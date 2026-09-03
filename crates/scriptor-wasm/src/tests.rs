use super::*;
use scriptor_crdt::{Paragraph, ParaProps, Run, Track};

/// Diagnostic probe against a local multi-section fixture (set `SCRIPTOR_FIXTURE` to a .docx
/// path; skips silently otherwise): simulates the click-into-a-footer flow and prints what the
/// caret machinery sees - page part map, per-page footer lines, hit-test routing, an edit.
#[test]
fn fixture_footer_probe() {
    let Ok(path) = std::env::var("SCRIPTOR_FIXTURE") else { return };
    let Ok(bytes) = std::fs::read(&path) else { return };
    let mut d = build_scriptor_doc(&bytes).expect("open");
    d.relayout(1.0).expect("relayout");
    println!("pages={} page_hf={:?}", d.layout.pages.len(), d.page_hf);
    for (i, s) in d.hf_sets.iter().enumerate() {
        println!(
            "set {i}: {} header={} blocks={} texts={:?}",
            s.part, s.header, s.blocks.len(), s.texts
        );
    }
    let stride = (d.layout.page_height + d.layout.gap) as f32;
    let page_h = d.layout.page_height as f32;
    for l in d.layout.lines.iter().filter(|l| l.para >= FOOTER_BASE) {
        let pg = (l.y / stride) as u32;
        if pg <= 2 {
            println!(
                "footer line: para={} page={pg} y_local={:.1} h={:.1} stops={}",
                l.para - FOOTER_BASE, l.y - pg as f32 * stride, l.height, l.stops.len()
            );
        }
    }
    // The click: page 2 (index 1), centre x, 40px above the sheet bottom (the footer band).
    d.set_header_footer_page(1);
    let (cx, cy) = (d.layout.page_width as f32 / 2.0, stride + page_h - 40.0);
    let hit = d.hit_test(cx, cy);
    println!("hit_test({cx},{cy}) -> {hit:?}");
    let para = hit[0];
    d.set_active_story(para);
    println!("active_region footer? {}", d.active_region == Region::Footer);
    println!("caret_rect -> {:?}", d.caret_rect(para, hit[1]));
    println!("insert_text({para}, 0) -> {:?}", d.insert_text(para, 0, "Z"));
    if let Some(f) = d.active_hf_doc() {
        let texts: Vec<String> = f
            .paragraphs()
            .unwrap_or_default()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect();
        println!("active footer part texts after insert: {texts:?}");
    }
}

fn tracked(text: &str, kind: TrackKind) -> Run {
    Run {
        track: Some(Track { kind, author: "Alice".into(), date: String::new(), id: 1 }),
        ..Run::plain(text)
    }
}

#[test]
fn text_frame_parse_and_placement() {
    // 816x1056 page, 96px margins -> content width 624. twips@scale 1: px = tw/20 * 96/72.
    let (pw, ph, m) = (816.0, 1056.0, 96.0);
    // w=2880tw = 192px; xAlign=right within the margin -> right edge at the right margin.
    let g = parse_frame(
        r#"w:w="2880" w:hAnchor="margin" w:xAlign="right" w:vAnchor="page" w:y="720""#,
        1.0,
    );
    assert!((g.w.unwrap() - 192.0).abs() < 0.5, "w px = {:?}", g.w);
    let (x, y) = place_frame(&g, 192.0, 100.0, pw, ph, m, m, m, m, m);
    assert!((x - (pw - m - 192.0)).abs() < 0.5, "xAlign=right margin: x={x}");
    assert!((y - 48.0).abs() < 0.5, "vAnchor=page y=720tw -> 48px: y={y}"); // 720/20*96/72

    // xAlign=center within the text column -> centred in [ml, ml+content_w].
    let gc = parse_frame(r#"w:w="2880" w:hAnchor="margin" w:xAlign="center""#, 1.0);
    let (xc, _) = place_frame(&gc, 192.0, 100.0, pw, ph, m, m, m, m, m);
    assert!((xc - (m + (624.0 - 192.0) * 0.5)).abs() < 0.5, "xAlign=center: x={xc}");

    // Absolute x offset from the text column left (no xAlign).
    let ga = parse_frame(r#"w:hAnchor="margin" w:x="1440" w:vAnchor="margin" w:y="0""#, 1.0);
    let (xa, ya) = place_frame(&ga, 100.0, 100.0, pw, ph, m, m, m, m, m);
    assert!((xa - (m + 96.0)).abs() < 0.5, "absolute x: {xa}"); // 1440tw=96px from ml
    assert!((ya - m).abs() < 0.5, "vAnchor=margin y=0 -> top margin: {ya}");

    // vAnchor=text: the frame floats from its anchor paragraph's flow y (passed in), plus the
    // y offset - NOT the margin band. Anchor flow y = 300px, y=720tw=48px -> 348px.
    let gt = parse_frame(r#"w:w="2880" w:hAnchor="margin" w:vAnchor="text" w:y="720""#, 1.0);
    let (_, yt) = place_frame(&gt, 192.0, 100.0, pw, ph, m, m, m, m, 300.0);
    assert!((yt - 348.0).abs() < 0.5, "vAnchor=text floats from anchor flow y: {yt}");
}

/// A frame's box height by `w:hRule` + `w:h`: `exact` clips to `h`; `atLeast` (and a bare `h`
/// with no rule) is a floor that grows for overflow; a rule-less, height-less frame fits content.
#[test]
fn frame_height_rules() {
    assert_eq!(resolve_frame_height("exact", Some(40.0), 100.0), 40.0, "exact clips to h");
    assert_eq!(resolve_frame_height("atLeast", Some(40.0), 100.0), 100.0, "atLeast grows for content");
    assert_eq!(resolve_frame_height("atLeast", Some(200.0), 100.0), 200.0, "atLeast holds its floor");
    // A height with no rule reads as atLeast (the tdf103544 box was rendering short as `auto`).
    assert_eq!(resolve_frame_height("", Some(200.0), 100.0), 200.0, "bare h is a floor, not auto");
    assert_eq!(resolve_frame_height("", None, 100.0), 100.0, "no rule + no h fits content");
}

/// `w:pBdr` -> the painter's `BlockBorders`: eighths-of-a-point weight + points spacing resolve to
/// px at the render scale; `auto` colour is black; a hex colour is kept; absent edges stay `None`.
#[test]
fn pbdr_parses_to_block_borders() {
    let pt = 96.0 / 72.0; // points->px at scale 1
    let b = parse_pbdr(Some("t=single,6,1,auto|l=single,12,4,FF0000"), pt).expect("borders");
    let top = b.top.expect("top edge");
    assert!((top.width_px - 1.0).abs() < 0.1, "sz=6 (0.75pt) -> ~1px: {}", top.width_px);
    assert_eq!(top.rgb, [0, 0, 0], "auto -> black");
    let left = b.left.expect("left edge");
    assert!((left.width_px - 2.0).abs() < 0.1, "sz=12 (1.5pt) -> 2px: {}", left.width_px);
    assert!((left.space_px - 4.0 * pt).abs() < 0.1, "space=4pt: {}", left.space_px);
    assert_eq!(left.rgb, [0xFF, 0, 0], "hex colour kept");
    assert!(b.bottom.is_none() && b.right.is_none(), "only t/l set");
    assert!(parse_pbdr(None, pt).is_none(), "no string -> no borders");
}

fn para(runs: Vec<Run>) -> Paragraph {
    Paragraph { style: None, props: ParaProps::default(), runs, prop_change: None, mark_change: None }
}

#[test]
fn track_display_parses_word_aliases() {
    assert_eq!(TrackDisplay::parse("all"), Some(TrackDisplay::AllMarkup));
    assert_eq!(TrackDisplay::parse("simple"), Some(TrackDisplay::SimpleMarkup));
    assert_eq!(TrackDisplay::parse("none"), Some(TrackDisplay::NoMarkup));
    assert_eq!(TrackDisplay::parse("final"), Some(TrackDisplay::NoMarkup));
    assert_eq!(TrackDisplay::parse("original"), Some(TrackDisplay::Original));
    assert_eq!(TrackDisplay::parse("bogus"), None);
}

#[test]
fn track_colour_is_deterministic_and_distinguishes_kind_and_author() {
    // Deterministic per (author, kind); insertions (cool) differ from deletions (warm) for the
    // same author so a single-author redline reads as "blue vs red"; different authors differ.
    assert_eq!(track_colour("Alice", TrackKind::Ins), track_colour("Alice", TrackKind::Ins));
    assert_ne!(track_colour("Alice", TrackKind::Ins), track_colour("Alice", TrackKind::Del));
    assert_ne!(track_colour("Alice", TrackKind::Ins), track_colour("Reviewer Two", TrackKind::Ins));
    // Moves get a distinct (green) hue from the same author's insertions + deletions, and both
    // halves share it so source + destination read as a matched pair.
    assert_ne!(track_colour("Alice", TrackKind::MoveFrom), track_colour("Alice", TrackKind::Ins));
    assert_ne!(track_colour("Alice", TrackKind::MoveFrom), track_colour("Alice", TrackKind::Del));
    assert_eq!(track_colour("Alice", TrackKind::MoveFrom), track_colour("Alice", TrackKind::MoveTo));
}

#[test]
fn display_mode_filters_runs_and_styles_markup() {
    let styles = scriptor_crdt::StyleTable::default();
    let none = std::collections::HashSet::<String>::new();
    let no_exp = std::collections::HashSet::<usize>::new();
    let paras = [para(vec![
        Run::plain("keep "),
        tracked("added", TrackKind::Ins),
        tracked("cut", TrackKind::Del),
    ])];

    // All-Markup: every run kept; insertion underlined, deletion struck.
    let (all, _, _) = resolve_blocks(&paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
    assert_eq!(all[0].spans.len(), 3);
    assert!(all[0].spans[1].underline && !all[0].spans[1].strike);
    assert!(all[0].spans[2].strike && !all[0].spans[2].underline);
    assert_eq!(all[0].spans[1].color, track_colour("Alice", TrackKind::Ins));
    assert_eq!(all[0].spans[2].color, track_colour("Alice", TrackKind::Del));

    // No-Markup / Simple: the deletion is gone (shorter flow), no struck text remains.
    for mode in [TrackDisplay::NoMarkup, TrackDisplay::SimpleMarkup] {
        let (r, _, _) = resolve_blocks(&paras, &styles, 1.0, mode, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
        assert_eq!(r[0].spans.len(), 2, "{mode:?} should hide the deletion");
        assert!(r[0].spans.iter().all(|s| !s.strike && !s.underline));
    }

    // Original: the insertion is gone; the (now-normal) deletion text survives.
    let (orig, _, _) = resolve_blocks(&paras, &styles, 1.0, TrackDisplay::Original, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
    assert_eq!(orig[0].spans.len(), 2);
    assert!(orig[0].spans.iter().all(|s| !s.strike && !s.underline));
    assert!(orig[0].spans.iter().any(|s| s.text == "cut"));
}

/// An inline picture run (its text is U+FFFC) never reaches the laid-out text - no tofu - and is
/// reserved as an inline picture sized from its EMU placement; a floating picture is stripped too
/// but not reserved inline (it's positioned + wrapped separately). (images-editing P2b.)
#[test]
fn inline_image_run_strips_placeholder_and_reserves_a_picture() {
    let styles = scriptor_crdt::StyleTable::default();
    let none = std::collections::HashSet::<String>::new();
    let no_exp = std::collections::HashSet::<usize>::new();
    let img_run = scriptor_crdt::Run { image: Some(7), ..Run::plain("\u{FFFC}") };
    let paras = [para(vec![Run::plain("Fig "), img_run])];
    let mut imgs = std::collections::HashMap::new();
    imgs.insert(
        7u64,
        scriptor_crdt::ImagePlacement {
            media: "word/media/image1.png".into(),
            w_emu: 914_400, // 1 inch -> 96px at scale 1
            h_emu: 685_800, // 0.75 inch -> 72px
            ..Default::default()
        },
    );
    let (blocks, _, _) =
        resolve_blocks(&paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, false, &[], &imgs, &Default::default(), &[]);
    let b = &blocks[0];
    assert!(b.spans.iter().all(|s| !s.text.contains('\u{FFFC}')), "placeholder stripped (no tofu)");
    assert!(b.spans.iter().any(|s| s.text == "Fig "), "the surrounding text stays");
    assert_eq!(b.inline_images.len(), 1, "one inline picture reserved");
    let im = &b.inline_images[0];
    assert_eq!(im.key, "word/media/image1.png");
    assert_eq!(im.byte, 4, "anchored after the 4-byte prefix");
    assert!((im.w - 96.0).abs() < 0.5 && (im.h - 72.0).abs() < 0.5, "EMU->px ({}, {})", im.w, im.h);

    // Floating: stripped from the line, but not an inline box.
    imgs.get_mut(&7).unwrap().floating = true;
    let (fb, _, _) =
        resolve_blocks(&paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, false, &[], &imgs, &Default::default(), &[]);
    assert!(fb[0].inline_images.is_empty(), "a floating picture is not an inline box");
    assert!(fb[0].spans.iter().all(|s| !s.text.contains('\u{FFFC}')), "floating picture: still no tofu");
}

/// A floating picture's top-left resolves from its align / offset + relativeFrom origin (the math
/// shared by the read-only header/footer projection + the editable body floats). (images-editing P2c.)
#[test]
fn floating_placement_resolves_align_and_offsets() {
    let g = FloatGeom { ml: 96.0, mr: 96.0, mt: 96.0, page_w: 816.0, scale: 1.0 };
    // Page-relative, right-aligned: hugs the right page edge.
    let (x, _y) = place_float(&g, 100.0, true, "page", "right", 0, "page", 0, 0.0);
    assert!((x - (816.0 - 100.0)).abs() < 0.01, "right page edge, got {x}");
    // Column/margin offsets: 1 inch (914400 EMU) -> 96px past the left/top margin.
    let (x, y) = place_float(&g, 100.0, true, "column", "", 914_400, "margin", 914_400, 0.0);
    assert!((x - (96.0 + 96.0)).abs() < 0.01, "ml + 1in, got {x}");
    assert!((y - (96.0 + 96.0)).abs() < 0.01, "mt + 1in, got {y}");
    // Paragraph-relative vertical: the offset is added to the anchor paragraph's top.
    let (_x, y) = place_float(&g, 100.0, true, "column", "", 0, "paragraph", 0, 300.0);
    assert!((y - 300.0).abs() < 0.01, "anchor top + 0, got {y}");
}

/// Paragraph-level square wrap: a paragraph whose band overlaps a float is indented away from the
/// float's side; non-overlapping, full-width-straddling, and other-page floats don't wrap it.
#[test]
fn square_wrap_narrows_paragraphs_beside_a_float() {
    let (ml, content_w, gutter) = (96.0_f32, 624.0_f32, 9.0_f32); // center at 408
    let rect = |x0, x1, top, bot| FloatRect { page: 0, x0, x1, top, bot, hspace: 0.0, vspace: 0.0 };
    let left = rect(96.0, 196.0, 100.0, 300.0);
    let right = rect(520.0, 620.0, 100.0, 300.0);
    let full = rect(200.0, 600.0, 100.0, 300.0);

    let (l, r) = square_wrap_indents(0, 120.0, 160.0, 0.0, 0.0, ml, content_w, gutter, &[left]);
    assert!(l > 100.0 && r == 0.0, "left float -> left indent ({l}, {r})");
    let (l, r) = square_wrap_indents(0, 120.0, 160.0, 0.0, 0.0, ml, content_w, gutter, &[right]);
    assert!(r > 100.0 && l == 0.0, "right float -> right indent ({l}, {r})");
    let (l, r) = square_wrap_indents(0, 500.0, 540.0, 0.0, 0.0, ml, content_w, gutter, &[left]);
    assert_eq!((l, r), (0.0, 0.0), "no vertical overlap -> no wrap");
    let (l, r) = square_wrap_indents(0, 120.0, 160.0, 0.0, 0.0, ml, content_w, gutter, &[full]);
    assert_eq!((l, r), (0.0, 0.0), "full-width float -> no side wrap");
    let (l, r) = square_wrap_indents(1, 120.0, 160.0, 0.0, 0.0, ml, content_w, gutter, &[left]);
    assert_eq!((l, r), (0.0, 0.0), "float on another page is ignored");

    // A right-aligned frame with a WIDE hSpace: the true box sits right of centre, so it still
    // wraps as a right float (the old code, which baked hSpace into x0, mis-read it as straddling).
    let wide = FloatRect { page: 0, x0: 540.0, x1: 620.0, top: 100.0, bot: 300.0, hspace: 200.0, vspace: 0.0 };
    let (l, r) = square_wrap_indents(0, 120.0, 160.0, 0.0, 0.0, ml, content_w, gutter, &[wide]);
    assert!(r > 0.0 && l == 0.0, "wide-hSpace right frame still wraps right ({l}, {r})");
    // The clearance honoured is the frame's hSpace: right edge pulled to x0 - hspace = 340.
    assert!((r - ((ml + content_w) - (540.0 - 200.0))).abs() < 0.5, "hSpace clearance honoured: {r}");
}

/// Image hit-test: the point picks the picture whose rect contains it, the topmost (last-pushed)
/// wins an overlap, and a miss returns nothing. (images-editing P2d.)
#[test]
fn image_hit_test_picks_the_topmost_rect() {
    let rects = vec![
        ImageHit { id: 1, x: 50.0, y: 50.0, w: 100.0, h: 100.0, page: 0 },
        ImageHit { id: 2, x: 120.0, y: 120.0, w: 100.0, h: 100.0, page: 0 }, // overlaps id 1, on top
    ];
    assert_eq!(hit_image(&rects, 60.0, 60.0), Some(1), "inside id 1 only");
    assert_eq!(hit_image(&rects, 200.0, 200.0), Some(2), "inside id 2 only");
    assert_eq!(hit_image(&rects, 130.0, 130.0), Some(2), "overlap -> topmost wins");
    assert_eq!(hit_image(&rects, 10.0, 10.0), None, "a miss selects nothing");
}

#[test]
fn emu_px_conversions_round_trip() {
    // 1 inch = 914400 EMU = 96 px at zoom 1.0; doubles at zoom 2.0.
    assert_eq!(emu_to_px(914_400.0, 1.0), 96.0);
    assert_eq!(emu_to_px(914_400.0, 2.0), 192.0);
    assert_eq!(px_to_emu(96.0, 1.0), 914_400.0);
    // px -> EMU -> px is the identity at any positive zoom.
    for &(px, scale) in &[(120.0, 1.0), (37.5, 1.5), (300.0, 0.75)] {
        assert!((emu_to_px(px_to_emu(px, scale), scale) - px).abs() < 1e-6, "round-trip {px}@{scale}");
    }
    // A non-positive scale has no sensible inverse -> 0 (guards a divide-by-zero).
    assert_eq!(px_to_emu(100.0, 0.0), 0.0);
}

#[test]
fn change_bar_flag_tracks_changed_paragraphs_per_mode() {
    let styles = scriptor_crdt::StyleTable::default();
    let none = std::collections::HashSet::<String>::new();
    let no_exp = std::collections::HashSet::<usize>::new();
    let paras = [
        para(vec![Run::plain("clean")]),
        para(vec![Run::plain("keep "), tracked("added", TrackKind::Ins)]),
    ];

    // Markup views (All / Simple): the changed paragraph lights its change-bar, the clean one not.
    for mode in [TrackDisplay::AllMarkup, TrackDisplay::SimpleMarkup] {
        let (b, _, _) = resolve_blocks(&paras, &styles, 1.0, mode, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
        assert!(!b[0].has_change, "{mode:?}: clean paragraph has no bar");
        assert!(b[1].has_change, "{mode:?}: changed paragraph gets a bar");
    }

    // Final / Original: no change-bars (Word hides them outside the markup views).
    for mode in [TrackDisplay::NoMarkup, TrackDisplay::Original] {
        let (b, _, _) = resolve_blocks(&paras, &styles, 1.0, mode, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
        assert!(b.iter().all(|x| !x.has_change), "{mode:?}: no change-bars");
    }
}

/// Filtering a reviewer suppresses their markup: their insertions vanish from the layout, their
/// deletions show as plain (un-struck) text, and a paragraph changed only by them loses its bar.
#[test]
fn reviewer_filter_suppresses_a_hidden_authors_markup() {
    let styles = scriptor_crdt::StyleTable::default();
    let tracked_by = |text: &str, kind: TrackKind, author: &str| Run {
        track: Some(Track { kind, author: author.into(), date: String::new(), id: 1 }),
        ..Run::plain(text)
    };
    let paras = [para(vec![
        Run::plain("keep "),
        tracked_by("added", TrackKind::Ins, "Bob"),
        tracked_by("cut", TrackKind::Del, "Bob"),
    ])];
    let hidden: std::collections::HashSet<String> = ["Bob".to_string()].into_iter().collect();
    let no_exp = std::collections::HashSet::<usize>::new();

    let (r, _, _) = resolve_blocks(&paras, &styles, 1.0, TrackDisplay::AllMarkup, &hidden, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
    // Bob's insertion is gone; his deletion stays but renders as plain text; no change-bar.
    assert_eq!(r[0].spans.iter().filter(|s| s.text == "added").count(), 0, "insertion hidden");
    let cut = r[0].spans.iter().find(|s| s.text == "cut").expect("deletion kept as plain text");
    assert!(!cut.strike, "a filtered reviewer's deletion isn't struck");
    assert!(!r[0].has_change, "a paragraph changed only by a filtered reviewer has no bar");

    // With Bob visible, both his changes show as markup again.
    let none = std::collections::HashSet::<String>::new();
    let shown = resolve_blocks(&paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]).0;
    assert!(shown[0].spans.iter().any(|s| s.text == "added"));
    assert!(shown[0].spans.iter().find(|s| s.text == "cut").unwrap().strike);
    assert!(shown[0].has_change);
}

/// P2: in Simple Markup a clean paragraph hides its deletion, but a paragraph the editor
/// "expanded" (clicked its change-bar) renders its inline redline - a per-paragraph All-Markup.
#[test]
fn expanding_a_paragraph_reveals_its_redline_in_simple_markup() {
    let styles = scriptor_crdt::StyleTable::default();
    let none = std::collections::HashSet::<String>::new();
    let paras = [para(vec![Run::plain("keep "), tracked("cut", TrackKind::Del)])];

    // Simple Markup, not expanded: the deletion is hidden (clean text, just the change-bar).
    let no_exp = std::collections::HashSet::<usize>::new();
    let (clean, _, _) = resolve_blocks(&paras, &styles, 1.0, TrackDisplay::SimpleMarkup, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
    assert_eq!(clean[0].spans.len(), 1, "deletion hidden in Simple Markup");
    assert!(clean[0].has_change, "but the paragraph still bars (it changed)");

    // Expand paragraph 0: its deletion now shows, struck (per-paragraph All-Markup).
    let exp: std::collections::HashSet<usize> = [0usize].into_iter().collect();
    let (shown, segs, _) = resolve_blocks(&paras, &styles, 1.0, TrackDisplay::SimpleMarkup, &none, &exp, false, &[], &Default::default(), &Default::default(), &[]);
    assert_eq!(shown[0].spans.len(), 2, "the expanded paragraph reveals the deletion");
    assert!(shown[0].spans.iter().any(|s| s.text == "cut" && s.strike), "deletion struck");
    // Nothing is hidden now, so its visible-run map covers both runs (the editing identity).
    assert_eq!(segs[0].len(), 2);
}

/// P3: the visible<->full offset bridge maps caret offsets across runs the display mode hides, so
/// editing in No-Markup / Original / Simple lands at the right model position. `body_segments` are
/// the surviving runs as full-text byte ranges; a full offset inside a hidden gap clamps to the
/// visible boundary, and a visible offset jumps the gap.
#[test]
fn visible_full_offset_bridge_crosses_hidden_runs() {
    let mut d = ScriptorDoc::new();
    // full "ABXXCD" (6 bytes) with "XX" hidden -> visible "ABCD"; visible runs are [(0,2),(4,6)].
    d.body_segments = vec![vec![(0, 2), (4, 6)]];

    // full -> visible: linear inside a visible run, clamped to the boundary inside the hidden gap.
    assert_eq!(d.full_to_visible(0, 0), 0);
    assert_eq!(d.full_to_visible(0, 2), 2);
    assert_eq!(d.full_to_visible(0, 3), 2, "inside the hidden run -> visible boundary");
    assert_eq!(d.full_to_visible(0, 4), 2);
    assert_eq!(d.full_to_visible(0, 5), 3);
    assert_eq!(d.full_to_visible(0, 6), 4);

    // visible -> full: the inverse; crossing the first run jumps the hidden gap.
    assert_eq!(d.visible_to_full(0, 0), 0);
    assert_eq!(d.visible_to_full(0, 2), 4, "after the first visible run -> past the hidden gap");
    assert_eq!(d.visible_to_full(0, 3), 5);
    assert_eq!(d.visible_to_full(0, 4), 6);

    // With nothing hidden the bridge is the identity (the common case - no perf / correctness cost).
    d.body_segments = vec![vec![(0, 5)]];
    assert_eq!(d.full_to_visible(0, 3), 3);
    assert_eq!(d.visible_to_full(0, 3), 3);
    // A paragraph with no map entry (out of range) is treated as the identity too.
    assert_eq!(d.full_to_visible(9, 4), 4);
    assert_eq!(d.visible_to_full(9, 4), 4);
}

/// P4: balloon mode pulls a tracked deletion out of the line into a margin balloon (text +
/// author), while the line keeps its change-bar; with balloons off the deletion shows inline.
#[test]
fn balloon_mode_pulls_deletions_into_the_margin() {
    let styles = scriptor_crdt::StyleTable::default();
    let none = std::collections::HashSet::<String>::new();
    let no_exp = std::collections::HashSet::<usize>::new();
    let paras = [para(vec![Run::plain("keep "), tracked("cut", TrackKind::Del)])];

    // Balloons off: the deletion is inline (struck), no balloon emitted.
    let (inline, _, off) =
        resolve_blocks(&paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, false, &[], &Default::default(), &Default::default(), &[]);
    assert!(inline[0].spans.iter().any(|s| s.text == "cut" && s.strike), "deletion shown inline");
    assert!(off[0].is_empty(), "no balloon when balloons are off");

    // Balloons on: the deletion leaves the line and becomes a balloon (its text + author).
    let (blocks, _, balloons) =
        resolve_blocks(&paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, true, &[], &Default::default(), &Default::default(), &[]);
    assert!(blocks[0].spans.iter().all(|s| s.text != "cut"), "deletion no longer inline");
    assert!(blocks[0].has_change, "the changed line still bars");
    assert_eq!(balloons[0].len(), 1, "one balloon for the deletion");
    assert!(balloons[0][0].kind == BalloonKind::Deletion);
    assert_eq!(balloons[0][0].text, "cut");
    assert_eq!(balloons[0][0].author, "Alice");
}

/// Format + comment balloons (the follow-on to deletion balloons): a tracked formatting change
/// keeps its text inline but adds a "Formatted: ..." balloon describing it; a comment anchored in
/// the paragraph adds a balloon carrying its body (once, even if it spans runs).
#[test]
fn balloon_mode_carries_formatting_and_comments() {
    let styles = scriptor_crdt::StyleTable::default();
    let none = std::collections::HashSet::<String>::new();
    let no_exp = std::collections::HashSet::<usize>::new();

    // A run that turned bold under a tracked rPrChange (old = not bold), also carrying comment #7.
    let mut bolded = Run::plain("word");
    bolded.bold = true;
    bolded.fmt_change = Some(scriptor_crdt::FormatChange {
        author: "Alice".into(),
        date: String::new(),
        id: 1,
        old: scriptor_crdt::RunProps::default(),
    });
    bolded.comments = vec![7];
    let paras = [para(vec![Run::plain("a "), bolded])];
    let comments = [scriptor_crdt::Comment {
        id: 7,
        author: "Ann".into(),
        initials: "A".into(),
        date: String::new(),
        parent: None,
        resolved: false,
        text: "fix this".into(),
    }];

    let (blocks, _, balloons) = resolve_blocks(
        &paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, true, &comments,
        &Default::default(), &Default::default(), &[],
    );
    // The formatted text stays inline (formatting changes aren't pulled out, only described).
    assert!(blocks[0].spans.iter().any(|s| s.text == "word"), "formatted text stays inline");
    // One Format balloon (Bold) + one Comment balloon (the body), anchored on this paragraph.
    let fmt = balloons[0].iter().find(|b| b.kind == BalloonKind::Format).expect("a format balloon");
    assert!(fmt.text.contains("Bold"), "describes the formatting: {}", fmt.text);
    let cmt = balloons[0].iter().find(|b| b.kind == BalloonKind::Comment).expect("a comment balloon");
    assert_eq!(cmt.text, "fix this");
    assert_eq!(cmt.author, "Ann");

    // A resolved comment doesn't balloon.
    let resolved = [scriptor_crdt::Comment { resolved: true, ..comments[0].clone() }];
    let (_, _, b2) = resolve_blocks(
        &paras, &styles, 1.0, TrackDisplay::AllMarkup, &none, &no_exp, true, &resolved,
        &Default::default(), &Default::default(), &[],
    );
    assert!(b2[0].iter().all(|b| b.kind != BalloonKind::Comment), "resolved comment is not ballooned");
}

/// P5 wart 2: undo/redo route to the story the caret is in. A header edit undoes in the header,
/// not the body, and is left alone while the caret is in the body.
#[test]
fn undo_routes_to_the_active_story() {
    let mut d = ScriptorDoc::new();
    d.set_header_text("Hi");
    let hp = HEADER_BASE as u32;
    d.insert_text(hp, 0, "X").unwrap(); // header para 0: "Hi" -> "XHi"
    assert_eq!(d.doc.header_text(), "XHi");

    // Caret in the body: undo finds nothing there and leaves the header edit alone.
    d.set_active_story(0);
    assert!(!d.undo().unwrap(), "the body has nothing to undo");
    assert_eq!(d.doc.header_text(), "XHi", "a body undo doesn't touch the header");

    // Caret in the header: undo reverts the header edit; redo re-applies it.
    d.set_active_story(hp);
    assert!(d.undo().unwrap());
    assert_eq!(d.doc.header_text(), "Hi", "header edit undone in the header story");
    assert!(d.redo().unwrap());
    assert_eq!(d.doc.header_text(), "XHi", "and redone in the header story");
}

/// The full authoring loop through the wasm surface: turn tracking on, type (-> tracked
/// insertion), delete your own just-typed text (removed outright), then accept the rest.
#[test]
fn tracked_authoring_through_wasm_surface() {
    let mut d = ScriptorDoc::new(); // seeds one empty paragraph
    d.set_author("u1", "Alice");
    d.set_track_changes(true);
    assert!(d.track_changes_on());
    d.set_now("2026-06-20T00:00:00Z");

    d.insert_text(0, 0, "Hello").unwrap();
    assert_eq!(d.paragraph_text(0).unwrap(), "Hello");
    let hit = d.track_at(0, 0).expect("the typed text is a tracked insertion");
    assert_eq!(hit.kind, "ins");
    assert_eq!(hit.author, "Alice");

    // Deleting your own un-accepted insertion removes it outright (no stacked w:del).
    d.delete_range(0, 0, 1).unwrap(); // delete "H"
    assert_eq!(d.paragraph_text(0).unwrap(), "ello");
    assert_eq!(d.track_at(0, 0).expect("still tracked").kind, "ins");

    // Accepting the change leaves plain text with no tracked change at the caret.
    assert!(d.accept_change(0, 0).unwrap());
    assert_eq!(d.paragraph_text(0).unwrap(), "ello");
    assert!(d.track_at(0, 0).is_none());

    // With tracking off, a delete is direct again.
    d.set_track_changes(false);
    d.delete_range(0, 0, 1).unwrap();
    assert_eq!(d.paragraph_text(0).unwrap(), "llo");
}

/// A picture inserted under Track Changes through the wasm surface is a tracked insertion; the
/// display modes hide it like a tracked text run (shown in All-Markup, gone in Original), and
/// rejecting it removes the run + (via the relayout gc) its placement.
#[test]
fn tracked_image_through_wasm_surface() {
    let mut d = ScriptorDoc::new();
    d.insert_text(0, 0, "Cap").unwrap();
    d.set_author("u1", "Alice");
    d.set_track_changes(true);
    d.set_now("2026-06-25T00:00:00Z");

    let png = vec![0x89u8, b'P', b'N', b'G', 1, 2];
    let id = d.insert_image(0, 3, &png, "image/png", 914_400.0, 685_800.0).unwrap();
    assert_eq!(d.track_at(0, 3).expect("tracked insertion at the picture").kind, "ins");
    let rev = d.track_at(0, 3).unwrap().id;

    // Shown in All-Markup; hidden in Original (the pre-change view hides insertions).
    d.set_track_display("all");
    d.relayout(1.0).unwrap();
    assert!(d.image_rect(id).is_some(), "picture shows in All-Markup");
    d.set_track_display("original");
    d.relayout(1.0).unwrap();
    assert!(d.image_rect(id).is_none(), "picture hidden in Original");

    // Reject removes the run; the next relayout gc's the orphaned placement; the text remains.
    d.set_track_display("all");
    assert!(d.reject_revision(0, rev).unwrap());
    d.relayout(1.0).unwrap();
    assert!(d.image_rect(id).is_none(), "rejected picture is gone");
    assert_eq!(d.paragraph_text(0).unwrap(), "Cap");
}

/// The header is a first-class editable story: typing into a header paragraph (namespaced at
/// `HEADER_BASE`) records a tracked insertion in the header - not the body - and accept/reject
/// resolves it there. Mirrors the body authoring loop, proving the per-region routing.
#[test]
fn header_is_editable_with_tracked_changes() {
    let mut d = ScriptorDoc::new(); // body seeded with one empty paragraph
    d.set_header_text("Title"); // create the header story
    d.set_author("u1", "Alice");
    d.set_track_changes(true);
    d.set_now("2026-06-21T00:00:00Z");
    d.relayout(1.0).unwrap(); // populate the header text cache

    let hp = HEADER_BASE as u32;
    assert_eq!(d.paragraph_text(HEADER_BASE).unwrap(), "Title");
    assert_eq!(d.paragraph_range(hp), vec![HEADER_BASE as u32, 1]);

    // Type at the end of the header -> a tracked insertion in the header story.
    d.insert_text(hp, 5, " X").unwrap();
    d.relayout(1.0).unwrap();
    assert_eq!(d.paragraph_text(HEADER_BASE).unwrap(), "Title X");
    let hit = d.track_at(hp, 6).expect("the header edit is a tracked insertion");
    assert_eq!(hit.kind, "ins");
    assert_eq!(hit.author, "Alice");
    // The body story is untouched.
    assert_eq!(d.paragraph_text(0).unwrap(), "");

    // Accept it in the header -> plain text, no remaining tracked change there.
    assert!(d.accept_change(hp, 6).unwrap());
    d.relayout(1.0).unwrap();
    assert_eq!(d.paragraph_text(HEADER_BASE).unwrap(), "Title X");
    assert!(d.track_at(hp, 6).is_none());
}

/// Insert > Header creates an empty header story on a document that has none, returns its first
/// paragraph's namespaced index (so the shell can drop the caret in), and is idempotent: a second
/// call returns the same index and leaves an existing header's content intact. Footer is analogous.
#[test]
fn ensure_header_creates_then_preserves() {
    let mut d = ScriptorDoc::new(); // body only - no header yet
    assert!(d.header_text().is_empty(), "no header to start");

    // First call creates an empty, editable header and returns its first paragraph index.
    let hp = d.ensure_header();
    assert_eq!(hp, HEADER_BASE as u32);
    d.relayout(1.0).unwrap();
    assert_eq!(d.paragraph_text(HEADER_BASE).unwrap(), "", "an empty, clickable header paragraph");

    // Type into it via the namespaced index - the edit lands in the header story.
    d.insert_text(hp, 0, "Draft").unwrap();
    d.relayout(1.0).unwrap();
    assert_eq!(d.paragraph_text(HEADER_BASE).unwrap(), "Draft");

    // Idempotent: a second call returns the same index and leaves the content untouched.
    assert_eq!(d.ensure_header(), HEADER_BASE as u32);
    assert_eq!(d.paragraph_text(HEADER_BASE).unwrap(), "Draft", "existing header preserved");

    // Footer is independent + analogous.
    assert_eq!(d.ensure_footer(), FOOTER_BASE as u32);
    d.relayout(1.0).unwrap();
    assert_eq!(d.paragraph_text(FOOTER_BASE).unwrap(), "");
}

/// Next / Previous navigation spans every story: a change that lives only in the header is found
/// from a body caret, returned at its namespaced index.
#[test]
fn navigation_finds_changes_across_stories() {
    let mut d = ScriptorDoc::new();
    d.set_header_text("Hi");
    d.set_author("u1", "Alice");
    d.set_track_changes(true);
    d.set_now("2026-06-21T00:00:00Z");
    d.relayout(1.0).unwrap();

    let hp = HEADER_BASE as u32;
    d.insert_text(hp, 2, "!").unwrap(); // tracked insertion in the header only

    // No changes anywhere -> empty; here the body caret still finds the header change.
    let r = d.next_change(0, 0);
    assert_eq!(r.len(), 2, "Next found the header change from a body caret");
    assert_eq!(r[0], hp, "returned at the header's namespaced index");
    assert_eq!(r[1], 2, "at the inserted run's start");

    // Previous from the body wraps to it too.
    assert_eq!(d.prev_change(0, 0), vec![hp, 2]);
}

/// The reviewing pane's data sources: `listChanges` enumerates every tracked change (kind /
/// author / text / location); `listComments` lists comments. The UI merges the two into one
/// document-ordered list.
#[test]
fn review_enumeration_lists_changes_and_comments() {
    let mut d = ScriptorDoc::new();
    d.set_author("u1", "Alice");
    d.set_now("2026-06-21T00:00:00Z");
    d.insert_text(0, 0, "The cat sat").unwrap(); // direct (tracking off)
    let cid = d.add_comment(0, 4, 0, 7, "which cat?").unwrap(); // comment on "cat"
    assert!(cid >= 0, "a comment was created");
    d.set_track_changes(true);
    d.insert_text(0, 11, "!").unwrap(); // tracked insertion at the end

    let changes = d.list_changes();
    assert!(changes.contains("\"kind\":\"ins\""), "the tracked insertion is listed: {changes}");
    assert!(changes.contains("\"author\":\"Alice\""), "with its author: {changes}");
    assert!(changes.contains("\"text\":\"!\""), "with its text: {changes}");

    let comments = d.list_comments();
    assert!(comments.contains("which cat?"), "the comment body is listed: {comments}");
    assert!(comments.contains("\"resolved\":false"), "with its thread state: {comments}");
}

/// A move authored through the wasm surface: marks a source (`movefrom`) + destination (`moveto`)
/// pair sharing one id, enumerates both halves, and accept drops the source while keeping the
/// destination - one clean copy remains.
#[test]
fn tracked_move_through_wasm_surface() {
    let mut d = ScriptorDoc::new();
    d.set_author("u1", "Alice");
    d.set_now("2026-06-21T00:00:00Z");
    d.insert_text(0, 0, "The quick fox.").unwrap(); // direct (tracking off)
    d.set_track_changes(true);

    // Move "quick " (chars 4..10) to the front of the paragraph.
    let id = d.move_range(0, 4, 10, 0, 0).unwrap();
    assert!(id >= 0, "the move was recorded");
    assert_eq!(d.paragraph_text(0).unwrap(), "quick The quick fox.", "source retained until resolved");
    assert_eq!(d.track_at(0, 0).expect("dest half").kind, "moveto");
    assert_eq!(d.track_at(0, 10).expect("source half").kind, "movefrom");

    // Both halves enumerate (one shared id), so the reviewing pane shows the move.
    let changes = d.list_changes();
    assert!(changes.contains("\"kind\":\"movefrom\""), "source listed: {changes}");
    assert!(changes.contains("\"kind\":\"moveto\""), "dest listed: {changes}");

    // Accept (from either half) drops the source, keeps the destination.
    assert!(d.accept_change(0, 0).unwrap());
    assert_eq!(d.paragraph_text(0).unwrap(), "quick The fox.");
    assert!(d.track_at(0, 0).is_none() && d.track_at(0, 10).is_none(), "no move marks remain");
}

/// A tracked table delete through the wasm surface: with tracking on, deleting a row *marks* it
/// (`rowdel`, retained) rather than removing it; it enumerates for the reviewing pane; and
/// accepting it by id (the pane's path) removes the row.
/// Keystroke-cost probe (manual; native only): times the full-document CRDT paragraph read and
/// the steady-state relayout on a real `.docx` - the per-keystroke cost profile that drives the
/// incremental-relayout work. Run explicitly:
/// `SCRIPTOR_BENCH_DOCX=<path> cargo test --release -p scriptor-wasm bench_relayout -- --ignored --nocapture`
#[test]
#[ignore = "manual perf probe - set SCRIPTOR_BENCH_DOCX"]
fn bench_relayout_cost() {
    let Some(path) = std::env::var_os("SCRIPTOR_BENCH_DOCX") else {
        println!("SCRIPTOR_BENCH_DOCX not set - nothing to bench");
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let n: u32 = 20;
    let t0 = std::time::Instant::now();
    let mut d = ScriptorDoc::open_docx(&bytes).unwrap();
    println!("open_docx: {:?}", t0.elapsed());
    let t = std::time::Instant::now();
    for _ in 0..n {
        let _ = d.doc.paragraphs().unwrap();
    }
    println!("paragraphs() read: {:?}/call", t.elapsed() / n);
    d.relayout(1.0).unwrap(); // warm the font system / caches once
    let t = std::time::Instant::now();
    for _ in 0..n {
        d.relayout(1.0).unwrap();
    }
    println!("relayout (steady state): {:?}/call", t.elapsed() / n);
}

#[test]
fn cell_anchored_float_places_at_its_cell() {
    // A floating picture anchored to a TABLE-CELL paragraph resolves its page position through
    // the cell's placement - cell paragraphs are not in `placements`, and resolving the anchor
    // against that list landed the NOBA price-table checkboxes on unrelated paragraphs pages
    // away (paint) or dropped them from the hit list entirely (selection).
    const XML: &[u8] = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>Row</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>After</w:t></w:r></w:p>
</w:body></w:document>"#;
    let src = scriptor_crdt::CollabDoc::from_document_xml(XML).unwrap();
    let bytes = src.to_docx_bytes().unwrap();
    let mut d = ScriptorDoc::open_docx(&bytes).unwrap();
    // Flat flow: Intro=0, the cell's "Row"=1, After=2. Float a small picture anchored in the cell.
    let id = d
        .insert_image(1, 0, &[0x89, b'P', b'N', b'G', 1, 2], "image/png", 91_440.0, 91_440.0)
        .unwrap();
    assert!(d.set_image_floating(id, true, "none", true).unwrap());
    d.relayout(1.0).unwrap();
    let r = d.image_rect(id).expect("the cell-anchored float is placed (and hit-testable)");
    let page_h = d.layout.page_height as f32;
    assert!(
        r[1] > 0.0 && r[1] < page_h,
        "anchored on page 0 at the table's row, not teleported (y={})",
        r[1]
    );
}

/// A passthrough object (a chart `<w:drawing>` with no `<a:blip>`, so it yields no modeled picture)
/// renders as a labelled placeholder box through the full wasm layout path (`resolve_blocks` ->
/// `layout_doc`), instead of the blank gap it used to leave. Exercises the whole chain: import
/// capture -> `.docx` bytes -> re-import -> passthrough_xml -> placeholder. See `docs/passthrough.md`.
#[test]
fn passthrough_object_renders_a_placeholder_box() {
    const XML: &[u8] = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><w:body>
<w:p><w:r><w:t>Before</w:t></w:r></w:p>
<w:p><w:r><w:drawing><wp:inline><wp:extent cx="5400000" cy="3000000"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId7"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:p><w:r><w:t>After</w:t></w:r></w:p>
</w:body></w:document>"#;
    let src = scriptor_crdt::CollabDoc::from_document_xml(XML).unwrap();
    let bytes = src.to_docx_bytes().unwrap();
    let mut d = ScriptorDoc::open_docx(&bytes).unwrap();
    d.relayout(1.0).unwrap();
    assert_eq!(d.layout.placeholders.len(), 1, "the unmodeled chart is a placeholder box");
    assert_eq!(d.layout.placeholders[0].label, "Chart", "labelled by kind");
    // It paints without panicking, producing a full-page buffer.
    let px = d.paint_page(0);
    assert_eq!(px.len(), (d.layout.page_width as usize) * (d.layout.page_height as usize) * 4);
}

#[test]
fn tracked_table_structure_through_wasm_surface() {
    const XML: &[u8] = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
</w:body></w:document>"#;
    let src = scriptor_crdt::CollabDoc::from_document_xml(XML).unwrap();
    let bytes = src.to_docx_bytes().unwrap();
    let mut d = ScriptorDoc::open_docx(&bytes).unwrap();
    d.set_author("u1", "Alice");
    d.set_now("2026-06-21T00:00:00Z");
    d.set_track_changes(true);

    // Flat cell paragraphs A1(0) B1(1) A2(2) B2(3); tracked-delete the second row (caret in A2).
    assert!(d.delete_table_row(2).unwrap() >= 0, "in a table");
    assert_eq!(d.paragraph_text(2).unwrap(), "A2", "row retained (marked, not removed)");
    let changes = d.list_changes();
    assert!(changes.contains("\"kind\":\"rowdel\""), "row deletion listed: {changes}");

    // Accept by id (the reviewing-pane path) removes the row + its cells.
    let id = d.doc.table_changes()[0].id as u32;
    assert!(d.accept_revision(2, id).unwrap());
    assert!(d.doc.table_changes().is_empty(), "resolved");
    assert_eq!(d.doc.paragraphs().unwrap().len(), 2, "only the first row remains");
}

/// A tracked numbering change through the wasm surface: with tracking on, setting a paragraph's
/// list records a `w:pPrChange` (it lists as a `fmt` change for the reviewing pane), and rejecting
/// at the caret restores the prior (no-list) state.
#[test]
fn tracked_numbering_through_wasm_surface() {
    let mut d = ScriptorDoc::new();
    d.set_author("u1", "Alice");
    d.set_now("2026-06-22T00:00:00Z");
    d.insert_text(0, 0, "Item").unwrap(); // direct (tracking off)
    d.set_track_changes(true);

    // Put the paragraph into list #2 -> a tracked numbering change (a pPrChange).
    d.set_numbering(0, 2, 0).unwrap();
    assert_eq!(d.paragraph_num_id(0), 2, "now in list #2");
    let changes = d.list_changes();
    assert!(changes.contains("\"kind\":\"fmt\""), "numbering lists as a format change: {changes}");

    // Reject at the caret restores the prior (no-list) state.
    assert!(d.reject_change(0, 0).unwrap());
    assert_eq!(d.paragraph_num_id(0), -1, "reject removed the list membership");
}

/// Lock Tracking forces tracking on (can't be turned off until unlocked), and `reviewers()` lists
/// each change author with a colour + hidden flag that `setReviewerHidden` toggles.
#[test]
fn lock_tracking_and_reviewer_legend() {
    let mut d = ScriptorDoc::new();
    d.set_author("u1", "Alice");
    d.set_now("2026-06-22T00:00:00Z");
    d.set_track_changes(true);
    d.insert_text(0, 0, "Hi").unwrap(); // a tracked insertion by Alice

    let rev = d.reviewers();
    assert!(rev.contains("\"name\":\"Alice\""), "Alice is listed: {rev}");
    assert!(rev.contains("\"hidden\":false"));

    // Locked: turning tracking off is refused.
    d.set_track_locked(true);
    assert!(d.track_locked());
    d.set_track_changes(false);
    assert!(d.track_changes_on(), "locked tracking stays on");

    // Unlocked: it can be turned off again.
    d.set_track_locked(false);
    d.set_track_changes(false);
    assert!(!d.track_changes_on());

    // Filtering Alice marks him hidden in the legend.
    d.set_reviewer_hidden("Alice", true);
    assert!(d.reviewers().contains("\"hidden\":true"), "Alice now filtered out");
}
