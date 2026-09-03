//! Numbering and bullets.
//! 
//! Binding a paragraph to a list definition, changing its level, and reading back what
//! format it currently resolves to.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// Set (or clear) paragraph `para`'s list numbering (`w:numPr`): `num_id < 0` removes it from any
    /// list; otherwise it joins list `num_id` at level `ilvl` (a negative `ilvl` defaults to 0). With
    /// Track-Changes on this records a `w:pPrChange` (a numbering change is a paragraph-property
    /// change); otherwise it applies directly. Routed through the shared edit path. Re-layout + re-paint.
    #[wasm_bindgen(js_name = setNumbering)]
    pub fn set_numbering(&self, para: u32, num_id: i32, ilvl: i32) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        let num = (num_id >= 0).then_some(num_id);
        let lvl = (ilvl >= 0).then_some(ilvl);
        scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::SetNumbering { para: p, num_id: num, ilvl: lvl },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// Paragraph `para`'s current list id (`w:numPr/w:numId`), or `-1` when it isn't in a list - lets
    /// the toolbar reflect / toggle the numbering state.
    #[wasm_bindgen(js_name = paragraphNumId)]
    pub fn paragraph_num_id(&self, para: u32) -> i32 {
        match self.route(para) {
            Some((doc, local)) => doc.paragraph_format(local).ok().and_then(|p| p.num_id).unwrap_or(-1),
            None => -1,
        }
    }

    /// Paragraph `para`'s list level (`w:numPr/w:ilvl`, 0-8), or `-1` when it isn't in a list - lets
    /// Tab / Shift+Tab demote / promote a list item to the next / previous level.
    #[wasm_bindgen(js_name = paragraphListLevel)]
    pub fn paragraph_list_level(&self, para: u32) -> i32 {
        match self.route(para) {
            Some((doc, local)) => doc
                .paragraph_format(local)
                .ok()
                .filter(|p| p.num_id.is_some())
                .map(|p| p.num_ilvl.unwrap_or(0))
                .unwrap_or(-1),
            None => -1,
        }
    }

    /// The kind of list paragraph `para` is in: `"bullet"`, `"number"`, or `""` (not a list) - lets the
    /// toolbar toggle the Bullets / Numbering buttons like Word.
    #[wasm_bindgen(js_name = paragraphListKind)]
    pub fn paragraph_list_kind(&self, para: u32) -> String {
        match self.route(para) {
            Some((doc, local)) => match doc.paragraph_list_kind(local) {
                Some(true) => "bullet".to_string(),
                Some(false) => "number".to_string(),
                None => String::new(),
            },
            None => String::new(),
        }
    }

    /// Ensure a list definition of `fmt` exists (reusing the doc's own, else synthesizing one written
    /// into numbering.xml on save), then set paragraph `para`'s numbering at level 0. Body only; tracked
    /// as a `w:pPrChange` when Track-Changes is on, otherwise direct. Shared by the list buttons + picker.
    fn apply_list_kind(&mut self, para: u32, fmt: scriptor_crdt::ListFormat) -> Result<(), JsError> {
        let (region, local) = decode_region(para as usize);
        if region != Region::Body {
            return Ok(());
        }
        let num_id = self.doc.ensure_list(fmt);
        scriptor_edit::apply(
            &self.doc,
            &self.ctx(),
            scriptor_edit::EditOp::SetNumbering { para: local, num_id: Some(num_id), ilvl: Some(0) },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// Apply a bullet (`bullet = true`) / decimal numbered list to paragraph `para` (body only). The
    /// Bullets / Numbering buttons. Re-layout + re-paint after.
    #[wasm_bindgen(js_name = applyList)]
    pub fn apply_list(&mut self, para: u32, bullet: bool) -> Result<(), JsError> {
        let fmt = if bullet {
            scriptor_crdt::ListFormat::Bullet
        } else {
            scriptor_crdt::ListFormat::Decimal
        };
        self.apply_list_kind(para, fmt)
    }

    /// Apply a numbered list with a specific number format to paragraph `para` (body only): `numFmt` is
    /// an OOXML token (`decimal` / `lowerLetter` / `upperLetter` / `lowerRoman` / `upperRoman`). The
    /// Numbering button's format picker. Tracked as a `w:pPrChange` when Track-Changes is on.
    #[wasm_bindgen(js_name = applyListFormat)]
    pub fn apply_list_format(&mut self, para: u32, num_fmt: &str) -> Result<(), JsError> {
        self.apply_list_kind(para, scriptor_crdt::ListFormat::from_numfmt(num_fmt))
    }

    /// Paragraph `para`'s list level-0 number format (`"decimal"` / `"lowerRoman"` / `"bullet"` / ...),
    /// or `""` when it isn't in a list - lets the Numbering format picker check the active format.
    #[wasm_bindgen(js_name = paragraphListFormat)]
    pub fn paragraph_list_format(&self, para: u32) -> String {
        match self.route(para) {
            Some((doc, local)) => doc.paragraph_list_format(local).unwrap_or_default(),
            None => String::new(),
        }
    }
}
