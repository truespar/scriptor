//! The JSON wire forms.
//! 
//! Every capability has a DTO variant so a non-Rust agent can drive the same surface
//! over RPC or as a tool call, passing anchors back as opaque tokens.

use crate::*;

impl AgentPeer {
    /// Seed an empty document with a plain-text paragraph - the wire form of
    /// [`append_paragraph`](Self::append_paragraph) (which takes the rich `Run` model a wire agent can't
    /// build). Apply run formatting afterwards with a Format proposal.
    pub fn append_paragraph_text_dto(&self, text: &str, style: Option<&str>) -> Result<()> {
        self.doc.append_paragraph(&[Run::plain(text)], style)
    }

    /// The outline as a serde-serializable [`wire::DocSnapshotDto`] - perception for a non-Rust agent.
    pub fn outline_dto(
        &self,
        preview_chars: usize,
        offset: usize,
        max_nodes: usize,
    ) -> Result<wire::DocSnapshotDto> {
        let snap = self.outline(preview_chars, offset, max_nodes)?;
        Ok((&snap).into())
    }

    /// `find_text` results as [`wire::TextMatchDto`]s - each carries an opaque anchor token the agent
    /// echoes back in a proposal.
    pub fn find_dto(&self, query: &str, match_case: bool) -> Result<Vec<wire::TextMatchDto>> {
        Ok(self.find(query, match_case)?.iter().map(wire::TextMatchDto::from).collect())
    }

    /// One node's full content as a [`wire::NodeContentDto`]; `node_id` is its string form.
    pub fn read_node_dto(&self, node_id: &str) -> Result<Option<wire::NodeContentDto>> {
        let id: NodeId = node_id.parse()?;
        Ok(self.read_node(&id)?.as_ref().map(wire::NodeContentDto::from))
    }

    /// Tracked changes as [`wire::ChangeSummaryDto`]s.
    pub fn list_changes_dto(&self) -> Result<Vec<wire::ChangeSummaryDto>> {
        Ok(self.list_changes()?.iter().map(wire::ChangeSummaryDto::from).collect())
    }

    /// The manifest of [`compare_with`](Self::compare_with) as a [`wire::CompareResultDto`] - the JSON
    /// reasoning surface for an out-of-process agent (summary + one entry per difference). The redline
    /// `.docx` bytes come from [`compare_with`](Self::compare_with), served out-of-band like
    /// [`to_docx_bytes`](Self::to_docx_bytes).
    pub fn compare_with_dto(&self, revised: &[u8]) -> Result<wire::CompareResultDto> {
        Ok(wire::CompareResultDto::from(&self.compare_with(revised)?.manifest))
    }

    /// Comment anchor spans as [`wire::CommentLocationDto`]s (pair with comment bodies by id).
    pub fn comment_locations_dto(&self) -> Result<Vec<wire::CommentLocationDto>> {
        Ok(self.comment_locations()?.iter().map(wire::CommentLocationDto::from).collect())
    }

    /// Comment bodies + thread state as [`wire::CommentDto`]s - the wire form of [`comments`](Self::comments).
    pub fn comments_dto(&self) -> Vec<wire::CommentDto> {
        self.comments().iter().map(wire::CommentDto::from).collect()
    }

    /// An anchor token at codepoint `off` in body paragraph `para`, biased to `side` (`"left"` for a
    /// range head, `"right"` for a tail, else middle). The wire form of [`anchor`](Self::anchor): lets a
    /// non-Rust agent build an addressing handle without a prior find.
    pub fn anchor_dto(&self, para: usize, off: usize, side: &str) -> Result<String> {
        Ok(wire::anchor_token(&self.anchor(para, off, side_from_str(side))?))
    }

    /// An anchor-range token pair over codepoints `start..end` in body paragraph `para` (wire form of
    /// [`anchor_range`](Self::anchor_range)).
    pub fn anchor_range_dto(&self, para: usize, start: usize, end: usize) -> Result<wire::AnchorRangeDto> {
        Ok((&self.anchor_range(para, start, end)?).into())
    }

    /// An anchor token at `off` within node `node_id` (the read -> edit bridge over the wire; mirror of
    /// [`anchor_in_node`](Self::anchor_in_node)).
    pub fn anchor_in_node_dto(&self, node_id: &str, off: usize, side: &str) -> Result<String> {
        let id: NodeId = node_id.parse()?;
        Ok(wire::anchor_token(&self.anchor_in_node(&id, off, side_from_str(side))?))
    }

    /// An anchor-range token pair over `start..end` within node `node_id` (mirror of
    /// [`anchor_range_in_node`](Self::anchor_range_in_node)).
    pub fn anchor_range_in_node_dto(
        &self,
        node_id: &str,
        start: usize,
        end: usize,
    ) -> Result<wire::AnchorRangeDto> {
        let id: NodeId = node_id.parse()?;
        Ok((&self.anchor_range_in_node(&id, start, end)?).into())
    }

