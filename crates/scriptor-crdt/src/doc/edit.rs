//! Direct edits to text and formatting.
//! 
//! The untracked path: insert and delete text, split and join paragraphs, apply run
//! and paragraph formatting, and report what a selection resolves to. Every one has a
//! tracked counterpart in `suggest`.

use crate::*;

impl CollabDoc {
    /// Append a paragraph (built from `runs`, with an optional style) to the document.
    pub fn append_paragraph(&self, runs: &[Run], style: Option<&str>) -> Result<()> {
        model::append_paragraph(&self.doc, runs, style)?;
        self.doc.commit();
        Ok(())
    }

    /// Materialize every paragraph in document order (for inspection and assertions).
    pub fn paragraphs(&self) -> Result<Vec<Paragraph>> {
        model::read_paragraphs(&self.doc)
    }

    /// Insert `text` at codepoint `pos` in paragraph `para` **directly** (no tracked change).
    /// `audit` becomes the synced loro commit message (the audit/identity layer).
    pub fn insert_text(&self, para: usize, pos: usize, text: &str, audit: &str) -> Result<()> {
        model::insert_text(&self.doc, para, pos, text)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Delete codepoint `range` in paragraph `para` **directly** (removes the text). `audit`
    /// becomes the synced loro commit message.
    pub fn delete_text(&self, para: usize, range: std::ops::Range<usize>, audit: &str) -> Result<()> {
        model::delete_text(&self.doc, para, range)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Split paragraph `para` at codepoint `pos`: text from `pos` onward moves into a new paragraph
    /// inserted right after (the Enter key). `audit` becomes the synced loro commit message. When the
    /// document has tables, the body structure is updated so the new paragraph lands in the same
    /// container as `para` (a new top-level paragraph, or a second paragraph inside the same cell).
    pub fn split_paragraph(&self, para: usize, pos: usize, audit: &str) -> Result<()> {
        model::split_paragraph(&self.doc, para, pos)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Join paragraph `para` into the previous one (the Backspace-at-start / Delete-at-end key).
    /// Returns `Some(caret)` - the codepoint length of the previous paragraph before the join (the
    /// merged caret position) - or `None` when the join would cross a container boundary (the start
    /// of a cell, or a paragraph adjacent to a table) and is refused so the table grid stays intact.
    /// `audit` becomes the synced loro commit message.
    pub fn join_paragraph(&self, para: usize, audit: &str) -> Result<Option<usize>> {
        if para == 0 || !self.same_container(para - 1, para) {
            return Ok(None);
        }
        let at = model::join_paragraph(&self.doc, para)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(Some(at))
    }

    /// Whether two flat paragraph indices live in the same editing container (both top-level, or the
    /// same table cell). A document with no tables has an empty `body`, so everything is top-level and
    /// joins are always allowed (preserving the table-free behaviour).
    pub(crate) fn same_container(&self, a: usize, b: usize) -> bool {
        let body = self.body();
        if body.is_empty() {
            return true;
        }
        match (body_locate(&body, a), body_locate(&body, b)) {
            (Some(BodyLoc::TopLevel), Some(BodyLoc::TopLevel)) => true,
            (
                Some(BodyLoc::Cell { item: i1, row: r1, cell: c1, .. }),
                Some(BodyLoc::Cell { item: i2, row: r2, cell: c2, .. }),
            ) => (i1, r1, c1) == (i2, r2, c2),
            _ => false,
        }
    }

    /// Apply a run-formatting command ([`RunFormat`]) over codepoint `range` in paragraph `para`
    /// (bold/italic/underline/strike/size/color/font - the Home tab's Font group). `audit` becomes
    /// the synced loro commit message.
    pub fn apply_run_format(
        &self,
        para: usize,
        range: std::ops::Range<usize>,
        fmt: &RunFormat,
        audit: &str,
    ) -> Result<()> {
        model::apply_run_format(&self.doc, para, range, fmt)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// The resolved formatting of codepoint `[start, end)` in paragraph `para` (mixed -> `None`) -
    /// drives toolbar button states + the font / size dropdowns.
    pub fn selection_format(&self, para: usize, start: usize, end: usize) -> Result<SelectionFormat> {
        let styles = self.styles();
        model::selection_format(&self.doc, &styles, para, start, end)
    }

    /// Apply paragraph-level formatting ([`ParaProps`]) to paragraph `para` (alignment / line
    /// spacing / indents - the Home tab's Paragraph group). `audit` is the synced commit message.
    pub fn apply_paragraph_format(&self, para: usize, props: &ParaProps, audit: &str) -> Result<()> {
        model::apply_paragraph_format(&self.doc, para, props)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// The paragraph-level formatting of paragraph `para` (for the toolbar's Paragraph group).
    pub fn paragraph_format(&self, para: usize) -> Result<ParaProps> {
        model::paragraph_format(&self.doc, para)
    }

    /// Clear all inline run formatting (bold / italic / size / color / highlight / vertAlign / ...) over
    /// codepoint `[start, end)` in paragraph `para` - the Home tab's Clear Formatting eraser. A direct
    /// edit (not a tracked rPrChange in v1); annotations + tracked-change marks are preserved.
    pub fn clear_run_format(&self, para: usize, start: usize, end: usize, audit: &str) -> Result<()> {
        model::clear_run_format(&self.doc, para, start..end)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }
}
