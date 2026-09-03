//! Reading the document.
//! 
//! Everything an agent uses to work out what it is looking at before proposing
//! anything: the outline, a single node's content, text search, the change list, and
//! the anchors that let it point at a place and still be pointing there after someone
//! else edits.

use crate::*;

impl AgentPeer {
    /// The current document as materialized paragraphs.
    pub fn paragraphs(&self) -> Result<Vec<Paragraph>> {
        self.doc.paragraphs()
    }

    /// Find every occurrence of `query` in the body (case-insensitive unless `match_case`), each with
    /// an edit-stable [`AnchorRange`] + a snippet. The agent's "locate the text to edit" primitive: it
    /// turns a quote into an anchor so edits never ride on raw offsets that a concurrent human edit
    /// would invalidate.
    pub fn find(&self, query: &str, match_case: bool) -> Result<Vec<TextMatch>> {
        self.doc.find_text(query, match_case)
    }

    /// An edit-stable anchor at codepoint `off` in body paragraph `para` (see [`CollabDoc::anchor`]).
    pub fn anchor(&self, para: usize, off: usize, side: Side) -> Result<Anchor> {
        self.doc.anchor(para, off, side)
    }

    /// An edit-stable range over codepoints `start..end` in body paragraph `para`.
    pub fn anchor_range(&self, para: usize, start: usize, end: usize) -> Result<AnchorRange> {
        self.doc.anchor_range(para, start, end)
    }

    /// Resolve an anchor to its current position (or [`Resolved::Deleted`] if its content is gone).
    pub fn resolve(&self, anchor: &Anchor) -> Resolved {
        self.doc.resolve(anchor)
    }

    /// The document's top-level comments + replies (for the agent to read existing discussion).
    pub fn comments(&self) -> Vec<Comment> {
        self.doc.comments()
    }

    /// The comment ids anchored at codepoint `off` in body paragraph `para` - perception of *where* a
    /// comment sits (e.g. to confirm a multi-paragraph comment covers the paragraphs it should).
    pub fn comments_at(&self, para: usize, off: usize) -> Result<Vec<u64>> {
        self.doc.comments_at(para, off)
    }

    /// The anchored span of every comment in the body (id + codepoint range, possibly multi-paragraph).
    /// Pair with [`comments`](Self::comments) by id to read both what a comment says and what it points
    /// at - the perception gap that left an agent able to read comment bodies but not their locations.
    pub fn comment_locations(&self) -> Result<Vec<CommentLocation>> {
        self.doc.comment_locations()
    }

    /// A token-budgeted structural outline of the body - paragraphs `[offset, offset + max_nodes)`
    /// (use `max_nodes == 0` for the whole body), each with a stable node id, kind, style, preview, and
    /// `has_changes`, plus a `revision` token and the true `total` so a large document can be paged.
    /// `preview_chars` caps each node's text preview. The agent reads this first, then drills into
    /// specific nodes with [`read_node`](Self::read_node).
    pub fn outline(&self, preview_chars: usize, offset: usize, max_nodes: usize) -> Result<DocSnapshot> {
        self.doc.outline(preview_chars, offset, max_nodes)
    }

    /// The full verbatim content (text + runs + style) of the paragraph identified by `node_id`, or
    /// `None` if its block was deleted. The read-before-write primitive.
    pub fn read_node(&self, node_id: &NodeId) -> Result<Option<NodeContent>> {
        self.doc.read_node(node_id)
    }

    /// The monotonic version token (advances on any edit) - compare across reads to detect that the
    /// document moved underneath.
    pub fn revision(&self) -> u64 {
        self.doc.revision()
    }

    /// The durable node id of body paragraph `para` (a point-in-time index -> stable handle).
    pub fn node_id(&self, para: usize) -> Option<NodeId> {
        self.doc.node_id(para)
    }

    /// The node id the paragraph an [`Anchor`] currently lives in - bridges a `find` anchor to a node
    /// the agent can `read_node`. Errors if the anchor is stale.
    pub fn node_at(&self, anchor: &Anchor) -> Result<Option<NodeId>> {
        let (para, _) = self.point(anchor)?;
        Ok(self.doc.node_id(para))
    }

    /// An edit-stable anchor at codepoint `off` within the node `node_id` (biased to `side`) - so the
    /// agent can address a sub-span of a node it just `read_node`'d **without re-running `find_text`**.
    /// Errors if the node was deleted.
    pub fn anchor_in_node(&self, node_id: &NodeId, off: usize, side: Side) -> Result<Anchor> {
        let para = self.doc.node_para(node_id).ok_or_else(|| anyhow!("node {node_id} is gone"))?;
        self.doc.anchor(para, off, side)
    }

    /// An edit-stable range over codepoints `start..end` within node `node_id` (the read -> edit
    /// bridge for a range). Errors if the node was deleted.
    pub fn anchor_range_in_node(&self, node_id: &NodeId, start: usize, end: usize) -> Result<AnchorRange> {
        let para = self.doc.node_para(node_id).ok_or_else(|| anyhow!("node {node_id} is gone"))?;
        self.doc.anchor_range(para, start, end)
    }

