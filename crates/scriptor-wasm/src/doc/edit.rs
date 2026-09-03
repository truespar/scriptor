//! Text editing and undo.
//! 
//! Insert, delete and move, each routed to whichever story the caret is in and stamped
//! with the current author and clock when tracking is on.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Insert `text` at codepoint `off` in paragraph `para` as a direct human edit, routed through
    /// the shared `scriptor_edit::apply` path (the same one the agent uses). Call [`paint`] after.
    #[wasm_bindgen(js_name = insertText)]
    pub fn insert_text(&self, para: u32, off: u32, text: &str) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::InsertText { para: p, pos: off as usize, text: text.to_string() },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// Delete codepoint range `[start, end)` in paragraph `para` as a direct human edit. No-op when
    /// the range is empty. Call [`paint`] after.
    #[wasm_bindgen(js_name = deleteRange)]
    pub fn delete_range(&self, para: u32, start: u32, end: u32) -> Result<(), JsError> {
        if end <= start {
            return Ok(());
        }
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        let (s, e) = (start as usize, end as usize);
        // Word nuance: deleting your own un-accepted insertion removes it outright rather than
        // stacking a `w:del` on a `w:ins`. (Always false when not tracking, so this is a no-op then.)
        if self.track_changes
            && doc.range_is_own_insertion(p, s, e, &self.author_name).map_err(to_js)?
        {
            doc.delete_text(p, s..e, "delete own insertion").map_err(to_js)?;
            return Ok(());
        }
        scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::DeleteRange { para: p, range: s..e },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// Move codepoint range `[from_start, from_end)` in paragraph `from_para` to codepoint `to_off` in
    /// paragraph `to_para` as a tracked move (`w:moveFrom` source + `w:moveTo` destination, one shared
    /// revision id). Both endpoints must be in the same story (body / header / footer); the destination
    /// must lie outside the source range. Returns the move's revision id, or `-1` when the endpoints
    /// span stories / the range is empty / the move is into itself. Re-paint after.
    #[wasm_bindgen(js_name = moveRange)]
    pub fn move_range(
        &self,
        from_para: u32,
        from_start: u32,
        from_end: u32,
        to_para: u32,
        to_off: u32,
    ) -> Result<i32, JsError> {
        if from_end <= from_start {
            return Ok(-1);
        }
        let (region, _) = decode_region(from_para as usize);
        let (to_region, tp) = decode_region(to_para as usize);
        if region != to_region {
            return Ok(-1);
        }
        let Some((doc, fp)) = self.route(from_para) else { return Ok(-1) };
        match doc.suggest_move(
            fp,
            from_start as usize..from_end as usize,
            tp,
            to_off as usize,
            &self.author_name,
            &self.now,
            "move",
        ) {
            Ok(id) => Ok(id as i32),
            // A move into itself is rejected by the model - surface it as a no-op, not a hard error.
            Err(_) => Ok(-1),
        }
    }

    /// Mark codepoint range `[start, end)` in paragraph `para` as the **source** of a move
    /// (`w:moveFrom`, text retained), returning the move's revision id (or `-1` for an empty range /
    /// missing story). The matching destination is added with [`add_move_dest`](Self::add_move_dest)
    /// using this id - the two-step path the editor's cut-then-paste move uses. Re-paint after.
    #[wasm_bindgen(js_name = markMoveSource)]
    pub fn mark_move_source(&self, para: u32, start: u32, end: u32) -> Result<i32, JsError> {
        if end <= start {
            return Ok(-1);
        }
        let Some((doc, p)) = self.route(para) else { return Ok(-1) };
        doc.suggest_move_source(
            p,
            start as usize..end as usize,
            &self.author_name,
            &self.now,
            "move (cut)",
        )
        .map(|id| id as i32)
        .map_err(to_js)
    }

    /// Insert `text` at codepoint `off` in paragraph `para` as the **destination** of move `id`
    /// (`w:moveTo`), pairing with a prior [`mark_move_source`](Self::mark_move_source). Re-paint after.
    #[wasm_bindgen(js_name = addMoveDest)]
    pub fn add_move_dest(&self, para: u32, off: u32, text: &str, id: u32) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        doc.suggest_move_dest(
            p,
            off as usize,
            text,
            id as u64,
            &self.author_name,
            &self.now,
            "move (paste)",
        )
        .map_err(to_js)
    }

    /// Undo the last local edit (Ctrl+Z) in the **active story** (body / header / footer - each child
    /// owns its own undo history). Returns whether anything changed. Re-paint after. The active story
    /// is set from the caret via [`set_active_story`](Self::set_active_story).
    #[wasm_bindgen(js_name = undo)]
    pub fn undo(&mut self) -> Result<bool, JsError> {
        match self.active_region {
            Region::Body => self.doc.undo(),
            Region::Header => match self.active_hf_doc_mut(false) {
                Some(h) => h.undo(),
                None => Ok(false),
            },
            Region::Footer => match self.active_hf_doc_mut(true) {
                Some(f) => f.undo(),
                None => Ok(false),
            },
        }
        .map_err(to_js)
    }

    /// Redo the last undone edit (Ctrl+Y / Ctrl+Shift+Z) in the active story. Returns whether anything
    /// changed.
    #[wasm_bindgen(js_name = redo)]
    pub fn redo(&mut self) -> Result<bool, JsError> {
        match self.active_region {
            Region::Body => self.doc.redo(),
            Region::Header => match self.active_hf_doc_mut(false) {
                Some(h) => h.redo(),
                None => Ok(false),
            },
            Region::Footer => match self.active_hf_doc_mut(true) {
                Some(f) => f.redo(),
                None => Ok(false),
            },
        }
        .map_err(to_js)
    }

    /// Whether there is anything to undo / redo in the active story (for greying the toolbar buttons).
    #[wasm_bindgen(js_name = canUndo)]
    pub fn can_undo(&self) -> bool {
        match self.active_region {
            Region::Body => self.doc.can_undo(),
            Region::Header | Region::Footer => self.active_hf_doc().is_some_and(|d| d.can_undo()),
        }
    }

    #[wasm_bindgen(js_name = canRedo)]
    pub fn can_redo(&self) -> bool {
        match self.active_region {
            Region::Body => self.doc.can_redo(),
            Region::Header | Region::Footer => self.active_hf_doc().is_some_and(|d| d.can_redo()),
        }
    }

    /// Split paragraph `para` at codepoint `off` (the Enter key) - text from `off` onward moves to a
    /// new paragraph after it. Routed through the shared `scriptor_edit::apply` path. Re-paint after.
    #[wasm_bindgen(js_name = splitParagraph)]
    pub fn split_paragraph(&self, para: u32, off: u32) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::SplitParagraph { para: p, pos: off as usize },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// Join paragraph `para` into the previous one (Backspace at paragraph start / Delete at end).
    /// Returns the codepoint offset in the previous paragraph where the two met (the merged caret),
    /// or `-1` when the join is refused because it would cross a table-cell boundary (the caller
    /// should leave the caret where it is). Routed through the shared `scriptor_edit::apply` path.
    #[wasm_bindgen(js_name = joinParagraph)]
    pub fn join_paragraph(&self, para: u32) -> Result<i32, JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(-1) };
        let outcome = scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::JoinParagraph { para: p },
        )
        .map_err(to_js)?;
        Ok(outcome.caret.map(|c| c as i32).unwrap_or(-1))
    }

    /// The edit context for the current local edit. With Track-Changes on, edits are recorded as
    /// tracked changes (suggesting mode) attributed to the current author + the last timestamp the JS
    /// shell handed in; otherwise they apply directly (and carry no revision timestamp). Structural +
    /// formatting ops apply directly in both modes (paragraph-mark / format revisions take a
    /// separate path), so passing this context is safe there.
    pub(crate) fn ctx(&self) -> scriptor_edit::EditContext {
        if self.track_changes {
            scriptor_edit::EditContext {
                actor: scriptor_edit::Actor::human(&self.author_id, &self.author_name),
                mode: scriptor_edit::EditMode::Tracked,
                at: self.now.clone(),
                rationale: None,
            }
        } else {
            scriptor_edit::EditContext {
                actor: scriptor_edit::Actor::human("local", "You"),
                mode: scriptor_edit::EditMode::Direct,
                at: String::new(),
                rationale: None,
            }
        }
    }
}
