//! Proposing tracked changes.
//! 
//! Every mutation an agent can make, each recorded as a suggestion attributed to it
//! rather than an edit in place, so a human accepts or rejects it afterwards. The
//! batch form applies a whole `Proposal` atomically against a base revision.

use crate::*;

impl AgentPeer {
    /// Seed an empty document with a paragraph (used to start a document the agent authors).
    pub fn append_paragraph(&self, runs: &[Run], style: Option<&str>) -> Result<()> {
        self.doc.append_paragraph(runs, style)
    }

    /// Propose inserting `text` at codepoint `pos` in paragraph `para` as a tracked change
    /// attributed to this agent. `date` is an ISO-8601 timestamp (the caller supplies it - the
    /// agent does not invent time); `rationale` is recorded in the replicated audit/commit
    /// metadata. Returns the allocated revision id.
    pub fn propose_insertion(
        &self,
        para: usize,
        pos: usize,
        text: &str,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        self.guard(AgentAction::Insert, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::InsertText { para, pos, text: text.to_string() };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose deleting codepoint `range` in paragraph `para` as a tracked change (text retained,
    /// so a human can reject it). Returns the allocated revision id.
    pub fn propose_deletion(
        &self,
        para: usize,
        range: std::ops::Range<usize>,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        self.guard(AgentAction::Delete, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::DeleteRange { para, range: range.clone() };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose inserting `text` at `at` as a tracked insertion. Errors if the anchor is stale.
    pub fn propose_insert(&self, at: &Anchor, text: &str, date: &str, rationale: &str) -> Result<u64> {
        let (para, pos) = self.point(at)?;
        self.guard(AgentAction::Insert, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::InsertText { para, pos, text: text.to_string() };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose deleting the text under `range` as a tracked deletion (text retained for rejection).
    /// The range **may span paragraphs**: a cross-paragraph deletion lands under one revision id (every
    /// spanned slice + the ¶ marks between), so a single accept removes the text and merges the
    /// paragraphs, a single reject restores them.
    pub fn propose_delete(&self, range: &AnchorRange, date: &str, rationale: &str) -> Result<u64> {
        let (sp, so, ep, eo) = self.multi_span(range)?;
        self.guard(AgentAction::Delete, self.doc.node_id(sp), Some(rationale), || {
            if sp == ep {
                let op = EditOp::DeleteRange { para: sp, range: so..eo };
                Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
            } else {
                self.doc.suggest_deletion_multi(sp, so, ep, eo, &self.author, date, &self.audit("delete", rationale))
            }
        })
    }

    /// Propose replacing the text under `range` with `new_text` - a tracked deletion of the old plus a
    /// tracked insertion of the new (Word's redline for an edit). The range **may span paragraphs** (the
    /// deletion merges them on accept; the new text is inserted at the range start, so accepting yields
    /// `text[..start] + new_text + text[end..]`). Returns `(deletion_id, insertion_id)`.
    pub fn propose_replace(
        &self,
        range: &AnchorRange,
        new_text: &str,
        date: &str,
        rationale: &str,
    ) -> Result<(u64, u64)> {
        let (sp, so, ep, eo) = self.multi_span(range)?;
        self.guard(AgentAction::Replace, self.doc.node_id(sp), Some(rationale), || {
            let del = if sp == ep {
                apply(&self.doc, &self.ctx(date, rationale), EditOp::DeleteRange { para: sp, range: so..eo })?
                    .revision_id
                    .unwrap_or(0)
            } else {
                self.doc.suggest_deletion_multi(sp, so, ep, eo, &self.author, date, &self.audit("delete", rationale))?
            };
            let ins = apply(
                &self.doc,
                &self.ctx(date, rationale),
                EditOp::InsertText { para: sp, pos: so, text: new_text.to_string() },
            )?
            .revision_id
            .unwrap_or(0);
            Ok((del, ins))
        })
    }

    /// Propose run formatting (bold / size / colour / …) over `range` as a tracked `w:rPrChange`.
    pub fn propose_format(
        &self,
        range: &AnchorRange,
        format: RunFormat,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        let (para, s, e) = self.span(range)?;
        self.guard(AgentAction::Format, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::ApplyRunFormat { para, range: s..e, format };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose paragraph-level formatting (alignment / spacing / …) on `at`'s paragraph as a tracked
    /// `w:pPrChange`.
    pub fn propose_paragraph_format(
        &self,
        at: &Anchor,
        props: ParaProps,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::ParagraphFormat, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::ApplyParagraphFormat { para, props };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose setting (or clearing, `None` -> Normal) the named paragraph style on `at`'s paragraph.
    pub fn propose_style(
        &self,
        at: &Anchor,
        style: Option<String>,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::Style, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::SetParagraphStyle { para, style };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose setting (or clearing, `None`) the list numbering on `at`'s paragraph.
    pub fn propose_numbering(
        &self,
        at: &Anchor,
        num_id: Option<i32>,
        ilvl: Option<i32>,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::Numbering, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::SetNumbering { para, num_id, ilvl };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose splitting `at`'s paragraph at the anchor (a tracked inserted paragraph mark).
    pub fn propose_split(&self, at: &Anchor, date: &str, rationale: &str) -> Result<u64> {
        let (para, pos) = self.point(at)?;
        self.guard(AgentAction::Split, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::SplitParagraph { para, pos };
            Ok(apply(&self.doc, &self.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose joining `at`'s paragraph into the previous one (a tracked deleted paragraph mark).
    /// Returns the merge-point caret, or `None` if the join was refused (crosses a table-cell boundary).
    pub fn propose_join(&self, at: &Anchor, date: &str, rationale: &str) -> Result<Option<usize>> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::Join, self.doc.node_id(para), Some(rationale), || {
            Ok(apply(&self.doc, &self.ctx(date, rationale), EditOp::JoinParagraph { para })?.caret)
        })
    }

    /// Propose moving the text under `from` to `to` as a tracked move (`w:moveFrom`/`w:moveTo`, one
    /// revision id so accept/reject resolves the whole move). The agent's redline-move primitive.
    pub fn propose_move(
        &self,
        from: &AnchorRange,
        to: &Anchor,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        let (fsp, fso, fep, feo) = self.multi_span(from)?;
        let (tp, to_pos) = self.point(to)?;
        self.guard(AgentAction::Move, self.doc.node_id(fsp), Some(rationale), || {
            if fsp == fep {
                self.doc.suggest_move(fsp, fso..feo, tp, to_pos, &self.author, date, &self.audit("move", rationale))
            } else {
                self.doc.suggest_move_multi(fsp, fso, fep, feo, tp, to_pos, &self.author, date, &self.audit("move", rationale))
            }
        })
    }

    /// Submit a [`Proposal`] - a set of ops applied as one unit against the revision the agent read:
    ///
    /// 1. **Optimistic concurrency** - if the document moved since `base_revision` (a concurrent
    ///    edit), nothing applies and [`ProposalResult::Stale`] is returned with the current revision.
    /// 2. **Validate-first / all-or-nothing** - every op's anchors must resolve before any op applies;
    ///    a bad op aborts the whole batch with [`ProposalResult::Invalid`] and leaves the document
    ///    untouched.
    /// 3. **Apply** - ops land as attributed tracked suggestions, the proposal `title` as their
    ///    rationale. Anchors are edit-stable, so each op re-resolves correctly even as earlier ops in
    ///    the batch shift offsets; the loro merge-interval coalesces them into one undo group.
    ///
    /// `date` is the ISO-8601 timestamp for the batch (the agent supplies it).
    pub fn submit_proposal(&self, proposal: &Proposal, date: &str) -> Result<ProposalResult> {
        let current = self.revision();
        if current != proposal.base_revision {
            return Ok(ProposalResult::Stale { current });
        }
        // Validate-first: anchors must resolve AND policy must allow every op, before any applies.
        for (index, op) in proposal.ops.iter().enumerate() {
            if let Err(reason) = self.validate_op(op) {
                return Ok(ProposalResult::Invalid { index, reason });
            }
            if let Err(reason) = self.check(proposal_op_action(op), self.proposal_op_node(op)) {
                return Ok(ProposalResult::Invalid { index, reason });
            }
        }
        // Trial pass (true all-or-nothing): some ops can pass validation yet still fail at apply time
        // (a multi-paragraph delete that crosses a table-cell boundary, a move into its own range). Apply
        // the whole batch to a snapshot-isolated fork first; if any op fails there, abort with Invalid
        // and leave the real document untouched. The fork shares history, so the proposal's anchors
        // resolve identically.
        let trial = self.trial()?;
        for (index, op) in proposal.ops.iter().enumerate() {
            if let Err(e) = trial.apply_op(op, date, &proposal.title) {
                return Ok(ProposalResult::Invalid { index, reason: format!("apply failed: {e}") });
            }
        }
        // Commit: the batch is proven to apply cleanly, so the real apply cannot fail mid-batch.
        let mut change_ids = Vec::new();
        for op in &proposal.ops {
            change_ids.extend(self.apply_op(op, date, &proposal.title)?);
        }
        self.notify(AgentAction::SubmitProposal, None, Some(&proposal.title));
        Ok(ProposalResult::Applied { revision: self.revision(), change_ids })
    }

    /// The node a proposal op targets (for the content-aware policy check), best-effort.
    fn proposal_op_node(&self, op: &ProposalOp) -> Option<NodeId> {
        let para = match op {
            ProposalOp::Insert { at, .. }
            | ProposalOp::ParagraphFormat { at, .. }
            | ProposalOp::Style { at, .. }
            | ProposalOp::Numbering { at, .. }
            | ProposalOp::Split { at }
            | ProposalOp::Join { at } => self.point(at).ok().map(|(p, _)| p),
            ProposalOp::Format { range, .. } => self.span(range).ok().map(|(p, _, _)| p),
            ProposalOp::Delete { range }
            | ProposalOp::Replace { range, .. }
            | ProposalOp::Comment { range, .. } => self.multi_span(range).ok().map(|(p, ..)| p),
            ProposalOp::Move { from, .. } => self.multi_span(from).ok().map(|(p, ..)| p),
        }?;
        self.doc.node_id(para)
    }

    /// Check that an op's anchors resolve against the current document (the validate-first pass).
    fn validate_op(&self, op: &ProposalOp) -> std::result::Result<(), String> {
        let point = |a: &Anchor| match self.doc.resolve(a) {
            Resolved::Live { .. } => Ok(()),
            Resolved::Shifted { .. } | Resolved::Deleted => {
                Err("anchor is stale (its content was deleted or moved)".to_string())
            }
        };
        let span = |r: &AnchorRange| {
            self.doc
                .resolve_range(r)
                .map(|_| ())
                .ok_or_else(|| "anchor range is stale or torn across paragraphs".to_string())
        };
        let multi_span = |r: &AnchorRange| {
            self.doc
                .resolve_range_multi(r)
                .map(|_| ())
                .ok_or_else(|| "anchor range is stale (an end's content was deleted or moved)".to_string())
        };
        match op {
            ProposalOp::Insert { at, .. } => point(at),
            ProposalOp::Format { range, .. } => span(range),
            ProposalOp::Delete { range }
            | ProposalOp::Replace { range, .. }
            | ProposalOp::Comment { range, .. } => multi_span(range),
            ProposalOp::ParagraphFormat { at, .. }
            | ProposalOp::Style { at, .. }
            | ProposalOp::Numbering { at, .. }
            | ProposalOp::Split { at }
            | ProposalOp::Join { at } => point(at),
            ProposalOp::Move { from, to } => multi_span(from).and_then(|()| point(to)),
        }
    }

    /// Apply one validated op, returning the change/comment id(s) it produced.
    fn apply_op(&self, op: &ProposalOp, date: &str, rationale: &str) -> Result<Vec<u64>> {
        Ok(match op {
            ProposalOp::Insert { at, text } => vec![self.propose_insert(at, text, date, rationale)?],
            ProposalOp::Delete { range } => vec![self.propose_delete(range, date, rationale)?],
            ProposalOp::Replace { range, text } => {
                let (del, ins) = self.propose_replace(range, text, date, rationale)?;
                vec![del, ins]
            }
            ProposalOp::Format { range, format } => {
                vec![self.propose_format(range, format.clone(), date, rationale)?]
            }
            ProposalOp::ParagraphFormat { at, props } => {
                vec![self.propose_paragraph_format(at, props.clone(), date, rationale)?]
            }
            ProposalOp::Style { at, style } => {
                vec![self.propose_style(at, style.clone(), date, rationale)?]
            }
            ProposalOp::Numbering { at, num_id, ilvl } => {
                vec![self.propose_numbering(at, *num_id, *ilvl, date, rationale)?]
            }
            ProposalOp::Split { at } => vec![self.propose_split(at, date, rationale)?],
            ProposalOp::Join { at } => {
                self.propose_join(at, date, rationale)?;
                vec![]
            }
            ProposalOp::Comment { range, text } => vec![self.add_comment(range, text, date)?],
            ProposalOp::Move { from, to } => vec![self.propose_move(from, to, date, rationale)?],
        })
    }

    /// Propose inserting a table row above (`below=false`) / below the row containing `at`, as a
    /// tracked change. Returns the new revision id, or `None` if `at` isn't in a table.
    pub fn propose_insert_table_row(
        &self,
        at: &Anchor,
        below: bool,
        date: &str,
        rationale: &str,
    ) -> Result<Option<u64>> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::InsertTableRow, self.doc.node_id(para), Some(rationale), || {
            let id = self.doc.next_revision_id()?;
            let caret =
                self.doc.suggest_insert_table_row(para, below, &self.author, date, &self.audit("insert table row", rationale))?;
            Ok(caret.map(|_| id))
        })
    }

    /// Propose deleting the table row containing `at`, as a tracked change. Returns the revision id, or
    /// `None` if `at` isn't in a table.
    pub fn propose_delete_table_row(&self, at: &Anchor, date: &str, rationale: &str) -> Result<Option<u64>> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::DeleteTableRow, self.doc.node_id(para), Some(rationale), || {
            let id = self.doc.next_revision_id()?;
            let caret = self.doc.suggest_delete_table_row(para, &self.author, date, &self.audit("delete table row", rationale))?;
            Ok(caret.map(|_| id))
        })
    }

    /// Propose inserting a table column to the left (`right=false`) / right of the cell containing
    /// `at`, as a tracked change. Returns the revision id, or `None` if `at` isn't in a table.
    pub fn propose_insert_table_column(
        &self,
        at: &Anchor,
        right: bool,
        date: &str,
        rationale: &str,
    ) -> Result<Option<u64>> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::InsertTableColumn, self.doc.node_id(para), Some(rationale), || {
            let id = self.doc.next_revision_id()?;
            let caret =
                self.doc.suggest_insert_table_column(para, right, &self.author, date, &self.audit("insert table column", rationale))?;
            Ok(caret.map(|_| id))
        })
    }

    /// Propose deleting the table column containing `at`, as a tracked change. Returns the revision id,
    /// or `None` if `at` isn't in a table.
    pub fn propose_delete_table_column(&self, at: &Anchor, date: &str, rationale: &str) -> Result<Option<u64>> {
        let (para, _) = self.point(at)?;
        self.guard(AgentAction::DeleteTableColumn, self.doc.node_id(para), Some(rationale), || {
            let id = self.doc.next_revision_id()?;
            let caret = self.doc.suggest_delete_table_column(para, &self.author, date, &self.audit("delete table column", rationale))?;
            Ok(caret.map(|_| id))
        })
    }

    /// Add a bookmark named `name` over `range`. Returns the bookmark id. A direct edit (bookmarks
    /// aren't tracked changes), attributed in the audit trail.
    pub fn add_bookmark(&self, range: &AnchorRange, name: &str) -> Result<u64> {
        let (para, s, e) = self.span(range)?;
        self.guard(AgentAction::AddBookmark, self.doc.node_id(para), None, || {
            self.doc.add_bookmark(para, s, e, name, &self.audit("add bookmark", name))
        })
    }

    /// Add a hyperlink to `target` over `range`. Returns the link id.
    pub fn add_hyperlink(&self, range: &AnchorRange, target: &str) -> Result<u64> {
        let (para, s, e) = self.span(range)?;
        self.guard(AgentAction::AddHyperlink, self.doc.node_id(para), None, || {
            self.doc.add_hyperlink(para, s, e, target, &self.audit("add hyperlink", target))
        })
    }

    /// Remove the hyperlink at `at` (if any). Returns whether one was removed.
    pub fn remove_hyperlink(&self, at: &Anchor) -> Result<bool> {
        let (para, off) = self.point(at)?;
        self.guard(AgentAction::RemoveHyperlink, self.doc.node_id(para), None, || {
            self.doc.remove_hyperlink(para, off, &self.audit("remove hyperlink", ""))
        })
    }

    /// Propose inserting a picture at `at` as a tracked change (`w:ins` on its run), attributed to the
    /// agent. `bytes` (MIME `mime`) ship as a fresh `word/media` part on save, shown at `w_emu` x
    /// `h_emu` (EMU). Returns the new picture id.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_image(
        &self,
        at: &Anchor,
        bytes: Vec<u8>,
        mime: &str,
        w_emu: i64,
        h_emu: i64,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        let (para, off) = self.point(at)?;
        self.guard(AgentAction::InsertImage, self.doc.node_id(para), Some(rationale), || {
            self.doc.suggest_insert_image(
                para, off, bytes.clone(), mime, w_emu, h_emu, &self.author, date,
                &self.audit("insert image", rationale),
            )
        })
    }

    /// Resize picture `id` to `w_emu` x `h_emu` (EMU). A direct edit (image geometry isn't a tracked
    /// change), guarded + attributed. Returns whether it existed.
    pub fn resize_image(&self, id: u64, w_emu: i64, h_emu: i64, rationale: &str) -> Result<bool> {
        self.guard(AgentAction::EditImage, self.image_node(id), Some(rationale), || {
            self.doc.set_image_size(id, w_emu, h_emu, &self.audit("resize image", rationale))
        })
    }

    /// Crop picture `id` (`<a:srcRect>` l/t/r/b, thousandths of a percent). Direct edit. Returns
    /// whether it existed.
    pub fn crop_image(&self, id: u64, l: i64, t: i64, r: i64, b: i64, rationale: &str) -> Result<bool> {
        self.guard(AgentAction::EditImage, self.image_node(id), Some(rationale), || {
            self.doc.set_image_crop(id, l, t, r, b, &self.audit("crop image", rationale))
        })
    }

    /// Make picture `id` floating (positioned + text-wrapped) or inline. `wrap` is the wrap type
    /// (`square` / `tight` / `topAndBottom` / `through` / `none`); `behind` paints it under the text.
    /// Direct edit. Returns whether it existed.
    pub fn float_image(&self, id: u64, floating: bool, wrap: &str, behind: bool, rationale: &str) -> Result<bool> {
        self.guard(AgentAction::EditImage, self.image_node(id), Some(rationale), || {
            self.doc.set_image_floating(id, floating, wrap, behind, &self.audit("float image", rationale))
        })
    }

    /// Propose removing picture `id` as a tracked change (`w:del` on its run, retained until accepted).
    /// Returns whether it existed.
    pub fn remove_image(&self, id: u64, date: &str, rationale: &str) -> Result<bool> {
        self.guard(AgentAction::RemoveImage, self.image_node(id), Some(rationale), || {
            self.doc.suggest_remove_image(id, &self.author, date, &self.audit("remove image", rationale))
        })
    }
}
