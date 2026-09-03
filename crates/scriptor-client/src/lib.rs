//! Rust client library for services and agents - the agent peer.
//!
//! [`AgentPeer`] lets an AI agent participate in a live document as a **headless loro peer**: it
//! holds its own replica (its own random `PeerID` via `CollabDoc::new`) and writes *tracked-change
//! marks* attributed to itself, syncing via the same opaque-update-bytes path as human peers (the
//! server relay - the bytes are interchangeable). This is the path the `document.*` MCP tools call,
//! so an agent co-edits as a first-class participant without a real-time editor UI.
//!
//! Identity is two-layered: the ephemeral `PeerID` is just a
//! replica handle; the durable, *replicated* audit lives in the loro commit message and the
//! `author`/`date` carried in every mark. A suggestion is always a tracked change - the agent
//! never edits text in place - so a human accepts or rejects it with the accept/reject engine.

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use scriptor_crdt::{
    Anchor, AnchorRange, ChangeSummary, CollabDoc, Comment, CommentLocation, DocSnapshot, NodeContent,
    NodeId, ParaProps, Paragraph, Resolved, Run, RunFormat, Side, TextMatch, TrackKind,
};
use scriptor_edit::{apply, Actor, EditContext, EditMode, EditOp};

pub mod wire;

mod peer;

// Document comparison (blacklining) - the agent's "redline this revision against the live doc" path,
// plus the semantic overlay schema. Re-exported so a caller doesn't need to depend on
// `scriptor-compare` directly.
pub use scriptor_compare::{
    self, AnnotatedManifest, Annotation, CompareOptions, CompareResult, Manifest, Materiality,
};

/// Compare two arbitrary `.docx` documents (the "what changed between v3 and v7" path, independent of
/// any live peer) and produce a redline + change manifest, attributing every revision to `author`.
pub fn compare_docx(original: &[u8], revised: &[u8], author: &str) -> Result<CompareResult> {
    let opts = CompareOptions { author: author.to_string(), ..Default::default() };
    scriptor_compare::compare(original, revised, &opts)
}

/// Attach an LLM's semantic annotations to a comparison's `manifest` and render the annotated result
/// for the wire. Enforces the trust boundary - a citation to a non-existent change is rejected - so
/// the overlay can describe the redline but never invent or alter it. The LLM produces `annotations`
/// (materiality / category / summary / risks per change) from [`Manifest::to_json`]; the redline
/// itself is unchanged.
pub fn annotate_comparison(
    manifest: Manifest,
    annotations: Vec<Annotation>,
) -> Result<wire::AnnotatedCompareResultDto> {
    Ok(wire::AnnotatedCompareResultDto::from(&manifest.annotate(annotations)?))
}

/// One operation in a [`Proposal`] - anchor-addressed, applied as a tracked suggestion. The anchors
/// encode to bytes, so a proposal can be built out-of-process and submitted over a wire.
#[derive(Debug, Clone)]
pub enum ProposalOp {
    Insert { at: Anchor, text: String },
    Delete { range: AnchorRange },
    Replace { range: AnchorRange, text: String },
    Format { range: AnchorRange, format: RunFormat },
    ParagraphFormat { at: Anchor, props: ParaProps },
    Style { at: Anchor, style: Option<String> },
    Numbering { at: Anchor, num_id: Option<i32>, ilvl: Option<i32> },
    Split { at: Anchor },
    Join { at: Anchor },
    Comment { range: AnchorRange, text: String },
    Move { from: AnchorRange, to: Anchor },
}

/// A set of operations the agent submits as one unit, against the `revision` it read - the human
/// reviews them as a group. Borrowed from Google Docs `batchUpdate`: validate-first, all-or-nothing,
/// with an optimistic-concurrency token. See [`AgentPeer::submit_proposal`].
#[derive(Debug, Clone)]
pub struct Proposal {
    /// The [`AgentPeer::revision`] the agent read before computing these ops. If the document has
    /// moved since (a concurrent edit), the proposal is rejected as [`ProposalResult::Stale`].
    pub base_revision: u64,
    /// Human-facing summary of the whole proposal (e.g. "Tighten the intro, fix 3 defined terms");
    /// also the rationale stamped on each change.
    pub title: String,
    pub ops: Vec<ProposalOp>,
}

