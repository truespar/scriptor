//! Comments and their on-page geometry.
//! 
//! The CRUD surface plus the rectangles that highlight a commented span, which have to
//! be recomputed against the current layout rather than stored.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// The comment ids anchored at `(para, off)` (the run under the caret, or one ending at it).
    /// Region-routed; empty when the caret isn't inside a comment anchor.
    #[wasm_bindgen(js_name = commentsAt)]
    pub fn comments_at(&self, para: u32, off: u32) -> Vec<u32> {
        let Some((doc, p)) = self.route(para) else { return Vec::new() };
        doc.comments_at(p, off as usize)
            .map(|v| v.into_iter().map(|x| x as u32).collect())
            .unwrap_or_default()
    }

    /// Add a comment over the selection `(start_para,start_off)..(end_para,end_off)` (one story) with
    /// `text` as the body, attributed to the current author + last timestamp. Returns the new comment
    /// id, or `-1` if the endpoints are in different stories / the story doesn't exist.
    #[wasm_bindgen(js_name = addComment)]
    pub fn add_comment(
        &self,
        start_para: u32,
        start_off: u32,
        end_para: u32,
        end_off: u32,
        text: &str,
    ) -> Result<i32, JsError> {
        let (region, a_para) = decode_region(start_para as usize);
        let (er, b_para) = decode_region(end_para as usize);
        if region != er {
            return Ok(-1);
        }
        // Normalize so the range runs start -> end within the story.
        let ((sp, so), (ep, eo)) = if (a_para, start_off) <= (b_para, end_off) {
            ((a_para, start_off as usize), (b_para, end_off as usize))
        } else {
            ((b_para, end_off as usize), (a_para, start_off as usize))
        };
        if region == Region::Body {
            return self
                .doc
                .add_comment(sp, so, ep, eo, text, &self.author_name, &self.now, "add comment")
                .map(|id| id as i32)
                .map_err(to_js);
        }
        // Header/footer: the body lives in the canonical (body) comments map; the anchor mark lives
        // in the child story - the part the caret's page shows (same routing as `route`).
        let child =
            self.active_hf_set(region).and_then(|s| self.doc.hf_part_doc(&s.part));
        let Some(child) = child else { return Ok(-1) };
        let id =
            self.doc.add_comment_body(text, &self.author_name, &self.now, "add comment").map_err(to_js)?;
        child.mark_comment(id, sp, so, ep, eo, "add comment").map_err(to_js)?;
        Ok(id as i32)
    }

    /// Reply to comment `parent` (a threaded child sharing the parent's anchor). Returns the new id.
    #[wasm_bindgen(js_name = replyComment)]
    pub fn reply_comment(&self, parent: u32, text: &str) -> Result<i32, JsError> {
        self.doc
            .reply_comment(parent as u64, text, &self.author_name, &self.now, "reply comment")
            .map(|id| id as i32)
            .map_err(to_js)
    }

    /// Mark comment `id`'s thread resolved / unresolved. Returns whether it existed.
    #[wasm_bindgen(js_name = resolveComment)]
    pub fn resolve_comment(&self, id: u32, resolved: bool) -> Result<bool, JsError> {
        self.doc.set_comment_resolved(id as u64, resolved, "resolve comment").map_err(to_js)
    }

    /// Delete comment `id` and its replies (clearing the anchor in whichever story holds it).
    #[wasm_bindgen(js_name = deleteComment)]
    pub fn delete_comment(&self, id: u32) -> Result<bool, JsError> {
        let n = self.doc.delete_comment(id as u64, "delete comment").map_err(to_js)?;
        for (part, _) in self.doc.hf_parts() {
            if let Some(d) = self.doc.hf_part_doc(&part) {
                let _ = d.clear_comment_marks(id as u64);
            }
        }
        Ok(n > 0)
    }

    /// Every comment as a JSON array string (for the popover / reviewing list): each object has
    /// `id`, `author`, `initials`, `date`, `text`, `parent` (id or null), `resolved`, and the anchor
    /// caret `para` / `off` (namespaced; `-1` when un-anchored). Replies inherit the parent's anchor.
    #[wasm_bindgen(js_name = listComments)]
    pub fn list_comments(&self) -> String {
        let comments = self.doc.comments();
        let mut out = String::from("[");
        for (i, c) in comments.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let loc = self
                .comment_anchor_loc(c.id)
                .or_else(|| c.parent.and_then(|p| self.comment_anchor_loc(p)));
            let (para, off) = loc.map(|(p, o)| (p as i64, o as i64)).unwrap_or((-1, -1));
            let parent = c.parent.map(|p| p.to_string()).unwrap_or_else(|| "null".into());
            out.push_str(&format!(
                "{{\"id\":{},\"author\":\"{}\",\"initials\":\"{}\",\"date\":\"{}\",\"text\":\"{}\",\"parent\":{},\"resolved\":{},\"para\":{},\"off\":{}}}",
                c.id,
                json_escape(&c.author),
                json_escape(&c.initials),
                json_escape(&c.date),
                json_escape(&c.text),
                parent,
                c.resolved,
                para,
                off,
            ));
        }
        out.push(']');
        out
    }

    /// Highlight rectangles (device px, flattened `[x,y,w,h,...]`) behind every text range anchored by
    /// at least one *unresolved* comment, across body + header/footer. Editor chrome (not exported) -
    /// the view paints these on its overlay like the selection.
    #[wasm_bindgen(js_name = commentRects)]
    pub fn comment_rects(&self) -> Vec<f32> {
        // Skip a comment's highlight when it's resolved OR its author's markup is filtered out.
        let resolved: std::collections::HashSet<u64> = self
            .doc
            .comments()
            .iter()
            .filter(|c| c.resolved || self.hidden_reviewers.contains(&c.author))
            .map(|c| c.id)
            .collect();
        let mut out = Vec::new();
        self.collect_comment_rects(&self.doc, 0, &resolved, &mut out);
        // Header/footer rects resolve against the namespaced line geometry, which each page emits
        // for ITS part - collect from the active page's parts (the same ones the caret routes to).
        if let Some(h) =
            self.active_hf_set(Region::Header).and_then(|s| self.doc.hf_part_doc(&s.part))
        {
            self.collect_comment_rects(h, HEADER_BASE, &resolved, &mut out);
        }
        if let Some(f) =
            self.active_hf_set(Region::Footer).and_then(|s| self.doc.hf_part_doc(&s.part))
        {
            self.collect_comment_rects(f, FOOTER_BASE, &resolved, &mut out);
        }
        out
    }

    /// Caret `[para, off]` of the next (`forward`) / previous comment anchor across stories (wraps),
    /// or an empty array when the document has no comments. For Review > Next/Previous comment.
    #[wasm_bindgen(js_name = nextComment)]
    pub fn next_comment(&self, para: u32, off: u32) -> Vec<u32> {
        self.adjacent_comment(para as usize, off as usize, true)
    }

    #[wasm_bindgen(js_name = prevComment)]
    pub fn prev_comment(&self, para: u32, off: u32) -> Vec<u32> {
        self.adjacent_comment(para as usize, off as usize, false)
    }

    /// The anchor caret `(namespaced_para, char_off)` of comment `id` - the first run carrying it,
    /// scanning body then header then footer. `None` for an un-anchored comment (e.g. a reply).
    pub(crate) fn comment_anchor_loc(&self, id: u64) -> Option<(usize, usize)> {
        fn scan(doc: &scriptor_crdt::CollabDoc, base: usize, id: u64) -> Option<(usize, usize)> {
            let paras = doc.paragraphs().ok()?;
            for (pi, p) in paras.iter().enumerate() {
                let mut off = 0usize;
                for r in &p.runs {
                    if r.comments.contains(&id) {
                        return Some((base + pi, off));
                    }
                    off += r.text.chars().count();
                }
            }
            None
        }
        scan(&self.doc, 0, id)
            .or_else(|| {
                self.active_hf_set(Region::Header)
                    .and_then(|s| self.doc.hf_part_doc(&s.part))
                    .and_then(|h| scan(h, HEADER_BASE, id))
            })
            .or_else(|| {
                self.active_hf_set(Region::Footer)
                    .and_then(|s| self.doc.hf_part_doc(&s.part))
                    .and_then(|f| scan(f, FOOTER_BASE, id))
            })
    }

    /// Append highlight rects for every comment-anchored run range in `doc` (namespaced by `base`),
    /// skipping ranges whose only comments are resolved. Coalesces adjacent anchored runs.
    pub(crate) fn collect_comment_rects(
        &self,
        doc: &scriptor_crdt::CollabDoc,
        base: usize,
        resolved: &std::collections::HashSet<u64>,
        out: &mut Vec<f32>,
    ) {
        let Ok(paras) = doc.paragraphs() else { return };
        for (pi, p) in paras.iter().enumerate() {
            let para = base + pi;
            let mut off = 0usize;
            let mut seg_start: Option<usize> = None;
            for r in &p.runs {
                let n = r.text.chars().count();
                let active = r.comments.iter().any(|id| !resolved.contains(id));
                match (active, seg_start) {
                    (true, None) => seg_start = Some(off),
                    (false, Some(s)) => {
                        self.push_comment_rect(para, s, off, out);
                        seg_start = None;
                    }
                    _ => {}
                }
                off += n;
            }
            if let Some(s) = seg_start {
                self.push_comment_rect(para, s, off, out);
            }
        }
    }

    /// Convert char range `[c1, c2)` in `para` to device-px rects and append them to `out`.
    pub(crate) fn push_comment_rect(&self, para: usize, c1: usize, c2: usize, out: &mut Vec<f32>) {
        let b1 = self.full_to_visible(para, self.char_to_byte(para, c1));
        let b2 = self.full_to_visible(para, self.char_to_byte(para, c2));
        out.extend(self.layout.selection_rects(para, b1, para, b2, self.page_hint_for(para)));
    }

    /// Next/previous comment-anchor caret across stories (namespaced, wrapping). Returns `[para, off]`
    /// or empty when there are no comments.
    pub(crate) fn adjacent_comment(&self, para: usize, off: usize, forward: bool) -> Vec<u32> {
        let mut all: Vec<(usize, usize)> = Vec::new();
        let mut gather = |doc: &scriptor_crdt::CollabDoc, base: usize| {
            let Ok(paras) = doc.paragraphs() else { return };
            for (pi, p) in paras.iter().enumerate() {
                let mut o = 0usize;
                let mut prev_active = false;
                for r in &p.runs {
                    let active = !r.comments.is_empty();
                    if active && !prev_active {
                        all.push((base + pi, o));
                    }
                    prev_active = active;
                    o += r.text.chars().count();
                }
            }
        };
        gather(&self.doc, 0);
        // Header/footer stories gather from the parts the caret's page shows - the same parts a
        // namespaced HEADER_BASE / FOOTER_BASE caret routes back to (see `route`).
        if let Some(h) =
            self.active_hf_set(Region::Header).and_then(|s| self.doc.hf_part_doc(&s.part))
        {
            gather(h, HEADER_BASE);
        }
        if let Some(f) =
            self.active_hf_set(Region::Footer).and_then(|s| self.doc.hf_part_doc(&s.part))
        {
            gather(f, FOOTER_BASE);
        }
        if all.is_empty() {
            return Vec::new();
        }
        all.sort_unstable();
        let cur = (para, off);
        let pick = if forward {
            all.iter().find(|&&p| p > cur).or_else(|| all.first())
        } else {
            all.iter().rev().find(|&&p| p < cur).or_else(|| all.last())
        };
        match pick {
            Some(&(p, o)) => vec![p as u32, o as u32],
            None => Vec::new(),
        }
    }
}
