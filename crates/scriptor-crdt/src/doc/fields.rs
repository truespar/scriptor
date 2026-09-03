//! Field constructs: table of contents, bookmarks and hyperlinks.
//! 
//! All three are run-level marks over a span rather than characters in the text, so
//! they survive edits inside the span. The TOC additionally writes real field codes
//! so Word can update it.

use crate::*;

impl CollabDoc {
    /// The document's headings, for building a table of contents: `(flat_paragraph_index, level, text)`
    /// for each body paragraph styled `Heading1`..`Heading9` (level = the trailing digit), in document
    /// order. Paragraphs already inside a field (e.g. an existing TOC's own result) are skipped so
    /// rebuilding a TOC doesn't fold its previous entries back in.
    pub fn headings(&self) -> Vec<(usize, u8, String)> {
        let Ok(paras) = self.paragraphs() else { return Vec::new() };
        let mut out = Vec::new();
        for (i, p) in paras.iter().enumerate() {
            if p.runs.iter().any(|r| r.field.is_some()) {
                continue; // inside a field's cached result (e.g. an existing TOC)
            }
            if let Some(level) = p.style.as_deref().and_then(model::heading_level) {
                let text: String = p.runs.iter().map(|r| r.text.as_str()).collect();
                out.push((i, level, text));
            }
        }
        out
    }

    /// Insert `entries` (level, heading text) as TOC-styled paragraphs ("{text}\t", style `TOC{level}`)
    /// at flat paragraph index `at`, keeping the body structure in sync. The page number is appended to
    /// each line afterwards (the caller, which has the layout) and the whole block is then wrapped as a
    /// field via [`finish_toc`](Self::finish_toc). Caller commits via this method.
    pub fn insert_toc_entries(&self, at: usize, entries: &[(u8, String)]) -> Result<()> {
        for (i, (level, text)) in entries.iter().enumerate() {
            let idx = at + i;
            model::insert_empty_paragraph(&self.doc, idx)?;
            model::set_paragraph_style(&self.doc, idx, Some(&format!("TOC{}", (*level).clamp(1, 9))))?;
            model::insert_text(&self.doc, idx, 0, &format!("{text}\t"))?;
        }
        // Body is derived from the loro block tree (TOC lines are top-level paragraph nodes inserted
        // above), so there's no separate structure to keep in sync.
        self.doc.commit();
        Ok(())
    }

    /// Wrap the `count` paragraphs starting at flat index `at` as a `TOC` field: allocate a field id,
    /// configure its `fld~{id}` mark key, mark each paragraph's full range, and store `instr`. Returns
    /// the field id. The lines must already hold their final text (heading + tab + page number) so the
    /// mark covers the page numbers too. Commits.
    pub fn finish_toc(&self, at: usize, count: usize, instr: &str) -> Result<u64> {
        let field_id = self.all_field_mark_ids().into_iter().max().map(|m| m + 1).unwrap_or(0);
        let mut fids = self.all_field_mark_ids();
        fids.push(field_id);
        configure_marks_with(
            &self.doc,
            &self.all_comment_mark_ids(),
            &fids,
            &self.all_bookmark_mark_ids(),
            &self.all_link_mark_ids(),
            &self.all_image_mark_ids(),
            &self.all_raw_mark_ids(),
        );
        let paras = model::read_paragraphs(&self.doc)?;
        for i in 0..count {
            let idx = at + i;
            let len: usize =
                paras.get(idx).map(|p| p.runs.iter().map(|r| r.text.chars().count()).sum()).unwrap_or(0);
            model::mark_field_range(&self.doc, field_id, idx, 0, len)?;
        }
        model::write_field(&self.doc, field_id, instr)?;
        self.doc.commit();
        Ok(field_id)
    }

    /// The next free `_Toc{n}` sequence number - one past the highest `_Toc<digits>` bookmark name in
    /// the document, so freshly-generated TOC anchors never collide with existing ones.
    pub fn next_toc_seq(&self) -> u64 {
        model::read_bookmarks(&self.doc)
            .values()
            .filter_map(|n| n.strip_prefix("_Toc").and_then(|s| s.parse::<u64>().ok()))
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }

