//! Pictures: placement, geometry and media bytes.
//! 
//! An image is a placeholder run carrying an `img~{id}` mark, with its placement in a
//! document-level map. Covers insertion and removal (tracked or not), resizing,
//! cropping, float and wrap, and garbage-collecting media no run references anymore.

use crate::*;

impl CollabDoc {
    /// Pictures (body + header/footer), with each blip resolved to a media part name. The read-only
    /// render projection (deriving this from the editable `images` map is a pending cleanup).
    pub fn images(&self) -> &[PlacedImage] {
        &self.images
    }

    /// The placement (size / crop / inline-or-floating / position / wrap) of editable picture `id`, or
    /// `None`. The editable image surface (the run carrying `img~{id}` anchors it in the flow).
    pub fn image_placement(&self, id: u64) -> Option<model::ImagePlacement> {
        model::read_image(&self.doc, id)
    }

    /// Every editable picture's id + placement.
    pub fn image_placements(&self) -> std::collections::HashMap<u64, model::ImagePlacement> {
        model::read_images(&self.doc)
    }

    /// Every verbatim-passthrough item's id -> captured `<w:r>...</w:r>` XML (OLE objects, charts,
    /// shapes). The renderer sniffs a label from this to paint a placeholder box where the object sits.
    pub fn passthrough_xml(&self) -> std::collections::HashMap<u64, String> {
        model::read_raw(&self.doc)
    }

    /// The raw bytes of a media part (e.g. `word/media/image1.png`), for decode + composite. An
    /// imported picture's bytes live in `source_parts`; an inserted picture's come from
    /// [`inserted_media_bytes`](Self::inserted_media_bytes).
    pub fn image_bytes(&self, part: &str) -> Option<Vec<u8>> {
        self.source_parts
            .iter()
            .find(|p| p.name == part)
            .map(|p| p.data.clone())
            .or_else(|| self.inserted_media_bytes(part))
    }

    /// Bytes for an inserted picture's media part: the in-session buffer (`pending_media`) first, then
    /// the CRDT-persisted copy (the `media` map), which survives a reopen-from-op-log once the
    /// session-local buffer is gone.
    pub(crate) fn inserted_media_bytes(&self, part: &str) -> Option<Vec<u8>> {
        self.pending_media
            .borrow()
            .get(part)
            .cloned()
            .or_else(|| model::read_media(&self.doc, part))
    }

