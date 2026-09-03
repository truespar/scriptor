//! A stable, serde-serializable JSON wire contract for the agent protocol.
//!
//! [`crate::AgentPeer`] is a Rust API; this module is its **language boundary**. An integrator in any
//! language drives Scriptor by exchanging these DTOs as JSON: read perception ([`DocSnapshotDto`] /
//! [`TextMatchDto`] / …), build a [`ProposalDto`], get a [`ProposalResultDto`] back. The same DTOs are
//! the payload whether the agent runs in-browser (calling Scriptor directly) or server-side (over
//! HTTP/MCP against a Rust authority).
//!
//! Loro-backed handles - anchors and node ids - travel as **opaque strings the agent only ever echoes
//! back**; it never constructs one (it can't run loro). An anchor token is the hex of the anchor's
//! bytes; a node id is its string form. The DTOs are deliberately decoupled from the engine's internal
//! types so the wire format stays stable as the engine evolves.

use anyhow::{anyhow, Result};
use scriptor_crdt::{
    Align, Anchor, AnchorRange, ChangeSummary, Comment, CommentLocation, DocSnapshot, NodeContent,
    NodeKind, OutlineNode, ParaProps, Resolved, Run, RunFormat, TextMatch, TrackKind,
};
use serde::{Deserialize, Serialize};

use crate::{AgentAction, AgentEvent, Observation, Proposal, ProposalOp, ProposalResult};

// ── opaque handles (hex, dependency-free) ─────────────────────────────────────

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return Err(anyhow!("odd-length hex token"));
    }
    (0..b.len())
        .step_by(2)
        .map(|i| {
            let hi = (b[i] as char).to_digit(16).ok_or_else(|| anyhow!("invalid hex token"))?;
            let lo = (b[i + 1] as char).to_digit(16).ok_or_else(|| anyhow!("invalid hex token"))?;
            Ok((hi * 16 + lo) as u8)
        })
        .collect()
}

/// Encode an anchor as an opaque token.
pub(crate) fn anchor_token(a: &Anchor) -> String {
    to_hex(&a.to_bytes())
}

/// Decode an anchor token produced by [`anchor_token`].
pub(crate) fn anchor_from_token(s: &str) -> Result<Anchor> {
    Anchor::from_bytes(&from_hex(s)?)
}

// ── perception (engine -> DTO) ────────────────────────────────────────────────

/// A range over the body, as two opaque anchor tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRangeDto {
    pub start: String,
    pub end: String,
}

impl From<&AnchorRange> for AnchorRangeDto {
    fn from(r: &AnchorRange) -> Self {
        Self { start: anchor_token(&r.start), end: anchor_token(&r.end) }
    }
}

impl AnchorRangeDto {
    pub(crate) fn decode(&self) -> Result<AnchorRange> {
        Ok(AnchorRange {
            start: anchor_from_token(&self.start)?,
            end: anchor_from_token(&self.end)?,
        })
    }
}

/// A hyperlink at a point: its id + resolved target (mirror of `link_at`'s return).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkDto {
    pub id: u64,
    pub target: String,
}

fn node_kind_str(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Paragraph => "paragraph",
        NodeKind::Heading => "heading",
        NodeKind::ListItem => "list_item",
        NodeKind::TableCell => "table_cell",
    }
}

/// One paragraph in the outline (mirror of `OutlineNode`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlineNodeDto {
    pub node_id: String,
    pub para: usize,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub heading_level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub style: Option<String>,
    pub char_count: usize,
    pub preview: String,
    pub has_changes: bool,
    /// For a table cell: `[row, col, n_rows, n_cols]`; omitted for a non-cell paragraph.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub table: Option<[usize; 4]>,
}

impl From<&OutlineNode> for OutlineNodeDto {
    fn from(n: &OutlineNode) -> Self {
        Self {
            node_id: n.node_id.to_string(),
            para: n.para,
            kind: node_kind_str(n.kind).to_string(),
            heading_level: n.heading_level,
            style: n.style.clone(),
            char_count: n.char_count,
            preview: n.preview.clone(),
            has_changes: n.has_changes,
            table: n.table.map(|(r, c, nr, nc)| [r, c, nr, nc]),
        }
    }
}

/// A token-budgeted structural snapshot of the body (mirror of `DocSnapshot`). When `nodes.len() <
/// total` the outline was capped - page with `offset` to read the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocSnapshotDto {
    pub revision: u64,
    pub total: usize,
    pub offset: usize,
    pub nodes: Vec<OutlineNodeDto>,
}

