//! Addressing and read-only projections.
//! 
//! Stable ways to point at a place in the document that survive concurrent edits -
//! loro cursors and durable node ids - plus the read models an agent perceives the
//! document through: the outline, a single node's content, and the change list.

use crate::*;

impl CollabDoc {
    /// Create an [`Anchor`] at codepoint `off` in body paragraph `para`, biased to `side`. The anchor
    /// is edit-stable: a later concurrent insert/delete shifts the integer offset but the anchor still
    /// resolves to the same logical spot. Use this instead of passing raw offsets across a turn / wire.
    pub fn anchor(&self, para: usize, off: usize, side: Side) -> Result<Anchor> {
        Ok(Anchor(model::cursor_at(&self.doc, para, off, side)?))
    }

    /// Create an [`AnchorRange`] over codepoints `start..end` in body paragraph `para` (head sticks
    /// left, tail sticks right).
    pub fn anchor_range(&self, para: usize, start: usize, end: usize) -> Result<AnchorRange> {
        Ok(AnchorRange {
            start: self.anchor(para, start, Side::Left)?,
            end: self.anchor(para, end, Side::Right)?,
        })
    }

    /// Resolve an [`Anchor`]: [`Resolved::Live`] (unmoved), [`Resolved::Shifted`] (its char was deleted
    /// and re-pinned to a neighbour), or [`Resolved::Deleted`] (its block is gone).
    pub fn resolve(&self, anchor: &Anchor) -> Resolved {
        match model::resolve_cursor(&self.doc, &anchor.0) {
            Some((para, off, false)) => Resolved::Live { para, off },
            Some((para, off, true)) => Resolved::Shifted { para, off },
            None => Resolved::Deleted,
        }
    }

    /// Resolve an [`AnchorRange`] to `(para, start, end)` within one paragraph (start <= end), or
    /// `None` if either end is gone / moved (`Shifted`/`Deleted`) or the ends now straddle different
    /// paragraphs (a torn range). Only an unmoved (`Live`) range resolves - a moved end means the agent
    /// should re-locate. Single-paragraph by design (it mirrors the single-paragraph edit ops).
    pub fn resolve_range(&self, range: &AnchorRange) -> Option<(usize, usize, usize)> {
        let (sp, so) = match self.resolve(&range.start) {
            Resolved::Live { para, off } => (para, off),
            _ => return None,
        };
        let (ep, eo) = match self.resolve(&range.end) {
            Resolved::Live { para, off } => (para, off),
            _ => return None,
        };
        if sp != ep {
            return None;
        }
        Some((sp, so.min(eo), so.max(eo)))
    }

    /// Resolve an [`AnchorRange`] that may span paragraphs to `(start_para, start_off, end_para,
    /// end_off)` in document order (start <= end), or `None` if either end is gone / moved
    /// (`Shifted`/`Deleted`). Unlike [`resolve_range`](Self::resolve_range) this does *not* refuse a
    /// range whose ends sit in different paragraphs - it is the basis for the multi-paragraph review
    /// actions (a comment, redline, or move that crosses a paragraph boundary). Normalizes orientation:
    /// whichever end is earlier in the flat paragraph order becomes the start, so a caller never has to
    /// worry which anchor is the head.
    pub fn resolve_range_multi(&self, range: &AnchorRange) -> Option<(usize, usize, usize, usize)> {
        let a = match self.resolve(&range.start) {
            Resolved::Live { para, off } => (para, off),
            _ => return None,
        };
        let b = match self.resolve(&range.end) {
            Resolved::Live { para, off } => (para, off),
            _ => return None,
        };
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        Some((start.0, start.1, end.0, end.1))
    }

