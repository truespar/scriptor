//! Recording edits as tracked changes.
//! 
//! The same operations as `edit`, but attributed and reversible: each writes Peritext
//! marks describing what would change rather than changing it, so a reviewer accepts
//! or rejects afterwards. Bulk mode shares one revision id across a batch.

use crate::*;

impl CollabDoc {
    /// The next free tracked-change id (max existing + 1). Revision ids share OOXML's
    /// bookmark/comment id space - see [`model::max_revision_id`]. Table-structure changes live in the
    /// in-memory body (not the loro op log), so their ids are folded in here too.
    pub fn next_revision_id(&self) -> Result<u64> {
        // In a bulk-emission batch, hand out ids from the seeded counter (no whole-document rescan).
        if let Some(n) = self.rev_counter.get() {
            self.rev_counter.set(Some(n + 1));
            return Ok(n);
        }
        let doc_max = model::max_revision_id(&self.doc)?;
        let table_max = model::table_change_ids(&self.body()).into_iter().max().unwrap_or(0);
        Ok(doc_max.max(table_max) + 1)
    }

    /// Begin a **bulk-emission batch**: a synchronous burst of many `suggest_*` ops on this one
    /// document (document comparison replays the A->B edit script this way). Two per-op O(N) rescans -
    /// resolving a flat paragraph index to its container ([`model::block_seq`]) and allocating a
    /// revision id ([`Self::next_revision_id`]) - are quadratic across a whole document, so a batch
    /// memoizes the block sequence and hands out ids from a seeded counter. **Must** be paired with
    /// [`Self::end_bulk`] (use a guard so it runs even on error), and used only for a single-threaded
    /// burst that no other document interleaves on this thread. A no-op-safe double `begin` re-seeds.
    pub fn begin_bulk(&self) -> Result<()> {
        let start = self.next_revision_id()?; // one scan up front (batch not yet active)
        self.rev_counter.set(Some(start));
        model::block_cache_begin();
        Ok(())
    }

    /// End the batch opened by [`Self::begin_bulk`]: drop the block-sequence memo and stop the counter
    /// (subsequent ids are rescanned from the document again). Idempotent.
    pub fn end_bulk(&self) {
        self.rev_counter.set(None);
        model::block_cache_end();
    }

