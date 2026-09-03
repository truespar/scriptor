//! Paragraph styles.
//! 
//! The style gallery, applying a style, and editing a style definition so every
//! paragraph using it reflows.

use crate::*;

#[wasm_bindgen]
impl ScriptorDoc {
    /// The Styles gallery as a JSON array (Title / Subtitle / Heading N / Normal / ... - the document's
    /// quick styles), for the Home tab's Styles gallery. Each entry carries the style's resolved preview
    /// formatting so the gallery can render each name in its own look:
    /// `{"id","name","size"(half-points,0=inherit),"bold","italic","color"(hex,""=inherit),"font"}`.
    #[wasm_bindgen(js_name = styleGallery)]
    pub fn style_gallery(&self) -> String {
        let mut out = String::from("[");
        for (i, (id, name)) in self.doc.style_gallery().into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let p = self.doc.resolve_style(&id);
            out.push_str(&format!(
                "{{\"id\":\"{}\",\"name\":\"{}\",\"size\":{},\"bold\":{},\"italic\":{},\"color\":\"{}\",\"font\":\"{}\"}}",
                json_escape(&id),
                json_escape(&name),
                p.size.unwrap_or(0),
                p.bold.unwrap_or(false),
                p.italic.unwrap_or(false),
                json_escape(p.color.as_deref().unwrap_or("")),
                json_escape(p.font.as_deref().unwrap_or("")),
            ));
        }
        out.push(']');
        out
    }

    /// Paragraph `para`'s current named style id (`w:pStyle`), or `""` for the default (Normal) - lets
    /// the Styles dropdown reflect the caret's paragraph.
    #[wasm_bindgen(js_name = paragraphStyle)]
    pub fn paragraph_style(&self, para: u32) -> String {
        match self.route(para) {
            Some((doc, local)) => doc.paragraph_style(local).unwrap_or_default(),
            None => String::new(),
        }
    }

    /// Set (or clear, when `style` is empty -> Normal) paragraph `para`'s named style (`w:pStyle`).
    /// With Track-Changes on this records a `w:pPrChange` (a style change is a paragraph-property
    /// change); otherwise it applies directly. Routed through the shared edit path. Re-layout + re-paint.
    #[wasm_bindgen(js_name = setParagraphStyle)]
    pub fn set_paragraph_style(&self, para: u32, style: &str) -> Result<(), JsError> {
        let Some((doc, p)) = self.route(para) else { return Ok(()) };
        let style = (!style.is_empty()).then(|| style.to_string());
        scriptor_edit::apply(
            doc,
            &self.ctx(),
            scriptor_edit::EditOp::SetParagraphStyle { para: p, style },
        )
        .map_err(to_js)?;
        Ok(())
    }

    /// The *resolved* definition of paragraph style `id` as JSON, for prefilling the Modify-Style
    /// dialog: `{"size"(half-pts,0=inherit),"bold","italic","color"(hex,""=inherit),"font"(""=inherit),
    /// "lineSpacing"(240ths,0=inherit),"spaceBefore"(twips,-1=inherit),"spaceAfter"(twips,-1=inherit)}`.
    /// Resolved through the style's `basedOn` chain over docDefaults, with any runtime edit folded in -
    /// so the dialog opens showing what the style currently renders at.
    #[wasm_bindgen(js_name = resolveStyleProps)]
    pub fn resolve_style_props(&self, id: &str) -> String {
        let p = self.doc.resolve_style(id);
        format!(
            "{{\"size\":{},\"bold\":{},\"italic\":{},\"color\":\"{}\",\"font\":\"{}\",\"lineSpacing\":{},\"lineRule\":\"{}\",\"spaceBefore\":{},\"spaceAfter\":{},\"align\":\"{}\"}}",
            p.size.unwrap_or(0),
            p.bold.unwrap_or(false),
            p.italic.unwrap_or(false),
            json_escape(p.color.as_deref().unwrap_or("")),
            json_escape(p.font.as_deref().unwrap_or("")),
            p.line_spacing.unwrap_or(0),
            p.line_rule.map(|r| r.as_str()).unwrap_or("auto"),
            p.space_before.map(|v| v as i64).unwrap_or(-1),
            p.space_after.map(|v| v as i64).unwrap_or(-1),
            p.align.map(|a| a.as_str()).unwrap_or(""),
        )
    }

    /// Edit style `id`'s *definition* (Word's Modify-Style): every paragraph resolving through `id`
    /// re-renders with the new properties. Per-field merge - each argument is a sentinel meaning
    /// "leave this field unchanged" so the dialog can write only what the user touched:
    /// `size`/`line_spacing`/`space_before`/`space_after` < 0 = unchanged (else the value);
    /// `bold`/`italic` < 0 = unchanged, 0 = off, 1 = on; `color`/`font` empty = unchanged. Direct, not
    /// a tracked revision (Word doesn't redline a style-definition change). Body story only. Re-layout.
    #[wasm_bindgen(js_name = setStyleProps)]
    #[allow(clippy::too_many_arguments)]
    pub fn set_style_props(
        &self,
        id: &str,
        size: i32,
        bold: i32,
        italic: i32,
        color: &str,
        font: &str,
        line_spacing: i32,
        space_before: i32,
        space_after: i32,
        align: &str,
        line_rule: &str,
    ) -> Result<(), JsError> {
        let props = scriptor_crdt::StyleProps {
            size: (size >= 0).then_some(size as u16),
            bold: (bold >= 0).then_some(bold != 0),
            italic: (italic >= 0).then_some(italic != 0),
            color: (!color.is_empty()).then(|| color.to_string()),
            font: (!font.is_empty()).then(|| font.to_string()),
            line_spacing: (line_spacing >= 0).then_some(line_spacing as u16),
            // The rule travels with the value: 'auto' (or empty) = a 240ths multiplier; 'exact' =
            // absolute twips (a fixed line height). 'atLeast' is accepted but left on the multiplier
            // path in render (deliberately unmodelled), so the dialog only offers auto + exact.
            line_rule: parse_line_rule(line_rule),
            space_before: (space_before >= 0).then_some(space_before as u32),
            space_after: (space_after >= 0).then_some(space_after as u32),
            align: scriptor_crdt::Align::parse(align), // "" / invalid -> None (unchanged)
            ..Default::default()
        };
        self.doc.set_style_props(id, &props).map_err(to_js)?;
        Ok(())
    }

    /// Create a new paragraph style (Word's New-Style / Save-Selection-as-a-Style) named `name`, based
    /// on `based_on` (empty = no parent), with the given formatting (same per-field sentinels as
    /// `setStyleProps`). Mints a unique style id from `name`, registers it (gallery + persistence), and
    /// returns the id so the caller can apply it to the selected paragraph(s). Body story only.
    #[wasm_bindgen(js_name = addStyle)]
    #[allow(clippy::too_many_arguments)]
    pub fn add_style(
        &self,
        name: &str,
        based_on: &str,
        size: i32,
        bold: i32,
        italic: i32,
        color: &str,
        font: &str,
        line_spacing: i32,
        space_before: i32,
        space_after: i32,
        align: &str,
        line_rule: &str,
    ) -> Result<String, JsError> {
        let existing: std::collections::HashSet<String> = {
            let s = self.doc.styles();
            s.by_id.keys().chain(s.names.keys()).cloned().collect()
        };
        let id = unique_style_id(&existing, name);
        let props = scriptor_crdt::StyleProps {
            size: (size >= 0).then_some(size as u16),
            bold: (bold >= 0).then_some(bold != 0),
            italic: (italic >= 0).then_some(italic != 0),
            color: (!color.is_empty()).then(|| color.to_string()),
            font: (!font.is_empty()).then(|| font.to_string()),
            line_spacing: (line_spacing >= 0).then_some(line_spacing as u16),
            line_rule: parse_line_rule(line_rule),
            space_before: (space_before >= 0).then_some(space_before as u32),
            space_after: (space_after >= 0).then_some(space_after as u32),
            align: scriptor_crdt::Align::parse(align),
            ..Default::default()
        };
        let based = (!based_on.is_empty()).then_some(based_on);
        self.doc.add_style(&id, name, based, &props).map_err(to_js)?;
        Ok(id)
    }

    /// The paragraph-level formatting of paragraph `para` (for the Paragraph-group toolbar state).
    #[wasm_bindgen(js_name = paragraphFormat)]
    pub fn paragraph_format(&self, para: u32) -> Result<ParaFmt, JsError> {
        let Some((doc, local)) = self.route(para) else {
            return Ok(ParaFmt {
                align: String::new(),
                line_spacing: 0,
                indent_left: 0,
                indent_right: 0,
                indent_first: 0,
            });
        };
        let p = doc.paragraph_format(local).map_err(to_js)?;
        Ok(ParaFmt {
            align: p.align.map(|a| a.as_str().to_string()).unwrap_or_default(),
            line_spacing: p.line_spacing.unwrap_or(0),
            indent_left: p.indent_left.unwrap_or(0),
            indent_right: p.indent_right.unwrap_or(0),
            indent_first: p.indent_first.unwrap_or(0),
        })
    }
}