/// The outcome of [`AgentPeer::submit_proposal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalResult {
    /// Every op applied. `revision` is the new version; `change_ids` are the tracked-change / comment
    /// ids produced (for later accept/reject or grouping).
    Applied { revision: u64, change_ids: Vec<u64> },
    /// The document moved since `base_revision` (a concurrent edit) - nothing applied. Re-read and
    /// rebuild against `current`.
    Stale { current: u64 },
    /// Op `index` failed validation (`reason`) - nothing applied (all-or-nothing).
    Invalid { index: usize, reason: String },
}

/// A governance hook's verdict on a proposed agent action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

/// A typed description of a mutating action the agent is about to take - what a [`AgentPolicy`]
/// authorizes and an [`AgentEvent`] reports. Cheap to clone / match; the document detail (which text,
/// which anchor) rides on the call itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAction {
    Insert,
    Delete,
    Replace,
    Format,
    ParagraphFormat,
    Style,
    Numbering,
    Split,
    Join,
    Move,
    AddComment,
    ReplyComment,
    ResolveComment,
    DeleteComment,
    AcceptChange,
    RejectChange,
    AcceptAll,
    RejectAll,
    InsertTableRow,
    DeleteTableRow,
    InsertTableColumn,
    DeleteTableColumn,
    AddBookmark,
    AddHyperlink,
    RemoveHyperlink,
    InsertImage,
    EditImage,
    RemoveImage,
    SubmitProposal,
}

/// A governance hook: decides whether the agent may perform an action *before* it touches the
/// document. Register with [`AgentPeer::add_policy`]; all registered policies must allow an action.
/// `node_id` is the body paragraph the action targets (when it is anchored to one), so a policy can be
/// **content-aware** - e.g. refuse edits to a protected clause / signature block - not just verb-scoped.
/// Implementations must be **pure / deterministic** (the same action authorizes the same way), because
/// an action can be authorized more than once - a proposal validates every op, then applies it. This
/// is the first-class veto seam; attribution (`w:author` + the commit rationale) already rides on
/// every change, so a policy adds *control* on top of the audit trail.
pub trait AgentPolicy {
    fn authorize(&self, action: &AgentAction, node_id: Option<NodeId>) -> Decision;
}

/// An observation of something the agent did, delivered to every registered [`EventSink`] *after* the
/// action succeeds. The integrator's read-side seam (audit feed, presence). Carries enough to build an
/// audit record without re-querying: who (`author` = the agent; `on_behalf_of` = the human principal,
/// when set), what (`action`), where (`node_id`), and why (`rationale`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEvent {
    pub author: String,
    pub on_behalf_of: Option<String>,
    pub action: AgentAction,
    pub node_id: Option<NodeId>,
    pub rationale: Option<String>,
}

/// A sink for [`AgentEvent`]s. Register with [`AgentPeer::add_sink`]; every sink receives every event.
pub trait EventSink {
    fn emit(&self, event: &AgentEvent);
}

/// Something **another peer** (a human reviewer, or another agent) did, surfaced to this agent when it
/// merges their updates - the inbound dual of [`AgentEvent`] (which reports what *this* agent did). It
/// lets an agent close the loop: learn that its suggestion was accepted or rejected, that a human added
/// their own change, or that a comment arrived - so it can advance, stop re-proposing, or notify the
/// user. Produced by [`AgentPeer::merge_observed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// A tracked change that was pending before the merge is no longer pending - a reviewer resolved it.
    /// `accepted` is a best-effort classification (`Some(true)` accepted, `Some(false)` rejected, `None`
    /// when it can't be told from text alone, e.g. a formatting / move / table change): for an insertion
    /// the text surviving as accepted content means accept; for a deletion the text surviving means
    /// reject. Duplicate identical text elsewhere can fool the heuristic.
    ChangeResolved { id: u64, kind: String, author: String, accepted: Option<bool> },
    /// A new tracked change appeared (another peer's edit), not authored by this agent.
    ChangeAdded { id: u64, kind: String, author: String },
    /// A new comment appeared (another peer's), not authored by this agent.
    CommentAdded { id: u64, author: String },
}