    /// Locate the document's TOC field: `(field_id, start_flat_para, paragraph_count)`, or `None` when
    /// there's no TOC. The field is the lowest-id one whose instruction's first token is `TOC`; its
    /// extent is the contiguous run of body paragraphs carrying its `fld~{id}` result mark.
    pub fn toc_field_range(&self) -> Result<Option<(u64, usize, usize)>> {
        let fields = model::read_fields(&self.doc);
        let toc_id = fields
            .iter()
            .filter(|(_, instr)| {
                instr.split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case("TOC"))
            })
            .map(|(id, _)| *id)
            .min();
        let Some(toc_id) = toc_id else { return Ok(None) };
        let paras = self.paragraphs()?;
        let idxs: Vec<usize> = paras
            .iter()
            .enumerate()
            .filter(|(_, p)| p.runs.iter().any(|r| r.field == Some(toc_id)))
            .map(|(i, _)| i)
            .collect();
        match (idxs.first(), idxs.last()) {
            (Some(&start), Some(&end)) => Ok(Some((toc_id, start, end - start + 1))),
            _ => Ok(None),
        }
    }

    /// Drop the generated TOC anchors so a regenerated TOC starts clean: clear + remove every `_Toc*`
    /// bookmark (Word's reserved TOC-anchor prefix) and drop hyperlink-map entries targeting `#_Toc*`
    /// (the entry links, whose marks vanished with the deleted TOC paragraphs). Caller commits.
    fn clear_toc_anchors(&self) -> Result<()> {
        for (id, name) in model::read_bookmarks(&self.doc) {
            if name.starts_with("_Toc") {
                model::clear_bookmark_marks(&self.doc, id)?;
                model::delete_bookmark(&self.doc, id)?;
            }
        }
        for (id, target) in model::read_hyperlinks(&self.doc) {
            if target.starts_with("#_Toc") {
                model::delete_hyperlink(&self.doc, id)?;
            }
        }
        Ok(())
    }

    /// Remove the existing TOC field block: delete its paragraphs, drop the field instruction, and clear
    /// its generated `_Toc*` anchors + entry links. Returns the flat index where the TOC began (so a
    /// caller can regenerate there), or `None` when there's no TOC. Commits.
    pub fn remove_toc(&self, audit: &str) -> Result<Option<usize>> {
        let Some((field_id, at, count)) = self.toc_field_range()? else { return Ok(None) };
        // Delete the field's paragraphs high -> low so earlier indices stay valid.
        for i in (0..count).rev() {
            model::delete_paragraph(&self.doc, at + i)?;
        }
        // Body is derived from the loro block tree (TOC lines are top-level paragraph nodes, deleted
        // above), so there's no separate structure to keep in sync.
        model::delete_field(&self.doc, field_id)?;
        self.clear_toc_anchors()?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(Some(at))
    }

    /// Configure every annotation mark key currently in use plus the extra ids in `extra_links` (used
    /// when a fresh hyperlink id must be markable before it's in the map). Replaces the whole style map.
    fn reconfigure_marks_adding_links(&self, extra_links: &[u64]) {
        let mut lids = self.all_link_mark_ids();
        lids.extend_from_slice(extra_links);
        configure_marks_with(
            &self.doc,
            &self.all_comment_mark_ids(),
            &self.all_field_mark_ids(),
            &self.all_bookmark_mark_ids(),
            &lids,
            &self.all_image_mark_ids(),
            &self.all_raw_mark_ids(),
        );
    }

    /// Configure every annotation mark key currently in use plus the extra ids in `extra_bookmarks`
    /// (used when a fresh bookmark id must be markable before it's in the map). Replaces the style map.
    fn reconfigure_marks_adding_bookmarks(&self, extra_bookmarks: &[u64]) {
        let mut bids = self.all_bookmark_mark_ids();
        bids.extend_from_slice(extra_bookmarks);
        configure_marks_with(
            &self.doc,
            &self.all_comment_mark_ids(),
            &self.all_field_mark_ids(),
            &bids,
            &self.all_link_mark_ids(),
            &self.all_image_mark_ids(),
            &self.all_raw_mark_ids(),
        );
    }

    /// Add a bookmark over codepoint `[start, end)` in body paragraph `para`, named `name`. Allocates a
    /// bookmark id, configures + applies the `bkm~{id}` mark, stores the name. Direct (not a tracked
    /// change). Returns the bookmark id.
    pub fn add_bookmark(&self, para: usize, start: usize, end: usize, name: &str, audit: &str) -> Result<u64> {
        let id = self.all_bookmark_mark_ids().into_iter().max().map(|m| m + 1).unwrap_or(0);
        self.reconfigure_marks_adding_bookmarks(&[id]);
        model::write_bookmark(&self.doc, id, name)?;
        model::mark_bookmark_range(&self.doc, id, para, start, end)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Add a hyperlink over codepoint `[start, end)` in body paragraph `para`, targeting `target` (an
    /// external URL, or `#bookmarkName` for an internal jump). Allocates a link id, configures + applies
    /// the `lnk~{id}` mark, stores the target. Direct (not a tracked change). Returns the link id.
    pub fn add_hyperlink(&self, para: usize, start: usize, end: usize, target: &str, audit: &str) -> Result<u64> {
        let id = self.all_link_mark_ids().into_iter().max().map(|m| m + 1).unwrap_or(0);
        self.reconfigure_marks_adding_links(&[id]);
        model::write_hyperlink(&self.doc, id, target)?;
        model::mark_link_range(&self.doc, id, para, start, end)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Remove the hyperlink covering codepoint `off` in paragraph `para` (unmark its whole range + drop
    /// its target). Returns whether a link was there.
    pub fn remove_hyperlink(&self, para: usize, off: usize, audit: &str) -> Result<bool> {
        let Some((id, _)) = self.link_at(para, off)? else { return Ok(false) };
        model::clear_link_marks(&self.doc, id)?;
        model::delete_hyperlink(&self.doc, id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// The hyperlink at codepoint `off` in paragraph `para`: `(id, target)`, or `None`. `target` is an
    /// external URL or `#bookmarkName`.
    pub fn link_at(&self, para: usize, off: usize) -> Result<Option<(u64, String)>> {
        let paras = self.paragraphs()?;
        let Some(p) = paras.get(para) else { return Ok(None) };
        let mut pos = 0usize;
        for run in &p.runs {
            let n = run.text.chars().count();
            if off >= pos && off < pos + n
                && let Some(id) = run.link {
                    return Ok(model::read_hyperlinks(&self.doc).get(&id).map(|t| (id, t.clone())));
                }
            pos += n;
        }
        Ok(None)
    }

    /// The flat paragraph index where bookmark `name` begins, or `None` - lets an internal hyperlink
    /// (`#name`) jump to its target.
    pub fn bookmark_paragraph(&self, name: &str) -> Option<usize> {
        let bookmarks = model::read_bookmarks(&self.doc);
        let id = bookmarks.iter().find(|(_, n)| n.as_str() == name).map(|(id, _)| *id)?;
        let paras = self.paragraphs().ok()?;
        paras.iter().position(|p| p.runs.iter().any(|r| r.bookmarks.contains(&id)))
    }
}