impl From<&DocSnapshot> for DocSnapshotDto {
    fn from(s: &DocSnapshot) -> Self {
        Self {
            revision: s.revision,
            total: s.total,
            offset: s.offset,
            nodes: s.nodes.iter().map(OutlineNodeDto::from).collect(),
        }
    }
}

/// The wire label for a tracked-change kind on a run.
fn track_kind_str(k: TrackKind) -> &'static str {
    match k {
        TrackKind::Ins => "ins",
        TrackKind::Del => "del",
        TrackKind::Fmt => "fmt",
        TrackKind::MoveFrom => "movefrom",
        TrackKind::MoveTo => "moveto",
    }
}

/// One run within a node, with its visible formatting, tracked-change state, and the annotations it
/// carries (comment / bookmark / hyperlink / field) - so a wire agent perceives that a span is a
/// hyperlink or a comment anchor and doesn't blindly clobber it when redlining.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDto {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub font: Option<String>,
    /// True when this run is part of a tracked change.
    pub tracked: bool,
    /// The tracked-change kind (`ins` / `del` / `fmt` / `movefrom` / `moveto`) when `tracked`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub change_kind: Option<String>,
    /// Comment ids anchored over this run (empty when none).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub comments: Vec<u64>,
    /// The hyperlink id this run belongs to, if any (target via `link_at`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link: Option<u64>,
    /// The bookmark ids whose ranges cover this run (several can overlap on one run).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub bookmarks: Vec<u64>,
    /// The field id this run is the cached result of, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub field: Option<u64>,
}

impl From<&Run> for RunDto {
    fn from(r: &Run) -> Self {
        Self {
            text: r.text.clone(),
            bold: r.bold,
            italic: r.italic,
            underline: r.underline,
            strike: r.strike,
            size: r.size,
            color: r.color.clone(),
            font: r.font.clone(),
            tracked: r.track.is_some(),
            change_kind: r.track.as_ref().map(|t| track_kind_str(t.kind).to_string()),
            comments: r.comments.clone(),
            link: r.link,
            bookmarks: r.bookmarks.clone(),
            field: r.field,
        }
    }
}

/// The full content of one node (mirror of `NodeContent`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeContentDto {
    pub node_id: String,
    pub para: usize,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub heading_level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub style: Option<String>,
    pub text: String,
    pub runs: Vec<RunDto>,
}

impl From<&NodeContent> for NodeContentDto {
    fn from(c: &NodeContent) -> Self {
        Self {
            node_id: c.node_id.to_string(),
            para: c.para,
            kind: node_kind_str(c.kind).to_string(),
            heading_level: c.heading_level,
            style: c.style.clone(),
            text: c.text.clone(),
            runs: c.runs.iter().map(RunDto::from).collect(),
        }
    }
}

/// A `find_text` hit (mirror of `TextMatch`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextMatchDto {
    pub para: usize,
    pub start: usize,
    pub end: usize,
    pub anchor: AnchorRangeDto,
    pub snippet: String,
    /// True when the match begins inside text already marked for deletion (don't redline a phantom).
    #[serde(default)]
    pub in_deletion: bool,
}

impl From<&TextMatch> for TextMatchDto {
    fn from(m: &TextMatch) -> Self {
        Self {
            para: m.para,
            start: m.start,
            end: m.end,
            anchor: AnchorRangeDto::from(&m.anchor),
            snippet: m.snippet.clone(),
            in_deletion: m.in_deletion,
        }
    }
}

/// A comment's anchored span (mirror of `CommentLocation`); pair with a comment body by `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentLocationDto {
    pub id: u64,
    pub start_para: usize,
    pub start_off: usize,
    pub end_para: usize,
    pub end_off: usize,
}

impl From<&CommentLocation> for CommentLocationDto {
    fn from(c: &CommentLocation) -> Self {
        Self {
            id: c.id,
            start_para: c.start_para,
            start_off: c.start_off,
            end_para: c.end_para,
            end_off: c.end_off,
        }
    }
}

/// A comment body + thread state (mirror of `Comment`); pair with a [`CommentLocationDto`] by `id` for
/// its anchor span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentDto {
    pub id: u64,
    pub author: String,
    pub initials: String,
    pub date: String,
    /// The parent comment id when this is a reply (a thread), else absent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent: Option<u64>,
    pub resolved: bool,
    pub text: String,
}

impl From<&Comment> for CommentDto {
    fn from(c: &Comment) -> Self {
        Self {
            id: c.id,
            author: c.author.clone(),
            initials: c.initials.clone(),
            date: c.date.clone(),
            parent: c.parent,
            resolved: c.resolved,
            text: c.text.clone(),
        }
    }
}

