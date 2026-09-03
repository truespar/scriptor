//! Threaded comments.
//! 
//! A comment is an id-keyed body plus a Peritext mark over the commented span, so the
//! anchor moves with the text. Threads are flat parent/child, matching the
//! `commentsExtended` model Word writes.

use crate::*;

impl CollabDoc {
    /// Every comment in the document (bodies + thread state), sorted by id.
    pub fn comments(&self) -> Vec<Comment> {
        model::read_comments(&self.doc)
    }

    /// The comment ids anchored at codepoint `off` in paragraph `para` (for the comment popover).
    pub fn comments_at(&self, para: usize, off: usize) -> Result<Vec<u64>> {
        model::comments_at(&self.doc, para, off)
    }

    /// The anchored span of every comment in this story, in id order: the codepoint range its
    /// `cmt~{id}` marks cover (possibly spanning paragraphs). Pair with [`comments`](Self::comments) by
    /// id to see both what a comment says and what it points at. A comment whose anchor isn't in this
    /// story (the body was authored cross-story) is omitted here and appears when its story is queried.
    pub fn comment_locations(&self) -> Result<Vec<CommentLocation>> {
        let paras = self.paragraphs()?;
        // id -> (start_para, start_off, end_para, end_off), extended as the id's marks are seen.
        let mut spans: HashMap<u64, (usize, usize, usize, usize)> = HashMap::new();
        for (pi, p) in paras.iter().enumerate() {
            let mut off = 0usize;
            for r in &p.runs {
                let n = r.text.chars().count();
                for id in &r.comments {
                    let e = spans.entry(*id).or_insert((pi, off, pi, off + n));
                    if (pi, off) < (e.0, e.1) {
                        e.0 = pi;
                        e.1 = off;
                    }
                    if (pi, off + n) > (e.2, e.3) {
                        e.2 = pi;
                        e.3 = off + n;
                    }
                }
                off += n;
            }
        }
        let mut out: Vec<CommentLocation> = spans
            .into_iter()
            .map(|(id, (sp, so, ep, eo))| CommentLocation {
                id,
                start_para: sp,
                start_off: so,
                end_para: ep,
                end_off: eo,
            })
            .collect();
        out.sort_by_key(|c| c.id);
        Ok(out)
    }

    /// Reconfigure the comment-anchor mark keys from the comments currently in the document (every id
    /// in the map plus any id already anchored on the text), so subsequent marks/edits resolve
    /// consistently after the comment set changes.
    pub(crate) fn reconfigure_comment_marks(&self) {
        configure_marks_with(
            &self.doc,
            &self.all_comment_mark_ids(),
            &self.all_field_mark_ids(),
            &self.all_bookmark_mark_ids(),
            &self.all_link_mark_ids(),
            &self.all_image_mark_ids(),
            &self.all_raw_mark_ids(),
        );
    }

    /// The union of comment ids in the comments map and ids already anchored on the body text.
    pub(crate) fn all_comment_mark_ids(&self) -> Vec<u64> {
        let mut ids = model::comment_ids(&self.doc);
        if let Ok(paras) = self.paragraphs() {
            for p in &paras {
                for r in &p.runs {
                    for id in &r.comments {
                        if !ids.contains(id) {
                            ids.push(*id);
                        }
                    }
                }
            }
        }
        ids
    }

    /// The field ids in the `fields` map (every `fld~{id}` mark key must stay configured so field
    /// result ranges survive subsequent edits / reconfiguration).
    pub(crate) fn all_field_mark_ids(&self) -> Vec<u64> {
        model::read_fields(&self.doc).keys().copied().collect()
    }

    /// The bookmark ids in the `bookmarks` map (keep `bkm~{id}` keys configured).
    pub(crate) fn all_bookmark_mark_ids(&self) -> Vec<u64> {
        model::read_bookmarks(&self.doc).keys().copied().collect()
    }

    /// The hyperlink ids in the `hyperlinks` map (keep `lnk~{id}` keys configured).
    pub(crate) fn all_link_mark_ids(&self) -> Vec<u64> {
        model::read_hyperlinks(&self.doc).keys().copied().collect()
    }

    /// The passthrough ids in the `rawxml` map (keep `raw~{id}` keys configured).
    pub(crate) fn all_raw_mark_ids(&self) -> Vec<u64> {
        model::read_raw(&self.doc).keys().copied().collect()
    }