/// A sink for [`Observation`]s. Register with [`AgentPeer::add_observation_sink`]; every sink receives
/// every observation synthesized by [`AgentPeer::merge_observed`].
pub trait ObservationSink {
    fn observe(&self, observation: &Observation);
}

/// Which story of the document an action addresses. The body is the default and the full surface
/// (tables, move, numbering). The header and footer are separate stories (child documents) that edit
/// through the same tracked-change path - reach them with [`AgentPeer::region`]. An [`Anchor`] is bound
/// to the story it was created in: a body anchor does not resolve in the header and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Body,
    Header,
    Footer,
}

impl Region {
    /// A short label for the audit trail (`"body"` / `"header"` / `"footer"`).
    fn label(self) -> &'static str {
        match self {
            Region::Body => "body",
            Region::Header => "header",
            Region::Footer => "footer",
        }
    }
}

/// The [`AgentAction`] a [`ProposalOp`] performs - so a proposal authorizes every op against policy
/// during its validate pass.
fn proposal_op_action(op: &ProposalOp) -> AgentAction {
    match op {
        ProposalOp::Insert { .. } => AgentAction::Insert,
        ProposalOp::Delete { .. } => AgentAction::Delete,
        ProposalOp::Replace { .. } => AgentAction::Replace,
        ProposalOp::Format { .. } => AgentAction::Format,
        ProposalOp::ParagraphFormat { .. } => AgentAction::ParagraphFormat,
        ProposalOp::Style { .. } => AgentAction::Style,
        ProposalOp::Numbering { .. } => AgentAction::Numbering,
        ProposalOp::Split { .. } => AgentAction::Split,
        ProposalOp::Join { .. } => AgentAction::Join,
        ProposalOp::Comment { .. } => AgentAction::AddComment,
        ProposalOp::Move { .. } => AgentAction::Move,
    }
}

/// An agent (or service) editing a document as a headless peer.
pub struct AgentPeer {
    doc: CollabDoc,
    author: String,
    /// The human principal the agent acts for (audit `on_behalf_of`), when known.
    on_behalf_of: Option<String>,
    /// Governance hooks consulted before every mutating action (all must allow).
    policies: Vec<Box<dyn AgentPolicy>>,
    /// Observers notified after every successful mutating action (this agent's own actions).
    sinks: Vec<Box<dyn EventSink>>,
    /// Observers notified about what *other* peers did, synthesized on [`merge_observed`](AgentPeer::merge_observed).
    observation_sinks: Vec<Box<dyn ObservationSink>>,
}

impl AgentPeer {
    /// A peer over a fresh, empty document, identified by `author` (e.g. `"AI Agent (legal-bot)"`).
    pub fn new(author: impl Into<String>) -> Self {
        Self::wrap(CollabDoc::new(), author)
    }

    /// Build a peer around an existing document with no policies / sinks yet.
    fn wrap(doc: CollabDoc, author: impl Into<String>) -> Self {
        Self {
            doc,
            author: author.into(),
            on_behalf_of: None,
            policies: Vec::new(),
            sinks: Vec::new(),
            observation_sinks: Vec::new(),
        }
    }

    /// Record the human principal this agent acts for - carried into every [`AgentEvent`] as
    /// `on_behalf_of` so the integrator's audit log can attribute the human behind the agent. Chainable.
    pub fn on_behalf_of(mut self, principal: impl Into<String>) -> Self {
        self.on_behalf_of = Some(principal.into());
        self
    }