    /// Insert a picture at codepoint `off` in paragraph `para`: its `bytes` (of MIME type `mime`, e.g.
    /// `image/png`) ship as a fresh `word/media` part on save, displayed at `w_emu` x `h_emu` (EMU). A
    /// placeholder run carries the `img~{id}` anchor; the placement lands in the `images` map. Returns
    /// the new picture id. `audit` is the synced commit message.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_image(
        &self,
        para: usize,
        off: usize,
        bytes: Vec<u8>,
        mime: &str,
        w_emu: i64,
        h_emu: i64,
        audit: &str,
    ) -> Result<u64> {
        let id = self.all_image_mark_ids().into_iter().max().map(|m| m + 1).unwrap_or(0);
        let media = self.fresh_media_key(ext_for_mime(mime));
        // Persist the bytes in the CRDT (so they survive snapshot + reopen) and keep the in-session
        // buffer for this peer. `bytes` moves into the buffer, so write loro first.
        model::write_media(&self.doc, &media, &bytes)?;
        self.pending_media.borrow_mut().insert(media.clone(), bytes);
        let placement = model::ImagePlacement {
            media,
            w_emu: w_emu.max(1),
            h_emu: h_emu.max(1),
            ..Default::default()
        };
        model::write_image(&self.doc, id, &placement)?;
        // The new id must be a configured mark key before we mark the placeholder (config replaces the
        // whole style map, so reconfigure from the now-updated images map).
        self.reconfigure_comment_marks();
        model::insert_text(&self.doc, para, off, &model::IMAGE_PLACEHOLDER.to_string())?;
        model::mark_image_range(&self.doc, id, para, off, off + 1)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Resize picture `id` to `w_emu` x `h_emu` (EMU). Returns whether it existed. Caller re-layouts.
    pub fn set_image_size(&self, id: u64, w_emu: i64, h_emu: i64, audit: &str) -> Result<bool> {
        let Some(mut p) = model::read_image(&self.doc, id) else { return Ok(false) };
        p.w_emu = w_emu.max(1);
        p.h_emu = h_emu.max(1);
        model::write_image(&self.doc, id, &p)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Set picture `id`'s crop (`<a:srcRect>`, thousandths of a percent, l/t/r/b). Returns whether it
    /// existed. Caller re-layouts + re-paints.
    pub fn set_image_crop(&self, id: u64, l: i64, t: i64, r: i64, b: i64, audit: &str) -> Result<bool> {
        let Some(mut p) = model::read_image(&self.doc, id) else { return Ok(false) };
        p.crop_l = l;
        p.crop_t = t;
        p.crop_r = r;
        p.crop_b = b;
        model::write_image(&self.doc, id, &p)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Reset picture `id`'s crop: clear `<a:srcRect>` and grow the display extent back to the
    /// uncropped size, so the whole image reappears at the same on-screen scale (no distortion) -
    /// Word's "Reset Crop". Returns whether it existed (and was actually cropped). Caller re-layouts.
    pub fn reset_image_crop(&self, id: u64, audit: &str) -> Result<bool> {
        let Some(mut p) = model::read_image(&self.doc, id) else { return Ok(false) };
        if p.crop_l == 0 && p.crop_t == 0 && p.crop_r == 0 && p.crop_b == 0 {
            return Ok(false); // nothing cropped
        }
        // The shown extent covers visible_frac of the original; divide it back out to recover the full
        // size (thousandths: visible = 100000 - cut).
        let vis_w = (100_000 - p.crop_l - p.crop_r).max(1);
        let vis_h = (100_000 - p.crop_t - p.crop_b).max(1);
        p.w_emu = (p.w_emu * 100_000 / vis_w).max(1);
        p.h_emu = (p.h_emu * 100_000 / vis_h).max(1);
        p.crop_l = 0;
        p.crop_t = 0;
        p.crop_r = 0;
        p.crop_b = 0;
        model::write_image(&self.doc, id, &p)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Make picture `id` floating (`anchor = true`, positioned + wrapped) or inline. `wrap` is the wrap
    /// type (`square` / `tight` / `topAndBottom` / `through` / `none`); `behind` paints under the text.
    /// Returns whether it existed.
    pub fn set_image_floating(
        &self,
        id: u64,
        floating: bool,
        wrap: &str,
        behind: bool,
        audit: &str,
    ) -> Result<bool> {
        let Some(mut p) = model::read_image(&self.doc, id) else { return Ok(false) };
        p.floating = floating;
        p.wrap = if floating { wrap.to_string() } else { String::new() };
        p.behind = floating && behind;
        model::write_image(&self.doc, id, &p)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Position floating picture `id`: set its `wp:positionH/V` origin (`h_from`/`v_from`
    /// `relativeFrom`, e.g. `column`/`page`/`margin`) and `wp:posOffset` (`x_emu`/`y_emu`, EMU).
    /// Clears any `wp:align` (an explicit offset overrides alignment, matching Word's drag-to-move).
    /// No-op on an inline picture (position is meaningless until it floats). Returns whether it
    /// existed *and* was floating. Caller re-layouts.
    pub fn set_image_position(
        &self,
        id: u64,
        h_from: &str,
        x_emu: i64,
        v_from: &str,
        y_emu: i64,
        audit: &str,
    ) -> Result<bool> {
        let Some(mut p) = model::read_image(&self.doc, id) else { return Ok(false) };
        if !p.floating {
            return Ok(false);
        }
        p.h_from = h_from.to_string();
        p.v_from = v_from.to_string();
        p.x_emu = x_emu;
        p.y_emu = y_emu;
        p.h_align = String::new();
        p.v_align = String::new();
        model::write_image(&self.doc, id, &p)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Remove picture `id`: delete its placeholder run + its placement. Returns whether it existed.
    pub fn remove_image(&self, id: u64, audit: &str) -> Result<bool> {
        if model::read_image(&self.doc, id).is_none() {
            return Ok(false);
        }
        // Locate the placeholder run (the one carrying `img~{id}`) and delete that single char.
        let mut at: Option<(usize, usize)> = None;
        for (pi, p) in self.paragraphs()?.iter().enumerate() {
            let mut off = 0usize;
            for run in &p.runs {
                if run.image == Some(id) {
                    at = Some((pi, off));
                    break;
                }
                off += run.text.chars().count();
            }
            if at.is_some() {
                break;
            }
        }
        if let Some((para, off)) = at {
            model::delete_text(&self.doc, para, off..off + 1)?;
        }
        model::delete_image(&self.doc, id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Suggest inserting a picture as a tracked change (`w:ins`): same as [`insert_image`](Self::insert_image)
    /// but the placeholder run is marked a tracked insertion attributed to `author`/`date`. Accepting
    /// keeps the picture; rejecting removes its run (then [`gc_orphan_images`](Self::gc_orphan_images)
    /// drops the now-anchorless placement). Returns the new picture id.
    #[allow(clippy::too_many_arguments)]
    pub fn suggest_insert_image(
        &self,
        para: usize,
        off: usize,
        bytes: Vec<u8>,
        mime: &str,
        w_emu: i64,
        h_emu: i64,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.all_image_mark_ids().into_iter().max().map(|m| m + 1).unwrap_or(0);
        let media = self.fresh_media_key(ext_for_mime(mime));
        // Persist the bytes in the CRDT (so they survive snapshot + reopen) and keep the in-session
        // buffer for this peer. `bytes` moves into the buffer, so write loro first.
        model::write_media(&self.doc, &media, &bytes)?;
        self.pending_media.borrow_mut().insert(media.clone(), bytes);
        let placement = model::ImagePlacement {
            media,
            w_emu: w_emu.max(1),
            h_emu: h_emu.max(1),
            ..Default::default()
        };
        model::write_image(&self.doc, id, &placement)?;
        self.reconfigure_comment_marks();
        let rid = self.next_revision_id()?;
        let track = Track { kind: TrackKind::Ins, author: author.into(), date: date.into(), id: rid };
        model::suggest_insertion(&self.doc, para, off, &model::IMAGE_PLACEHOLDER.to_string(), &track)?;
        model::mark_image_range(&self.doc, id, para, off, off + 1)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Suggest removing picture `id` as a tracked change (`w:del`): the placeholder run is marked a
    /// tracked deletion (text retained) rather than deleted, and the placement is kept until the change
    /// resolves. Accepting removes the run (then [`gc_orphan_images`](Self::gc_orphan_images) drops the
    /// placement); rejecting restores the picture. Returns whether it existed.
    pub fn suggest_remove_image(&self, id: u64, author: &str, date: &str, audit: &str) -> Result<bool> {
        if model::read_image(&self.doc, id).is_none() {
            return Ok(false);
        }
        let mut at: Option<(usize, usize)> = None;
        for (pi, p) in self.paragraphs()?.iter().enumerate() {
            let mut off = 0usize;
            for run in &p.runs {
                if run.image == Some(id) {
                    at = Some((pi, off));
                    break;
                }
                off += run.text.chars().count();
            }
            if at.is_some() {
                break;
            }
        }
        if let Some((para, off)) = at {
            let rid = self.next_revision_id()?;
            let track = Track { kind: TrackKind::Del, author: author.into(), date: date.into(), id: rid };
            model::suggest_deletion(&self.doc, para, off..off + 1, &track)?;
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Drop every picture placement whose `img~{id}` anchor run no longer exists - e.g. after a tracked
    /// insertion was rejected or a tracked deletion accepted (the run is gone, but the `images` map
    /// entry lingers). Returns how many were removed. Call after accept/reject so the map stays tidy.
    pub fn gc_orphan_images(&self) -> Result<usize> {
        let paras = self.paragraphs()?;
        self.gc_orphan_images_against(&paras)
    }

    /// [`Self::gc_orphan_images`] against an already-read paragraph list. The full CRDT paragraph
    /// read is the dominant cost of a relayout; the caller has just paid for it, and re-reading it
    /// here doubled that cost on every keystroke of an image-carrying document.
    pub fn gc_orphan_images_against(&self, paras: &[Paragraph]) -> Result<usize> {
        let mut anchored = std::collections::HashSet::new();
        for p in paras {
            for r in &p.runs {
                if let Some(id) = r.image {
                    anchored.insert(id);
                }
            }
        }
        let mut dropped = 0;
        for id in model::read_images(&self.doc).keys() {
            if !anchored.contains(id) {
                model::delete_image(&self.doc, *id)?;
                dropped += 1;
            }
        }
        if dropped > 0 {
            self.doc.set_next_commit_message("gc orphan images");
            self.doc.commit();
        }
        Ok(dropped)
    }

    /// A `word/media/image{N}.{ext}` key not already used by a source part, a pending insert, or another
    /// image's placement.
    fn fresh_media_key(&self, ext: &str) -> String {
        let used: std::collections::HashSet<String> = self
            .source_parts
            .iter()
            .map(|p| p.name.clone())
            .chain(self.pending_media.borrow().keys().cloned())
            .chain(model::read_all_media(&self.doc).into_keys())
            .chain(model::read_images(&self.doc).into_values().map(|p| p.media))
            .collect();
        let mut n = used.len() + 1;
        loop {
            let key = format!("word/media/image{n}.{ext}");
            if !used.contains(&key) {
                return key;
            }
            n += 1;
        }
    }

    /// The picture ids in the `images` map (keep `img~{id}` keys configured).
    pub(crate) fn all_image_mark_ids(&self) -> Vec<u64> {
        model::read_images(&self.doc).keys().copied().collect()
    }
}