    /// The string node id of body paragraph `para` (wire form of [`node_id`](Self::node_id)).
    pub fn node_id_dto(&self, para: usize) -> Option<String> {
        self.node_id(para).map(|n| n.to_string())
    }

    /// The string node id the anchor token currently lives in (wire form of [`node_at`](Self::node_at)).
    pub fn node_at_dto(&self, token: &str) -> Result<Option<String>> {
        let a = wire::anchor_from_token(token)?;
        Ok(self.node_at(&a)?.map(|n| n.to_string()))
    }

    /// Resolve an anchor token to its current state (wire form of [`resolve`](Self::resolve)) - so a
    /// wire agent can detect that a handle it holds went stale before proposing against it.
    pub fn resolve_dto(&self, token: &str) -> Result<wire::ResolvedDto> {
        let a = wire::anchor_from_token(token)?;
        Ok(self.resolve(&a).into())
    }

    /// Merge other peers' updates and return what they did as [`wire::ObservationDto`]s (wire form of
    /// [`merge_observed`](Self::merge_observed) - the agent's feedback loop over the wire).
    pub fn merge_observed_dto(&self, bytes: &[u8]) -> Result<Vec<wire::ObservationDto>> {
        Ok(self.merge_observed(bytes)?.iter().map(wire::ObservationDto::from).collect())
    }

    /// The table shape `[row, col, n_rows, n_cols]` at the anchor token, if it is in a cell (wire form
    /// of [`table_context`](Self::table_context)).
    pub fn table_context_dto(&self, token: &str) -> Result<Option<[usize; 4]>> {
        let a = wire::anchor_from_token(token)?;
        Ok(self.table_context(&a)?.map(|(r, c, nr, nc)| [r, c, nr, nc]))
    }

    /// Propose a tracked table-row insertion at the anchor token (wire form of
    /// [`propose_insert_table_row`](Self::propose_insert_table_row)).
    pub fn propose_insert_table_row_dto(
        &self,
        token: &str,
        below: bool,
        date: &str,
        rationale: &str,
    ) -> Result<Option<u64>> {
        let a = wire::anchor_from_token(token)?;
        self.propose_insert_table_row(&a, below, date, rationale)
    }

    /// Propose a tracked table-row deletion at the anchor token.
    pub fn propose_delete_table_row_dto(&self, token: &str, date: &str, rationale: &str) -> Result<Option<u64>> {
        let a = wire::anchor_from_token(token)?;
        self.propose_delete_table_row(&a, date, rationale)
    }

    /// Propose a tracked table-column insertion at the anchor token.
    pub fn propose_insert_table_column_dto(
        &self,
        token: &str,
        right: bool,
        date: &str,
        rationale: &str,
    ) -> Result<Option<u64>> {
        let a = wire::anchor_from_token(token)?;
        self.propose_insert_table_column(&a, right, date, rationale)
    }

    /// Propose a tracked table-column deletion at the anchor token.
    pub fn propose_delete_table_column_dto(&self, token: &str, date: &str, rationale: &str) -> Result<Option<u64>> {
        let a = wire::anchor_from_token(token)?;
        self.propose_delete_table_column(&a, date, rationale)
    }

    /// Add a bookmark over the range (wire form of [`add_bookmark`](Self::add_bookmark)).
    pub fn add_bookmark_dto(&self, range: &wire::AnchorRangeDto, name: &str) -> Result<u64> {
        self.add_bookmark(&range.decode()?, name)
    }

    /// Add a hyperlink over the range (wire form of [`add_hyperlink`](Self::add_hyperlink)).
    pub fn add_hyperlink_dto(&self, range: &wire::AnchorRangeDto, target: &str) -> Result<u64> {
        self.add_hyperlink(&range.decode()?, target)
    }

    /// Remove the hyperlink at the anchor token (wire form of [`remove_hyperlink`](Self::remove_hyperlink)).
    pub fn remove_hyperlink_dto(&self, token: &str) -> Result<bool> {
        let a = wire::anchor_from_token(token)?;
        self.remove_hyperlink(&a)
    }

    /// The hyperlink at the anchor token, if any (wire form of [`link_at`](Self::link_at)).
    pub fn link_at_dto(&self, token: &str) -> Result<Option<wire::LinkDto>> {
        let a = wire::anchor_from_token(token)?;
        Ok(self.link_at(&a)?.map(|(id, target)| wire::LinkDto { id, target }))
    }

    /// Submit a proposal from its wire form: decode anchor tokens, then run it through
    /// [`submit_proposal`](Self::submit_proposal). The integrator's RPC handler is just
    /// `json -> ProposalDto -> submit_proposal_dto -> ProposalResultDto -> json`.
    pub fn submit_proposal_dto(
        &self,
        proposal: wire::ProposalDto,
        date: &str,
    ) -> Result<wire::ProposalResultDto> {
        let p = proposal.decode()?;
        Ok(self.submit_proposal(&p, date)?.into())
    }
}