    /// Register a governance policy (composable - every registered policy must allow an action, else
    /// it's refused). Chainable.
    pub fn add_policy(mut self, policy: Box<dyn AgentPolicy>) -> Self {
        self.policies.push(policy);
        self
    }

    /// Register an event sink (every sink receives every event). Chainable.
    pub fn add_sink(mut self, sink: Box<dyn EventSink>) -> Self {
        self.sinks.push(sink);
        self
    }

    /// Register an observation sink - notified about what *other* peers did, each time the agent calls
    /// [`merge_observed`](Self::merge_observed). Chainable.
    pub fn add_observation_sink(mut self, sink: Box<dyn ObservationSink>) -> Self {
        self.observation_sinks.push(sink);
        self
    }

    /// Join a live document by loading the snapshot the relay sends on connect. Carries paragraph
    /// text, tracked changes, and comments (everything in the loro op log). Note: table *structure* is
    /// not yet a loro citizen, so a joined peer does not see tables - load a document with tables via
    /// [`from_docx_bytes`](Self::from_docx_bytes) instead (the standalone editing path).
    pub fn join(author: impl Into<String>, snapshot: &[u8]) -> Result<Self> {
        let doc = CollabDoc::new();
        doc.merge(snapshot)?;
        Ok(Self::wrap(doc, author))
    }

    /// Load a full document from `.docx` bytes - the standalone editing path: the agent fetches the
    /// document it is asked to edit (tables, bookmarks, links and all), proposes its redline, and
    /// returns the result via [`to_docx_bytes`](Self::to_docx_bytes). Use this (not [`join`](Self::join))
    /// whenever the work touches tables, which the live relay does not yet carry.
    pub fn from_docx_bytes(author: impl Into<String>, bytes: &[u8]) -> Result<Self> {
        Ok(Self::wrap(CollabDoc::from_docx_bytes(bytes)?, author))
    }

    /// Load from `word/document.xml` bytes (the modeled subset; lighter than a full `.docx` when the
    /// caller already has the part).
    pub fn from_document_xml(author: impl Into<String>, xml: &[u8]) -> Result<Self> {
        Ok(Self::wrap(CollabDoc::from_document_xml(xml)?, author))
    }

    /// Serialize the (edited) document back to `.docx` bytes - how a standalone agent returns its work.
    pub fn to_docx_bytes(&self) -> Result<Vec<u8>> {
        self.doc.to_docx_bytes()
    }

    /// Compare this document against a `revised` `.docx` and produce a **redline** - this document with
    /// every difference as a tracked change attributed to the agent (`author`) - plus the change
    /// manifest. This is the agent's blacklining path: draft a revision, `compare_with` the live doc,
    /// reason over the manifest, then present the redline (an ordinary Word tracked-changes document a
    /// human accepts/rejects) or inject its changes. The manifest is deterministic
    /// and provably consistent with the redline; the binary redline is returned alongside it (served
    /// out-of-band like [`to_docx_bytes`](Self::to_docx_bytes)). See [`compare_with_dto`](Self::compare_with_dto)
    /// for the JSON reasoning surface.
    pub fn compare_with(&self, revised: &[u8]) -> Result<CompareResult> {
        let original = self.doc.to_docx_bytes()?;
        let opts = CompareOptions { author: self.author.clone(), ..Default::default() };
        scriptor_compare::compare(&original, revised, &opts)
    }

    /// The agent's actor identity (carried into every mark + the audit log).
    pub fn author(&self) -> &str {
        &self.author
    }

    // ── perception + addressing ──────────────────────────────────────────────────────────────────

    // ── content suggestions (anchor-addressed; always tracked + attributed) ───────────────────────

    // ── comments ─────────────────────────────────────────────────────────────────────────────────

    // ── review / triage (accept / reject) ────────────────────────────────────────────────────────

    // ── proposals (atomic, validate-first batch with optimistic concurrency) ──────────────────────

    // ── wire DTOs (the language-agnostic boundary: JSON in / JSON out) ────────────────────────────