    /// Every occurrence of `query` in the body, in document order, each with an edit-stable
    /// [`AnchorRange`] and a surrounding snippet. Case-insensitive unless `match_case`. Searches the
    /// full run text (tracked-deleted text included) so the returned offsets share the anchor / edit
    /// codepoint space. Empty `query` matches nothing. This is the agent's "find the text to edit"
    /// primitive - it turns a quote into an anchor, sidestepping raw offsets entirely.
    pub fn find_text(&self, query: &str, match_case: bool) -> Result<Vec<TextMatch>> {
        let needle: Vec<char> = query.chars().collect();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let eq = |a: char, b: char| {
            if match_case { a == b } else { a.to_lowercase().eq(b.to_lowercase()) }
        };
        let mut out = Vec::new();
        for (para, p) in self.paragraphs()?.iter().enumerate() {
            // Flatten run text into a codepoint haystack, tracking per-codepoint whether it sits in a
            // tracked deletion (so a hit can report `in_deletion`).
            let mut hay: Vec<char> = Vec::new();
            let mut deleted: Vec<bool> = Vec::new();
            for r in &p.runs {
                let is_del = matches!(
                    r.track.as_ref().map(|t| t.kind),
                    Some(TrackKind::Del) | Some(TrackKind::MoveFrom)
                );
                for c in r.text.chars() {
                    hay.push(c);
                    deleted.push(is_del);
                }
            }
            if hay.len() < needle.len() {
                continue;
            }
            let mut i = 0;
            while i + needle.len() <= hay.len() {
                if (0..needle.len()).all(|k| eq(hay[i + k], needle[k])) {
                    let (start, end) = (i, i + needle.len());
                    let from = start.saturating_sub(24);
                    let to = (end + 24).min(hay.len());
                    let mut snippet = String::new();
                    if from > 0 {
                        snippet.push('…');
                    }
                    snippet.extend(&hay[from..to]);
                    if to < hay.len() {
                        snippet.push('…');
                    }
                    out.push(TextMatch {
                        para,
                        start,
                        end,
                        anchor: self.anchor_range(para, start, end)?,
                        snippet,
                        in_deletion: deleted.get(start).copied().unwrap_or(false),
                    });
                    i = end; // non-overlapping matches
                } else {
                    i += 1;
                }
            }
        }
        Ok(out)
    }

    /// A monotonic op count that advances on *any* edit (loro's op-log length). The freshness /
    /// version token: compare it across reads to learn whether the document moved underneath.
    pub fn revision(&self) -> u64 {
        self.doc.len_ops() as u64
    }

    /// The durable [`NodeId`] of body paragraph `para`, or `None` if out of range. A top-level paragraph
    /// yields its block node's tree id; a table-cell paragraph yields its text container id.
    pub fn node_id(&self, para: usize) -> Option<NodeId> {
        Some(self.node_id_of(&model::block_seq(&self.doc).into_iter().nth(para)?))
    }

    /// The [`NodeId`] for one flat block (a top-level node tree id, or a cell paragraph's text container
    /// id). A cell with no live text container degrades to its table node id (shouldn't happen for a
    /// materialized cell).
    fn node_id_of(&self, r: &model::BlockRef) -> NodeId {
        match r {
            model::BlockRef::Top(id) => NodeId(NodeRef::Block(*id)),
            model::BlockRef::Cell { node, .. } => match model::block_ref_text(&self.doc, r) {
                Some(t) => NodeId(NodeRef::Cell(t.id())),
                None => NodeId(NodeRef::Block(*node)),
            },
        }
    }

    /// The current body-paragraph index of `node_id`, or `None` if its block is gone.
    pub fn node_para(&self, node_id: &NodeId) -> Option<usize> {
        match &node_id.0 {
            NodeRef::Block(id) => model::block_index_of(&self.doc, *id),
            NodeRef::Cell(cid) => model::block_index_of_container(&self.doc, cid),
        }
    }

