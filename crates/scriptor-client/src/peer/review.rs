//! Reviewing: comments, and accepting or rejecting changes.
//! 
//! An agent can act as a reviewer as well as an author, including resolving only the
//! changes attributed to a particular person.

use crate::*;

impl AgentPeer {
    /// Add a comment anchored over `range`, which **may span paragraphs**. Returns the new comment id.
    /// Always available (a comment is not a tracked change) - the universal fallback when a redline
    /// isn't the right move, and the most common multi-paragraph review action (a reviewer marks a
    /// whole clause). The policy / event `node_id` is the range's start paragraph.
    pub fn add_comment(&self, range: &AnchorRange, text: &str, date: &str) -> Result<u64> {
        let (sp, so, ep, eo) = self.multi_span(range)?;
        self.guard(AgentAction::AddComment, self.doc.node_id(sp), None, || {
            self.doc.add_comment(sp, so, ep, eo, text, &self.author, date, &self.audit("add comment", ""))
        })
    }

    /// Reply to comment `parent` (shares the parent's anchor). Returns the new comment id.
    pub fn reply_comment(&self, parent: u64, text: &str, date: &str) -> Result<u64> {
        self.guard(AgentAction::ReplyComment, None, None, || {
            self.doc.reply_comment(parent, text, &self.author, date, &self.audit("reply comment", ""))
        })
    }

    /// Resolve (or reopen) comment `id`'s thread. Returns whether the comment existed.
    pub fn resolve_comment(&self, id: u64, resolved: bool) -> Result<bool> {
        self.guard(AgentAction::ResolveComment, None, None, || {
            let verb = if resolved { "resolve comment" } else { "reopen comment" };
            self.doc.set_comment_resolved(id, resolved, &self.audit(verb, ""))
        })
    }

    /// Delete comment `id` (and its replies). Returns how many were removed.
    pub fn delete_comment(&self, id: u64) -> Result<usize> {
        self.guard(AgentAction::DeleteComment, None, None, || {
            self.doc.delete_comment(id, &self.audit("delete comment", ""))
        })
    }

    /// Accept tracked change `id` (insertion keeps text, deletion removes it, paragraph marks merge).
    pub fn accept_change(&self, id: u64) -> Result<bool> {
        self.guard(AgentAction::AcceptChange, None, None, || self.doc.accept_revision(id, &self.audit("accept change", "")))
    }

    /// Reject tracked change `id` (insertion removed, deletion restored).
    pub fn reject_change(&self, id: u64) -> Result<bool> {
        self.guard(AgentAction::RejectChange, None, None, || self.doc.reject_revision(id, &self.audit("reject change", "")))
    }

    /// Accept every tracked change in the document. Returns how many resolved.
    pub fn accept_all(&self) -> Result<usize> {
        self.guard(AgentAction::AcceptAll, None, None, || self.doc.accept_all(&self.audit("accept all changes", "")))
    }

    /// Reject every tracked change in the document. Returns how many resolved.
    pub fn reject_all(&self) -> Result<usize> {
        self.guard(AgentAction::RejectAll, None, None, || self.doc.reject_all(&self.audit("reject all changes", "")))
    }

    /// Accept every pending tracked change authored by `author` (selective triage - e.g. accept the
    /// agent's own suggestions, or one reviewer's). Returns how many resolved.
    pub fn accept_by_author(&self, author: &str) -> Result<usize> {
        self.resolve_by_author(author, true)
    }

    /// Reject every pending tracked change authored by `author`. Returns how many resolved.
    pub fn reject_by_author(&self, author: &str) -> Result<usize> {
        self.resolve_by_author(author, false)
    }

    fn resolve_by_author(&self, author: &str, accept: bool) -> Result<usize> {
        let ids: Vec<u64> =
            self.list_changes()?.into_iter().filter(|c| c.author == author).map(|c| c.id).collect();
        let mut n = 0;
        for id in ids {
            let done = if accept { self.accept_change(id)? } else { self.reject_change(id)? };
            if done {
                n += 1;
            }
        }
        Ok(n)
    }
}