    /// Store (insert or overwrite) a comment's body + thread state without touching its anchor - the
    /// cross-story path (the body lives in this canonical map; its anchor mark may be in a
    /// header/footer child). `audit` is the synced commit message.
    pub fn put_comment(&self, c: &Comment, audit: &str) -> Result<()> {
        model::write_comment(&self.doc, c)?;
        self.reconfigure_comment_marks();
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Anchor comment `id` over codepoint range `(start_para,start_off)..(end_para,end_off)` in THIS
    /// document (body or a header/footer child). The body is stored separately (see
    /// [`put_comment`](Self::put_comment)); call that first so the id's mark key is configured.
    pub fn mark_comment(
        &self,
        id: u64,
        start_para: usize,
        start_off: usize,
        end_para: usize,
        end_off: usize,
        audit: &str,
    ) -> Result<()> {
        // Configure the id's mark key (the body may live in another story, so it's not yet in this
        // document's comments map or text).
        let mut ids = self.all_comment_mark_ids();
        if !ids.contains(&id) {
            ids.push(id);
        }
        configure_marks_with(
            &self.doc,
            &ids,
            &self.all_field_mark_ids(),
            &self.all_bookmark_mark_ids(),
            &self.all_link_mark_ids(),
            &self.all_image_mark_ids(),
            &self.all_raw_mark_ids(),
        );
        let anchor = model::CommentAnchor { id, start_para, start_off, end_para, end_off };
        model::apply_comment_anchors(&self.doc, &[anchor])?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Allocate an id and store a comment *body* (no anchor) in this document's canonical comments
    /// map; returns the id. The cross-story authoring path: the body lives here while the anchor is
    /// marked in the relevant story via [`mark_comment`](Self::mark_comment).
    pub fn add_comment_body(&self, text: &str, author: &str, date: &str, audit: &str) -> Result<u64> {
        let id = self.next_revision_id()?;
        let c = Comment {
            id,
            author: author.into(),
            initials: model::initials_of(author),
            date: date.into(),
            parent: None,
            resolved: false,
            text: text.into(),
        };
        model::write_comment(&self.doc, &c)?;
        self.reconfigure_comment_marks();
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Add a comment in THIS document: allocate an id, store the body, and anchor it over the range.
    /// Returns the new id. (Body + anchor in one document - the common body path + the agent / CLI
    /// path; a multi-paragraph range is supported.)
    #[allow(clippy::too_many_arguments)]
    pub fn add_comment(
        &self,
        start_para: usize,
        start_off: usize,
        end_para: usize,
        end_off: usize,
        text: &str,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        let c = Comment {
            id,
            author: author.into(),
            initials: model::initials_of(author),
            date: date.into(),
            parent: None,
            resolved: false,
            text: text.into(),
        };
        model::write_comment(&self.doc, &c)?;
        self.reconfigure_comment_marks();
        let anchor = model::CommentAnchor { id, start_para, start_off, end_para, end_off };
        model::apply_comment_anchors(&self.doc, &[anchor])?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Reply to comment `parent`: a new comment with no anchor of its own (it shares the parent's).
    /// Returns the new id.
    pub fn reply_comment(
        &self,
        parent: u64,
        text: &str,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        let c = Comment {
            id,
            author: author.into(),
            initials: model::initials_of(author),
            date: date.into(),
            parent: Some(parent),
            resolved: false,
            text: text.into(),
        };
        model::write_comment(&self.doc, &c)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Mark comment `id`'s thread (the comment + its replies) resolved / unresolved, like Word.
    /// Returns whether the comment existed.
    pub fn set_comment_resolved(&self, id: u64, resolved: bool, audit: &str) -> Result<bool> {
        let all = self.comments();
        let Some(mut c) = all.iter().find(|c| c.id == id).cloned() else {
            return Ok(false);
        };
        c.resolved = resolved;
        model::write_comment(&self.doc, &c)?;
        for kid in all.iter().filter(|k| k.parent == Some(id)) {
            let mut k = kid.clone();
            k.resolved = resolved;
            model::write_comment(&self.doc, &k)?;
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(true)
    }

    /// Delete comment `id` and its replies: remove their bodies + clear their anchor marks in THIS
    /// document. (Anchors that live in another story are scrubbed via
    /// [`clear_comment_marks`](Self::clear_comment_marks).) Returns the count removed.
    pub fn delete_comment(&self, id: u64, audit: &str) -> Result<usize> {
        let all = self.comments();
        // The comment + every descendant reply (one + multi level).
        let mut targets = vec![id];
        let mut i = 0;
        while i < targets.len() {
            let parent = targets[i];
            for c in &all {
                if c.parent == Some(parent) && !targets.contains(&c.id) {
                    targets.push(c.id);
                }
            }
            i += 1;
        }
        for &t in &targets {
            model::clear_comment_marks(&self.doc, t)?;
            model::delete_comment_entry(&self.doc, t)?;
        }
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(targets.len())
    }

    /// Clear comment `id`'s anchor marks in THIS document only (used to scrub a deleted comment's
    /// anchor from a header/footer child after its body was removed from the body map).
    pub fn clear_comment_marks(&self, id: u64) -> Result<()> {
        model::clear_comment_marks(&self.doc, id)?;
        self.doc.commit();
        Ok(())
    }

    /// Add / refresh `word/comments.xml` + `word/commentsExtended.xml` from the model, registering
    /// them in `[Content_Types].xml` + `document.xml.rels` (idempotent). When there are no comments,
    /// a previously-present comments part is blanked rather than left stale.
    pub(crate) fn write_comment_parts(&self, parts: &mut Vec<scriptor_ooxml::Part>) {
        let comments = self.comments();
        // Nothing about the comments changed since import, and the source package still holds the
        // part: keep those bytes. Re-emitting from the model would flatten run formatting, paragraph
        // properties and any table inside a comment - real loss on a document that was only opened
        // and saved. Mirrors the header/footer rule in `to_docx_bytes`.
        if comments == self.imported_comments
            && parts.iter().any(|p| p.name == "word/comments.xml")
        {
            return;
        }
        if comments.is_empty() {
            if parts.iter().any(|p| p.name == "word/comments.xml") {
                set_part(parts, "word/comments.xml", model::export_comments_xml(&[]).into_bytes());
            }
            return;
        }
        set_part(parts, "word/comments.xml", model::export_comments_xml(&comments).into_bytes());
        set_part(
            parts,
            "word/commentsExtended.xml",
            model::export_comments_extended_xml(&comments).into_bytes(),
        );
        ensure_comment_parts_registered(parts);
    }
}