    /// A token-budgeted structural outline of the body: each paragraph with a stable [`NodeId`], its
    /// kind/style/heading-level, a `preview_chars`-capped text preview, and whether it carries a
    /// tracked change - plus a `revision` token and `total`/`offset` so a large document can be paged.
    /// The window is `[offset, offset + max_nodes)`; `max_nodes == 0` means no cap (whole body). The
    /// agent reads this first, then drills into specific nodes with [`read_node`](Self::read_node).
    /// Body only (header/footer are separate stories).
    pub fn outline(&self, preview_chars: usize, offset: usize, max_nodes: usize) -> Result<DocSnapshot> {
        let paras = self.paragraphs()?;
        let seq = model::block_seq(&self.doc);
        let levels: HashMap<usize, u8> = self.headings().into_iter().map(|(p, l, _)| (p, l)).collect();
        let changed: HashSet<usize> = self.change_carets()?.into_iter().map(|(p, _)| p).collect();
        let end = if max_nodes == 0 { paras.len() } else { (offset + max_nodes).min(paras.len()) };
        let mut nodes = Vec::with_capacity(end.saturating_sub(offset));
        #[allow(clippy::needless_range_loop)]
        for para in offset..end {
            let Some(bref) = seq.get(para) else { continue };
            let node = self.node_id_of(bref);
            let p = &paras[para];
            let text: String = p.runs.iter().map(|r| r.text.as_str()).collect();
            let heading_level = levels.get(&para).copied();
            nodes.push(OutlineNode {
                node_id: node,
                para,
                kind: self.classify_para(para, heading_level),
                heading_level,
                style: self.paragraph_style(para),
                char_count: text.chars().count(),
                preview: preview_of(&text, preview_chars),
                has_changes: changed.contains(&para),
                table: self.table_context(para),
            });
        }
        Ok(DocSnapshot { revision: self.revision(), total: paras.len(), offset, nodes })
    }

    /// The full content of the body paragraph identified by `node_id` (verbatim text + runs + style),
    /// or `None` if the block was deleted. The read-before-write primitive.
    pub fn read_node(&self, node_id: &NodeId) -> Result<Option<NodeContent>> {
        let Some(para) = self.node_para(node_id) else { return Ok(None) };
        let paras = self.paragraphs()?;
        let Some(p) = paras.get(para) else { return Ok(None) };
        let heading_level = self.headings().into_iter().find(|(pp, _, _)| *pp == para).map(|(_, l, _)| l);
        Ok(Some(NodeContent {
            node_id: node_id.clone(),
            para,
            kind: self.classify_para(para, heading_level),
            heading_level,
            style: self.paragraph_style(para),
            text: p.runs.iter().map(|r| r.text.as_str()).collect(),
            runs: p.runs.clone(),
        }))
    }

    /// Every tracked change in the document, in the agent's shape (`id` / `kind` / `author` / `date` /
    /// `text` / `para` / `node_id`), one entry per revision id - so the agent can triage (accept /
    /// reject by id) or report what's pending. Run-level changes + tracked table changes.
    pub fn list_changes(&self) -> Result<Vec<ChangeSummary>> {
        let seq = model::block_seq(&self.doc);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (para, off) in self.change_carets()? {
            let Some(region) = self.track_at(para, off)? else { continue };
            if !seen.insert(region.track.id) {
                continue;
            }
            let Some(bref) = seq.get(para) else { continue };
            out.push(ChangeSummary {
                id: region.track.id,
                kind: track_kind_label(region.track.kind).to_string(),
                author: region.track.author,
                date: region.track.date,
                text: region.text,
                para,
                node_id: self.node_id_of(bref),
            });
        }
        for tc in self.table_changes() {
            if !seen.insert(tc.id) {
                continue;
            }
            let Some(bref) = seq.get(tc.para) else { continue };
            out.push(ChangeSummary {
                id: tc.id,
                kind: table_change_label(&tc).to_string(),
                author: tc.author,
                date: tc.date,
                text: String::new(),
                para: tc.para,
                node_id: self.node_id_of(bref),
            });
        }
        Ok(out)
    }

    /// Classify a body paragraph for the outline: table cell, then heading, then list item, else a
    /// plain paragraph.
    fn classify_para(&self, para: usize, heading_level: Option<u8>) -> NodeKind {
        if self.table_context(para).is_some() {
            NodeKind::TableCell
        } else if heading_level.is_some() {
            NodeKind::Heading
        } else if self.paragraph_list_format(para).is_some() {
            NodeKind::ListItem
        } else {
            NodeKind::Paragraph
        }
    }
}
