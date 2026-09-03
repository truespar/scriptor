//! CRDT paragraphs -> layout blocks.
//! 
//! The first half of a relayout: resolve each paragraph's styles, tracked changes and
//! table-style inheritance into a `scriptor_layout::Block`, decide which runs the
//! current review mode shows, and pull deletions into margin balloons. Nothing here
//! touches `ScriptorDoc`; it is a pure function of the model plus the display mode.

use crate::*;

/// Clone `blocks`, replacing computed-field placeholders with this page's values: `FIELD_PAGE` ->
/// the 1-based page number, `FIELD_NUMPAGES` -> the total page count. Spans without a placeholder
/// are cloned untouched.
pub(crate) fn substitute_fields(
    blocks: &[scriptor_layout::Block],
    pageno: u32,
    total: u32,
) -> Vec<scriptor_layout::Block> {
    blocks
        .iter()
        .map(|b| {
            let mut b = b.clone();
            for s in &mut b.spans {
                if s.text.contains(FIELD_PAGE) || s.text.contains(FIELD_NUMPAGES) {
                    s.text = s
                        .text
                        .replace(FIELD_PAGE, &pageno.to_string())
                        .replace(FIELD_NUMPAGES, &total.to_string());
                }
            }
            b
        })
        .collect()
}

/// Resolve materialized paragraphs into renderer blocks: each run's effective size / weight / italic
/// / underline / strike / color is its inline value over the paragraph style; paragraph alignment /
/// spacing / indents become block fields. `scale` converts OOXML units to device px. Shared by the
/// body, headers, and footers.
/// What a margin balloon carries (Word's "Show Revisions in Balloons" content): a tracked **deletion**
/// pulled out of the line, a **formatting** change described in words (the text stays inline), or a
/// **comment** body. Insertions stay inline in every mode, so they never balloon.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BalloonKind {
    Deletion,
    Format,
    Comment,
}

/// One margin balloon: its kind (drives style + label), text, and the author whose hue colours it.
pub(crate) struct BalloonItem {
    pub(crate) kind: BalloonKind,
    pub(crate) text: String,
    pub(crate) author: String,
}

/// Amber hue for a comment balloon (matches the reviewing pane's comment badge), since comments aren't
/// a [`TrackKind`] with an author-rotated colour.
pub(crate) const COMMENT_BALLOON_RGB: [u8; 3] = [0x92, 0x56, 0x0b];

/// Build the content block for a margin balloon, styled by kind: a deletion is the struck deleted text
/// in the author's hue; a formatting change is a "Formatted: ..." description (not struck); a comment is
/// its body in the comment hue. ~10pt, like Word's balloon text. Painted inside the balloon box.
pub(crate) fn balloon_block(item: &BalloonItem, scale: f32) -> scriptor_layout::Block {
    let (text, struck, color) = match item.kind {
        BalloonKind::Deletion => (item.text.clone(), true, track_colour(&item.author, TrackKind::Del)),
        BalloonKind::Format => {
            (format!("Formatted: {}", item.text), false, track_colour(&item.author, TrackKind::Fmt))
        }
        BalloonKind::Comment => (item.text.clone(), false, COMMENT_BALLOON_RGB),
    };
    let span = scriptor_layout::Span {
        text,
        size_px: 10.0 * (96.0 / 72.0) * scale, // ~10pt
        bold: false,
        italic: false,
        underline: false,
        strike: struck,
        color,
        highlight: None,
        baseline_shift: 0.0,
        family: scriptor_layout::resolve_family("Calibri").to_string(),
    };
    scriptor_layout::Block { spans: vec![span], line_mult: 1.0, ..Default::default() }
}

