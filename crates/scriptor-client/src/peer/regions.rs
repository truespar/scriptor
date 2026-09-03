//! The same surface, scoped to a header or footer story.
//! 
//! Body, header and footer are separate documents, so an agent addressing one has to
//! say which; these route the DTO calls into the right story.

use crate::*;

impl AgentPeer {
    /// This story's full text (cheap read for a short header/footer).
    pub fn region_text(&self, region: &str) -> Result<String> {
        self.region(region_from_str(region)?)?.text()
    }

    /// Find in this story (wire form of [`RegionView::find`]).
    pub fn region_find_dto(&self, region: &str, query: &str, match_case: bool) -> Result<Vec<wire::TextMatchDto>> {
        Ok(self.region(region_from_str(region)?)?.find(query, match_case)?.iter().map(wire::TextMatchDto::from).collect())
    }

    /// Outline this story (wire form of [`RegionView::outline`]).
    pub fn region_outline_dto(
        &self,
        region: &str,
        preview_chars: usize,
        offset: usize,
        max_nodes: usize,
    ) -> Result<wire::DocSnapshotDto> {
        Ok((&self.region(region_from_str(region)?)?.outline(preview_chars, offset, max_nodes)?).into())
    }

    /// Read a node in this story (wire form of [`RegionView::read_node`]).
    pub fn region_read_node_dto(&self, region: &str, node_id: &str) -> Result<Option<wire::NodeContentDto>> {
        let id: NodeId = node_id.parse()?;
        Ok(self.region(region_from_str(region)?)?.read_node(&id)?.as_ref().map(wire::NodeContentDto::from))
    }

    /// An anchor token in this story (wire form of [`RegionView::anchor`]).
    pub fn region_anchor_dto(&self, region: &str, para: usize, off: usize, side: &str) -> Result<String> {
        Ok(wire::anchor_token(&self.region(region_from_str(region)?)?.anchor(para, off, side_from_str(side))?))
    }

    /// An anchor-range token pair in this story (wire form of [`RegionView::anchor_range`]).
    pub fn region_anchor_range_dto(&self, region: &str, para: usize, start: usize, end: usize) -> Result<wire::AnchorRangeDto> {
        Ok((&self.region(region_from_str(region)?)?.anchor_range(para, start, end)?).into())
    }

    /// This story's comments (wire form of [`RegionView::comments`]).
    pub fn region_comments_dto(&self, region: &str) -> Result<Vec<wire::CommentDto>> {
        Ok(self.region(region_from_str(region)?)?.comments().iter().map(wire::CommentDto::from).collect())
    }

    /// This story's tracked changes (wire form of [`RegionView::list_changes`]).
    pub fn region_list_changes_dto(&self, region: &str) -> Result<Vec<wire::ChangeSummaryDto>> {
        Ok(self.region(region_from_str(region)?)?.list_changes()?.iter().map(wire::ChangeSummaryDto::from).collect())
    }

    /// Propose a tracked insertion in this story at the anchor token.
    pub fn region_propose_insert_dto(&self, region: &str, token: &str, text: &str, date: &str, rationale: &str) -> Result<u64> {
        let a = wire::anchor_from_token(token)?;
        self.region(region_from_str(region)?)?.propose_insert(&a, text, date, rationale)
    }

    /// Propose a tracked deletion (may span paragraphs) in this story.
    pub fn region_propose_delete_dto(&self, region: &str, range: &wire::AnchorRangeDto, date: &str, rationale: &str) -> Result<u64> {
        self.region(region_from_str(region)?)?.propose_delete(&range.decode()?, date, rationale)
    }

    /// Propose a tracked replacement in this story; returns `[deletion_id, insertion_id]`.
    pub fn region_propose_replace_dto(&self, region: &str, range: &wire::AnchorRangeDto, text: &str, date: &str, rationale: &str) -> Result<[u64; 2]> {
        let (del, ins) = self.region(region_from_str(region)?)?.propose_replace(&range.decode()?, text, date, rationale)?;
        Ok([del, ins])
    }

    /// Propose tracked run formatting over a range in this story.
    pub fn region_propose_format_dto(&self, region: &str, range: &wire::AnchorRangeDto, format: wire::RunFormatDto, date: &str, rationale: &str) -> Result<u64> {
        self.region(region_from_str(region)?)?.propose_format(&range.decode()?, format.into(), date, rationale)
    }

    /// Add a comment over a range in this story.
    pub fn region_add_comment_dto(&self, region: &str, range: &wire::AnchorRangeDto, text: &str, date: &str) -> Result<u64> {
        self.region(region_from_str(region)?)?.add_comment(&range.decode()?, text, date)
    }

    /// Accept tracked change `id` in this story.
    pub fn region_accept_change_dto(&self, region: &str, id: u64) -> Result<bool> {
        self.region(region_from_str(region)?)?.accept_change(id)
    }

    /// Reject tracked change `id` in this story.
    pub fn region_reject_change_dto(&self, region: &str, id: u64) -> Result<bool> {
        self.region(region_from_str(region)?)?.reject_change(id)
    }

    /// Accept every tracked change in this story.
    pub fn region_accept_all_dto(&self, region: &str) -> Result<usize> {
        self.region(region_from_str(region)?)?.accept_all()
    }

    /// Reject every tracked change in this story.
    pub fn region_reject_all_dto(&self, region: &str) -> Result<usize> {
        self.region(region_from_str(region)?)?.reject_all()
    }
}