/// The result of resolving an anchor token (mirror of `Resolved`). `live` / `shifted` carry the current
/// `[para, off]`; `deleted` carries none (the anchored block is gone).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResolvedDto {
    Live { para: usize, off: usize },
    Shifted { para: usize, off: usize },
    Deleted,
}

impl From<Resolved> for ResolvedDto {
    fn from(r: Resolved) -> Self {
        match r {
            Resolved::Live { para, off } => ResolvedDto::Live { para, off },
            Resolved::Shifted { para, off } => ResolvedDto::Shifted { para, off },
            Resolved::Deleted => ResolvedDto::Deleted,
        }
    }
}

/// Something another peer did, observed across a merge (mirror of [`Observation`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationDto {
    ChangeResolved { id: u64, change_kind: String, author: String, accepted: Option<bool> },
    ChangeAdded { id: u64, change_kind: String, author: String },
    CommentAdded { id: u64, author: String },
}

impl From<&Observation> for ObservationDto {
    fn from(o: &Observation) -> Self {
        match o {
            Observation::ChangeResolved { id, kind, author, accepted } => ObservationDto::ChangeResolved {
                id: *id,
                change_kind: kind.clone(),
                author: author.clone(),
                accepted: *accepted,
            },
            Observation::ChangeAdded { id, kind, author } => {
                ObservationDto::ChangeAdded { id: *id, change_kind: kind.clone(), author: author.clone() }
            }
            Observation::CommentAdded { id, author } => {
                ObservationDto::CommentAdded { id: *id, author: author.clone() }
            }
        }
    }
}

/// A tracked change (mirror of `ChangeSummary`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSummaryDto {
    pub id: u64,
    pub kind: String,
    pub author: String,
    pub date: String,
    pub text: String,
    pub para: usize,
    pub node_id: String,
}

impl From<&ChangeSummary> for ChangeSummaryDto {
    fn from(c: &ChangeSummary) -> Self {
        Self {
            id: c.id,
            kind: c.kind.clone(),
            author: c.author.clone(),
            date: c.date.clone(),
            text: c.text.clone(),
            para: c.para,
            node_id: c.node_id.to_string(),
        }
    }
}

// ── document comparison (blacklining) ─────────────────────────────────────────

/// One entry in a comparison's change manifest - the wire form of `scriptor_compare::Change`. `kind`
/// is one of `insert` / `delete` / `replace` / `para-insert` / `para-delete` / `format` /
/// `para-format` / `table-row-insert` / `table-row-delete` / `move`. `before` / `after` carry the
/// affected text (empty where a side does not apply).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompareChangeDto {
    pub id: u64,
    pub kind: String,
    pub para: usize,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub before: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub after: String,
}

/// The result of a comparison, sans the binary redline: a one-line summary plus every change in
/// document order. The agent reasons over this; the redline `.docx` is served out-of-band.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompareResultDto {
    pub summary: String,
    pub changes: Vec<CompareChangeDto>,
}

/// The stable wire label for a comparison change kind.
fn compare_kind_str(kind: scriptor_compare::ChangeKind) -> &'static str {
    use scriptor_compare::ChangeKind::*;
    match kind {
        Insert => "insert",
        Delete => "delete",
        Replace => "replace",
        ParaInsert => "para-insert",
        ParaDelete => "para-delete",
        Format => "format",
        ParaFormat => "para-format",
        TableRowInsert => "table-row-insert",
        TableRowDelete => "table-row-delete",
        TableColumnDelete => "table-column-delete",
        Move => "move",
    }
}

impl From<&scriptor_compare::Change> for CompareChangeDto {
    fn from(c: &scriptor_compare::Change) -> Self {
        Self {
            id: c.id,
            kind: compare_kind_str(c.kind).to_string(),
            para: c.para,
            before: c.before.clone(),
            after: c.after.clone(),
        }
    }
}

impl From<&scriptor_compare::Manifest> for CompareResultDto {
    fn from(m: &scriptor_compare::Manifest) -> Self {
        Self { summary: m.summary(), changes: m.changes.iter().map(CompareChangeDto::from).collect() }
    }
}

/// The stable wire label for a materiality level.
fn materiality_str(m: scriptor_compare::Materiality) -> &'static str {
    match m {
        scriptor_compare::Materiality::Trivial => "trivial",
        scriptor_compare::Materiality::Substantive => "substantive",
    }
}

