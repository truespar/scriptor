//! The one edit path.
//!
//! Typed, attributed operations applied to a [`scriptor_crdt::CollabDoc`]. The canvas editor and the
//! headless agent both construct an [`EditOp`] + [`EditContext`] and call [`apply`] - identical
//! semantics, identical attribution, on one loro op log. This is the pillar that keeps agents
//! **first-class participants** rather than a bolted-on "edit at rest" path: edit semantics live
//! here (Rust), shared by both front-ends, never duplicated in the TS editor shell.

use std::ops::Range;

use anyhow::Result;
use scriptor_crdt::{CollabDoc, ParaProps, Run, RunFormat};

/// Whether an actor is a human or an automated agent (rendered + audited distinctly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorKind {
    Human,
    Agent,
}

/// Who is making an edit. Attribution is first-class on every operation.
#[derive(Clone, Debug)]
pub struct Actor {
    /// Stable identity (user id / agent id) for the audit trail.
    pub id: String,
    /// Display name stamped on tracked changes (OOXML `w:author`).
    pub display: String,
    pub kind: ActorKind,
}

impl Actor {
    pub fn human(id: impl Into<String>, display: impl Into<String>) -> Self {
        Self { id: id.into(), display: display.into(), kind: ActorKind::Human }
    }
    pub fn agent(id: impl Into<String>, display: impl Into<String>) -> Self {
        Self { id: id.into(), display: display.into(), kind: ActorKind::Agent }
    }
}

/// How an edit is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    /// Apply directly, no revision marks (normal human typing).
    Direct,
    /// Record as a tracked change (redline) attributed to the actor (suggesting mode; the agent's default).
    Tracked,
}

/// A typed edit - the single vocabulary the editor and the agent both speak.
#[derive(Clone, Debug)]
pub enum EditOp {
    /// Insert `text` at codepoint `pos` in paragraph `para`.
    InsertText { para: usize, pos: usize, text: String },
    /// Delete codepoint `range` in paragraph `para`.
    DeleteRange { para: usize, range: Range<usize> },
    /// Append a new paragraph built from `runs` (with an optional style).
    AppendParagraph { runs: Vec<Run>, style: Option<String> },
    /// Split paragraph `para` at codepoint `pos` (the Enter key): text from `pos` onward moves into
    /// a new paragraph inserted right after.
    SplitParagraph { para: usize, pos: usize },
    /// Join paragraph `para` into the previous one (Backspace at paragraph start / Delete at end).
    JoinParagraph { para: usize },
    /// Apply a run-formatting command over codepoint `range` in paragraph `para` (the Font group).
    ApplyRunFormat { para: usize, range: Range<usize>, format: RunFormat },
    /// Apply paragraph-level formatting to paragraph `para` (the Paragraph group).
    ApplyParagraphFormat { para: usize, props: ParaProps },
    /// Set (or clear, when `num_id` is `None`) paragraph `para`'s list numbering (`w:numPr`).
    SetNumbering { para: usize, num_id: Option<i32>, ilvl: Option<i32> },
    /// Set (or clear, when `style` is `None` -> Normal) paragraph `para`'s named style (`w:pStyle`).
    SetParagraphStyle { para: usize, style: Option<String> },
}

/// Who / how / when / why for an applied operation.
#[derive(Clone, Debug)]
pub struct EditContext {
    pub actor: Actor,
    pub mode: EditMode,
    /// ISO-8601 timestamp stamped on tracked changes. Callers supply it - the engine never invents time.
    pub at: String,
    /// Optional rationale recorded in the synced commit message / audit trail.
    pub rationale: Option<String>,
}

impl EditContext {
    fn audit(&self, verb: &str) -> String {
        match &self.rationale {
            Some(r) => format!("{} ({:?}): {verb} - {r}", self.actor.display, self.actor.kind),
            None => format!("{} ({:?}): {verb}", self.actor.display, self.actor.kind),
        }
    }
}

/// What an applied op produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditOutcome {
    /// The allocated revision id, for tracked changes.
    pub revision_id: Option<u64>,
    /// A suggested caret position (codepoint offset) in the affected paragraph - set by structural
    /// ops where the natural caret isn't obvious to the caller (e.g. a join's merge point).
    pub caret: Option<usize>,
}

