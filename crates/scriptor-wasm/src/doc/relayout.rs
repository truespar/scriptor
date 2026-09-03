//! The relayout pass.
//! 
//! Resolves the model into blocks, flows them onto pages, places floats and frames,
//! and records the per-page fingerprints that decide what needs repainting. Everything
//! the view asks about geometry afterwards is answered from what this produced.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Render the whole document to a [`PaintResult`] (RGBA8 + dimensions) at the document's real
    /// Re-resolve + lay out the whole document at the document's real page geometry (`w:sectPr` size
    /// + margins), WITHOUT rasterizing - the cheap pass run on every edit. `scale` is the device-
    /// pixel ratio. Each run's size / bold / italic / color is resolved from inline run formatting
    /// over the paragraph's `styles.xml` style (so headings / title get their real sizing). Returns
    /// a [`LayoutInfo`] (page dimensions + per-page fingerprints); the caller diffs the fingerprints
    /// and calls [`paint_page`] only for the pages that changed.
    #[wasm_bindgen(js_name = relayout)]
    pub fn relayout(&mut self, scale: f32) -> Result<LayoutInfo, JsError> {
        let mut paras = self.doc.paragraphs().map_err(to_js)?;
        // Drop any picture placement orphaned by a resolved tracked change (a tracked deletion
        // accepted, a tracked insertion rejected) - the run is gone but its `images` map entry
        // lingers. Checked against the paragraphs just read: the full paragraph read is relayout's
        // dominant cost, and paying it twice per keystroke made large image-carrying documents
        // visibly sluggish to type in. (Gc only deletes placement-map entries, never runs, so the
        // list stays valid.)
        if !self.doc.image_placements().is_empty() {
            let _ = self.doc.gc_orphan_images_against(&paras);
        }
        let para_texts: Vec<String> = paras
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
            .collect();
        let page = self.doc.page_geometry();
        // We don't lay out newspaper columns. In a document with NO multi-column section, a manual
        // column break has no next column to move to, so Word treats it as a page break - map it onto
        // the page-break machinery for layout (the model keeps the column-break flag for round-trip).
        if !page.multi_column {
            for p in paras.iter_mut() {
                if p.props.column_break_after {
                    p.props.page_break_after = true;
                }
            }
        }

        let mode = self.track_display;
        let hidden = &self.hidden_reviewers;
        // Click-to-expand (Simple Markup) renders one paragraph's inline redline while the rest stay
        // clean. The override only applies in Simple Markup - the one mode with clickable bars and
        // clean text to expand; any other mode ignores the set.
        let no_expand = std::collections::HashSet::new();
        let expanded: &std::collections::HashSet<usize> =
            if mode == TrackDisplay::SimpleMarkup { &self.expanded } else { &no_expand };
        // Revision balloons take effect only in the markup modes (All / Simple) - the modes that show
        // deletions; in Final / Original there's nothing to balloon.
        let balloons_eff = self.balloons && matches!(mode, TrackDisplay::AllMarkup | TrackDisplay::SimpleMarkup);
        // Comment bodies feed the comment balloons (body story only); empty when balloons are off.
        let doc_comments = if balloons_eff { self.doc.comments() } else { Vec::new() };
        // Editable pictures (body story); the resolver strips each placeholder run + reserves an inline
        // picture's line. Header/footer pictures stay on the read-only PlacedImage path (no map here).
        let body_images = self.doc.image_placements();
        let no_images: std::collections::HashMap<u64, scriptor_crdt::ImagePlacement> =
            std::collections::HashMap::new();
        // Verbatim-passthrough objects (body story): each `raw` run becomes a labelled placeholder box.
        // Header/footer passthrough is rare and stays a blank gap for now (empty map below).
        let body_raws = self.doc.passthrough_xml();
        let no_raws: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        // The body is needed up front to map cell paragraphs to their table style (so cell runs
        // inherit the table style's rPr), then reused to build the flow below.
        let body = self.doc.body();
        let body_table_styles = table_style_per_para(&body, paras.len());
        // Snapshot the effective style table once (it folds in runtime style-definition edits via a
        // loro reconcile on read). Cloning to an owned value - rather than holding the `Ref` guard -
        // keeps it usable across the later `&mut self` work without a borrow conflict.
        let styles = self.doc.styles().clone();
        let (mut blocks, body_segments, body_balloons) = resolve_blocks(
            &paras, &styles, scale, mode, hidden, expanded, balloons_eff, &doc_comments,
            &body_images, &body_raws, &body_table_styles,
        );
        // Paragraph-spacing compatibility is doc-level (`word/settings.xml`): stamp the legacy
        // (sum, not max-consolidate) mode on the body blocks. Headers/footers stack separately.
        if self.doc.legacy_para_spacing() {
            for b in blocks.iter_mut() {
                b.legacy_spacing = true;
            }
        }
        // Build the document flow (paragraphs interleaved with tables) + apply list markers across
        // the whole document (body + cells) with one shared counter.
        let mut frame_specs: Vec<FrameSpec> = Vec::new();
        // `numbering()` returns a `Ref` guard (the synth defs reconcile in from loro on read). Scope the
        // borrow to just the `build_flow` call so it drops before the later `&mut self` work below.
        let content = {
            let numbering = self.doc.numbering();
            build_flow(&body, &paras, &mut blocks, &numbering, &styles, scale, mode, hidden, &mut frame_specs)
        };
        // Header/footer parts: resolve blocks + caret data for EVERY part (a multi-section document
        // has one per distinct headerN/footerN file; several sections may share one). Which part
        // paints on which page is decided below (`page_hf`), from each page's section + `titlePg`.
        let mut hf_sets: Vec<HfSet> = Vec::new();
        for (part, is_header) in self.doc.hf_parts() {
            let Some(child) = self.doc.hf_part_doc(&part) else { continue };
            let paras = child.paragraphs().unwrap_or_default();
            let (mut part_blocks, segments, _) = resolve_blocks(
                &paras, &styles, scale, mode, hidden, &no_expand, false, &[], &no_images, &no_raws, &[],
            );
            inherit_hf_table_align(Some(child), &paras, &mut part_blocks);
            // Fold the part NAME into the hash: two parts with identical text must still repaint a
            // page whose section assignment swaps one for the other.
            let mut hash = hf_fingerprint(&part_blocks);
            for b in part.bytes() {
                hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
            hf_sets.push(HfSet {
                part,
                header: is_header,
                texts: hf_plain(&paras),
                segments,
                hash,
                blocks: part_blocks,
            });
        }

        // OOXML twips -> device px (1pt = 96/72 px, 1 twip = 1/20 pt).
        let twip_to_px = |tw: u32| (tw as f32 / 20.0) * (96.0 / 72.0) * scale;
        let page_w = twip_to_px(page.width).round() as u32;
        let page_h = twip_to_px(page.height).round() as u32;
        let gap = (24.0 * scale).round() as u32; // gutter between page sheets
        // Margins are signed (negative page margins enlarge the usable area), so convert with a signed
        // helper - a negative margin yields a negative px offset, widening the content box.
        let twip_px_signed = |tw: i32| (tw as f32 / 20.0) * (96.0 / 72.0) * scale;
        let (ml, mr, mt, mb) = (
            twip_px_signed(page.margin_left),
            twip_px_signed(page.margin_right),
            twip_px_signed(page.margin_top),
            twip_px_signed(page.margin_bottom),
        );
        // Header push-down. Word starts the body at the top margin, but a header taller than the
        // margin band (a tall logo, several header lines) reaches below it - so Word grows the top of
        // the body until it clears the header. Without this the first body lines collide with the
        // header. Measure the lowest point any header that can paint reaches (default + first-page,
        // text run + anchored pictures), mirroring `page_images`' placement so the body clears
        // wherever the header actually draws; take the max so no page collides. `mt` itself stays the
        // true top margin (body float "from margin" anchors resolve against it) - only the flow moves.
        let header_y = twip_to_px(page.header_dist);
        let mt_body = {
            let content_w = (page_w as f32 - ml - mr).max(1.0);
            let geom = FloatGeom { ml, mr, mt, page_w: page_w as f32, scale };
            let emu_to_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * scale;
            // The header text flow's height (taller of the default + first-page header), and the height
            // of its first line - the line a logo sits in. A header is commonly [logo-paragraph][empty]
            // [empty]; the logo grows its OWN line to the image height but the trailing paragraphs still
            // stack below it, so the header reaches text_h + (image - one line), not max(text, image).
            let mut text_h = 0.0_f32;
            let mut one_line = 0.0_f32;
            for set in hf_sets.iter().filter(|s| s.header) {
                text_h = text_h.max(self.renderer.run_height(&set.blocks, content_w));
                if !set.blocks.is_empty() {
                    one_line = one_line.max(self.renderer.run_height(&set.blocks[..1], content_w));
                }
            }
            let mut img_excess = 0.0_f32; // extra height an inline header image adds to its flow line
            let mut float_bottom = header_y + text_h; // bottom of any absolutely-positioned header image
            for img in self.doc.images() {
                if !matches!(img.context, scriptor_crdt::ImageContext::Hf { header: true, .. }) {
                    continue;
                }
                let (w, h) = (emu_to_px(img.w_emu), emu_to_px(img.h_emu));
                if h < 1.0 {
                    continue;
                }
                if img.anchored {
                    // A floating header image only pushes the body down when it wraps topAndBottom;
                    // square / tight / through / none / behind floats sit in their own layer (text
                    // flows around them per-paragraph) and must NOT reserve body-top space - else a
                    // full-page header watermark shoves every page's text off the bottom margin
                    // (tdf38575: a through-wrapped 848pt header image -> 31 pages vs Word's 4).
                    if img.wrap == "topAndBottom" {
                        let (_, y) = place_float(
                            &geom, w, img.anchored, &img.h_from, &img.h_align, img.x_emu, &img.v_from,
                            img.y_emu, header_y,
                        );
                        float_bottom = float_bottom.max(y + h);
                    }
                } else {
                    // Inline header image: it grows the line it occupies from a text line to its height.
                    img_excess = img_excess.max(h - one_line);
                }
            }
            let header_bottom = (header_y + text_h + img_excess.max(0.0)).max(float_bottom);
            mt.max(header_bottom)
        };
        // Reserve a right-margin band for balloons (narrowing the body) only when balloons are on and
        // something will actually balloon - otherwise the body keeps its full width. ~22% of the page,
        // floored so the bubbles have room to read.
        let balloon_band = if balloons_eff && body_balloons.iter().any(|v| !v.is_empty()) {
            (page_w as f32 * 0.22).max(120.0 * scale)
        } else {
            0.0
        };
        let mut layout = self
            .renderer
            .layout_doc(&blocks, &content, page_w, page_h, ml, mr, mt_body, mb, gap, balloon_band);

        // Text frames (`w:framePr`): position each off pass-1's page geometry, measure it, and keep
        // it for the paint pass; its box also becomes a wrap rect so body text reflows around it.
        let (frames, frame_rects) = self.compute_frames(
            &frame_specs, &blocks, &layout, page_w as f32, page_h as f32, ml, mr, mt_body, mb, scale,
        );
        self.frames = frames;

        // Paragraph-level square wrap: a body paragraph whose vertical band intersects a square/tight/
        // through float (on the same page) is narrowed away from the float's side, then the body is
        // laid out once more. One extra pass (Word iterates to convergence; we approximate with pass
        // 1's vertical bands - exact when the wrap doesn't reflow the float's own anchor region).
        let mut wrap_rects = self.wrap_float_rects(&layout);
        wrap_rects.extend(frame_rects);
        if !wrap_rects.is_empty() {
            let content_w = (page_w as f32 - ml - mr).max(1.0);
            let gutter = 9.0 * scale; // ~0.1in clearance between the float and the text
            let stride = (page_h + gap) as f32;
            let mut adjusted = false;
            for (bi, blk) in blocks.iter_mut().enumerate() {
                let Some(pl) = layout.placements.iter().find(|q| q.block == bi) else { continue };
                let origin = pl.page as f32 * stride;
                // The paragraph's page-local vertical band, from its pass-1 lines (fall back to the
                // placement top for an empty paragraph that produced none).
                let (mut top, mut bot) = (f32::INFINITY, f32::NEG_INFINITY);
                for l in layout.lines.iter().filter(|l| l.para == bi) {
                    top = top.min(l.y - origin);
                    bot = bot.max(l.y - origin + l.height);
                }
                if !top.is_finite() {
                    (top, bot) = (pl.y, pl.y);
                }
                let (add_l, add_r) = square_wrap_indents(
                    pl.page, top, bot, blk.indent_left_px, blk.indent_right_px,
                    ml, content_w, gutter, &wrap_rects,
                );
                if add_l > 0.5 {
                    blk.indent_left_px += add_l;
                    adjusted = true;
                }
                if add_r > 0.5 {
                    blk.indent_right_px += add_r;
                    adjusted = true;
                }
                // A frame straddling the content centre wraps on BOTH sides (the shaper flows the
                // text around the hole), instead of running full-width through it.
                if !blk.spans.iter().all(|s| s.text.is_empty()) {
                    let holes = centre_wrap_holes(pl.page, top, bot, ml, content_w, gutter, &wrap_rects);
                    if !holes.is_empty() {
                        blk.wrap_holes = holes;
                        adjusted = true;
                    }
                }
            }
            if adjusted {
                layout = self
                    .renderer
                    .layout_doc(&blocks, &content, page_w, page_h, ml, mr, mt_body, mb, gap, balloon_band);
            }
        }

        // Which header/footer part each page shows - Word's per-section rule. A page belongs to the
        // section of its first block (section index = count of section-terminator blocks before it;
        // a terminator is the LAST paragraph of its section, so it doesn't count itself). The first
        // page of a section under that section's `titlePg` takes the `first` slots (blank band when
        // unset - Word does NOT fall back to the default there); every other page the `default`
        // slots. This is what puts section 1's inherited first-page footer (the legal stamp) on
        // section 2's opening page while the later pages run the numbered header.
        let page_hf: Vec<[Option<usize>; 2]> = {
            let mut sec_before = vec![0usize; blocks.len() + 1];
            for (i, b) in blocks.iter().enumerate() {
                sec_before[i + 1] =
                    sec_before[i] + usize::from(b.section_terminator || b.continuous_break);
            }
            let npages = layout.pages.len();
            let mut first_block: Vec<Option<usize>> = vec![None; npages];
            for pl in &layout.placements {
                if let Some(slot) = first_block.get_mut(pl.page as usize) {
                    *slot = Some(slot.map_or(pl.block, |b| b.min(pl.block)));
                }
            }
            for c in &layout.cells {
                if let (Some(slot), Some(&min_para)) =
                    (first_block.get_mut(c.page as usize), c.para_ids.iter().min())
                {
                    *slot = Some(slot.map_or(min_para, |b| b.min(min_para)));
                }
            }
            let set_idx = |name: Option<&String>| {
                name.and_then(|n| hf_sets.iter().position(|s| s.part == *n))
            };
            let mut out = Vec::with_capacity(npages);
            let mut prev_sec = usize::MAX;
            for first_blk in first_block.iter().take(npages) {
                let sec = first_blk
                    .map(|b| sec_before[b.min(blocks.len())])
                    .unwrap_or(if prev_sec == usize::MAX { 0 } else { prev_sec });
                let first_of_sec = sec != prev_sec;
                prev_sec = sec;
                let sh = self.doc.section_hf(sec);
                let first = sh.title_pg && first_of_sec;
                out.push([
                    set_idx(if first { sh.header_first.as_ref() } else { sh.header_default.as_ref() }),
                    set_idx(if first { sh.footer_first.as_ref() } else { sh.footer_default.as_ref() }),
                ]);
            }
            out
        };

        // Header/footer caret geometry: the body flows through `layout_doc`, but the header/footer
        // paint on every page via `paint_block_run`, so emit their visual lines here too (with
        // namespaced para indices, one set per page) so they're hit-testable + caret-editable like the
        // body. Each page emits ITS parts' lines (per-section selection above), so a click into any
        // page's band resolves against what that page actually shows. The footer is bottom-aligned,
        // so its top depends on its own height, which differs per part.
        let content_w = (page_w as f32 - ml - mr).max(1.0);
        let footer_dist_px = twip_to_px(page.footer_dist);
        for p in 0..(layout.pages.len() as u32) {
            let origin = (p * (page_h + gap)) as f32;
            let [h_idx, f_idx] = page_hf[p as usize];
            if let Some(hdr) = h_idx.map(|i| &hf_sets[i].blocks)
                && !hdr.is_empty()
            {
                let mut hl = self.renderer.block_run_lines(hdr, content_w, ml, header_y, origin, HEADER_BASE);
                layout.lines.append(&mut hl);
            }
            if let Some(ftr) = f_idx.map(|i| &hf_sets[i].blocks)
                && !ftr.is_empty()
            {
                let footer_h = self.renderer.run_height(ftr, content_w);
                let footer_top = page_h as f32 - footer_dist_px - footer_h;
                let mut fl = self.renderer.block_run_lines(ftr, content_w, ml, footer_top, origin, FOOTER_BASE);
                layout.lines.append(&mut fl);
            }
        }

        // Anchored text-box stamps are render-only ink, but a click on one must still land the
        // caret in its story (the rotated legal stamp reads as "the footer" to the user): emit one
        // synthetic caret line covering each stamp's page box, targeting the box's anchor
        // paragraph. `hit_test` resolves among y-band candidates by horizontal distance, so the
        // line only wins clicks near the stamp itself, never body clicks at the same height.
        {
            let emu_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * scale;
            for tb in self.doc.textboxes() {
                let scriptor_crdt::ImageContext::Hf { part, header } = &tb.context else { continue };
                let Some(set_idx) = hf_sets.iter().position(|s| &s.part == part) else { continue };
                let (base, slot) = if *header { (HEADER_BASE, 0) } else { (FOOTER_BASE, 1) };
                let sx = match tb.h_from.as_str() {
                    "page" => emu_px(tb.x_emu),
                    _ => ml + emu_px(tb.x_emu), // margin / column: from the content-box left
                };
                let sy = match tb.v_from.as_str() {
                    "page" => emu_px(tb.y_emu),
                    _ => mt + emu_px(tb.y_emu),
                };
                let box_h = emu_px(tb.h_emu).max(12.0);
                for (p, sets) in page_hf.iter().enumerate() {
                    if sets[slot] != Some(set_idx) {
                        continue;
                    }
                    let origin = (p as u32 * (page_h + gap)) as f32;
                    layout.lines.push(scriptor_layout::LineBox {
                        para: base + tb.para,
                        y: origin + sy,
                        height: box_h,
                        stops: vec![scriptor_layout::CaretStop { byte: 0, x: sx }],
                    });
                }
            }
        }

        // Revision balloons: one bubble per balloon item (deletion / formatting / comment). Anchor each
        // at its paragraph's top y (the layout placement), build the content block, then stack bubbles
        // down each page (in y order) so they don't overlap - several items in one paragraph cascade.
        let mut balloon_placements: Vec<scriptor_layout::BalloonPlacement> = Vec::new();
        if balloon_band > 0.0 {
            let pad = 6.0_f32;
            let cw = (balloon_band - 4.0 * pad).max(1.0);
            let mut items: Vec<(u32, f32, scriptor_layout::Block, f32)> = Vec::new();
            for (i, para_items) in body_balloons.iter().enumerate() {
                if para_items.is_empty() {
                    continue;
                }
                let Some(pl) = layout.placements.iter().find(|p| p.block == i) else { continue };
                for item in para_items {
                    let block = balloon_block(item, scale);
                    let h = self.renderer.run_height(std::slice::from_ref(&block), cw) + 2.0 * pad;
                    items.push((pl.page, pl.y, block, h));
                }
            }
            items.sort_by(|a, b| {
                (a.0, a.1).partial_cmp(&(b.0, b.1)).unwrap_or(std::cmp::Ordering::Equal)
            });
            let (mut cur_page, mut cursor) = (u32::MAX, 0.0_f32);
            for (page, anchor_y, block, h) in items {
                if page != cur_page {
                    cur_page = page;
                    cursor = 0.0;
                }
                let top = anchor_y.max(cursor); // never above its anchor line, never overlapping the prior
                cursor = top + h + 8.0;
                balloon_placements.push(scriptor_layout::BalloonPlacement {
                    page,
                    y: top,
                    height: h,
                    anchor_y,
                    blocks: vec![block],
                });
            }
        }

        // The header/footer paint on every page but aren't part of the body page fingerprints, so an
        // (incremental) header/footer edit would otherwise not repaint. Fold their content hash into
        // every page's fingerprint - a header change dirties all pages (it shows on all of them). Page
        // 0 under titlePg shows the first-page header/footer, so it folds in *those* instead.
        // A header/footer picture's geometry lives in its placement, not the text blocks, so a
        // resize / crop / move of one wouldn't change the text hash and the page wouldn't repaint
        // (shrinking a header logo "did nothing"). Fold every H/F picture's geometry into the hash so
        // any picture edit dirties the pages (they repeat on all of them).
        let hf_img_hash = {
            let mut h = 0u64;
            for (enc_id, _ctx, p) in self.hf_images() {
                for v in [
                    enc_id, p.w_emu as u64, p.h_emu as u64, p.crop_l as u64, p.crop_t as u64,
                    p.crop_r as u64, p.crop_b as u64, p.x_emu as u64, p.y_emu as u64, p.floating as u64,
                ] {
                    h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
                }
                for s in [&p.media, &p.h_from, &p.v_from, &p.h_align, &p.v_align, &p.wrap] {
                    for b in s.bytes() {
                        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
                    }
                }
            }
            h
        };
        // Per PAGE: fold in the hash of the parts THAT page shows (the per-section map above), so a
        // header edit repaints exactly the pages showing that part, and a page whose section
        // assignment changed repaints even when its body text didn't.
        let page_hf_hash = |pg: usize| -> u64 {
            let [h, f] = page_hf.get(pg).copied().unwrap_or([None, None]);
            let g = |i: Option<usize>| i.map(|i| hf_sets[i].hash).unwrap_or(0);
            g(h) ^ g(f).rotate_left(1) ^ hf_img_hash
        };
        // The active region greys the OTHER regions on every page (Word's active/inactive header-footer
        // dimming), so fold it into each page's fingerprint - entering/leaving a header/footer then
        // dirties all pages and the dimming repaints everywhere, not just where text changed.
        let region_salt: u64 = match self.active_region {
            Region::Body => 0,
            Region::Header => 0x9E37_79B9_7F4A_7C15,
            Region::Footer => 0xC2B2_AE3D_27D4_EB4F,
        };
        let info = LayoutInfo {
            page_width: layout.page_width,
            page_height: layout.page_height,
            gap: layout.gap,
            total_height: layout.total_height,
            fingerprints: layout
                .pages
                .iter()
                .enumerate()
                .map(|(i, p)| p.fingerprint ^ page_hf_hash(i) ^ region_salt)
                .collect(),
        };
        // A frame can sit on a page past the body's last (e.g. it follows a trailing page break, so
        // the body alone ends a page early): extend the page count so that page is rendered + counted.
        if let Some(mp) = self.frames.iter().map(|f| f.page).max() {
            while (layout.pages.len() as u32) <= mp {
                layout.pages.push(scriptor_layout::PageInfo { fingerprint: 0 });
            }
        }
        self.layout = layout; // retained for hit-testing + caret/selection geometry
        self.para_texts = para_texts; // retained for byte<->char conversion at the API boundary
        self.blocks = blocks; // retained so paint_page can rasterize a single page
        self.hf_sets = hf_sets;
        self.page_hf = page_hf;
        self.body_segments = body_segments; // visible-run maps, retained for caret offset translation
        self.balloon_placements = balloon_placements;
        self.header_y = header_y;
        self.footer_dist_px = footer_dist_px;
        self.ml_px = ml;
        self.mr_px = mr;
        self.mt_px = mt;
        self.scale_last = scale;

        // Tracked pictures hidden under the current display mode (a deleted picture in Final/Simple, an
        // inserted one in Original, or a filtered reviewer's addition): the floating render + the
        // hit-rects skip these. Inline pictures are filtered per-paragraph inside resolve_blocks, so
        // they never reach the layout in the first place.
        let mut hidden_images = std::collections::HashSet::new();
        for p in &paras {
            for r in &p.runs {
                if let Some(id) = r.image {
                    let mode_hides = r.track.as_ref().is_some_and(|t| mode.hides(t.kind));
                    let author_hides = r.track.as_ref().is_some_and(|t| {
                        self.hidden_reviewers.contains(&t.author)
                            && matches!(t.kind, TrackKind::Ins | TrackKind::MoveTo)
                    });
                    if mode_hides || author_hides {
                        hidden_images.insert(id);
                    }
                }
            }
        }
        self.hidden_images = hidden_images;

        // Editable picture hit-rects in absolute canvas px (page-stacked y): inline pictures from the
        // laid-out flow, then floating pictures resolved against the final placements. Floats are
        // pushed last so they win an overlap (they paint over the text). For click-to-select + handles.
        let stride = (page_h + gap) as f32;
        let mut hits: Vec<ImageHit> = Vec::new();
        for im in &self.layout.inline_images {
            if let Some(id) = im.id {
                hits.push(ImageHit { id, x: im.x, y: im.y + im.page as f32 * stride, w: im.w, h: im.h, page: im.page });
            }
        }
        {
            let emu_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * scale;
            let anchors = self.image_anchor_paras();
            let geom = self.float_geom();
            for (id, p) in self.doc.image_placements() {
                if !p.floating || self.hidden_images.contains(&id) {
                    continue;
                }
                let (w, h) = (emu_px(p.w_emu), emu_px(p.h_emu));
                if w < 1.0 || h < 1.0 {
                    continue;
                }
                let Some(&para) = anchors.get(&id) else { continue };
                let Some((pg, top)) = self.para_page_pos(para) else { continue };
                let (x, y) =
                    place_float(&geom, w, true, &p.h_from, &p.h_align, p.x_emu, &p.v_from, p.y_emu, top);
                hits.push(ImageHit { id, x, y: y + pg as f32 * stride, w, h, page: pg });
            }
        }
        // Header/footer editable pictures: a hit-rect on every page the picture's story paints on, so
        // a header logo (etc.) is click-selectable + resizable like a body picture. The id is
        // region-encoded so the edit routes back to the owning story (see `hf_images` / `image_doc`).
        {
            let emu_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * scale;
            let geom = self.float_geom();
            let content_w = (page_w as f32 - ml - mr).max(1.0);
            let header_top = self.header_y;
            // Each part's band top: headers share the header distance; a footer is bottom-aligned,
            // so its top depends on its OWN height.
            let mut set_tops = Vec::with_capacity(self.hf_sets.len());
            for set in &self.hf_sets {
                set_tops.push(if set.header {
                    header_top
                } else {
                    page_h as f32 - self.footer_dist_px - self.renderer.run_height(&set.blocks, content_w)
                });
            }
            let num_pages = (self.layout.pages.len() as u32).max(1);
            for (enc_id, ctx, p) in self.hf_images() {
                let scriptor_crdt::ImageContext::Hf { part, header } = &ctx else { continue };
                let Some(set_idx) = self.hf_sets.iter().position(|s| &s.part == part) else { continue };
                let (w, h) = (emu_px(p.w_emu), emu_px(p.h_emu));
                if w < 1.0 || h < 1.0 {
                    continue;
                }
                let slot = usize::from(!*header);
                for page in 0..num_pages {
                    // A hit-rect on exactly the pages whose section shows this part.
                    if self.page_hf_at(page)[slot] != Some(set_idx) {
                        continue;
                    }
                    let (mut x, y) = place_float(&geom, w, p.floating, &p.h_from, &p.h_align, p.x_emu, &p.v_from, p.y_emu, set_tops[set_idx]);
                    if !p.floating {
                        x = self.inline_hf_x(set_idx, enc_id, w, &geom, x);
                    }
                    hits.push(ImageHit { id: enc_id, x, y: y + page as f32 * stride, w, h, page });
                }
            }
        }
        self.image_rects = hits;
        self.has_body_fields = self
            .blocks
            .iter()
            .any(|b| b.spans.iter().any(|s| s.text.contains(FIELD_PAGE) || s.text.contains(FIELD_NUMPAGES)));

        // Decode + cache any pictures once (idempotent): the read-only projection (header/footer +
        // floating body) plus the editable `images` map (body inline + inserted-this-session, whose
        // bytes live in `pending_media`). De-dup keys so a shared media part is decoded once.
        let mut parts: Vec<String> = self.doc.images().iter().map(|i| i.part.clone()).collect();
        parts.extend(self.doc.image_placements().into_values().map(|p| p.media));
        parts.sort_unstable();
        parts.dedup();
        for part in &parts {
            if let Some(bytes) = self.doc.image_bytes(part) {
                self.renderer.register_image(part, &bytes);
            }
        }
        Ok(info)
    }

    /// Build the image placements (device px) that fall on page `index`: header/footer pictures on
    /// every page, body pictures on the page their paragraph landed on. Anchors resolve against the
    /// page (`page`), the margin box (`margin`/`column`), or the anchoring paragraph - using either a
    /// `posOffset` or a `wp:align` (left/center/right). The header/footer image anchors to where the
    /// header/footer actually paints (header top, or footer top = page bottom - footerDist - height).
    pub(crate) fn page_images(&mut self, index: u32) -> Vec<scriptor_layout::PageImage> {
        let scale = self.scale_last;
        let emu_px = move |emu: i64| (emu as f32 / 914_400.0) * 96.0 * scale;
        let page_w = self.layout.page_width as f32;
        let page_h = self.layout.page_height as f32;
        let content_w = (page_w - self.ml_px - self.mr_px).max(1.0);

        // The parts THIS page shows (per-section selection + titlePg, resolved in `relayout`). The
        // footer height keys off the page's own footer part (so its images line up with the band).
        let [h_set, f_set] = self.page_hf_at(index);
        let header_top = self.header_y;
        let footer_h = f_set
            .map(|i| self.renderer.run_height(&self.hf_sets[i].blocks, content_w))
            .unwrap_or(0.0);
        let footer_top = page_h - self.footer_dist_px - footer_h;

        let mut out = Vec::new();
        // Header / footer pictures from the editable child stories: each is story-encoded so it
        // hit-tests + edits like a body picture, and resizes/crops/moves render live. A picture
        // paints on this page exactly when the page shows its part. Body pictures - inline AND
        // floating - come from the body editable `images` map below.
        let geom = self.float_geom();
        for (enc_id, ctx, p) in self.hf_images() {
            let scriptor_crdt::ImageContext::Hf { part, header } = &ctx else { continue };
            let set_idx = self.hf_sets.iter().position(|s| &s.part == part);
            if set_idx.is_none() || (if *header { h_set } else { f_set }) != set_idx {
                continue;
            }
            let (w, h) = (emu_px(p.w_emu), emu_px(p.h_emu));
            if w < 1.0 || h < 1.0 {
                continue;
            }
            let anchor_top = if *header { header_top } else { footer_top };
            let (mut x, y) =
                place_float(&geom, w, p.floating, &p.h_from, &p.h_align, p.x_emu, &p.v_from, p.y_emu, anchor_top);
            // An inline header/footer picture follows its paragraph's alignment (the centred-logo
            // footer): place_float pins inline pictures at the content left, so re-place from the
            // anchor paragraph's resolved block alignment.
            if !p.floating {
                x = self.inline_hf_x(set_idx.unwrap(), enc_id, w, &geom, x);
            }
            out.push(scriptor_layout::PageImage {
                key: p.media.clone(),
                x, y, w, h,
                behind: p.behind,
                crop: [p.crop_l, p.crop_t, p.crop_r, p.crop_b], // incl. negative (padding) srcRect
                page: index,
                id: Some(enc_id),
                dim: 0.0, // set per region below
            });
        }

        // Floating body pictures from the editable map (inline ones are appended below from the flow).
        // Each is anchored to the paragraph carrying its placeholder run - that paragraph's placement
        // fixes the page (and, for a paragraph-relative offset, the vertical origin).
        let anchors = self.image_anchor_paras();
        let mut floats: Vec<(u64, scriptor_crdt::ImagePlacement)> =
            self.doc.image_placements().into_iter().filter(|(_, p)| p.floating).collect();
        floats.sort_by_key(|(id, _)| *id); // deterministic stacking order
        for (id, p) in floats {
            // A tracked picture the current display mode hides (deleted in Final, inserted in Original)
            // doesn't paint - same as its inline counterpart vanishing from the flow.
            if self.hidden_images.contains(&id) {
                continue;
            }
            let (w, h) = (emu_px(p.w_emu), emu_px(p.h_emu));
            if w < 1.0 || h < 1.0 {
                continue;
            }
            let Some(&para) = anchors.get(&id) else { continue };
            let Some((pg, top)) = self.para_page_pos(para) else { continue };
            if pg != index {
                continue;
            }
            let (x, y) =
                place_float(&self.float_geom(), w, true, &p.h_from, &p.h_align, p.x_emu, &p.v_from, p.y_emu, top);
            out.push(scriptor_layout::PageImage {
                key: p.media.clone(),
                x, y, w, h,
                behind: p.behind,
                crop: [p.crop_l, p.crop_t, p.crop_r, p.crop_b],
                page: index,
                id: Some(id),
                dim: 0.0, // set per region below
            });
        }
        // Inline body pictures reserved in the flow by `layout_doc` (page-local coords, page-tagged).
        out.extend(self.layout.inline_images.iter().filter(|i| i.page == index).cloned());
        // Dim each picture to match its region's active/inactive state (a header logo greys while the
        // body is active, etc.). The region comes from the picture's story band; body ids use body.
        for im in &mut out {
            let story = im.id.map(img_story).unwrap_or(IMG_BODY);
            im.dim = self.region_dim(self.img_region(story));
        }
        out
    }

    /// The page + page-local top where paragraph `para` was placed: a body paragraph from its block
    /// placement; a table-cell paragraph (cells are NOT in `placements`) from its cell's rect. A
    /// float anchored to a cell paragraph must resolve here - indexing `placements` positionally
    /// landed the NOBA price-table checkboxes on unrelated paragraphs pages away.
    pub(crate) fn para_page_pos(&self, para: usize) -> Option<(u32, f32)> {
        if let Some(pl) = self.layout.placements.iter().find(|q| q.block == para) {
            return Some((pl.page, pl.y));
        }
        self.layout
            .cells
            .iter()
            .find(|c| c.para_ids.contains(&para))
            .map(|c| (c.page, c.y + c.margins[0]))
    }

    /// The page geometry [`place_float`] needs (device px): left / right / top margins, page width,
    /// and the EMU->px scale. Pulled off `self` so the placement math is a pure, testable function.
    pub(crate) fn float_geom(&self) -> FloatGeom {
        FloatGeom { ml: self.ml_px, mr: self.mr_px, mt: self.mt_px, page_w: self.layout.page_width as f32, scale: self.scale_last }
    }

    /// Page-local exclusion rects for floating pictures whose wrap excludes text (square / tight /
    /// through). Resolved against `layout` (pass 1): each float's page + position come from its anchor
    /// paragraph's placement. Drives the paragraph-level square wrap second pass.
    pub(crate) fn wrap_float_rects(&self, layout: &scriptor_layout::DocLayout) -> Vec<FloatRect> {
        let emu_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * self.scale_last;
        let anchors = self.image_anchor_paras();
        let mut out = Vec::new();
        for (id, p) in self.doc.image_placements() {
            if !p.floating || !matches!(p.wrap.as_str(), "square" | "tight" | "through") {
                continue;
            }
            let (w, h) = (emu_px(p.w_emu), emu_px(p.h_emu));
            if w < 1.0 || h < 1.0 {
                continue;
            }
            let Some(&para) = anchors.get(&id) else { continue };
            let Some(pl) = layout.placements.iter().find(|q| q.block == para) else { continue };
            let (x, y) =
                place_float(&self.float_geom(), w, true, &p.h_from, &p.h_align, p.x_emu, &p.v_from, p.y_emu, pl.y);
            out.push(FloatRect { page: pl.page, x0: x, x1: x + w, top: y, bot: y + h, hspace: 0.0, vspace: 0.0 });
        }
        out
    }

    /// Position + measure each text frame (`w:framePr`) off pass-1's page geometry: resolve its box
    /// (`place_frame`), lay its paragraphs out at the frame width to size it, assign it the page of its
    /// anchor body block, and return the painted [`PageFrame`]s plus a wrap rect per frame (so body
    /// text reflows around it). `none`-wrap frames contribute no wrap rect; `notBeside` is approximated
    /// as side-wrap for now.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compute_frames(
        &mut self,
        specs: &[FrameSpec],
        blocks: &[scriptor_layout::Block],
        layout: &scriptor_layout::DocLayout,
        page_w: f32,
        page_h: f32,
        ml: f32,
        mr: f32,
        mt: f32,
        mb: f32,
        scale: f32,
    ) -> (Vec<scriptor_layout::PageFrame>, Vec<FloatRect>) {
        let content_w = (page_w - ml - mr).max(1.0);
        let mut frames = Vec::new();
        let mut rects = Vec::new();
        for spec in specs {
            let g = parse_frame(&spec.raw, scale);
            let fw = g.w.unwrap_or(content_w).clamp(1.0, content_w);
            let mut fblocks: Vec<scriptor_layout::Block> =
                spec.blocks.iter().filter_map(|&i| blocks.get(i).cloned()).collect();
            if fblocks.is_empty() {
                continue;
            }
            // The frame's border box (`w:pBdr`) is drawn once at the frame's full size, not per
            // paragraph - lift it off the blocks so `paint_cell` doesn't also box the text.
            let border = fblocks.iter().find_map(|b| b.borders.clone());
            for b in &mut fblocks {
                b.borders = None;
            }
            let content_h = self.renderer.frame_height(&fblocks, fw);
            let fh = resolve_frame_height(&g.h_rule, g.h, content_h);
            // The frame floats on the page where its anchor body paragraph lands (else page 0); a
            // frame that follows a page-break paragraph lands on the next page. `Placement.y` is
            // page-local (the painter adds `page * stride`), so it is already the right base for a
            // text-anchored frame (`vAnchor="text"`), mirroring how a `v_from="text"` float anchors.
            let anchor_pl = layout.placements.iter().find(|p| p.block == spec.anchor);
            let anchor_page = anchor_pl.map(|p| p.page).unwrap_or(0);
            let page = anchor_page + spec.after_break as u32;
            // When the frame is pushed onto the *next* page by a break, the anchor's flow y is on the
            // page it left, so a text-anchored frame restarts from the top margin of its own page.
            let anchor_y = if spec.after_break { mt } else { anchor_pl.map(|p| p.y).unwrap_or(mt) };
            let (fx, fy) = place_frame(&g, fw, fh, page_w, page_h, ml, mr, mt, mb, anchor_y);
            if g.wrap != "none" {
                // True box + the frame's own wrap distances as clearance, so `square_wrap_indents`
                // reads the frame's real side (a wide `hSpace` no longer makes a right-aligned frame
                // look page-centred) and holds `hSpace`/`vSpace` of gap to the reflowed text.
                rects.push(FloatRect {
                    page,
                    x0: fx,
                    x1: fx + fw,
                    top: fy,
                    bot: fy + fh,
                    hspace: g.h_space,
                    vspace: g.v_space,
                });
            }
            frames.push(scriptor_layout::PageFrame {
                page,
                x: fx,
                y: fy,
                w: fw,
                h: fh,
                blocks: fblocks,
                border,
            });
        }
        (frames, rects)
    }
}