/// A human description of a tracked **run** formatting change (`w:rPrChange`) - what changed between the
/// recorded old props and the run's current (new) props, comma-joined (e.g. "Bold, Font: Arial").
pub(crate) fn describe_run_format_change(old: &scriptor_crdt::RunProps, r: &scriptor_crdt::Run) -> String {
    let mut parts: Vec<String> = Vec::new();
    if old.bold != r.bold {
        parts.push(if r.bold { "Bold".into() } else { "Not bold".into() });
    }
    if old.italic != r.italic {
        parts.push(if r.italic { "Italic".into() } else { "Not italic".into() });
    }
    if old.underline != r.underline {
        parts.push(if r.underline { "Underline".into() } else { "No underline".into() });
    }
    if old.strike != r.strike {
        parts.push(if r.strike { "Strikethrough".into() } else { "No strikethrough".into() });
    }
    if old.size != r.size {
        parts.push("Font size".into());
    }
    if old.color != r.color {
        parts.push("Font colour".into());
    }
    if old.font != r.font {
        match &r.font {
            Some(f) => parts.push(format!("Font: {f}")),
            None => parts.push("Font".into()),
        }
    }
    if parts.is_empty() {
        "formatting".into()
    } else {
        parts.join(", ")
    }
}

/// A human description of a tracked **paragraph** formatting change (`w:pPrChange`) - what changed
/// between the recorded old style / props and the paragraph's current ones.
pub(crate) fn describe_para_format_change(
    c: &scriptor_crdt::ParaPropChange,
    p: &scriptor_crdt::Paragraph,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if c.old_style != p.style {
        match &p.style {
            Some(s) => parts.push(format!("Style: {s}")),
            None => parts.push("Style".into()),
        }
    }
    if c.old.align != p.props.align {
        parts.push("Alignment".into());
    }
    if c.old.line_spacing != p.props.line_spacing {
        parts.push("Line spacing".into());
    }
    if c.old.space_before != p.props.space_before || c.old.space_after != p.props.space_after {
        parts.push("Spacing".into());
    }
    if c.old.indent_left != p.props.indent_left
        || c.old.indent_right != p.props.indent_right
        || c.old.indent_first != p.props.indent_first
    {
        parts.push("Indentation".into());
    }
    if c.old.num_id != p.props.num_id || c.old.num_ilvl != p.props.num_ilvl {
        parts.push("Numbering".into());
    }
    if parts.is_empty() {
        "paragraph formatting".into()
    } else {
        parts.join(", ")
    }
}


#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
/// Map each flow paragraph index to the style id of the table it sits in (`None` outside a table),
/// walking the body in document order exactly like [`build_flow`]. Lets [`resolve_blocks`] layer a
/// cell's table-style `rPr` (colour / size / font) below the paragraph style - e.g. a `TableGrid`
/// table whose cells are blue size-9 by style, with no direct run formatting.
pub(crate) fn table_style_per_para(body: &[scriptor_crdt::BodyItem], n: usize) -> Vec<Option<String>> {
    let mut out = vec![None; n];
    let mut cursor = 0usize;
    for item in body {
        match item {
            scriptor_crdt::BodyItem::Paragraph => cursor += 1,
            scriptor_crdt::BodyItem::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for _ in 0..cell.para_count {
                            if let Some(slot) = out.get_mut(cursor) {
                                *slot = t.style.clone();
                            }
                            cursor += 1;
                        }
                    }
                }
            }
        }
    }
    out
}

/// A short caption for a passthrough placeholder box, sniffed from the captured `<w:r>` XML. Best-effort
/// (checks the most distinctive element/prog-id substrings); falls back to a generic "Object".
pub(crate) fn passthrough_label(xml: &str) -> &'static str {
    if xml.contains("<w:control") {
        "ActiveX Control"
    } else if xml.contains("<w:object") {
        "Embedded Object"
    } else if xml.contains("c:chart") {
        "Chart"
    } else if xml.contains("dgm:") || xml.contains("SmartArt") {
        "SmartArt"
    } else if xml.contains("w:txbxContent") || xml.contains("wps:wsp") {
        "Text Box"
    } else if xml.contains("<w:drawing") || xml.contains("<w:pict") || xml.contains("v:shape") || xml.contains("v:line") {
        "Shape"
    } else {
        "Object"
    }
}