    /// Suggest an insertion: insert `text` at codepoint `pos` in paragraph `para`, marked as a
    /// tracked insertion attributed to `author`/`date`. `audit` becomes the loro commit message,
    /// which is persisted and replicates to every peer (the audit-as-primitive layer). Returns the
    /// allocated revision id.
    pub fn suggest_insertion(
        &self,
        para: usize,
        pos: usize,
        text: &str,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        let track = Track { kind: TrackKind::Ins, author: author.into(), date: date.into(), id };
        model::suggest_insertion(&self.doc, para, pos, text, &track)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Suggest a tracked deletion spanning paragraphs - `(start_para, start_off)..(end_para, end_off)`
    /// in document order - under **one** revision id. Every spanned run slice is marked deleted (text
    /// retained) and the paragraph marks *between* the spanned paragraphs are marked deleted ¶s, all
    /// sharing the id. The effect: accepting it removes the text and merges the paragraphs into one
    /// (`text[..start_off] + text[end_off..]`, the start paragraph's properties surviving); rejecting it
    /// restores every paragraph intact. A single-paragraph range delegates to the run-only path. Refuses
    /// a range that crosses a table-cell boundary (the ¶-merge would tear the grid). Returns the id.
    ///
    /// This is the engine half of the agent's multi-paragraph redline (audit H1): the single most
    /// common real review action a single-paragraph delete couldn't express.
    #[allow(clippy::too_many_arguments)]
    pub fn suggest_deletion_multi(
        &self,
        start_para: usize,
        start_off: usize,
        end_para: usize,
        end_off: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        let track = Track { kind: TrackKind::Del, author: author.into(), date: date.into(), id };
        self.mark_span(&track, start_para, start_off, end_para, end_off)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Mark the span `(start_para, start_off)..(end_para, end_off)` with `track` - the shared body of a
    /// tracked deletion and a move's source (`w:del` vs `w:moveFrom`): every spanned run slice gets the
    /// mark (text retained), and the paragraph marks *between* the spanned paragraphs are marked so that
    /// accepting a deletion / moveFrom merges them into one. Refuses a range that crosses a table-cell
    /// boundary. Does not commit.
    fn mark_span(
        &self,
        track: &Track,
        start_para: usize,
        start_off: usize,
        end_para: usize,
        end_off: usize,
    ) -> Result<()> {
        if start_para == end_para {
            model::suggest_deletion(&self.doc, start_para, start_off..end_off, track)?;
            return Ok(());
        }
        // Every spanned paragraph must share a container, else the ¶-merge on accept would join
        // across a table-cell boundary and tear the grid.
        for p in (start_para + 1)..=end_para {
            if !self.same_container(start_para, p) {
                return Err(anyhow::anyhow!("multi-paragraph revision cannot cross a table-cell boundary"));
            }
        }
        let paras = self.paragraphs()?;
        let len = |p: usize| -> usize {
            paras.get(p).map(|x| x.runs.iter().map(|r| r.text.chars().count()).sum()).unwrap_or(0)
        };
        // The start paragraph's tail, every intermediate paragraph whole, the end paragraph's head.
        let sl = len(start_para);
        if start_off < sl {
            model::suggest_deletion(&self.doc, start_para, start_off..sl, track)?;
        }
        for p in (start_para + 1)..end_para {
            let l = len(p);
            if l > 0 {
                model::suggest_deletion(&self.doc, p, 0..l, track)?;
            }
        }
        if end_off > 0 {
            model::suggest_deletion(&self.doc, end_para, 0..end_off, track)?;
        }
        // Mark the ¶ of every paragraph except the last so accepting merges them into one.
        for p in start_para..end_para {
            model::set_para_mark(&self.doc, p, track)?;
        }
        Ok(())
    }

    /// Suggest the **source** half of a whole-paragraph move: mark the span
    /// `(start_para, start_off)..(end_para, end_off)` as `w:moveFrom` (runs + the paragraph marks
    /// between them, text retained) so accepting the move removes and merges the source away, and
    /// rejecting restores it. Returns the allocated revision id - pass it to
    /// [`suggest_move_dest`](Self::suggest_move_dest) / [`suggest_move_split`](Self::suggest_move_split)
    /// so both halves share one id and resolve together. Caller commits (this does).
    #[allow(clippy::too_many_arguments)]
    pub fn suggest_move_span(
        &self,
        start_para: usize,
        start_off: usize,
        end_para: usize,
        end_off: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        let track = Track { kind: TrackKind::MoveFrom, author: author.into(), date: date.into(), id };
        self.mark_span(&track, start_para, start_off, end_para, end_off)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Split paragraph `para` at `pos` and mark the new boundary as a `w:moveTo` ¶ under move `id` -
    /// the destination-side counterpart of [`suggest_split`](Self::suggest_split) for a move. Pairs
    /// with [`suggest_move_dest`](Self::suggest_move_dest) (the moved text) so the destination
    /// paragraph appears / disappears with the rest of move `id`.
    pub fn suggest_move_split(
        &self,
        para: usize,
        pos: usize,
        id: u64,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<()> {
        model::split_paragraph(&self.doc, para, pos)?;
        model::set_para_mark(
            &self.doc,
            para,
            &Track { kind: TrackKind::MoveTo, author: author.into(), date: date.into(), id },
        )?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Split paragraph `para` at `pos` as a tracked change (a tracked Enter): the split is applied
    /// (visible as two paragraphs) and the *first* paragraph's ending mark is recorded as an inserted
    /// ¶ revision attributed to `author`/`date`. Returns the allocated revision id.
    pub fn suggest_split(
        &self,
        para: usize,
        pos: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        model::split_paragraph(&self.doc, para, pos)?;
        let id = self.next_revision_id()?;
        model::set_para_mark(
            &self.doc,
            para,
            &Track { kind: TrackKind::Ins, author: author.into(), date: date.into(), id },
        )?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Join paragraph `para` into the previous one as a tracked change (a tracked Backspace): rather
    /// than merging, the previous paragraph's ending mark is recorded as a deleted ¶ revision - the
    /// two paragraphs stay separate until accepted. Returns `Some(caret)` (the previous paragraph's
    /// length, so the caret lands at its end) or `None` when the join would cross a container.
    pub fn suggest_join(
        &self,
        para: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<Option<usize>> {
        if para == 0 || !self.same_container(para - 1, para) {
            return Ok(None);
        }
        let paras = self.paragraphs()?;
        let prev = paras.get(para - 1);
        let prev_len: usize =
            prev.map(|p| p.runs.iter().map(|r| r.text.chars().count()).sum()).unwrap_or(0);
        // Idempotent: if the previous mark is already a pending deletion, just move the caret.
        let already_del = prev
            .and_then(|p| p.mark_change.as_ref())
            .is_some_and(|m| m.kind == TrackKind::Del);
        if !already_del {
            let id = self.next_revision_id()?;
            model::set_para_mark(
                &self.doc,
                para - 1,
                &Track { kind: TrackKind::Del, author: author.into(), date: date.into(), id },
            )?;
            self.doc.set_next_commit_message(audit);
            self.doc.commit();
        }
        Ok(Some(prev_len))
    }

    /// Suggest a paragraph-property change: apply `props` to paragraph `para` as a tracked
    /// `w:pPrChange` (the paragraph keeps the new props; the old style + props are recorded for
    /// reject) attributed to `author`/`date`. Returns the allocated revision id.
    pub fn suggest_paragraph_format(
        &self,
        para: usize,
        props: &ParaProps,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        model::suggest_paragraph_format(&self.doc, para, props, author, date, id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Suggest a deletion: mark codepoint `range` in paragraph `para` as a tracked deletion (text
    /// retained, so it can be rejected) attributed to `author`/`date`. `audit` becomes the synced
    /// commit message. Returns the allocated revision id.
    pub fn suggest_deletion(
        &self,
        para: usize,
        range: std::ops::Range<usize>,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        let track = Track { kind: TrackKind::Del, author: author.into(), date: date.into(), id };
        model::suggest_deletion(&self.doc, para, range, &track)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Suggest a run-property change: apply `fmt` over codepoint `range` in paragraph `para` as a
    /// tracked `w:rPrChange` (the run keeps the new formatting; the old props are recorded for reject)
    /// attributed to `author`/`date`. Returns the allocated revision id.
    pub fn suggest_format(
        &self,
        para: usize,
        range: std::ops::Range<usize>,
        fmt: &RunFormat,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        model::suggest_format(&self.doc, para, range, fmt, author, date, id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Suggest a move: mark codepoint `from_range` in paragraph `from_para` as the source half
    /// (`w:moveFrom`) and insert a formatting-preserving copy at codepoint `to_pos` in paragraph
    /// `to_para` as the destination half (`w:moveTo`), both attributed to `author`/`date` and sharing
    /// one revision id (so accept/reject resolves the whole move). Returns the allocated id.
    #[allow(clippy::too_many_arguments)]
    pub fn suggest_move(
        &self,
        from_para: usize,
        from_range: std::ops::Range<usize>,
        to_para: usize,
        to_pos: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        model::suggest_move(&self.doc, from_para, from_range, to_para, to_pos, author, date, id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Suggest a **multi-paragraph** move: source `(start_para,start_off)..(end_para,end_off)` to
    /// `(to_para, to_pos)`, under one revision id (`w:moveFrom` on the source + the ¶s between it;
    /// `w:moveTo` content rebuilt at the destination with its internal ¶s). Accepting performs the move
    /// (source removed + merged, destination kept); rejecting restores the source + removes the
    /// destination. The destination must lie outside the source span, and the whole move must be
    /// top-level (moves touching a table cell are refused in v1). Returns the allocated id.
    #[allow(clippy::too_many_arguments)]
    pub fn suggest_move_multi(
        &self,
        start_para: usize,
        start_off: usize,
        end_para: usize,
        end_off: usize,
        to_para: usize,
        to_pos: usize,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        // The destination must not fall within the (closed) source span, in document order.
        if (start_para, start_off) <= (to_para, to_pos) && (to_para, to_pos) <= (end_para, end_off) {
            return Err(anyhow::anyhow!("cannot move a range into itself"));
        }
        // Top-level only: a move touching a table cell isn't supported yet (the ¶-merge would tear the
        // grid, and the in-memory body would desync).
        for p in start_para..=end_para {
            if self.table_context(p).is_some() {
                return Err(anyhow::anyhow!("multi-paragraph move from inside a table is not supported"));
            }
        }
        if self.table_context(to_para).is_some() {
            return Err(anyhow::anyhow!("multi-paragraph move into a table is not supported"));
        }
        let id = self.next_revision_id()?;
        let new_paras = model::suggest_move_multi(
            &self.doc, start_para, start_off, end_para, end_off, to_para, to_pos, author, date, id,
        )?;
        let _ = new_paras; // body is derived from the grid now; no per-split bookkeeping needed
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Mark codepoint `range` in paragraph `para` as the **source** half of a move (`w:moveFrom`, text
    /// retained) attributed to `author`/`date`; returns the allocated revision id. The destination is
    /// added later via [`suggest_move_dest`](Self::suggest_move_dest) with this id - the two-step path
    /// the editor uses for a cut-then-paste move.
    pub fn suggest_move_source(
        &self,
        para: usize,
        range: std::ops::Range<usize>,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        let track = Track { kind: TrackKind::MoveFrom, author: author.into(), date: date.into(), id };
        model::suggest_deletion(&self.doc, para, range, &track)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Insert `text` at codepoint `pos` in paragraph `para` as the **destination** half of move `id`
    /// (`w:moveTo`), attributed to `author`/`date`. Pairs with a prior
    /// [`suggest_move_source`](Self::suggest_move_source).
    #[allow(clippy::too_many_arguments)]
    pub fn suggest_move_dest(
        &self,
        para: usize,
        pos: usize,
        text: &str,
        id: u64,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<()> {
        let track = Track { kind: TrackKind::MoveTo, author: author.into(), date: date.into(), id };
        model::suggest_insertion(&self.doc, para, pos, text, &track)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }
}