    // ── region wire surface (header / footer over the wire; `region` is "body"/"header"/"footer") ──
    //
    // Mirrors the RegionView surface as DTOs so a non-Rust integrator can perceive + redline the
    // header/footer story too. Anchors created via `region_anchor*` are bound to that region.

    // ── tables (anchor sits in a cell; structural edits are tracked) ──────────────────────────────

    // ── bookmarks + hyperlinks (direct edits, matching the editor - not redline in v1) ────────────

    // ── pictures (insert / delete are tracked redlines; geometry edits are direct) ─────────────────

    // ── internals ────────────────────────────────────────────────────────────────────────────────

    /// Run `perform` as a governed action: every policy must allow `action` (else it's refused and
    /// nothing runs), and on success every sink is notified. The single funnel every mutating method
    /// passes through, so policy + observation coverage is uniform.
    fn guard<T>(
        &self,
        action: AgentAction,
        node_id: Option<NodeId>,
        rationale: Option<&str>,
        perform: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if let Err(reason) = self.check(action, node_id.clone()) {
            return Err(anyhow!("agent action {action:?} denied by policy: {reason}"));
        }
        let out = perform()?;
        self.notify(action, node_id, rationale);
        Ok(out)
    }

    /// Ask every policy about `action` on `node_id`; the first denial's reason wins, else `Ok`.
    fn check(&self, action: AgentAction, node_id: Option<NodeId>) -> std::result::Result<(), String> {
        for p in &self.policies {
            if let Decision::Deny(reason) = p.authorize(&action, node_id.clone()) {
                return Err(reason);
            }
        }
        Ok(())
    }

    /// Deliver an enriched event for `action` to every registered sink.
    fn notify(&self, action: AgentAction, node_id: Option<NodeId>, rationale: Option<&str>) {
        if self.sinks.is_empty() {
            return;
        }
        let event = AgentEvent {
            author: self.author.clone(),
            on_behalf_of: self.on_behalf_of.clone(),
            action,
            node_id,
            rationale: rationale.map(|s| s.to_string()),
        };
        for s in &self.sinks {
            s.emit(&event);
        }
    }

    /// A synced audit/commit message for a direct CollabDoc call (move / comment / review), matching
    /// the `scriptor-edit` actor-attributed format.
    fn audit(&self, verb: &str, note: &str) -> String {
        if note.is_empty() {
            format!("{} (Agent): {verb}", self.author)
        } else {
            format!("{} (Agent): {verb} - {note}", self.author)
        }
    }

    /// Build this agent's edit context (tracked-mode, attributed to the agent) - the same
    /// `scriptor-edit` path a human editor uses, which is what makes the agent a first-class peer.
    fn ctx(&self, date: &str, rationale: &str) -> EditContext {
        EditContext {
            actor: Actor::agent(self.author.clone(), self.author.clone()),
            mode: EditMode::Tracked,
            at: date.to_string(),
            rationale: Some(rationale.to_string()),
        }
    }

    /// A governance-free peer over a snapshot-isolated fork of this document, for trial application (see
    /// [`submit_proposal`](Self::submit_proposal)). Policies + sinks are empty (the real submit already
    /// checked policy and only fires sinks on the committed apply); identity is preserved so trial
    /// revision-id allocation matches the real apply.
    fn trial(&self) -> Result<AgentPeer> {
        Ok(AgentPeer {
            doc: self.doc.trial_fork()?,
            author: self.author.clone(),
            on_behalf_of: self.on_behalf_of.clone(),
            policies: Vec::new(),
            sinks: Vec::new(),
            observation_sinks: Vec::new(),
        })
    }

    /// Export the agent's state for the relay: a full snapshot (the simplest correct merge unit);
    /// version-vector diffs are the production optimization.
    pub fn export(&self) -> Result<Vec<u8>> {
        self.doc.snapshot()
    }