    /// Every tracked change in the document (id / kind / author / date / text / node), so the agent
    /// can triage them by id (accept / reject) or report what's pending.
    pub fn list_changes(&self) -> Result<Vec<ChangeSummary>> {
        self.doc.list_changes()
    }

    /// If `at` is inside a table cell, its `(row, col, n_rows, n_cols)` - so the agent knows the table
    /// shape before proposing a row/column edit. `None` outside a table (or on a joined peer, which
    /// doesn't carry table structure - see [`from_docx_bytes`](Self::from_docx_bytes)).
    pub fn table_context(&self, at: &Anchor) -> Result<Option<(usize, usize, usize, usize)>> {
        let (para, _) = self.point(at)?;
        Ok(self.doc.table_context(para))
    }

    /// The hyperlink `(id, target)` at `at`, if the position is inside one.
    pub fn link_at(&self, at: &Anchor) -> Result<Option<(u64, String)>> {
        let (para, off) = self.point(at)?;
        self.doc.link_at(para, off)
    }

    /// Every editable picture's id + placement, so the agent can discover the pictures it may edit
    /// (the perception side of picture parity).
    pub fn image_placements(&self) -> std::collections::HashMap<u64, scriptor_crdt::ImagePlacement> {
        self.doc.image_placements()
    }

    /// The body node id of the paragraph anchoring picture `id` (so a content-aware policy can gate an
    /// edit to a picture in a protected clause), or `None` if it isn't found.
    pub(crate) fn image_node(&self, id: u64) -> Option<NodeId> {
        let para = self
            .doc
            .paragraphs()
            .ok()?
            .iter()
            .position(|p| p.runs.iter().any(|r| r.image == Some(id)))?;
        self.doc.node_id(para)
    }

    /// Resolve an anchor to `(para, off)` or fail loudly when its content was deleted - so the agent
    /// learns its reference is stale instead of editing the wrong place.
    pub(crate) fn point(&self, anchor: &Anchor) -> Result<(usize, usize)> {
        match self.doc.resolve(anchor) {
            Resolved::Live { para, off } => Ok((para, off)),
            Resolved::Shifted { .. } | Resolved::Deleted => {
                Err(anyhow!("anchor is stale: its content was deleted or moved; re-locate via find_text"))
            }
        }
    }

    /// Resolve an anchor range to `(para, start, end)`, or fail when either end is gone / the range
    /// now straddles two paragraphs.
    pub(crate) fn span(&self, range: &AnchorRange) -> Result<(usize, usize, usize)> {
        self.doc
            .resolve_range(range)
            .ok_or_else(|| anyhow!("anchor range is stale or torn across paragraphs"))
    }

    /// Resolve a possibly-multi-paragraph range to `(start_para, start_off, end_para, end_off)` in
    /// document order, or fail when either end is gone. The basis for the multi-paragraph review
    /// actions (a comment / redline that crosses a paragraph boundary).
    pub(crate) fn multi_span(&self, range: &AnchorRange) -> Result<(usize, usize, usize, usize)> {
        self.doc
            .resolve_range_multi(range)
            .ok_or_else(|| anyhow!("anchor range is stale (an end's content was deleted or moved)"))
    }

    /// The body's accepted / visible text - every run except a pending deletion (`Del` / `MoveFrom`),
    /// paragraphs newline-joined. Used to classify whether a resolved change was accepted or rejected.
    pub(crate) fn visible_text(&self) -> Result<String> {
        let mut s = String::new();
        for p in self.doc.paragraphs()? {
            for r in &p.runs {
                let keep = match &r.track {
                    None => true,
                    Some(t) => matches!(t.kind, TrackKind::Ins | TrackKind::MoveTo),
                };
                if keep {
                    s.push_str(&r.text);
                }
            }
            s.push('\n');
        }
        Ok(s)
    }

    /// Whether `region` exists in this document (the body always does; a header / footer only when the
    /// document defines one).
    pub fn has_region(&self, region: Region) -> bool {
        match region {
            Region::Body => true,
            Region::Header => self.doc.header_doc().is_some(),
            Region::Footer => self.doc.footer_doc().is_some(),
        }
    }

    /// A [`RegionView`] over `region` - the same perception + tracked-edit + review surface as the body
    /// methods, but targeting the header or footer story (a child document that edits through the same
    /// path). Governance (policies + sinks) and identity are this peer's. Errors if the region does not
    /// exist. The body is reachable too (`Region::Body`), for a uniform interface.
    pub fn region(&self, region: Region) -> Result<RegionView<'_>> {
        let doc = match region {
            Region::Body => &self.doc,
            Region::Header => {
                self.doc.header_doc().ok_or_else(|| anyhow!("document has no header"))?
            }
            Region::Footer => {
                self.doc.footer_doc().ok_or_else(|| anyhow!("document has no footer"))?
            }
        };
        Ok(RegionView { peer: self, doc, region })
    }
}
