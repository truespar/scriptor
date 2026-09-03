//! Table of contents, hyperlinks and bookmarks.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Insert a table of contents at body paragraph `at` (the caret), built from the document's current
    /// headings (`Heading1`..`Heading9`): one `TOC{level}` line per heading - "{heading}\t{page}" -
    /// wrapped as a real `TOC` field so Word can update it (F9). Page numbers come from a fresh layout
    /// that already includes the inserted lines. Returns whether a TOC was inserted (`false` when there
    /// are no headings, or `at` isn't a body paragraph). Re-layout + re-paint after.
    #[wasm_bindgen(js_name = insertToc)]
    pub fn insert_toc(&mut self, at: u32) -> Result<bool, JsError> {
        let (region, local) = decode_region(at as usize);
        if region != Region::Body {
            return Ok(false);
        }
        self.insert_toc_at(local)
    }

    /// Update (regenerate) the document's TOC in place: delete the old field block, then rebuild it from
    /// the current headings + page numbers (Word's F9). When there's no existing TOC, insert one at the
    /// caret `at` instead. Returns whether a TOC was written. Re-layout + re-paint after.
    #[wasm_bindgen(js_name = updateToc)]
    pub fn update_toc(&mut self, at: u32) -> Result<bool, JsError> {
        match self.doc.remove_toc("toc: update").map_err(to_js)? {
            Some(start) => self.insert_toc_at(start),
            None => {
                let (region, local) = decode_region(at as usize);
                if region != Region::Body {
                    return Ok(false);
                }
                self.insert_toc_at(local)
            }
        }
    }

    /// Add a hyperlink over codepoint `[start, end)` in paragraph `para`, targeting `target` (an
    /// external URL, or `#bookmarkName` for an internal jump). Re-layout + re-paint after.
    #[wasm_bindgen(js_name = addHyperlink)]
    pub fn add_hyperlink(&self, para: u32, start: u32, end: u32, target: &str) -> Result<(), JsError> {
        let Some((doc, local)) = self.route(para) else { return Ok(()) };
        doc.add_hyperlink(local, start as usize, end as usize, target, "hyperlink: add").map_err(to_js)?;
        Ok(())
    }

    /// Remove the hyperlink at codepoint `off` in paragraph `para`. Returns whether one was removed.
    #[wasm_bindgen(js_name = removeHyperlink)]
    pub fn remove_hyperlink(&self, para: u32, off: u32) -> Result<bool, JsError> {
        let Some((doc, local)) = self.route(para) else { return Ok(false) };
        doc.remove_hyperlink(local, off as usize, "hyperlink: remove").map_err(to_js)
    }

    /// The hyperlink target at codepoint `off` in paragraph `para` (external URL or `#bookmarkName`),
    /// or `""` when the caret isn't on a link - lets the toolbar reflect / the caret follow it.
    #[wasm_bindgen(js_name = linkAt)]
    pub fn link_at(&self, para: u32, off: u32) -> String {
        match self.route(para) {
            Some((doc, local)) => {
                doc.link_at(local, off as usize).ok().flatten().map(|(_, t)| t).unwrap_or_default()
            }
            None => String::new(),
        }
    }

    /// The body paragraph index where bookmark `name` begins, or `-1` - lets an internal hyperlink
    /// (`#name`) jump to its target. Body only.
    #[wasm_bindgen(js_name = bookmarkParagraph)]
    pub fn bookmark_paragraph(&self, name: &str) -> i32 {
        self.doc.bookmark_paragraph(name).map(|p| p as i32).unwrap_or(-1)
    }

    /// Add a named bookmark over codepoint `[start, end)` in paragraph `para`. The name should already be
    /// a valid Word bookmark name (letters/digits/underscore, letter-initial); the caller sanitizes.
    /// Re-paint after (bookmarks are invisible but become hyperlink targets).
    #[wasm_bindgen(js_name = addBookmark)]
    pub fn add_bookmark(&self, para: u32, start: u32, end: u32, name: &str) -> Result<(), JsError> {
        let Some((doc, local)) = self.route(para) else { return Ok(()) };
        doc.add_bookmark(local, start as usize, end as usize, name, "bookmark: add").map_err(to_js)?;
        Ok(())
    }

    /// Build a table of contents at body paragraph `local`: one `TOC{level}` line per heading
    /// ("{heading}\t{page}"), each heading anchored by a fresh `_Toc{n}` bookmark and each TOC entry
    /// wrapped as an internal hyperlink to that anchor, the whole block wrapped as a real `TOC` field
    /// (so Word can update it with F9 and it round-trips). Returns whether anything was inserted
    /// (`false` when the document has no headings). Shared by `insertToc` + `updateToc`.
    pub(crate) fn insert_toc_at(&mut self, local: usize) -> Result<bool, JsError> {
        let headings = self.doc.headings();
        if headings.is_empty() {
            return Ok(false);
        }
        let entries: Vec<(u8, String)> = headings.iter().map(|(_, l, t)| (*l, t.clone())).collect();
        let count = entries.len();
        // Pass 1: insert the stub lines (heading text + tab, no page numbers, not yet a field).
        self.doc.insert_toc_entries(local, &entries).map_err(to_js)?;
        // Pass 2: lay out the doc (now including the stub), then per heading drop a `_Toc{n}` anchor
        // bookmark, append its page number to the matching TOC line, and wrap the whole line as an
        // internal hyperlink to the anchor. The page index is scale-invariant, so any scale works; the
        // next paint relayouts at the real scale.
        self.relayout(1.0).ok();
        let after = self.doc.headings(); // headings now at their shifted indices (TOC lines excluded)
        let paras = self.doc.paragraphs().map_err(to_js)?;
        let mut seq = self.doc.next_toc_seq();
        for (i, (hidx, _, _)) in after.iter().enumerate() {
            let para = local + i;
            // Anchor the heading (skip an empty one - a zero-width range can't carry a mark).
            let hlen: usize =
                paras.get(*hidx).map(|p| p.runs.iter().map(|r| r.text.chars().count()).sum()).unwrap_or(0);
            let anchor = if hlen > 0 {
                let name = format!("_Toc{seq}");
                seq += 1;
                self.doc.add_bookmark(*hidx, 0, hlen, &name, "toc: anchor").map_err(to_js)?;
                Some(name)
            } else {
                None
            };
            // Append the page number, then wrap the whole entry as an internal hyperlink to the anchor.
            let stub_len: usize =
                paras.get(para).map(|p| p.runs.iter().map(|r| r.text.chars().count()).sum()).unwrap_or(0);
            let page = self.layout.page_of_para(*hidx).unwrap_or(1).to_string();
            self.doc.insert_text(para, stub_len, &page, "toc: page number").map_err(to_js)?;
            if let Some(name) = anchor {
                let full = stub_len + page.chars().count();
                self.doc.add_hyperlink(para, 0, full, &format!("#{name}"), "toc: entry link").map_err(to_js)?;
            }
        }
        // Pass 3: wrap the lines as a TOC field (so it round-trips + Word can update it).
        self.doc.finish_toc(local, count, " TOC \\o \"1-3\" \\h \\z \\u ").map_err(to_js)?;
        Ok(true)
    }
}
