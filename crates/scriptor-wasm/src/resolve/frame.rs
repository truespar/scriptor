//! Text frames (`w:framePr`).
//! 
//! Parses the legacy frame attribute string and resolves a frame's height and page
//! position, which is how pre-DrawingML documents float a block of paragraphs.

/// A text-frame group collected by [`build_flow`]: the flat block indices of its (consecutive,
/// same-`framePr`) paragraphs, the raw `framePr` attribute string, and an `anchor` body-block index
/// near it (whose page the frame inherits). The frame's paragraphs are pulled OUT of the inline flow.
pub(crate) struct FrameSpec {
    pub(crate) blocks: Vec<usize>,
    pub(crate) raw: String,
    pub(crate) anchor: usize,
    /// The anchor body paragraph ends with a page break, so the frame (which follows it in document
    /// order) lands on the NEXT page - even though it was pulled out of the inline flow.
    pub(crate) after_break: bool,
}

/// The layout-relevant subset of a `w:framePr`, parsed from the raw attribute string and converted to
/// device px. Empty strings = the attribute was absent.
pub(crate) struct FrameGeom {
    pub(crate) w: Option<f32>,
    pub(crate) h: Option<f32>,
    pub(crate) h_rule: String,
    pub(crate) x: Option<f32>,
    pub(crate) y: Option<f32>,
    pub(crate) x_align: String,
    pub(crate) y_align: String,
    pub(crate) h_anchor: String,
    pub(crate) v_anchor: String,
    pub(crate) wrap: String,
    pub(crate) h_space: f32,
    pub(crate) v_space: f32,
}

/// Parse a raw `framePr` attribute string (`w:w="2880" w:hAnchor="margin" ...`) into a [`FrameGeom`]
/// (twips -> px via `scale`). A best-effort substring parse - framePr values are numbers / enum names.
/// A text frame's box height (px) from its `w:hRule` + `w:h` and the laid-out content height:
/// - `exact` - exactly `w:h` (clip; falls back to content if `w:h` is missing).
/// - `atLeast`, OR an explicit `w:h` with NO rule - a FLOOR: at least `w:h`, growing for overflow.
///   (Word / LibreOffice read a bare `w:h` as `atLeast`, not `auto`.)
/// - `auto` / nothing - just fit the content.
pub(crate) fn resolve_frame_height(h_rule: &str, h: Option<f32>, content_h: f32) -> f32 {
    match h_rule {
        "exact" => h.unwrap_or(content_h),
        "atLeast" => content_h.max(h.unwrap_or(0.0)),
        _ => match h {
            Some(h) => content_h.max(h),
            None => content_h,
        },
    }
}

pub(crate) fn parse_frame(s: &str, scale: f32) -> FrameGeom {
    let get = |key: &str| -> Option<String> {
        let pat = format!("{key}=\"");
        let i = s.find(&pat)? + pat.len();
        let j = s[i..].find('"')? + i;
        Some(s[i..j].to_string())
    };
    let px = |k: &str| get(k).and_then(|v| v.parse::<f32>().ok()).map(|t| (t / 20.0) * (96.0 / 72.0) * scale);
    FrameGeom {
        w: px("w:w"),
        h: px("w:h"),
        h_rule: get("w:hRule").unwrap_or_default(),
        x: px("w:x"),
        y: px("w:y"),
        x_align: get("w:xAlign").unwrap_or_default(),
        y_align: get("w:yAlign").unwrap_or_default(),
        h_anchor: get("w:hAnchor").unwrap_or_default(),
        v_anchor: get("w:vAnchor").unwrap_or_default(),
        wrap: get("w:wrap").unwrap_or_default(),
        h_space: px("w:hSpace").unwrap_or(0.0),
        v_space: px("w:vSpace").unwrap_or(0.0),
    }
}

/// Resolve a frame's top-left on its page (device px) from its anchor/align/offset, mirroring Word's
/// framePr positioning. Horizontal: `xAlign` (left/center/right/inside/outside) relative to the page
/// (`hAnchor="page"`) or the text column (margin/text); else an absolute `x` offset. Vertical: `yAlign`
/// (top/center/bottom) within the page / margin region, else an absolute `y` from the region top.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_frame(
    g: &FrameGeom,
    fw: f32,
    fh: f32,
    page_w: f32,
    page_h: f32,
    ml: f32,
    mr: f32,
    mt: f32,
    mb: f32,
    anchor_y: f32,
) -> (f32, f32) {
    let content_w = (page_w - ml - mr).max(1.0);
    let h_page = g.h_anchor == "page";
    let x = if !g.x_align.is_empty() {
        match g.x_align.as_str() {
            "right" | "outside" => if h_page { page_w - fw } else { page_w - mr - fw },
            "center" => if h_page { (page_w - fw) * 0.5 } else { ml + (content_w - fw) * 0.5 },
            _ => if h_page { 0.0 } else { ml }, // left / inside / start
        }
    } else {
        // Absolute offset: from the page edge (page) or the text column left (margin / text).
        (if h_page { 0.0 } else { ml }) + g.x.unwrap_or(0.0)
    };
    let y = if g.v_anchor == "text" {
        // Anchored to the text: the frame floats from its anchoring paragraph's flow position.
        anchor_y + g.y.unwrap_or(0.0)
    } else {
        // Page = the whole page; margin (the default) = the content band between the margins.
        let (region_top, region_h) = if g.v_anchor == "page" {
            (0.0, page_h)
        } else {
            (mt, (page_h - mt - mb).max(1.0))
        };
        if !g.y_align.is_empty() {
            match g.y_align.as_str() {
                "bottom" | "outside" => region_top + region_h - fh,
                "center" => region_top + (region_h - fh) * 0.5,
                _ => region_top, // top / inside
            }
        } else {
            region_top + g.y.unwrap_or(0.0)
        }
    };
    (x.max(0.0), y.max(0.0))
}