/// What [`resolve_blocks`] returns: the laid-out blocks, each block's visible text segments as
/// `(start, end)` byte spans, and the revision balloons pulled into the margin for each block.
pub(crate) type ResolvedBlocks =
    (Vec<scriptor_layout::Block>, Vec<Vec<(usize, usize)>>, Vec<Vec<BalloonItem>>);

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_blocks(
    paras: &[scriptor_crdt::Paragraph],
    styles: &scriptor_crdt::StyleTable,
    scale: f32,
    mode: TrackDisplay,
    hidden: &std::collections::HashSet<String>,
    expanded: &std::collections::HashSet<usize>,
    balloons: bool,
    comments: &[scriptor_crdt::Comment],
    images: &std::collections::HashMap<u64, scriptor_crdt::ImagePlacement>,
    // Verbatim-passthrough objects (id -> captured `<w:r>` XML): a `raw` run becomes a labelled
    // placeholder box instead of laying out its `U+FFFC`. Empty when there are no unmodeled objects.
    raws: &std::collections::HashMap<u64, String>,
    // Per-paragraph table style id (from [`table_style_per_para`]); `Some` for a table-cell
    // paragraph so its runs inherit the table style's rPr. Pass `&[]` when there are no tables.
    table_style_of: &[Option<String>],
) -> ResolvedBlocks {
    let pt_to_px = (96.0 / 72.0) * scale;
    let halfpt_to_px = pt_to_px / 2.0;
    let twip_to_px = |tw: u32| (tw as f32 / 20.0) * pt_to_px;
    let twip_px = |tw: i32| (tw as f32 / 20.0) * pt_to_px;
    let emu_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * scale;
    const DEFAULT_HALFPT: u16 = 22; // 11pt body default when nothing specifies a size
    // A run whose tracked-change author is filtered out ("Show Markup by reviewer"): its markup is
    // suppressed (the additions vanish, the deletions show as plain text) - a per-reviewer Original.
    let author_hidden = |r: &scriptor_crdt::Run| -> bool {
        r.track.as_ref().is_some_and(|t| hidden.contains(&t.author))
            || r.fmt_change.as_ref().is_some_and(|f| hidden.contains(&f.author))
    };

    // Comment balloons (balloon mode): a comment whose anchored range spans several paragraphs must
    // balloon only once - at the first paragraph it touches. Pre-compute that paragraph per comment id,
    // plus a body/author/thread lookup. Only top-level, unresolved comments balloon (replies thread
    // under their parent; resolved threads are out of the review surface).
    let comment_meta: std::collections::HashMap<u64, &scriptor_crdt::Comment> =
        comments.iter().map(|c| (c.id, c)).collect();
    let mut comment_first_para: std::collections::HashMap<u64, usize> =
        std::collections::HashMap::new();
    if balloons {
        for (pi, p) in paras.iter().enumerate() {
            for r in &p.runs {
                for &cid in &r.comments {
                    comment_first_para.entry(cid).or_insert(pi);
                }
            }
        }
    }

    // Each paragraph yields its laid-out block plus its visible-run map: the full-text byte ranges of
    // the runs that survived display-mode / reviewer filtering, in order. Layout (and the caret
    // geometry) index the *visible* text; the model + the JS shell index the *full* text. The map
    // bridges the two so the caret lands correctly even when runs are hidden (No-Markup / Original /
    // Simple). When nothing is hidden it covers `[0, len)` contiguously and the bridge is the identity.
    // The third element is the paragraph's balloon content (deletions / formatting / comments) when
    // balloon mode is on.
    type Resolved = (scriptor_layout::Block, Vec<(usize, usize)>, Vec<BalloonItem>);
    let resolved: Vec<Resolved> = paras
        .iter()
        .enumerate()
        .map(|(pi, p)| {
            // Per-paragraph effective display mode: a paragraph the editor "expanded" (clicked its
            // change-bar in Simple Markup) renders in All-Markup so its inline redline shows, while the
            // rest of the document stays clean. Everything below keys off `eff`, not the doc `mode`.
            let eff = if expanded.contains(&pi) { TrackDisplay::AllMarkup } else { mode };
            // Word draws the margin change-bar in the markup views (All / Simple), not Final / Original.
            let shows_bars = matches!(eff, TrackDisplay::AllMarkup | TrackDisplay::SimpleMarkup);
            // Run-prop base: a table-cell paragraph layers the table style's rPr below the paragraph
            // style (docDefaults < table style < paragraph style < direct run), so cells inherit the
            // table style's colour / size / font; a body paragraph just resolves its own style chain.
            let base = match table_style_of.get(pi).and_then(|o| o.as_deref()) {
                Some(ts) => styles.resolve_in_table(p.style.as_deref(), Some(ts)),
                None => styles.resolve(p.style.as_deref()),
            };
            // Build the laid-out spans + the byte ranges that carry a change (for the per-line bar) +
            // the visible-run map, in one pass over the runs. `byte` tracks the concatenated (filtered)
            // text - the space cosmic-text lays out + the caret stops index; `full` tracks the model's
            // full text (every run, hidden or not), so a visible run maps `byte -> full`.
            let mut spans: Vec<scriptor_layout::Span> = Vec::new();
            let mut change_ranges: Vec<(usize, usize)> = Vec::new();
            let mut vis_segments: Vec<(usize, usize)> = Vec::new();
            // Inline pictures anchored in this paragraph (their placeholder run is stripped from the
            // laid-out text below; each reserves a line of its own height in `layout_doc`).
            let mut inline_imgs: Vec<scriptor_layout::InlineImage> = Vec::new();
            // Passthrough objects anchored here (OLE / chart / shape): each stripped from the laid-out
            // text and shown as a labelled placeholder box (see `docs/passthrough.md`).
            let mut placeholders: Vec<scriptor_layout::Placeholder> = Vec::new();
            // Revision balloons: the paragraph's deletions pulled out of the line into a right-margin
            // bubble (the deleted text + its author for colour), plus a formatting description, plus any
            // comments anchored here. All empty unless balloon mode is on.
            let mut balloon_items: Vec<BalloonItem> = Vec::new();
            let mut balloon_text = String::new();
            let mut balloon_author = String::new();
            // Formatting changes anchored in this paragraph (run rPrChange descriptions, deduped); the
            // paragraph pPrChange is added after the run loop. Joined into one "Formatted: ..." balloon.
            let mut fmt_descs: Vec<String> = Vec::new();
            let mut fmt_author = String::new();
            let mut byte = 0usize;
            let mut full = 0usize;
            for r in &p.runs {
                let n = r.text.len(); // byte length in both the full + (if visible) the laid-out text
                // A picture run (its text is the U+FFFC placeholder): never laid out as a glyph (no
                // tofu). An inline picture reserves a line of its own height in the flow; a floating
                // one is positioned + wrapped separately (page_images). Either way the placeholder
                // occupies the model's full text but contributes no visible/laid-out bytes.
                if let Some(id) = r.image {
                    // A tracked picture the mode hides (a deletion in Final/Simple, an insertion in
                    // Original) or a filtered reviewer's inserted picture doesn't render - the inline
                    // picture's line isn't reserved, so the paragraph collapses, exactly as Word does.
                    let img_hidden = r.track.as_ref().is_some_and(|t| eff.hides(t.kind))
                        || (author_hidden(r)
                            && r.track.as_ref().is_some_and(|t| matches!(t.kind, TrackKind::Ins | TrackKind::MoveTo)));
                    if let Some(pl) = images.get(&id)
                        && !pl.floating
                        && !img_hidden {
                            inline_imgs.push(scriptor_layout::InlineImage {
                                id,
                                byte,
                                w: emu_px(pl.w_emu),
                                h: emu_px(pl.h_emu),
                                key: pl.media.clone(),
                                crop: [pl.crop_l, pl.crop_t, pl.crop_r, pl.crop_b],
                            });
                        }
                    full += n;
                    continue;
                }
                // A passthrough object run (OLE / chart / shape; its text is the U+FFFC placeholder):
                // like a picture it never lays out as a glyph. It reserves a labelled placeholder box,
                // unless the display mode hides its tracked change (a deletion in Final/Simple, an
                // insertion in Original) or a filtered reviewer inserted it - then it collapses.
                if let Some(id) = r.raw {
                    let obj_hidden = r.track.as_ref().is_some_and(|t| eff.hides(t.kind))
                        || (author_hidden(r)
                            && r.track.as_ref().is_some_and(|t| matches!(t.kind, TrackKind::Ins | TrackKind::MoveTo)));
                    if let Some(xml) = raws.get(&id)
                        && !obj_hidden {
                            placeholders.push(scriptor_layout::Placeholder {
                                id,
                                byte,
                                // The object has no modeled size; a neutral ~2in x 1.2in footprint.
                                w: emu_px(1_828_800),
                                h: emu_px(1_097_280),
                                label: passthrough_label(xml).to_string(),
                            });
                        }
                    full += n;
                    continue;
                }
                // Balloon mode pulls a tracked deletion (del / moveFrom) out of the line into the
                // margin bubble, so it's hidden inline like a Final-view deletion but kept for the
                // balloon. (Insertions stay inline; a filtered reviewer's runs are handled below.)
                let is_del = r.track.as_ref().is_some_and(|t| matches!(t.kind, TrackKind::Del | TrackKind::MoveFrom));
                let to_balloon = balloons && shows_bars && is_del && !author_hidden(r);
                // Per-mode filtering: a deletion vanishes in Final / Simple, an insertion in Original;
                // a filtered reviewer's additions (ins / moveTo) vanish, their deletions stay.
                let mode_hides = to_balloon || r.track.as_ref().is_some_and(|t| eff.hides(t.kind));
                let author_hides = author_hidden(r)
                    && r.track.as_ref().is_some_and(|t| matches!(t.kind, TrackKind::Ins | TrackKind::MoveTo));
                // A visible (non-filtered) tracked change contributes a change-bar on its line.
                let is_change =
                    shows_bars && !author_hidden(r) && (r.track.is_some() || r.fmt_change.is_some());
                if mode_hides || author_hides {
                    // The run isn't laid out; if the *mode* hid a visible change (a deletion in Simple
                    // Markup), still bar its line with a zero-width marker at the current position.
                    if is_change && mode_hides {
                        change_ranges.push((byte, byte));
                    }
                    if to_balloon {
                        balloon_text.push_str(&r.text);
                        if balloon_author.is_empty() {
                            balloon_author =
                                r.track.as_ref().map(|t| t.author.clone()).unwrap_or_default();
                        }
                    }
                    full += n; // a hidden run still occupies the model's full text
                    continue;
                }
                // A visible run carrying a tracked formatting change (rPrChange) keeps its new
                // formatting inline; in balloon mode its description also goes to a margin balloon.
                if balloons && shows_bars && !author_hidden(r)
                    && let Some(fc) = &r.fmt_change {
                        let d = describe_run_format_change(&fc.old, r);
                        if !fmt_descs.contains(&d) {
                            fmt_descs.push(d);
                        }
                        if fmt_author.is_empty() {
                            fmt_author = fc.author.clone();
                        }
                    }
                let halfpt = r.size.or(base.size).unwrap_or(DEFAULT_HALFPT);
                let hex = r.color.as_deref().or(base.color.as_deref()).unwrap_or("1A1A1A");
                // Run font, else the style/docDefaults font, else Word's Calibri default; resolved to
                // the metric-compatible clone we actually shape with.
                let req = r.font.as_deref().or(base.font.as_deref()).unwrap_or("Calibri");
                // All-Markup styling: insertions underlined, deletions struck, both in the author's
                // colour. A filtered reviewer's surviving runs (their deletions) render as plain text.
                let mut underline = r.underline;
                let mut strike = r.strike;
                let mut color = parse_hex(hex);
                if eff == TrackDisplay::AllMarkup && !author_hidden(r) {
                    if let Some(t) = &r.track {
                        color = track_colour(&t.author, t.kind);
                        match t.kind {
                            // Moved text reads like its resolution twin: the destination underlined
                            // (an arrival), the source struck (a departure).
                            TrackKind::Ins | TrackKind::MoveTo => underline = true,
                            TrackKind::Del | TrackKind::MoveFrom => strike = true,
                            TrackKind::Fmt => {}
                        }
                    } else if let Some(fc) = &r.fmt_change {
                        // A tracked formatting change keeps its new formatting but is recoloured to the
                        // author's hue so the change is visible (no underline/strike).
                        color = track_colour(&fc.author, TrackKind::Fmt);
                    }
                }
                // Hyperlinks render in Word's link style (blue + underline) unless the run was recoloured
                // by a tracked change above, or carries its own explicit colour. A link that's also part
                // of a field result (a TOC entry) inherits the field's style instead - Word's TOC entries
                // are clickable but keep the TOC paragraph colour, not hyperlink blue.
                let recoloured = eff == TrackDisplay::AllMarkup
                    && !author_hidden(r)
                    && (r.track.is_some() || r.fmt_change.is_some());
                if r.link.is_some() && r.field.is_none() && !recoloured {
                    if r.color.is_none() {
                        color = [0x05, 0x63, 0xC1];
                    }
                    underline = true;
                }
                if is_change {
                    change_ranges.push((byte, byte + n));
                }
                vis_segments.push((full, full + n)); // this visible run maps `byte..byte+n` -> `full..full+n`
                byte += n;
                full += n;
                // Super/subscript render as smaller glyphs raised / lowered from the baseline (the
                // layout engine applies `baseline_shift` per span; size is reduced to ~65%).
                let full_px = halfpt as f32 * halfpt_to_px;
                let (size_px, baseline_shift) = match r.vert_align.as_deref() {
                    Some("superscript") => (full_px * 0.65, full_px * 0.33),
                    Some("subscript") => (full_px * 0.65, -full_px * 0.12),
                    _ => (full_px, 0.0),
                };
                // Highlight precedence: direct run > character style (rStyle) > paragraph/table style.
                let cs_hl = r.char_style.as_deref().and_then(|cs| styles.resolve(Some(cs)).highlight);
                // The fill behind the glyphs: a highlight wins; otherwise the run's own shading
                // (w:rPr/w:shd) - so a run that cancels its highlight but sets shd shows the shd colour.
                let fill = r
                    .highlight
                    .as_deref()
                    .or(cs_hl.as_deref())
                    .or(base.highlight.as_deref())
                    .and_then(highlight_rgb)
                    .or_else(|| r.shading.as_deref().map(parse_hex));
                spans.push(scriptor_layout::Span {
                    text: r.text.clone(),
                    size_px,
                    bold: r.bold || base.bold.unwrap_or(false),
                    italic: r.italic || base.italic.unwrap_or(false),
                    underline,
                    strike,
                    color,
                    highlight: fill,
                    baseline_shift,
                    family: scriptor_layout::resolve_family(req).to_string(),
                });
            }
            // Paragraph-level changes map to byte ranges over the (filtered) text: a property change
            // bars the whole paragraph; a paragraph-mark revision bars the ¶ at the paragraph end.
            if shows_bars {
                if p.prop_change.as_ref().is_some_and(|c| !hidden.contains(&c.author)) {
                    change_ranges.push((0, byte));
                    // The paragraph-property change (pPrChange) also describes itself in a balloon.
                    if balloons
                        && let Some(c) = &p.prop_change {
                            fmt_descs.push(describe_para_format_change(c, p));
                            if fmt_author.is_empty() {
                                fmt_author = c.author.clone();
                            }
                        }
                }
                if p.mark_change.as_ref().is_some_and(|m| !hidden.contains(&m.author)) {
                    change_ranges.push((byte, byte));
                }
            }
            // An empty paragraph carries one sized (empty-text) span so its blank line gets the
            // right height - the paragraph MARK's size when set (Word sizes an empty line by its
            // mark: legal templates' sz=10 spacer paragraphs are 5pt lines, not full text lines),
            // else the style chain's.
            if spans.is_empty() {
                let halfpt =
                    p.props.mark_size.or(base.size.map(u32::from)).unwrap_or(DEFAULT_HALFPT.into())
                        as u16;
                spans.push(scriptor_layout::Span {
                    text: String::new(),
                    size_px: halfpt as f32 * halfpt_to_px,
                    bold: base.bold.unwrap_or(false),
                    italic: base.italic.unwrap_or(false),
                    underline: false,
                    strike: false,
                    color: parse_hex(base.color.as_deref().unwrap_or("1A1A1A")),
                    highlight: None,
                    baseline_shift: 0.0,
                    family: scriptor_layout::resolve_family(base.font.as_deref().unwrap_or("Calibri"))
                        .to_string(),
                });
            }
            // Direct paragraph alignment wins; else the style's (Title/Heading centred, Quote right) -
            // a paragraph with no `w:jc` must follow its style, not default left.
            let align = match p.props.align.or(base.align) {
                Some(scriptor_crdt::Align::Center) => scriptor_layout::BlockAlign::Center,
                Some(scriptor_crdt::Align::Right) => scriptor_layout::BlockAlign::Right,
                Some(scriptor_crdt::Align::Justify) => scriptor_layout::BlockAlign::Justify,
                _ => scriptor_layout::BlockAlign::Left,
            };
            // Direct paragraph spacing wins over the style's (a paragraph with w:after="0" must be
            // tight even when docDefaults / the style add space); fall back to style / docDefaults,
            // then to 0 - Word's true default when nothing specifies spacing. (A nonzero fallback
            // here added phantom space to every paragraph in docs without docDefault spacing, which
            // wrecked pagination - we fit ~half of Word's content per page.)
            let space_before = p.props.space_before.or(base.space_before);
            let space_after = p.props.space_after.or(base.space_after);
            // keepNext: direct paragraph wins, else the style (heading styles carry it).
            let keep_next = p.props.keep_next.or(base.keep_next).unwrap_or(false);
            // contextualSpacing + a style identity (for the same-style adjacency test). Unstyled body
            // paragraphs share the empty-string group, so consecutive ones collapse their spacing.
            let contextual_spacing = p.props.contextual_spacing.or(base.contextual_spacing).unwrap_or(false);
            let style_group = {
                let mut hsh = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(p.style.as_deref().unwrap_or(""), &mut hsh);
                std::hash::Hasher::finish(&hsh)
            };
            // A tracked paragraph-mark revision paints a coloured (struck, for a deletion) "¶" after
            // the text in All-Markup, so a tracked join isn't silent. Other modes don't show it.
            let (trailing, trailing_color, trailing_strike) = match (&p.mark_change, eff) {
                (Some(m), TrackDisplay::AllMarkup) if !hidden.contains(&m.author) => {
                    ("¶".to_string(), track_colour(&m.author, m.kind), m.kind == TrackKind::Del)
                }
                _ => (String::new(), [0, 0, 0], false),
            };
            // Line spacing: direct value + rule win, else the style / docDefaults pair. `auto` is a
            // 240ths-of-a-line multiplier; `exact` / `atLeast` are absolute twips (a fixed height /
            // a floor). The value's rule travels with it (direct value -> direct rule).
            let (ls_val, ls_rule) = if let Some(v) = p.props.line_spacing {
                (Some(v), p.props.line_rule)
            } else {
                (base.line_spacing, base.line_rule)
            };
            // `exact` pins an absolute line height. `atLeast` is left on the legacy multiplier path for
            // now: the spec-correct floor (max of natural / value) is right in isolation but exposes a
            // sub-point line-height accumulation that tips some Word-matching docs over by a page
            // (mixednumberings) - deferred until the line-metric fidelity is tightened.
            let (line_mult, line_exact_px, line_min_px) = match (ls_val, ls_rule) {
                (Some(v), Some(scriptor_crdt::LineRule::Exact)) if v > 0 => (1.0, twip_to_px(v as u32), 0.0),
                (Some(v), _) if v > 0 => (v as f32 / 240.0, 0.0, 0.0),
                _ => (1.0, 0.0, 0.0),
            };
            // Effective left indent (direct wins, else the style chain; signed). Hoisted because
            // tab stops measure from the MARGIN, not the indent origin (Word ruler semantics) -
            // the shaper resolves them relative to the block's box, so each stop shifts by the
            // indent: a Header style's negative indent pushes its right stop out to the widened
            // box edge, where Word parks the page number on the rule's end.
            let eff_il_px = p.props.indent_left.or(base.indent_left).map(twip_px).unwrap_or(0.0);
            let block = scriptor_layout::Block {
                spans,
                byte_offset: 0,
                space_before_px: space_before.map(twip_to_px).unwrap_or(0.0),
                space_after_px: space_after.map(twip_to_px).unwrap_or(0.0),
                align,
                line_mult,
                line_exact_px,
                line_min_px,
                // Direct indents win; the style chain supplies them otherwise (signed - Word's
                // Header styles widen the header box into the margins with negative indents).
                indent_left_px: eff_il_px,
                indent_right_px: p.props.indent_right.or(base.indent_right).map(twip_px).unwrap_or(0.0),
                marker: String::new(),
                hang_px: 0.0, // set by mark_block for list items (hanging indent)
                keep_next,
                // Force a new page before this paragraph for its own pageBreakBefore. Break propagation
                // from a PREVIOUS break-carrying paragraph (manual `<w:br type="page"/>` or a section
                // break) is applied later, in document order, by the content-stream builder - it sees
                // true body order (skipping cell paragraphs + out-of-flow frames) and also reaches a
                // table that follows the break, which a paragraph-index heuristic here would miss.
                // Direct paragraph pageBreakBefore wins; else the style's (a "page break before"
                // style forces its paragraphs onto a new page - tdf89377's NewPageBreak / Title).
                page_break_before: p.props.page_break_before || base.page_break_before.unwrap_or(false),
                section_terminator: p.props.section_end,
                continuous_break: p.props.continuous_break,
                contextual_spacing,
                // Doc-level; stamped in `relayout` (this resolver has no CollabDoc in scope).
                legacy_spacing: false,
                style_group,
                shading: p.props.shading.as_deref().map(parse_hex),
                // A paragraph with its own stops uses them; otherwise the style chain's (Word's
                // Header styles right-align the page number via style-defined right stops).
                // Margin-relative: shift by the block's indent (see eff_il_px above).
                tab_stops_px: if p.props.tab_stops.is_empty() {
                    base.tab_stops.iter().map(|&t| twip_to_px(t) - eff_il_px).collect()
                } else {
                    p.props.tab_stops.iter().map(|&t| twip_to_px(t) - eff_il_px).collect()
                },
                tab_kinds: if p.props.tab_stops.is_empty() {
                    base.tab_kinds.clone()
                } else {
                    p.props.tab_kinds.clone()
                },
                default_tab_px: twip_to_px(720), // Word default tab interval (0.5in)
                trailing,
                trailing_color,
                trailing_strike,
                // `has_change` is the any-change flag (fingerprint + external readers); the bar itself
                // is now per visual line via `change_ranges` (built above, markup modes only, excluding
                // filtered reviewers + a mode's hidden runs except a zero-width marker for those).
                has_change: !change_ranges.is_empty(),
                change_ranges,
                inline_images: inline_imgs,
                placeholders,
                // Paragraph border box (`w:pBdr`) - draws a text frame's rectangle, or any bordered
                // paragraph's box; the style chain supplies it when the paragraph doesn't (Word's
                // Header style carries its horizontal rule as a style pBdr). Edge weights / spacing
                // resolved to px at the render scale here.
                borders: parse_pbdr(p.props.border.as_deref().or(base.border.as_deref()), pt_to_px),
                // Two-sided frame wrap holes are filled in by the wrap pass (it needs pass-1 geometry).
                wrap_holes: Vec::new(),
            };
            // Assemble this paragraph's balloons: deletion (the pulled-out struck text), then a single
            // formatting balloon (run + paragraph descriptions joined), then one per comment anchored
            // here (top-level + unresolved only, at the comment's first paragraph so it shows once).
            if !balloon_text.is_empty() {
                balloon_items.push(BalloonItem {
                    kind: BalloonKind::Deletion,
                    text: balloon_text,
                    author: balloon_author,
                });
            }
            if !fmt_descs.is_empty() {
                balloon_items.push(BalloonItem {
                    kind: BalloonKind::Format,
                    text: fmt_descs.join("; "),
                    author: fmt_author,
                });
            }
            if balloons {
                let mut cids: Vec<u64> = comment_first_para
                    .iter()
                    .filter(|&(_, &fp)| fp == pi)
                    .map(|(&cid, _)| cid)
                    .collect();
                cids.sort_unstable();
                for cid in cids {
                    if let Some(c) = comment_meta.get(&cid)
                        && c.parent.is_none() && !c.resolved {
                            balloon_items.push(BalloonItem {
                                kind: BalloonKind::Comment,
                                text: c.text.clone(),
                                author: c.author.clone(),
                            });
                        }
                }
            }
            (block, vis_segments, balloon_items)
        })
        .collect();
    // Split the per-paragraph triples into parallel vectors (std `unzip` is only 2-way).
    let mut blocks = Vec::with_capacity(resolved.len());
    let mut segments = Vec::with_capacity(resolved.len());
    let mut balloons_out = Vec::with_capacity(resolved.len());
    for (block, segs, items) in resolved {
        blocks.push(block);
        segments.push(segs);
        balloons_out.push(items);
    }
    (blocks, segments, balloons_out)
}
