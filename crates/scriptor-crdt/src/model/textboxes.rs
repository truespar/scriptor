//! Anchored text boxes (`wp:anchor` -> `wps:wsp` -> `w:txbxContent`).
//! 
//! Finds text boxes in a body part and reports their text, first-run formatting,
//! vertical flow and anchor geometry, which is what the renderer needs to paint a
//! rotated margin stamp. Also reports the body spans the boxes occupy so the main
//! import can skip them.

use super::*;

/// An anchored text box (`wp:anchor` -> `wps:wsp` + `w:txbxContent`) found in a body XML: its plain
/// text (cached field results included, field codes excluded), the first run's size/color, the
/// `wps:bodyPr w:vert` flow, and the anchor geometry - enough to paint a rotated margin stamp.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBoxInfo {
    pub text: String,
    /// First `w:rFonts w:ascii` seen in the box, `None` = inherit default.
    pub font: Option<String>,
    /// First `w:sz` seen in the box (half-points), 0 = unset.
    pub size_half_points: u32,
    /// First `w:color` seen in the box (`RRGGBB`), `None` = auto.
    pub color: Option<String>,
    /// `0` horizontal, `1` = `vert` (top-to-bottom), `2` = `vert270` (bottom-to-top).
    pub vert: u8,
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub h_from: String,
    pub v_from: String,
    /// 0-based index of the top-level `w:p` carrying the anchor (its story paragraph - a caret
    /// target for clicking the box).
    pub para_index: usize,
}

/// Scan a WordprocessingML body for **anchored text boxes** (`wp:anchor` carrying a
/// WordprocessingShape with `w:txbxContent`) - the floating stamps legal templates put in a footer.
/// The `mc:Fallback` subtree is skipped (it repeats the same box as VML - reading both would emit it
/// twice), as are `w:object` / `w:control` (preserved wholesale elsewhere). Inline (`wp:inline`)
/// boxes are not collected - they flow with the text and are covered by the passthrough placeholder.
pub fn parse_textboxes(xml: &[u8]) -> Vec<TextBoxInfo> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur: Option<TextBoxInfo> = None;
    let mut txbx_depth = 0usize;
    let mut para_index = 0usize;
    let (mut in_posh, mut in_posv, mut in_offset) = (false, false, false);
    let (mut in_text, mut in_instr) = (false, false);
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e)
                if matches!(e.name().as_ref(), b"mc:Fallback" | b"w:object" | b"w:control") =>
            {
                let name = e.name();
                let mut skip = Vec::new();
                let _ = reader.read_to_end_into(name, &mut skip);
            }
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"wp:anchor" => {
                    cur = Some(TextBoxInfo {
                        text: String::new(),
                        font: None,
                        size_half_points: 0,
                        color: None,
                        vert: 0,
                        x_emu: 0,
                        y_emu: 0,
                        w_emu: 0,
                        h_emu: 0,
                        h_from: String::new(),
                        v_from: String::new(),
                        para_index,
                    });
                }
                b"wp:positionH" => {
                    in_posh = true;
                    if let Some(c) = cur.as_mut() {
                        c.h_from = attr(&e, b"relativeFrom").unwrap_or_default();
                    }
                }
                b"wp:positionV" => {
                    in_posv = true;
                    if let Some(c) = cur.as_mut() {
                        c.v_from = attr(&e, b"relativeFrom").unwrap_or_default();
                    }
                }
                b"wp:posOffset" => in_offset = true,
                b"wp:extent" => {
                    if let Some(c) = cur.as_mut() {
                        let g = |k: &[u8]| attr(&e, k).and_then(|s| s.parse().ok()).unwrap_or(0);
                        c.w_emu = g(b"cx");
                        c.h_emu = g(b"cy");
                    }
                }
                b"wps:bodyPr" => {
                    if let Some(c) = cur.as_mut() {
                        c.vert = match attr(&e, b"vert").as_deref() {
                            Some("vert") => 1,
                            Some("vert270") => 2,
                            _ => 0,
                        };
                    }
                }
                b"w:txbxContent" => txbx_depth += 1,
                b"w:sz" if txbx_depth > 0 => {
                    if let Some(c) = cur.as_mut()
                        && c.size_half_points == 0
                    {
                        c.size_half_points =
                            attr(&e, b"w:val").and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                }
                b"w:rFonts" if txbx_depth > 0 => {
                    if let Some(c) = cur.as_mut()
                        && c.font.is_none()
                    {
                        c.font = attr(&e, b"w:ascii");
                    }
                }
                b"w:color" if txbx_depth > 0 => {
                    if let Some(c) = cur.as_mut()
                        && c.color.is_none()
                    {
                        c.color = attr(&e, b"w:val").filter(|v| v != "auto");
                    }
                }
                b"w:t" if txbx_depth > 0 => in_text = true,
                b"w:instrText" => in_instr = true,
                _ => {}
            },
            Event::Text(t) => {
                if in_offset {
                    if let (Some(c), Ok(s)) = (cur.as_mut(), t.decode()) {
                        let v: i64 = s.trim().parse().unwrap_or(0);
                        if in_posh {
                            c.x_emu = v;
                        } else if in_posv {
                            c.y_emu = v;
                        }
                    }
                } else if in_text && !in_instr
                    && let (Some(c), Ok(s)) = (cur.as_mut(), t.decode()) {
                        c.text.push_str(&s);
                    }
            }
            Event::GeneralRef(r) if in_text && !in_instr => {
                if let (Some(c), Ok(s)) = (cur.as_mut(), resolve_reference(&r)) {
                    c.text.push_str(&s);
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"wp:positionH" => in_posh = false,
                b"wp:positionV" => in_posv = false,
                b"wp:posOffset" => in_offset = false,
                b"w:t" => in_text = false,
                b"w:instrText" => in_instr = false,
                b"w:txbxContent" => txbx_depth = txbx_depth.saturating_sub(1),
                // Top-level paragraphs only: a w:p inside the box's own content doesn't advance
                // the story index (mirrors parse_images' txbx guard).
                b"w:p" if txbx_depth == 0 => para_index += 1,
                b"wp:anchor" => {
                    if let Some(c) = cur.take()
                        && !c.text.trim().is_empty()
                    {
                        out.push(c);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}
