//! Per-page content fingerprints.
//! 
//! An FNV-1a hash folded over everything that affects a page's pixels, so a relayout
//! can repaint only the pages whose content actually changed.

use crate::*;

// FNV-1a page-content fingerprint: cheap, used only to decide which pages need re-rasterizing.
pub(crate) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
pub(crate) const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub(crate) fn fnv_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Fold a block's appearance (its spans + the y it lands at) into a page fingerprint. MUST include
/// every visible attribute - an omitted one (e.g. underline) means that edit won't trigger a
/// repaint, so the change silently doesn't show until something else dirties the page.
pub(crate) fn fold_block(mut h: u64, block: &Block, top: f32) -> u64 {
    h = fnv_bytes(h, &top.to_bits().to_le_bytes());
    // Paragraph-level appearance: alignment, line-height rules, left/right indent. NB this hash
    // doubles as the shape-cache key (content + width -> shaped lines), so EVERY field shaping or
    // painting reads must be folded - a missed field is both a stale-repaint and stale-layout bug.
    h = fnv_bytes(h, &[block.align as u8]);
    h = fnv_bytes(h, &block.line_mult.to_bits().to_le_bytes());
    h = fnv_bytes(h, &block.line_exact_px.to_bits().to_le_bytes());
    h = fnv_bytes(h, &block.line_min_px.to_bits().to_le_bytes());
    h = fnv_bytes(h, &block.indent_left_px.to_bits().to_le_bytes());
    h = fnv_bytes(h, &block.indent_right_px.to_bits().to_le_bytes());
    h = fnv_bytes(h, block.marker.as_bytes());
    h = fnv_bytes(h, &block.hang_px.to_bits().to_le_bytes());
    // Tab geometry: a moved/retyped stop re-lays the tabbed segments.
    h = fnv_bytes(h, &block.default_tab_px.to_bits().to_le_bytes());
    for t in &block.tab_stops_px {
        h = fnv_bytes(h, &t.to_bits().to_le_bytes());
    }
    h = fnv_bytes(h, &block.tab_kinds);
    // Paragraph shading fill (painted behind the text).
    match block.shading {
        Some(rgb) => h = fnv_bytes(h, &rgb),
        None => h = fnv_bytes(h, &[0xFE]),
    }
    // Trailing paragraph-mark glyph (tracked ¶): colour + strike must dirty the page when they change.
    h = fnv_bytes(h, block.trailing.as_bytes());
    h = fnv_bytes(h, &[block.trailing_color[0], block.trailing_color[1], block.trailing_color[2], block.trailing_strike as u8]);
    // The paragraph border box: changing any edge (weight / spacing / colour) must repaint the page.
    if let Some(b) = &block.borders {
        for e in [&b.top, &b.left, &b.bottom, &b.right] {
            if let Some(l) = e {
                h = fnv_bytes(h, &l.width_px.to_bits().to_le_bytes());
                h = fnv_bytes(h, &l.space_px.to_bits().to_le_bytes());
                h = fnv_bytes(h, &l.rgb);
            } else {
                h = fnv_bytes(h, &[0xFF]); // an absent edge differs from a present one
            }
        }
    }
    // Two-sided wrap holes: changing a frame's box reflows the columns, so it must repaint.
    for hole in &block.wrap_holes {
        for v in [hole.x0, hole.x1, hole.top, hole.bot] {
            h = fnv_bytes(h, &v.to_bits().to_le_bytes());
        }
    }
    // The margin change-bar: toggling it (e.g. accepting the last change in a paragraph) - or moving
    // which lines it bars (per-line) - must repaint the page.
    h = fnv_bytes(h, &[block.has_change as u8]);
    for &(cs, ce) in &block.change_ranges {
        h = fnv_bytes(h, &(cs as u64).to_le_bytes());
        h = fnv_bytes(h, &(ce as u64).to_le_bytes());
    }
    for s in &block.spans {
        h = fnv_bytes(h, s.text.as_bytes());
        h = fnv_bytes(h, &s.size_px.to_bits().to_le_bytes());
        // The resolved family + baseline shift change the shaped advances/positions; the highlight
        // changes the painted band - all three must dirty the fingerprint.
        h = fnv_bytes(h, s.family.as_bytes());
        h = fnv_bytes(h, &s.baseline_shift.to_bits().to_le_bytes());
        match s.highlight {
            Some(rgb) => h = fnv_bytes(h, &rgb),
            None => h = fnv_bytes(h, &[0xFD]),
        }
        h = fnv_bytes(
            h,
            &[
                s.bold as u8,
                s.italic as u8,
                s.underline as u8,
                s.strike as u8,
                s.color[0],
                s.color[1],
                s.color[2],
            ],
        );
    }
    h
}