/// Apply one operation under `ctx`. **The** edit path - editor and agent both route through here.
pub fn apply(doc: &CollabDoc, ctx: &EditContext, op: EditOp) -> Result<EditOutcome> {
    match op {
        EditOp::InsertText { para, pos, text } => match ctx.mode {
            EditMode::Tracked => {
                let id = doc.suggest_insertion(
                    para, pos, &text, &ctx.actor.display, &ctx.at, &ctx.audit("insert"),
                )?;
                Ok(EditOutcome { revision_id: Some(id), ..Default::default() })
            }
            EditMode::Direct => {
                doc.insert_text(para, pos, &text, &ctx.audit("insert"))?;
                Ok(EditOutcome::default())
            }
        },
        EditOp::DeleteRange { para, range } => match ctx.mode {
            EditMode::Tracked => {
                let id = doc.suggest_deletion(
                    para, range, &ctx.actor.display, &ctx.at, &ctx.audit("delete"),
                )?;
                Ok(EditOutcome { revision_id: Some(id), ..Default::default() })
            }
            EditMode::Direct => {
                doc.delete_text(para, range, &ctx.audit("delete"))?;
                Ok(EditOutcome::default())
            }
        },
        EditOp::AppendParagraph { runs, style } => {
            doc.append_paragraph(&runs, style.as_deref())?;
            Ok(EditOutcome::default())
        }
        // Enter (split): tracked records an inserted ¶ revision (the split is still applied + visible).
        EditOp::SplitParagraph { para, pos } => match ctx.mode {
            EditMode::Tracked => {
                let id = doc.suggest_split(
                    para, pos, &ctx.actor.display, &ctx.at, &ctx.audit("split paragraph"),
                )?;
                Ok(EditOutcome { revision_id: Some(id), ..Default::default() })
            }
            EditMode::Direct => {
                doc.split_paragraph(para, pos, &ctx.audit("split paragraph"))?;
                Ok(EditOutcome::default())
            }
        },
        // Backspace-join: tracked records a deleted ¶ revision (non-destructive; the paragraphs stay
        // separate until accepted). `None` caret = refused (crosses a table-cell boundary).
        EditOp::JoinParagraph { para } => {
            let at = match ctx.mode {
                EditMode::Tracked => {
                    doc.suggest_join(para, &ctx.actor.display, &ctx.at, &ctx.audit("join paragraph"))?
                }
                EditMode::Direct => doc.join_paragraph(para, &ctx.audit("join paragraph"))?,
            };
            Ok(EditOutcome { caret: at, ..Default::default() })
        }
        // Run formatting: tracked as a `w:rPrChange` when suggesting, else applied directly.
        EditOp::ApplyRunFormat { para, range, format } => match ctx.mode {
            EditMode::Tracked => {
                let id = doc.suggest_format(
                    para, range, &format, &ctx.actor.display, &ctx.at, &ctx.audit("format"),
                )?;
                Ok(EditOutcome { revision_id: Some(id), ..Default::default() })
            }
            EditMode::Direct => {
                doc.apply_run_format(para, range, &format, &ctx.audit("format"))?;
                Ok(EditOutcome::default())
            }
        },
        // Paragraph formatting: tracked as a `w:pPrChange` when suggesting, else applied directly.
        EditOp::ApplyParagraphFormat { para, props } => match ctx.mode {
            EditMode::Tracked => {
                let id = doc.suggest_paragraph_format(
                    para, &props, &ctx.actor.display, &ctx.at, &ctx.audit("paragraph format"),
                )?;
                Ok(EditOutcome { revision_id: Some(id), ..Default::default() })
            }
            EditMode::Direct => {
                doc.apply_paragraph_format(para, &props, &ctx.audit("paragraph format"))?;
                Ok(EditOutcome::default())
            }
        },
        // Numbering change: tracked as a `w:pPrChange` (a numbering change is a paragraph-property
        // change), else applied directly.
        EditOp::SetNumbering { para, num_id, ilvl } => match ctx.mode {
            EditMode::Tracked => {
                let id = doc.suggest_numbering(
                    para, num_id, ilvl, &ctx.actor.display, &ctx.at, &ctx.audit("numbering"),
                )?;
                Ok(EditOutcome { revision_id: Some(id), ..Default::default() })
            }
            EditMode::Direct => {
                doc.set_numbering(para, num_id, ilvl, &ctx.audit("numbering"))?;
                Ok(EditOutcome::default())
            }
        },
        EditOp::SetParagraphStyle { para, style } => match ctx.mode {
            EditMode::Tracked => {
                let id = doc.suggest_paragraph_style(
                    para, style.as_deref(), &ctx.actor.display, &ctx.at, &ctx.audit("style"),
                )?;
                Ok(EditOutcome { revision_id: Some(id), ..Default::default() })
            }
            EditMode::Direct => {
                doc.set_paragraph_style(para, style.as_deref(), &ctx.audit("style"))?;
                Ok(EditOutcome::default())
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(mode: EditMode) -> EditContext {
        EditContext {
            actor: Actor::human("u1", "Alice"),
            mode,
            at: "2026-06-19T00:00:00Z".into(),
            rationale: None,
        }
    }
    fn agent(mode: EditMode) -> EditContext {
        EditContext {
            actor: Actor::agent("legal-bot", "AI Agent"),
            mode,
            at: "2026-06-19T00:00:00Z".into(),
            rationale: Some("tighten phrasing".into()),
        }
    }
    fn base() -> CollabDoc {
        let doc = CollabDoc::new();
        apply(&doc, &human(EditMode::Direct),
            EditOp::AppendParagraph { runs: vec![Run::plain("The cat sat.")], style: None }).unwrap();
        doc
    }
    fn text_of(doc: &CollabDoc) -> String {
        doc.paragraphs().unwrap()[0].runs.iter().map(|r| r.text.clone()).collect()
    }

    #[test]
    fn tracked_insert_by_agent_is_attributed() -> Result<()> {
        let doc = base();
        let out = apply(&doc, &agent(EditMode::Tracked),
            EditOp::InsertText { para: 0, pos: 4, text: "quick ".into() })?;
        assert_eq!(out.revision_id, Some(1));
        let paras = doc.paragraphs()?;
        let runs = &paras[0].runs;
        let ins = runs.iter().find(|r| r.track.is_some()).expect("tracked insertion");
        assert_eq!(ins.text, "quick ");
        assert_eq!(ins.track.as_ref().unwrap().author, "AI Agent");
        assert_eq!(text_of(&doc), "The quick cat sat.");
        Ok(())
    }

    #[test]
    fn direct_insert_by_human_has_no_revision() -> Result<()> {
        let doc = base();
        let out = apply(&doc, &human(EditMode::Direct),
            EditOp::InsertText { para: 0, pos: 4, text: "big ".into() })?;
        assert_eq!(out.revision_id, None);
        assert!(doc.paragraphs()?[0].runs.iter().all(|r| r.track.is_none()), "direct edit not tracked");
        assert_eq!(text_of(&doc), "The big cat sat.");
        Ok(())
    }

    #[test]
    fn direct_delete_removes_text() -> Result<()> {
        let doc = base();
        apply(&doc, &human(EditMode::Direct), EditOp::DeleteRange { para: 0, range: 0..4 })?;
        assert_eq!(text_of(&doc), "cat sat.");
        Ok(())
    }

    #[test]
    fn tracked_delete_retains_text_with_attribution() -> Result<()> {
        let doc = base();
        let out = apply(&doc, &agent(EditMode::Tracked), EditOp::DeleteRange { para: 0, range: 0..4 })?;
        assert_eq!(out.revision_id, Some(1));
        let paras = doc.paragraphs()?;
        let del = paras[0].runs.iter().find(|r| r.track.is_some()).expect("tracked deletion");
        assert_eq!(del.track.as_ref().unwrap().author, "AI Agent");
        assert_eq!(text_of(&doc), "The cat sat."); // retained until accepted
        Ok(())
    }

    #[test]
    fn human_and_agent_share_one_path() -> Result<()> {
        let doc = base();
        apply(&doc, &human(EditMode::Direct), EditOp::InsertText { para: 0, pos: 0, text: "Note: ".into() })?;
        apply(&doc, &agent(EditMode::Tracked), EditOp::InsertText { para: 0, pos: 0, text: "DRAFT ".into() })?;
        let paras = doc.paragraphs()?;
        let runs = &paras[0].runs;
        assert!(runs.iter().any(|r| r.track.as_ref().is_some_and(|t| t.author == "AI Agent")));
        let full = text_of(&doc);
        assert!(full.contains("DRAFT") && full.contains("Note:") && full.contains("cat sat."));
        Ok(())
    }
}
