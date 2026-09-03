//! Tracked-change display and resolution.
//! 
//! Word's four display modes, the reviewer filter, and accept/reject over a single
//! change or the whole document. Simple Markup additionally lets one paragraph be
//! expanded to show its markup inline.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Toggle whether paragraph `para` is expanded to inline All-Markup while the document is in
    /// Simple Markup (click-to-expand). Returns the new state. Re-layout + paint after. The override is
    /// only consulted in Simple Markup, but the toggle is always recorded.
    #[wasm_bindgen(js_name = toggleParagraphExpanded)]
    pub fn toggle_paragraph_expanded(&mut self, para: u32) -> bool {
        let p = para as usize;
        if self.expanded.remove(&p) {
            false
        } else {
            self.expanded.insert(p);
            true
        }
    }

    /// Whether paragraph `para` is currently expanded (click-to-expand).
    #[wasm_bindgen(js_name = isParagraphExpanded)]
    pub fn is_paragraph_expanded(&self, para: u32) -> bool {
        self.expanded.contains(&(para as usize))
    }

    /// Clear every click-to-expand paragraph (on a display-mode change or a new document).
    #[wasm_bindgen(js_name = clearExpandedParagraphs)]
    pub fn clear_expanded_paragraphs(&mut self) {
        self.expanded.clear();
    }

    /// Set how tracked changes are displayed: `all` (insertions underlined + deletions struck, in
    /// author colours), `simple`/`none` (deletions hidden - the Final view), or `original`
    /// (insertions hidden). Unknown values are ignored. Call [`relayout`] + re-paint after. The
    /// non-`all` modes are render/preview only: the caret geometry still indexes the full
    /// (All-Markup) text, so edit in `all`.
    #[wasm_bindgen(js_name = setTrackDisplay)]
    pub fn set_track_display(&mut self, mode: &str) {
        if let Some(m) = TrackDisplay::parse(mode) {
            if m != self.track_display {
                // Click-to-expand is per Simple-Markup session; leaving (or re-entering) the mode
                // collapses everything so a stale expansion doesn't reappear.
                self.expanded.clear();
            }
            self.track_display = m;
        }
    }

    /// Turn revision balloons on/off (Word's "Show Revisions in Balloons"). When on, tracked deletions
    /// move from the line into right-margin bubbles; it only takes visible effect in the markup display
    /// modes (All / Simple). Re-layout + paint after.
    #[wasm_bindgen(js_name = setBalloons)]
    pub fn set_balloons(&mut self, on: bool) {
        self.balloons = on;
    }

    /// Whether revision balloons are on.
    #[wasm_bindgen(js_name = balloonsOn)]
    pub fn balloons_on(&self) -> bool {
        self.balloons
    }

    /// Turn Track-Changes (suggesting) mode on/off. While on, typing / deleting author tracked
    /// changes attributed to the current author instead of editing the document directly. Ignored when
    /// tracking is **locked** (see [`set_track_locked`](Self::set_track_locked)) - it stays on.
    #[wasm_bindgen(js_name = setTrackChanges)]
    pub fn set_track_changes(&mut self, on: bool) {
        if self.track_locked && !on {
            return; // a locked document forces tracking on
        }
        self.track_changes = on;
    }

    /// Whether Track-Changes mode is on.
    #[wasm_bindgen(js_name = trackChangesOn)]
    pub fn track_changes_on(&self) -> bool {
        self.track_changes
    }

    /// Lock / unlock Track-Changes (Review > Lock Tracking): while locked, tracking can't be turned
    /// off (and is forced on). v1 is session state, not yet persisted to `settings.xml`.
    #[wasm_bindgen(js_name = setTrackLocked)]
    pub fn set_track_locked(&mut self, locked: bool) {
        self.track_locked = locked;
        if locked {
            self.track_changes = true;
        }
    }

    /// Whether Track-Changes is locked on.
    #[wasm_bindgen(js_name = trackLocked)]
    pub fn track_locked(&self) -> bool {
        self.track_locked
    }

    /// Filter a reviewer's markup in / out of the display by `w:author` name (display-only; the model
    /// is untouched). Hidden reviewers' tracked changes + comments are suppressed on the next
    /// [`relayout`]. Re-layout + re-paint after.
    #[wasm_bindgen(js_name = setReviewerHidden)]
    pub fn set_reviewer_hidden(&mut self, author: &str, hidden: bool) {
        if hidden {
            self.hidden_reviewers.insert(author.to_string());
        } else {
            self.hidden_reviewers.remove(author);
        }
    }

    /// Every reviewer who authored a tracked change or comment, as a JSON array (the "Show Markup"
    /// legend): each object has `name`, `color` (the author's hue as `#rrggbb`), and `hidden`. Sorted
    /// by name.
    #[wasm_bindgen(js_name = reviewers)]
    pub fn reviewers(&self) -> String {
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut gather = |doc: &scriptor_crdt::CollabDoc| {
            if let Ok(paras) = doc.paragraphs() {
                for p in &paras {
                    for r in &p.runs {
                        if let Some(t) = &r.track {
                            names.insert(t.author.clone());
                        }
                        if let Some(f) = &r.fmt_change {
                            names.insert(f.author.clone());
                        }
                    }
                    if let Some(c) = &p.prop_change {
                        names.insert(c.author.clone());
                    }
                    if let Some(m) = &p.mark_change {
                        names.insert(m.author.clone());
                    }
                }
            }
        };
        gather(&self.doc);
        for (part, _) in self.doc.hf_parts() {
            if let Some(d) = self.doc.hf_part_doc(&part) {
                gather(d);
            }
        }
        for tc in self.doc.table_changes() {
            names.insert(tc.author);
        }
        for c in self.doc.comments() {
            names.insert(c.author);
        }
        names.remove(""); // an unattributed change isn't a named reviewer
        let mut out = String::from("[");
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let [r, g, b] = track_colour(name, TrackKind::Ins);
            out.push_str(&format!(
                "{{\"name\":\"{}\",\"color\":\"#{:02x}{:02x}{:02x}\",\"hidden\":{}}}",
                json_escape(name),
                r,
                g,
                b,
                self.hidden_reviewers.contains(name),
            ));
        }
        out.push(']');
        out
    }

    /// Set the current author: a stable `id` (audit trail) + a display `name` (stamped as `w:author`
    /// on tracked changes, and shown in the hover tooltip).
    #[wasm_bindgen(js_name = setAuthor)]
    pub fn set_author(&mut self, id: &str, name: &str) {
        self.author_id = id.to_string();
        self.author_name = name.to_string();
    }

    /// Hand the engine the current wall-clock time (ISO-8601) to stamp on the next tracked change.
    /// The engine never invents time; the JS shell calls this with `new Date().toISOString()` before
    /// a tracked edit.
    #[wasm_bindgen(js_name = setNow)]
    pub fn set_now(&mut self, iso: &str) {
        self.now = iso.to_string();
    }

    /// Accept the tracked change under the caret `(para, off)` (insertion -> keep text, deletion ->
    /// remove text). Returns whether one was resolved. Re-layout + re-paint after.
    #[wasm_bindgen(js_name = acceptChange)]
    pub fn accept_change(&self, para: u32, off: u32) -> Result<bool, JsError> {
        self.resolve_at(para, off, true)
    }

    /// Reject the tracked change under the caret (insertion -> remove text, deletion -> keep text).
    #[wasm_bindgen(js_name = rejectChange)]
    pub fn reject_change(&self, para: u32, off: u32) -> Result<bool, JsError> {
        self.resolve_at(para, off, false)
    }

    /// Accept a specific revision id in the region of `para` (the inline click popup carries both the
    /// click's paragraph and the id from [`track_at`]; revision ids are per-story, so the region picks
    /// the right child document).
    #[wasm_bindgen(js_name = acceptRevision)]
    pub fn accept_revision(&self, para: u32, id: u32) -> Result<bool, JsError> {
        let Some((doc, _)) = self.route(para) else { return Ok(false) };
        doc.accept_revision(id as u64, "accept change").map_err(to_js)
    }

    /// Reject a specific revision id in the region of `para`.
    #[wasm_bindgen(js_name = rejectRevision)]
    pub fn reject_revision(&self, para: u32, id: u32) -> Result<bool, JsError> {
        let Some((doc, _)) = self.route(para) else { return Ok(false) };
        doc.reject_revision(id as u64, "reject change").map_err(to_js)
    }

    /// Accept every tracked change in the document - body, header, and footer. Returns the total
    /// count resolved.
    #[wasm_bindgen(js_name = acceptAll)]
    pub fn accept_all(&self) -> Result<usize, JsError> {
        let mut n = self.doc.accept_all("accept all changes").map_err(to_js)?;
        for (part, _) in self.doc.hf_parts() {
            if let Some(d) = self.doc.hf_part_doc(&part) {
                n += d.accept_all("accept all changes").map_err(to_js)?;
            }
        }
        Ok(n)
    }

    /// Reject every tracked change in the document - body, header, and footer. Returns the total count.
    #[wasm_bindgen(js_name = rejectAll)]
    pub fn reject_all(&self) -> Result<usize, JsError> {
        let mut n = self.doc.reject_all("reject all changes").map_err(to_js)?;
        for (part, _) in self.doc.hf_parts() {
            if let Some(d) = self.doc.hf_part_doc(&part) {
                n += d.reject_all("reject all changes").map_err(to_js)?;
            }
        }
        Ok(n)
    }

    /// The caret `[para, off]` of the next tracked change after `(para, off)`, searched **across all
    /// stories** (body + header + footer) and wrapping, or an empty array when the document has no
    /// tracked changes. For Review > Next.
    #[wasm_bindgen(js_name = nextChange)]
    pub fn next_change(&self, para: u32, off: u32) -> Vec<u32> {
        self.adjacent_change(para as usize, off as usize, true)
    }

    /// The caret `[para, off]` of the previous tracked change before `(para, off)`, across all
    /// stories (wraps).
    #[wasm_bindgen(js_name = prevChange)]
    pub fn prev_change(&self, para: u32, off: u32) -> Vec<u32> {
        self.adjacent_change(para as usize, off as usize, false)
    }

    /// The tracked change under `(para, off)` for the hover tooltip + click popup, or `undefined`
    /// when the point isn't over a change.
    #[wasm_bindgen(js_name = trackAt)]
    pub fn track_at(&self, para: u32, off: u32) -> Option<TrackHit> {
        let (doc, p) = self.route(para)?;
        match doc.track_at(p, off as usize) {
            Ok(Some(r)) => Some(TrackHit {
                id: r.track.id as u32,
                kind: track_kind_str(r.track.kind).to_string(),
                author: r.track.author,
                date: r.track.date,
                text: r.text,
            }),
            _ => None,
        }
    }

    /// Resolve (accept/reject) the change under `(para, off)` by looking up its revision id.
    fn resolve_at(&self, para: u32, off: u32, accept: bool) -> Result<bool, JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(false) };
        let Some(r) = doc.track_at(p, off as usize).map_err(to_js)? else {
            return Ok(false);
        };
        if accept {
            doc.accept_revision(r.track.id, "accept change").map_err(to_js)
        } else {
            doc.reject_revision(r.track.id, "reject change").map_err(to_js)
        }
    }

    /// Every tracked change across stories as a JSON array (for the reviewing pane): each object has
    /// `id`, `kind` (`"ins"` / `"del"` / `"fmt"` / `"movefrom"` / `"moveto"` for run changes;
    /// `"rowins"` / `"rowdel"` / `"colins"` / `"coldel"` for table-structure changes), `author`,
    /// `date`, `text`, and the caret `para` (namespaced) / `off`. Run changes come from each story's
    /// `change_carets` + `track_at`; table changes from `table_changes()`. Sorted in document order.
    /// (Comments come from [`list_comments`](Self::list_comments) - the pane merges the two.)
    #[wasm_bindgen(js_name = listChanges)]
    pub fn list_changes(&self) -> String {
        // (para, off, json-object) so run + table changes interleave in document order.
        let mut entries: Vec<(usize, usize, String)> = Vec::new();
        let mut gather = |doc: &scriptor_crdt::CollabDoc, base: usize| {
            if let Ok(carets) = doc.change_carets() {
                for (p, o) in carets {
                    if let Ok(Some(r)) = doc.track_at(p, o) {
                        entries.push((
                            base + p,
                            o,
                            format!(
                                "{{\"id\":{},\"kind\":\"{}\",\"author\":\"{}\",\"date\":\"{}\",\"text\":\"{}\",\"para\":{},\"off\":{}}}",
                                r.track.id,
                                track_kind_str(r.track.kind),
                                json_escape(&r.track.author),
                                json_escape(&r.track.date),
                                json_escape(&r.text),
                                base + p,
                                o,
                            ),
                        ));
                    }
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
        // Table-structure changes (body only; one per distinct id). `text` is empty so the pane's jump
        // is a collapsed-caret move to the row / column's first cell.
        for tc in self.doc.table_changes() {
            use scriptor_crdt::TablePropLevel;
            let kind = match tc.prop_level {
                Some(TablePropLevel::Table) => "tblprop",
                Some(TablePropLevel::Row) => "rowprop",
                Some(TablePropLevel::Cell) => "cellprop",
                None => match (tc.is_row, tc.kind) {
                    (true, TrackKind::Del) => "rowdel",
                    (true, _) => "rowins",
                    (false, TrackKind::Del) => "coldel",
                    (false, _) => "colins",
                },
            };
            entries.push((
                tc.para,
                0,
                format!(
                    "{{\"id\":{},\"kind\":\"{}\",\"author\":\"{}\",\"date\":\"{}\",\"text\":\"\",\"para\":{},\"off\":0}}",
                    tc.id,
                    kind,
                    json_escape(&tc.author),
                    json_escape(&tc.date),
                    tc.para,
                ),
            ));
        }
        entries.sort_by_key(|a| (a.0, a.1));
        let mut out = String::from("[");
        for (i, (_, _, json)) in entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(json);
        }
        out.push(']');
        out
    }

    /// Next/previous tracked-change caret across every story (body, header, footer), namespaced and
    /// wrapping. Returns `[para, off]`, or empty when the whole document has no tracked changes.
    pub(crate) fn adjacent_change(&self, para: usize, off: usize, forward: bool) -> Vec<u32> {
        let mut all: Vec<(usize, usize)> = Vec::new();
        let mut gather = |doc: &scriptor_crdt::CollabDoc, base: usize| {
            if let Ok(carets) = doc.change_carets() {
                for (p, o) in carets {
                    all.push((p + base, o));
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