    /// Merge bytes received from the relay (other peers' updates).
    pub fn merge(&self, bytes: &[u8]) -> Result<()> {
        self.doc.merge(bytes)
    }

    /// Merge other peers' updates **and observe what they did**: diff the tracked-change + comment state
    /// across the merge, synthesize an [`Observation`] for each transition (a pending change resolved -
    /// accepted or rejected; a new change or comment added by someone else), deliver them to every
    /// registered observation sink, and return them. This is the agent's feedback loop (audit H3): the
    /// inbound counterpart to the [`AgentEvent`]s it emits for its own actions. Bodies only (the body
    /// story is what the relay carries); header/footer review is local.
    pub fn merge_observed(&self, bytes: &[u8]) -> Result<Vec<Observation>> {
        let before = self.list_changes()?;
        let before_ids: HashSet<u64> = before.iter().map(|c| c.id).collect();
        let before_comments: HashSet<u64> = self.comments().iter().map(|c| c.id).collect();

        self.doc.merge(bytes)?;

        let after = self.list_changes()?;
        let after_ids: HashSet<u64> = after.iter().map(|c| c.id).collect();
        // Post-merge accepted/visible text (excludes still-pending deletions), for classifying a
        // resolved change as accepted vs rejected.
        let visible = self.visible_text()?;

        let mut out = Vec::new();
        // Resolved: pending before, gone after.
        for c in &before {
            if after_ids.contains(&c.id) {
                continue;
            }
            let accepted = if c.text.is_empty() {
                None // formatting / move / table changes carry no text to classify on
            } else if c.kind == "ins" {
                Some(visible.contains(&c.text))
            } else if c.kind == "del" {
                Some(!visible.contains(&c.text))
            } else {
                None
            };
            out.push(Observation::ChangeResolved {
                id: c.id,
                kind: c.kind.clone(),
                author: c.author.clone(),
                accepted,
            });
        }
        // Added by someone else: present after, absent before, not this agent's own.
        for c in &after {
            if !before_ids.contains(&c.id) && c.author != self.author {
                out.push(Observation::ChangeAdded {
                    id: c.id,
                    kind: c.kind.clone(),
                    author: c.author.clone(),
                });
            }
        }
        for cm in self.comments() {
            if !before_comments.contains(&cm.id) && cm.author != self.author {
                out.push(Observation::CommentAdded { id: cm.id, author: cm.author.clone() });
            }
        }

        for obs in &out {
            for s in &self.observation_sinks {
                s.observe(obs);
            }
        }
        Ok(out)
    }

    // ── regions (header / footer stories) ─────────────────────────────────────────────────────────

}

/// A region-scoped view of a document for an [`AgentPeer`] (obtained from [`AgentPeer::region`]). It
/// exposes the perception, tracked-edit, comment, and review surface that applies to any story - so the
/// agent can read and redline the **header / footer** the way it does the body. Tables, moves, and
/// numbering are body-only by nature and are not on this view. Anchors created here resolve here; an
/// anchor from another story is refused as stale. Governance + identity are the peer's; the audit
/// message records which region the edit touched.
pub struct RegionView<'a> {
    peer: &'a AgentPeer,
    doc: &'a CollabDoc,
    region: Region,
}