/// A change with its (optional) semantic annotation joined in - the deterministic diff plus the LLM
/// overlay's judgment on it. `materiality` is `trivial` / `substantive` when annotated, absent
/// otherwise; a change with no annotation carries the diff fields only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotatedChangeDto {
    pub id: u64,
    pub kind: String,
    pub para: usize,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub before: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub after: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub materiality: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub category: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub risks: Vec<String>,
}

/// A comparison with its validated semantic overlay: every change, each carrying its annotation when
/// the model provided one. The redline is untouched - this only describes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotatedCompareResultDto {
    pub summary: String,
    pub changes: Vec<AnnotatedChangeDto>,
}

impl From<&scriptor_compare::AnnotatedManifest> for AnnotatedCompareResultDto {
    fn from(am: &scriptor_compare::AnnotatedManifest) -> Self {
        let changes = am
            .manifest
            .changes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let ann = am.annotation_for(i);
                AnnotatedChangeDto {
                    id: c.id,
                    kind: compare_kind_str(c.kind).to_string(),
                    para: c.para,
                    before: c.before.clone(),
                    after: c.after.clone(),
                    materiality: ann.map(|a| materiality_str(a.materiality).to_string()),
                    category: ann.map(|a| a.category.clone()).unwrap_or_default(),
                    summary: ann.map(|a| a.summary.clone()).unwrap_or_default(),
                    risks: ann.map(|a| a.risks.clone()).unwrap_or_default(),
                }
            })
            .collect();
        Self { summary: am.summary(), changes }
    }
}

// ── formatting payloads ───────────────────────────────────────────────────────

/// Run-formatting command (mirror of `RunFormat`); every field optional - set only what changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunFormatDto {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub italic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub underline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub strike: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub highlight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub vert_align: Option<String>,
}

impl From<RunFormatDto> for RunFormat {
    fn from(d: RunFormatDto) -> Self {
        RunFormat {
            bold: d.bold,
            italic: d.italic,
            underline: d.underline,
            strike: d.strike,
            size: d.size,
            color: d.color,
            font: d.font,
            highlight: d.highlight,
            vert_align: d.vert_align,
        }
    }
}

fn align_from_str(s: &str) -> Option<Align> {
    match s {
        "left" => Some(Align::Left),
        "center" => Some(Align::Center),
        "right" => Some(Align::Right),
        "justify" => Some(Align::Justify),
        _ => None,
    }
}

/// Paragraph-formatting payload (mirror of `ParaProps`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParaPropsDto {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub align: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line_spacing: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub space_before: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub space_after: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub indent_left: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub indent_right: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub indent_first: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub num_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub num_ilvl: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shading: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tab_stops: Vec<u32>,
}

impl From<ParaPropsDto> for ParaProps {
    fn from(d: ParaPropsDto) -> Self {
        ParaProps {
            align: d.align.as_deref().and_then(align_from_str),
            line_spacing: d.line_spacing,
            space_before: d.space_before,
            space_after: d.space_after,
            indent_left: d.indent_left,
            indent_right: d.indent_right,
            indent_first: d.indent_first,
            num_id: d.num_id,
            num_ilvl: d.num_ilvl,
            shading: d.shading,
            tab_stops: d.tab_stops,
            // Everything else (keep-next, contextual spacing, page/section/column breaks, text
            // frames, paragraph borders) is layout, structural, or round-trip state - not something
            // the agent wire sets. Defaulted so a new `ParaProps` field extends the model without
            // (silently) extending the wire surface.
            ..ParaProps::default()
        }
    }
}

// ── proposals (DTO -> engine) ─────────────────────────────────────────────────

/// One operation in a [`ProposalDto`] - the wire form of [`ProposalOp`]. `at` / `to` are anchor
/// tokens; `range` is an [`AnchorRangeDto`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProposalOpDto {
    Insert { at: String, text: String },
    Delete { range: AnchorRangeDto },
    Replace { range: AnchorRangeDto, text: String },
    Format { range: AnchorRangeDto, format: RunFormatDto },
    ParagraphFormat { at: String, props: ParaPropsDto },
    Style { at: String, style: Option<String> },
    Numbering { at: String, num_id: Option<i32>, ilvl: Option<i32> },
    Split { at: String },
    Join { at: String },
    Comment { range: AnchorRangeDto, text: String },
    Move { from: AnchorRangeDto, to: String },
}

