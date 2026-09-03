//! Page flow.
//! 
//! Places blocks down the page and decides where each one breaks: explicit page
//! breaks, keep-with-next so a heading is never orphaned, consolidated inter-paragraph
//! spacing, and the change bars and page fingerprints that fall out of the walk.

use crate::*;

impl Renderer {
    /// Lay out the document into discrete pages WITHOUT rasterizing: flow each block onto a page,
    /// compute caret geometry + block placements, and fingerprint each page's content. This is the
    /// cheap pass run on every keystroke; [`paint_page`] then rasterizes only the pages whose
    /// fingerprint changed. `ml`/`mr`/`mt`/`mb` are the page margins (px); `gap` the gutter between
    /// page sheets. (A paragraph that doesn't fit moves whole to the next page; line-level splitting
    /// across a boundary is not yet implemented.)
    #[allow(clippy::too_many_arguments)]
    pub fn layout_doc(
        &mut self,
        blocks: &[Block],
        content: &[Content],
        page_w: u32,
        page_h: u32,
        ml: f32,
        mr: f32,
        mt: f32,
        mb: f32,
        gap: u32,
        balloon_band: f32,
    ) -> DocLayout {
        self.begin_shape_pass();
        // A non-zero balloon band (revision balloons on) is reserved on the right by narrowing the
        // body's content width, so the body text shifts left and the band is free for the bubbles.
        let content_w = (page_w as f32 - ml - mr - balloon_band.max(0.0)).max(1.0);
        let page_bottom = (page_h as f32 - mb).max(mt + 1.0);

        let mut placements: Vec<Placement> = Vec::with_capacity(blocks.len());
        let mut lines: Vec<LineBox> = Vec::new();
        let mut cells: Vec<CellPlacement> = Vec::new();
        let mut change_bars: Vec<ChangeBar> = Vec::new();
        let mut inline_images: Vec<PageImage> = Vec::new();
        let mut placeholders: Vec<PagePlaceholder> = Vec::new();
        let mut hashes: Vec<u64> = vec![FNV_OFFSET]; // one per page
        let mut page: u32 = 0;
        let mut y = mt;
        // Contextual-spacing state: the previous paragraph's space-after (already folded into `y`),
        // its style group, and whether it opted into contextualSpacing - so when this paragraph is the
        // same style and also opts in, the inter-paragraph gap collapses to zero. A table resets this.
        let mut prev_space_after = 0.0_f32;
        let mut prev_group = u64::MAX;
        let mut prev_ctx = false;

        for (ci, item) in content.iter().enumerate() {
            match item {
                Content::Para(idx) => {
                    let Some(block) = blocks.get(*idx) else { continue };
                    // Per-block content box: indents narrow the width + shift the left edge.
                    let il = block.indent_left_px.max(0.0);
                    let ir = block.indent_right_px.max(0.0);
                    let bx = ml + il;
                    let bw = (content_w - il - ir).max(1.0);

                    // An object-only paragraph (just inline images or a passthrough placeholder, no
                    // text) skips the blank text line - the object lines below ARE its content; a text
                    // paragraph lays its text out as usual.
                    let text_empty = block.spans.iter().all(|s| s.text.is_empty());
                    let only_objects =
                        text_empty && (!block.inline_images.is_empty() || !block.placeholders.is_empty());
                    // An empty paragraph that only carries a continuous section break is consolidated
                    // away by Word: it advances the flow by nothing (no line height, no space-after),
                    // so the following paragraphs ride up over it. We still push a caret line below so
                    // the empty paragraph stays selectable - only the FLOW height is zeroed here.
                    let empty_cont_carrier = text_empty && !only_objects && block.continuous_break;
                    let (bh, geom) = if text_empty {
                        let h = if only_objects || empty_cont_carrier { 0.0 } else { empty_line_height(block) };
                        (h, Vec::new())
                    } else {
                        self.shape_block_lines(block, bw, bx)
                    };
                    // contextualSpacing: between two adjacent same-style paragraphs that both opt in,
                    // Word adds NO space - collapse the gap (drop this paragraph's space-before and the
                    // previous one's space-after, which `y` already includes) rather than summing them.
                    let collapse = ci > 0
                        && block.contextual_spacing
                        && prev_ctx
                        && prev_group == block.style_group;
                    // Word consolidates inter-paragraph spacing: the gap between two paragraphs is the
                    // MAX of the previous one's space-after and this one's space-before, NOT the sum
                    // (tdf169986: 20pt-after meeting 20pt-before is a ~20pt gap in Word; LibreOffice
                    // models the same Word rule as ParaSpaceMax). `y` already includes the previous
                    // space-after, so back it out and re-apply the max. And at the top of a page
                    // beyond the first (nothing placed yet - `y` still sits at the margin), Word
                    // applies no space-before at all (tdf170119: `w:before=3000` right after a hard
                    // page break starts at the margin exactly) - but it DOES honor space-before at
                    // the start of the document (tdf160049). Legacy documents
                    // (`Block::legacy_spacing`) keep the old summing behavior everywhere.
                    let mut top = if collapse {
                        (y - prev_space_after).max(mt)
                    } else if block.legacy_spacing {
                        y + block.space_before_px
                    } else if y <= mt && page > 0 {
                        y
                    } else {
                        (y - prev_space_after + prev_space_after.max(block.space_before_px)).max(mt)
                    };
                    // A manual page break / pageBreakBefore forces a new page, unless nothing has
                    // been PLACED on this page yet (no leading blank page). Tested on `y`, not `top`:
                    // a space-before at the top of an empty page must not fire the break - Word
                    // honors the doc-start space-before AND suppresses the break (tdf95495: a
                    // pageBreakBefore style on the document's first paragraph renders on page 1 at
                    // margin + before).
                    if block.page_break_before && y > mt {
                        page += 1;
                        top = mt;
                    }
                    // keepNext: the paragraph must share a page with the START of the next paragraph
                    // (Word keeps a heading with its following body). Reserve one line of the next
                    // block so the pair breaks together rather than orphaning this one at the foot.
                    let mut need = bh;
                    if block.keep_next
                        && let Some(Content::Para(nidx)) = content.get(ci + 1)
                        && let Some(nb) = blocks.get(*nidx)
                    {
                        need += empty_line_height(nb);
                    }
                    // An empty section-terminator paragraph (a bare `w:sectPr` carrier) doesn't spill to
                    // a new page just because its line won't fit at the foot - Word lets the trailing
                    // mark sit at the bottom and the break-after starts the next content on a new page.
                    let empty_terminator = text_empty && !only_objects && block.section_terminator;
                    if top + need > page_bottom && top > mt && !empty_terminator {
                        page += 1;
                        top = mt;
                    }
                    while hashes.len() <= page as usize {
                        hashes.push(FNV_OFFSET);
                    }
                    hashes[page as usize] = fold_block(hashes[page as usize], block, top);

                    let abs_y = (page * (page_h + gap)) as f32 + top;
                    if text_empty {
                        if !only_objects {
                            lines.push(LineBox {
                                para: *idx,
                                y: abs_y,
                                height: empty_line_height(block),
                                stops: vec![CaretStop { byte: 0, x: bx }],
                            });
                        }
                        // An empty changed paragraph (e.g. a paragraph-property change) bars its line.
                        if !block.change_ranges.is_empty() {
                            change_bars.push(ChangeBar { page, y: top, height: bh, para: *idx });
                        }
                    } else {
                        // Bar only the visual lines that actually carry a change (Word's per-line bar).
                        for (rel_top, lh, stops) in geom {
                            if line_has_change(&block.change_ranges, &stops) {
                                change_bars.push(ChangeBar { page, y: top + rel_top, height: lh, para: *idx });
                            }
                            lines.push(LineBox { para: *idx, y: abs_y + rel_top, height: lh, stops });
                        }
                    }
                    placements.push(Placement { block: *idx, page, y: top });

                    // Reserve a line of its own height for each inline picture, below the paragraph's
                    // text, breaking to a new page when one would overflow. Each becomes a page-local
                    // placement (foreground composite) + a caret line so the picture is a caret stop.
                    let mut iy = top + bh;
                    for img in &block.inline_images {
                        let iw = img.w.max(1.0);
                        let ih = img.h.clamp(1.0, page_bottom - mt);
                        if iy + ih > page_bottom && iy > mt {
                            page += 1;
                            iy = mt;
                            while hashes.len() <= page as usize {
                                hashes.push(FNV_OFFSET);
                            }
                        }
                        let mut hh = hashes[page as usize];
                        hh = fnv_bytes(hh, img.key.as_bytes());
                        hh = fnv_bytes(hh, &iy.to_bits().to_le_bytes());
                        hh = fnv_bytes(hh, &iw.to_bits().to_le_bytes());
                        hh = fnv_bytes(hh, &ih.to_bits().to_le_bytes());
                        hashes[page as usize] = hh;
                        let img_abs_y = (page * (page_h + gap)) as f32 + iy;
                        inline_images.push(PageImage {
                            key: img.key.clone(),
                            x: bx,
                            y: iy,
                            w: iw,
                            h: ih,
                            behind: false,
                            crop: img.crop,
                            page,
                            id: Some(img.id),
                            dim: 0.0,
                        });
                        lines.push(LineBox {
                            para: *idx,
                            y: img_abs_y,
                            height: ih,
                            stops: vec![CaretStop { byte: img.byte, x: bx }],
                        });
                        iy += ih;
                    }
                    // Reserve a line + placement for each passthrough object (OLE / chart / shape) the
                    // same way, breaking to a new page on overflow. Painted as a neutral labelled box by
                    // `paint_page`; a caret line makes it a selectable stop (see `docs/passthrough.md`).
                    for ph in &block.placeholders {
                        let pw = ph.w.clamp(1.0, content_w);
                        let ph_h = ph.h.clamp(1.0, page_bottom - mt);
                        if iy + ph_h > page_bottom && iy > mt {
                            page += 1;
                            iy = mt;
                            while hashes.len() <= page as usize {
                                hashes.push(FNV_OFFSET);
                            }
                        }
                        let mut hh = hashes[page as usize];
                        hh = fnv_bytes(hh, ph.label.as_bytes());
                        hh = fnv_bytes(hh, &iy.to_bits().to_le_bytes());
                        hh = fnv_bytes(hh, &pw.to_bits().to_le_bytes());
                        hh = fnv_bytes(hh, &ph_h.to_bits().to_le_bytes());
                        hashes[page as usize] = hh;
                        let abs_y = (page * (page_h + gap)) as f32 + iy;
                        placeholders.push(PagePlaceholder {
                            x: bx,
                            y: iy,
                            w: pw,
                            h: ph_h,
                            label: ph.label.clone(),
                            page,
                        });
                        lines.push(LineBox {
                            para: *idx,
                            y: abs_y,
                            height: ph_h,
                            stops: vec![CaretStop { byte: ph.byte, x: bx }],
                        });
                        iy += ph_h;
                    }
                    // The empty continuous-break carrier contributes no space-after - Word collapses
                    // it away, so the next paragraph follows the carrier's predecessor, not its `after`.
                    let eff_space_after = if empty_cont_carrier { 0.0 } else { block.space_after_px };
                    y = iy + eff_space_after;
                    // The consolidated carrier is transparent to the spacing collapse too: the NEXT
                    // paragraph's space-before must consolidate against the carrier's PREDECESSOR's
                    // space-after (tdf169986: 20pt-after + carrier + 20pt-before is one 20pt gap in
                    // Word, not 40), so keep the predecessor's value across the carrier.
                    if !empty_cont_carrier {
                        prev_space_after = eff_space_after;
                        prev_group = block.style_group;
                        prev_ctx = block.contextual_spacing;
                    }
                }
                Content::Table(table) => {
                    // A section / manual page break before the table (paragraph-only break propagation
                    // misses this): start the table at the top of a new page.
                    if table.page_break_before && y > mt {
                        page += 1;
                        y = mt;
                    }
                    self.layout_table(
                        table, content_w, ml, mt, page_bottom, page_h, gap, &mut page, &mut y,
                        &mut cells, &mut lines, &mut hashes, &mut change_bars,
                    );
                    // A table breaks the same-style paragraph run - the next paragraph keeps its full
                    // space-before.
                    prev_ctx = false;
                    prev_group = u64::MAX;
                    prev_space_after = 0.0;
                }
            }
        }

        let num_pages = page + 1;
        let total_h = num_pages * page_h + num_pages.saturating_sub(1) * gap;
        let pages = hashes.into_iter().map(|fingerprint| PageInfo { fingerprint }).collect();
        // Margin change-bar geometry (device px): a thin vertical line set a little way into the left
        // margin, out from the text. Derived from the (already scaled) left margin so it tracks
        // zoom / DPR. `max` keeps it on-page even with a tiny / zero margin.
        let change_bar_w = (ml * 0.025).clamp(1.0, 4.0);
        let change_bar_x = (ml - ml * 0.18).max(change_bar_w);
        DocLayout {
            lines,
            placements,
            cells,
            pages,
            page_width: page_w,
            page_height: page_h,
            gap,
            total_height: total_h,
            margin_left: ml,
            content_width: content_w,
            change_bars,
            change_bar_x,
            change_bar_w,
            balloon_band: balloon_band.max(0.0),
            inline_images,
            placeholders,
        }
    }
}
