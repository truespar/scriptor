//! Character and paragraph formatting.
//! 
//! The Home tab: the run-level toggles, size, colour, font and highlight, plus
//! alignment, line spacing and indents, and the query that reports what the current
//! selection resolves to.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    pub(crate) fn para_format(&self, para: u32, props: scriptor_crdt::ParaProps) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::ApplyParagraphFormat { para: p, props },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// Set paragraph alignment ("left" | "center" | "right" | "justify"). Re-paint after.
    #[wasm_bindgen(js_name = setAlignment)]
    pub fn set_alignment(&self, para: u32, align: &str) -> Result<(), JsError> {
        let a = scriptor_crdt::Align::parse(align)
            .ok_or_else(|| JsError::new("invalid alignment"))?;
        self.para_format(para, scriptor_crdt::ParaProps { align: Some(a), ..Default::default() })
    }

    /// Set line spacing in 240ths (240 = single, 360 = 1.5, 480 = double). Re-paint after.
    #[wasm_bindgen(js_name = setLineSpacing)]
    pub fn set_line_spacing(&self, para: u32, x240: u16) -> Result<(), JsError> {
        self.para_format(para, scriptor_crdt::ParaProps { line_spacing: Some(x240), ..Default::default() })
    }

    /// Set the left indent (twips). Re-paint after.
    #[wasm_bindgen(js_name = setIndentLeft)]
    pub fn set_indent_left(&self, para: u32, twips: i32) -> Result<(), JsError> {
        self.para_format(para, scriptor_crdt::ParaProps { indent_left: Some(twips), ..Default::default() })
    }

    /// Set the right indent (twips). Re-paint after.
    #[wasm_bindgen(js_name = setIndentRight)]
    pub fn set_indent_right(&self, para: u32, twips: i32) -> Result<(), JsError> {
        self.para_format(para, scriptor_crdt::ParaProps { indent_right: Some(twips), ..Default::default() })
    }

    /// Set the first-line indent (twips; negative = hanging). Re-paint after.
    #[wasm_bindgen(js_name = setIndentFirst)]
    pub fn set_indent_first(&self, para: u32, twips: i32) -> Result<(), JsError> {
        self.para_format(para, scriptor_crdt::ParaProps { indent_first: Some(twips), ..Default::default() })
    }

    pub(crate) fn run_format(&self, para: u32, start: u32, end: u32, fmt: scriptor_crdt::RunFormat) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::ApplyRunFormat {
                para: p,
                range: (start as usize)..(end as usize),
                format: fmt,
            },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// Toggle/set bold over codepoint `[start, end)` in paragraph `para`. Re-paint after.
    #[wasm_bindgen(js_name = formatBold)]
    pub fn format_bold(&self, para: u32, start: u32, end: u32, on: bool) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::bold(on))
    }

    /// Toggle/set italic over `[start, end)`. Re-paint after.
    #[wasm_bindgen(js_name = formatItalic)]
    pub fn format_italic(&self, para: u32, start: u32, end: u32, on: bool) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::italic(on))
    }

    /// Toggle/set underline over `[start, end)`. Re-paint after.
    #[wasm_bindgen(js_name = formatUnderline)]
    pub fn format_underline(&self, para: u32, start: u32, end: u32, on: bool) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::underline(on))
    }

    /// Toggle/set strikethrough over `[start, end)`. Re-paint after.
    #[wasm_bindgen(js_name = formatStrike)]
    pub fn format_strike(&self, para: u32, start: u32, end: u32, on: bool) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::strike(on))
    }

    /// Set font size (half-points, OOXML `w:sz`) over `[start, end)`. Re-paint after.
    #[wasm_bindgen(js_name = formatSize)]
    pub fn format_size(&self, para: u32, start: u32, end: u32, half_points: u16) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::size(half_points))
    }

    /// Set text color (RRGGBB hex) over `[start, end)`. Re-paint after.
    #[wasm_bindgen(js_name = formatColor)]
    pub fn format_color(&self, para: u32, start: u32, end: u32, hex: &str) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::color(hex))
    }

    /// Set font family over `[start, end)`. Re-paint after.
    #[wasm_bindgen(js_name = formatFont)]
    pub fn format_font(&self, para: u32, start: u32, end: u32, family: &str) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::font(family))
    }

    /// Set / clear the highlight color over `[start, end)` (`""` clears it). Re-paint after.
    #[wasm_bindgen(js_name = formatHighlight)]
    pub fn format_highlight(&self, para: u32, start: u32, end: u32, color: &str) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::highlight(color))
    }

    /// Set / clear vertical alignment over `[start, end)` ("superscript" / "subscript", or `""` to
    /// clear back to baseline). Re-paint after.
    #[wasm_bindgen(js_name = formatVertAlign)]
    pub fn format_vert_align(&self, para: u32, start: u32, end: u32, value: &str) -> Result<(), JsError> {
        self.run_format(para, start, end, scriptor_crdt::RunFormat::vert_align(value))
    }

    /// Clear all inline run formatting over `[start, end)` (the Clear Formatting eraser). Re-paint after.
    #[wasm_bindgen(js_name = clearFormatting)]
    pub fn clear_formatting(&self, para: u32, start: u32, end: u32) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        doc.clear_run_format(p, start as usize, end as usize, "format: clear").map_err(to_js)?;
        Ok(())
    }

    /// The resolved formatting of codepoint `[start, end)` in paragraph `para`, for driving toolbar
    /// state. Boolean getters are tri-state via `*IsMixed` (true = the selection spans both).
    #[wasm_bindgen(js_name = selectionFormat)]
    pub fn selection_format(&self, para: u32, start: u32, end: u32) -> Result<SelFormat, JsError> {
        let Some((doc, p)) = self.route(para) else {
            return Ok(SelFormat {
                bold: false, bold_mixed: false, italic: false, italic_mixed: false,
                underline: false, underline_mixed: false, strike: false, strike_mixed: false,
                size: 0, color: String::new(), font: String::new(),
                highlight: String::new(), vert_align: String::new(),
            });
        };
        let f = doc
            .selection_format(p, start as usize, end as usize)
            .map_err(to_js)?;
        Ok(SelFormat {
            bold: f.bold.unwrap_or(false),
            bold_mixed: f.bold.is_none(),
            italic: f.italic.unwrap_or(false),
            italic_mixed: f.italic.is_none(),
            underline: f.underline.unwrap_or(false),
            underline_mixed: f.underline.is_none(),
            strike: f.strike.unwrap_or(false),
            strike_mixed: f.strike.is_none(),
            size: f.size.unwrap_or(0),
            color: f.color.unwrap_or_default(),
            font: f.font.unwrap_or_default(),
            highlight: f.highlight.unwrap_or_default(),
            vert_align: f.vert_align.unwrap_or_default(),
        })
    }
}