impl ProposalOpDto {
    fn decode(self) -> Result<ProposalOp> {
        Ok(match self {
            ProposalOpDto::Insert { at, text } => {
                ProposalOp::Insert { at: anchor_from_token(&at)?, text }
            }
            ProposalOpDto::Delete { range } => ProposalOp::Delete { range: range.decode()? },
            ProposalOpDto::Replace { range, text } => {
                ProposalOp::Replace { range: range.decode()?, text }
            }
            ProposalOpDto::Format { range, format } => {
                ProposalOp::Format { range: range.decode()?, format: format.into() }
            }
            ProposalOpDto::ParagraphFormat { at, props } => {
                ProposalOp::ParagraphFormat { at: anchor_from_token(&at)?, props: props.into() }
            }
            ProposalOpDto::Style { at, style } => {
                ProposalOp::Style { at: anchor_from_token(&at)?, style }
            }
            ProposalOpDto::Numbering { at, num_id, ilvl } => {
                ProposalOp::Numbering { at: anchor_from_token(&at)?, num_id, ilvl }
            }
            ProposalOpDto::Split { at } => ProposalOp::Split { at: anchor_from_token(&at)? },
            ProposalOpDto::Join { at } => ProposalOp::Join { at: anchor_from_token(&at)? },
            ProposalOpDto::Comment { range, text } => {
                ProposalOp::Comment { range: range.decode()?, text }
            }
            ProposalOpDto::Move { from, to } => {
                ProposalOp::Move { from: from.decode()?, to: anchor_from_token(&to)? }
            }
        })
    }
}

/// A batch of operations to submit (mirror of [`Proposal`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalDto {
    pub base_revision: u64,
    pub title: String,
    pub ops: Vec<ProposalOpDto>,
}

impl ProposalDto {
    /// Decode to an engine [`Proposal`], failing if any anchor token is malformed.
    pub fn decode(self) -> Result<Proposal> {
        let ops = self.ops.into_iter().map(ProposalOpDto::decode).collect::<Result<Vec<_>>>()?;
        Ok(Proposal { base_revision: self.base_revision, title: self.title, ops })
    }
}

/// The outcome of a proposal (mirror of [`ProposalResult`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProposalResultDto {
    Applied { revision: u64, change_ids: Vec<u64> },
    Stale { current: u64 },
    Invalid { index: usize, reason: String },
}

impl From<ProposalResult> for ProposalResultDto {
    fn from(r: ProposalResult) -> Self {
        match r {
            ProposalResult::Applied { revision, change_ids } => {
                ProposalResultDto::Applied { revision, change_ids }
            }
            ProposalResult::Stale { current } => ProposalResultDto::Stale { current },
            ProposalResult::Invalid { index, reason } => ProposalResultDto::Invalid { index, reason },
        }
    }
}

// ── events ─────────────────────────────────────────────────────────────────────

/// The wire label for an [`AgentAction`].
pub fn action_str(action: AgentAction) -> &'static str {
    match action {
        AgentAction::Insert => "insert",
        AgentAction::Delete => "delete",
        AgentAction::Replace => "replace",
        AgentAction::Format => "format",
        AgentAction::ParagraphFormat => "paragraph_format",
        AgentAction::Style => "style",
        AgentAction::Numbering => "numbering",
        AgentAction::Split => "split",
        AgentAction::Join => "join",
        AgentAction::Move => "move",
        AgentAction::AddComment => "add_comment",
        AgentAction::ReplyComment => "reply_comment",
        AgentAction::ResolveComment => "resolve_comment",
        AgentAction::DeleteComment => "delete_comment",
        AgentAction::AcceptChange => "accept_change",
        AgentAction::RejectChange => "reject_change",
        AgentAction::AcceptAll => "accept_all",
        AgentAction::RejectAll => "reject_all",
        AgentAction::InsertTableRow => "insert_table_row",
        AgentAction::DeleteTableRow => "delete_table_row",
        AgentAction::InsertTableColumn => "insert_table_column",
        AgentAction::DeleteTableColumn => "delete_table_column",
        AgentAction::AddBookmark => "add_bookmark",
        AgentAction::AddHyperlink => "add_hyperlink",
        AgentAction::RemoveHyperlink => "remove_hyperlink",
        AgentAction::InsertImage => "insert_image",
        AgentAction::EditImage => "edit_image",
        AgentAction::RemoveImage => "remove_image",
        AgentAction::SubmitProposal => "submit_proposal",
    }
}

/// An observed agent action (mirror of [`AgentEvent`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEventDto {
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub on_behalf_of: Option<String>,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rationale: Option<String>,
}

impl From<&AgentEvent> for AgentEventDto {
    fn from(e: &AgentEvent) -> Self {
        Self {
            author: e.author.clone(),
            on_behalf_of: e.on_behalf_of.clone(),
            action: action_str(e.action).to_string(),
            node_id: e.node_id.as_ref().map(|n| n.to_string()),
            rationale: e.rationale.clone(),
        }
    }
}
