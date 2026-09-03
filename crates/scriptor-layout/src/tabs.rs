//! Tab stop resolution.
//! 
//! Finds the next stop past the pen, and how a centred, right or decimal stop shifts
//! the segment that follows it.

/// The next left tab stop strictly past `pen`: the first custom stop (px from `left`), else the next
/// multiple of the default interval. Returns an absolute canvas x.
pub(crate) fn next_tab_stop(pen: f32, left: f32, stops: &[f32], kinds: &[u8], default: f32) -> (f32, u8) {
    let eps = 0.5;
    for (i, &st) in stops.iter().enumerate() {
        if left + st > pen + eps {
            return (left + st, kinds.get(i).copied().unwrap_or(0));
        }
    }
    // Past the last custom stop: Word falls back to regularly-spaced left tabs.
    let d = if default > 1.0 { default } else { 48.0 };
    let rel = (pen - left).max(0.0);
    (left + ((rel / d).floor() + 1.0) * d, 0)
}

/// Horizontal shift to apply to a tab segment of natural width `segw` that was shaped with its left
/// edge at the stop position `pen`, so the text aligns per `kind`: 0=left (no shift), 1=center
/// (centred on the stop), 2=right / 3=decimal (text ends at the stop). Decimal is approximated as
/// right-align - exact decimal-point alignment is a rare case not yet worth the extra shaping pass.
/// Clamped so the segment never starts left of the content-box left edge (`left`).
pub(crate) fn tab_align_offset(kind: u8, segw: f32, pen: f32, left: f32) -> f32 {
    let raw = match kind {
        1 => -segw / 2.0,
        2 | 3 => -segw,
        _ => 0.0,
    };
    raw.max(left - pen)
}

/// The shaping width for the text after a tab that sits `avail` px from the content's right edge.
/// When a tab lands within a sliver of the edge - a right- or centre-aligned stop we currently render
/// as a left tab, or a stop placed past the margin - the following text must NOT wrap into the
/// sliver: that shatters a one-line header into dozens of single-character lines, ballooning its
/// measured height and paginating the document into hundreds of pages (FDO77715: 639 pages vs 46).
/// Below a quarter of the line, let the segment overflow on (near) one line at the full content width.
pub(crate) fn tab_segment_width(avail: f32, content_w: f32) -> f32 {
    if avail < content_w * 0.25 { content_w.max(1.0) } else { avail.max(1.0) }
}