impl RegionView<'_> {
    /// Which story this view addresses.
    pub fn region(&self) -> Region {
        self.region
    }

    // ── perception ─────────────────────────────────────────────────────────────────────────────

    /// This story's paragraphs (materialized).
    pub fn paragraphs(&self) -> Result<Vec<Paragraph>> {
        self.doc.paragraphs()
    }

    /// This story's full text (paragraphs joined by newlines) - the cheap read for a short header/footer.
    pub fn text(&self) -> Result<String> {
        Ok(self
            .doc
            .paragraphs()?
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Find every occurrence of `query` in this story (case-insensitive unless `match_case`).
    pub fn find(&self, query: &str, match_case: bool) -> Result<Vec<TextMatch>> {
        self.doc.find_text(query, match_case)
    }

    /// A token-budgeted outline of this story (see [`AgentPeer::outline`]).
    pub fn outline(&self, preview_chars: usize, offset: usize, max_nodes: usize) -> Result<DocSnapshot> {
        self.doc.outline(preview_chars, offset, max_nodes)
    }

    /// One node's full content in this story.
    pub fn read_node(&self, node_id: &NodeId) -> Result<Option<NodeContent>> {
        self.doc.read_node(node_id)
    }

    /// An edit-stable anchor in this story.
    pub fn anchor(&self, para: usize, off: usize, side: Side) -> Result<Anchor> {
        self.doc.anchor(para, off, side)
    }

    /// An edit-stable range in this story.
    pub fn anchor_range(&self, para: usize, start: usize, end: usize) -> Result<AnchorRange> {
        self.doc.anchor_range(para, start, end)
    }

    /// An edit-stable range within a node of this story (the read -> edit bridge).
    pub fn anchor_range_in_node(&self, node_id: &NodeId, start: usize, end: usize) -> Result<AnchorRange> {
        let para = self.doc.node_para(node_id).ok_or_else(|| anyhow!("node {node_id} is gone"))?;
        self.doc.anchor_range(para, start, end)
    }

    /// This story's version token.
    pub fn revision(&self) -> u64 {
        self.doc.revision()
    }

    /// This story's comments.
    pub fn comments(&self) -> Vec<Comment> {
        self.doc.comments()
    }

    /// This story's tracked changes.
    pub fn list_changes(&self) -> Result<Vec<ChangeSummary>> {
        self.doc.list_changes()
    }

    // ── tracked edits (governed + attributed, same as the body) ──────────────────────────────────

    /// Propose inserting `text` at `at` in this story as a tracked insertion.
    pub fn propose_insert(&self, at: &Anchor, text: &str, date: &str, rationale: &str) -> Result<u64> {
        let (para, pos) = self.point(at)?;
        self.peer.guard(AgentAction::Insert, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::InsertText { para, pos, text: text.to_string() };
            Ok(apply(self.doc, &self.peer.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Propose deleting the text under `range` (may span paragraphs) in this story as a tracked deletion.
    pub fn propose_delete(&self, range: &AnchorRange, date: &str, rationale: &str) -> Result<u64> {
        let (sp, so, ep, eo) = self.multi_span(range)?;
        self.peer.guard(AgentAction::Delete, self.doc.node_id(sp), Some(rationale), || {
            if sp == ep {
                let op = EditOp::DeleteRange { para: sp, range: so..eo };
                Ok(apply(self.doc, &self.peer.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
            } else {
                self.doc.suggest_deletion_multi(sp, so, ep, eo, self.peer.author(), date, &self.audit("delete", rationale))
            }
        })
    }

    /// Propose replacing the text under `range` (may span paragraphs) with `new_text` in this story.
    pub fn propose_replace(
        &self,
        range: &AnchorRange,
        new_text: &str,
        date: &str,
        rationale: &str,
    ) -> Result<(u64, u64)> {
        let (sp, so, ep, eo) = self.multi_span(range)?;
        self.peer.guard(AgentAction::Replace, self.doc.node_id(sp), Some(rationale), || {
            let del = if sp == ep {
                apply(self.doc, &self.peer.ctx(date, rationale), EditOp::DeleteRange { para: sp, range: so..eo })?
                    .revision_id
                    .unwrap_or(0)
            } else {
                self.doc.suggest_deletion_multi(sp, so, ep, eo, self.peer.author(), date, &self.audit("delete", rationale))?
            };
            let ins = apply(
                self.doc,
                &self.peer.ctx(date, rationale),
                EditOp::InsertText { para: sp, pos: so, text: new_text.to_string() },
            )?
            .revision_id
            .unwrap_or(0);
            Ok((del, ins))
        })
    }

    /// Propose run formatting over `range` (single paragraph) in this story as a tracked `w:rPrChange`.
    pub fn propose_format(
        &self,
        range: &AnchorRange,
        format: RunFormat,
        date: &str,
        rationale: &str,
    ) -> Result<u64> {
        let (para, s, e) = self.span(range)?;
        self.peer.guard(AgentAction::Format, self.doc.node_id(para), Some(rationale), || {
            let op = EditOp::ApplyRunFormat { para, range: s..e, format };
            Ok(apply(self.doc, &self.peer.ctx(date, rationale), op)?.revision_id.unwrap_or(0))
        })
    }

    /// Add a comment over `range` (may span paragraphs) in this story.
    pub fn add_comment(&self, range: &AnchorRange, text: &str, date: &str) -> Result<u64> {
        let (sp, so, ep, eo) = self.multi_span(range)?;
        self.peer.guard(AgentAction::AddComment, self.doc.node_id(sp), None, || {
            self.doc.add_comment(sp, so, ep, eo, text, self.peer.author(), date, &self.audit("add comment", ""))
        })
    }

    // ── review (accept / reject within this story) ───────────────────────────────────────────────

    /// Accept tracked change `id` in this story.
    pub fn accept_change(&self, id: u64) -> Result<bool> {
        self.peer.guard(AgentAction::AcceptChange, None, None, || {
            self.doc.accept_revision(id, &self.audit("accept change", ""))
        })
    }

    /// Reject tracked change `id` in this story.
    pub fn reject_change(&self, id: u64) -> Result<bool> {
        self.peer.guard(AgentAction::RejectChange, None, None, || {
            self.doc.reject_revision(id, &self.audit("reject change", ""))
        })
    }

    /// Accept every tracked change in this story.
    pub fn accept_all(&self) -> Result<usize> {
        self.peer.guard(AgentAction::AcceptAll, None, None, || {
            self.doc.accept_all(&self.audit("accept all changes", ""))
        })
    }

    /// Reject every tracked change in this story.
    pub fn reject_all(&self) -> Result<usize> {
        self.peer.guard(AgentAction::RejectAll, None, None, || {
            self.doc.reject_all(&self.audit("reject all changes", ""))
        })
    }

    // ── internals (resolve against THIS story's doc) ─────────────────────────────────────────────

    fn point(&self, anchor: &Anchor) -> Result<(usize, usize)> {
        match self.doc.resolve(anchor) {
            Resolved::Live { para, off } => Ok((para, off)),
            Resolved::Shifted { .. } | Resolved::Deleted => {
                Err(anyhow!("anchor is stale: its content was deleted or moved; re-locate via find"))
            }
        }
    }

    fn span(&self, range: &AnchorRange) -> Result<(usize, usize, usize)> {
        self.doc
            .resolve_range(range)
            .ok_or_else(|| anyhow!("anchor range is stale or torn across paragraphs"))
    }

    fn multi_span(&self, range: &AnchorRange) -> Result<(usize, usize, usize, usize)> {
        self.doc
            .resolve_range_multi(range)
            .ok_or_else(|| anyhow!("anchor range is stale (an end's content was deleted or moved)"))
    }

    /// A region-tagged audit message (so the trail records which story the edit touched).
    fn audit(&self, verb: &str, note: &str) -> String {
        self.peer.audit(&format!("{verb} [{}]", self.region.label()), note)
    }
}

/// Parse a wire `side` string into a [`Side`] - `"left"` (range head), `"right"` (tail), else middle.
fn side_from_str(s: &str) -> Side {
    match s {
        "left" => Side::Left,
        "right" => Side::Right,
        _ => Side::Middle,
    }
}

/// Parse a wire `region` string (`"body"` / `"header"` / `"footer"`) into a [`Region`].
fn region_from_str(s: &str) -> Result<Region> {
    match s {
        "body" => Ok(Region::Body),
        "header" => Ok(Region::Header),
        "footer" => Ok(Region::Footer),
        other => Err(anyhow!("unknown region {other:?} (expected body / header / footer)")),
    }
}

#[cfg(test)]
mod tests;
