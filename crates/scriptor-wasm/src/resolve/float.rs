//! Floating-picture geometry.
//! 
//! Places an anchored image on the page and computes what it does to the text around
//! it: square-wrap indents, centre-straddling wrap holes, and hit-testing.

use crate::*;

/// Page geometry for [`place_float`] (device px + EMU->px scale).
pub(crate) struct FloatGeom {
    pub(crate) ml: f32,
    pub(crate) mr: f32,
    pub(crate) mt: f32,
    pub(crate) page_w: f32,
    pub(crate) scale: f32,
}

/// A floating picture's page-local exclusion rect, for paragraph-level square wrap.
#[derive(Clone, Copy)]
pub(crate) struct FloatRect {
    pub(crate) page: u32,
    /// The obstacle's TRUE box (no clearance baked in) - the side/straddle decision keys off this, so
    /// a wide wrap distance can't fool it into reading a margin-hugging frame as page-centred.
    pub(crate) x0: f32,
    pub(crate) x1: f32,
    pub(crate) top: f32,
    pub(crate) bot: f32,
    /// Horizontal / vertical clearance to hold between the obstacle and the text (a frame's `hSpace` /
    /// `vSpace`; `0` for a picture float, which falls back to the caller's default gutter).
    pub(crate) hspace: f32,
    pub(crate) vspace: f32,
}

/// The extra (left, right) indent px needed to wrap a paragraph - whose page-local vertical band is
/// `[top, bot]` on `page`, with current indents `cur_l`/`cur_r` - around square-wrap floats. A float
/// that doesn't overlap the band is ignored; one straddling the content centre is treated as
/// full-width (no side wrap, text flows above/below - deferred); otherwise a left float pushes the
/// left edge past it and a right float pulls the right edge in, each with a `gutter` of clearance.
/// Returns the additional indent to ADD to the paragraph's existing indents.
#[allow(clippy::too_many_arguments)]
pub(crate) fn square_wrap_indents(
    page: u32,
    top: f32,
    bot: f32,
    cur_l: f32,
    cur_r: f32,
    ml: f32,
    content_w: f32,
    gutter: f32,
    floats: &[FloatRect],
) -> (f32, f32) {
    let center = ml + content_w * 0.5;
    let (mut add_l, mut add_r) = (0.0_f32, 0.0_f32);
    for f in floats.iter().filter(|f| f.page == page) {
        if top.max(f.top - f.vspace) >= bot.min(f.bot + f.vspace) {
            continue; // no vertical overlap with this float (its band widened by any vSpace)
        }
        if f.x0 <= center && f.x1 >= center {
            continue; // the float's TRUE box straddles the centre: full-width, no side wrap
        }
        // Clearance to hold to the float: its own (a frame's hSpace), else the caller's default gutter.
        let clear = if f.hspace > 0.0 { f.hspace } else { gutter };
        if (f.x0 + f.x1) * 0.5 < center {
            add_l = add_l.max((f.x1 + clear - ml) - cur_l); // left float: indent the left edge past it
        } else {
            add_r = add_r.max(((ml + content_w) - (f.x0 - clear)) - cur_r); // right float: pull the right edge in
        }
    }
    (add_l.max(0.0), add_r.max(0.0))
}

/// The two-sided-wrap holes for a paragraph whose page-local band is `[top, bot]` on `page`: a float
/// that straddles the content centre (the case [`square_wrap_indents`] defers to full-width) becomes
/// a [`WrapHole`] the shaper flows the text around on both sides. The hole's `x0`/`x1` fold in the
/// clearance (the frame's hSpace, else the default gutter); its `top`/`bot` are made RELATIVE to the
/// paragraph's top so the shaper can place them against its own first line.
pub(crate) fn centre_wrap_holes(
    page: u32,
    top: f32,
    bot: f32,
    ml: f32,
    content_w: f32,
    gutter: f32,
    floats: &[FloatRect],
) -> Vec<scriptor_layout::WrapHole> {
    let center = ml + content_w * 0.5;
    // Each side column must hold readable text for a two-sided wrap to make sense; a frame wider than
    // this leaves a sliver that would force the text down many tiny lines (and is usually a `notBeside`
    // top-and-bottom frame, not a side-wrap one). Below the bar, leave it to the existing path.
    let min_col = content_w * 0.15;
    let mut out = Vec::new();
    for f in floats.iter().filter(|f| f.page == page) {
        if top.max(f.top - f.vspace) >= bot.min(f.bot + f.vspace) {
            continue; // no vertical overlap
        }
        if !(f.x0 <= center && f.x1 >= center) {
            continue; // only a centre-straddling float wraps on both sides
        }
        let clear = if f.hspace > 0.0 { f.hspace } else { gutter };
        let left_col = (f.x0 - clear) - ml;
        let right_col = (ml + content_w) - (f.x1 + clear);
        if left_col < min_col || right_col < min_col {
            continue; // too little room on a side: not a two-sided wrap (e.g. a full-width frame)
        }
        out.push(scriptor_layout::WrapHole {
            x0: f.x0 - clear,
            x1: f.x1 + clear,
            top: f.top - top,
            bot: f.bot - top,
        });
    }
    out
}

/// Resolve a floating picture's top-left on the page (device px). Horizontal: a `wp:align`
/// (left/center/right) wins, else a `posOffset` from the page edge (`relativeFrom="page"`) or the left
/// margin. Vertical: `page`/`margin` are absolute (from the page top / top-margin); otherwise the
/// offset is from `anchor_top` (the anchoring paragraph's top, or the header/footer top). A
/// non-anchored (inline-projected) picture falls at the margin / `anchor_top`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_float(
    g: &FloatGeom,
    w: f32,
    anchored: bool,
    h_from: &str,
    h_align: &str,
    x_emu: i64,
    v_from: &str,
    y_emu: i64,
    anchor_top: f32,
) -> (f32, f32) {
    let emu_px = |emu: i64| (emu as f32 / 914_400.0) * 96.0 * g.scale;
    let content_w = (g.page_w - g.ml - g.mr).max(1.0);
    let page_rel = h_from == "page";
    let x = if !h_align.is_empty() {
        match h_align {
            "right" => if page_rel { g.page_w - w } else { g.page_w - g.mr - w },
            "center" => if page_rel { (g.page_w - w) * 0.5 } else { g.ml + (content_w - w) * 0.5 },
            _ => if page_rel { 0.0 } else { g.ml },
        }
    } else if !anchored {
        g.ml
    } else if page_rel {
        emu_px(x_emu)
    } else {
        g.ml + emu_px(x_emu)
    };
    let y = if !anchored {
        anchor_top
    } else {
        match v_from {
            "page" => emu_px(y_emu),
            "margin" => g.mt + emu_px(y_emu),
            _ => anchor_top + emu_px(y_emu),
        }
    };
    (x, y.max(0.0))
}

/// The topmost editable picture whose rect contains the canvas point `(x, y)` (absolute px), or
/// `None`. Last match wins - floats are appended after inline pictures, so a float over text wins the
/// overlap. The pure half of [`ScriptorDoc::image_at_point`].
pub(crate) fn hit_image(rects: &[ImageHit], x: f32, y: f32) -> Option<u64> {
    rects
        .iter()
        .rev()
        .find(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
        .map(|r| r.id)
}
