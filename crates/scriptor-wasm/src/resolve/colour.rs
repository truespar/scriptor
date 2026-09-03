//! Colour and border parsing for the resolver.
//! 
//! OOXML named highlights, per-author revision colours, hex parsing, and the compact
//! paragraph-border encoding the CRDT stores.

use crate::*;

/// Parse an OOXML `RRGGBB` hex color into RGB bytes (falls back to near-black on garbage).
/// Map an OOXML `w:highlight` named color to RGB. `none`/unknown -> `None`.
pub(crate) fn highlight_rgb(name: &str) -> Option<[u8; 3]> {
    Some(match name {
        "black" => [0x00, 0x00, 0x00],
        "blue" => [0x00, 0x00, 0xff],
        "cyan" => [0x00, 0xff, 0xff],
        "green" => [0x00, 0xff, 0x00],
        "magenta" => [0xff, 0x00, 0xff],
        "red" => [0xff, 0x00, 0x00],
        "yellow" => [0xff, 0xff, 0x00],
        "white" => [0xff, 0xff, 0xff],
        "darkBlue" => [0x00, 0x00, 0x80],
        "darkCyan" => [0x00, 0x80, 0x80],
        "darkGreen" => [0x00, 0x80, 0x00],
        "darkMagenta" => [0x80, 0x00, 0x80],
        "darkRed" => [0x80, 0x00, 0x00],
        "darkYellow" => [0x80, 0x80, 0x00],
        "darkGray" => [0x80, 0x80, 0x80],
        "lightGray" => [0xc0, 0xc0, 0xc0],
        _ => return None,
    })
}

/// A per-author, per-kind colour for All-Markup tracked changes. Insertions get a cool hue and
/// deletions a warm one - so a single-author redline reads as "blue vs red" like Word (the actual
/// colours aren't stored in the `.docx`; Word computes them, so we pick a recognizable scheme) - and
/// the hue rotates by author (FNV-1a over the name, deterministic) so multiple reviewers stay
/// distinguishable while insertions stay cool and deletions stay warm across the board.
/// The stable lowercase token for a track kind, shared by `trackAt` (the hover/click hit) and
/// `listChanges` (the reviewing pane). Moves split into `movefrom` / `moveto` so the UI can label each
/// half; the pane / popup map both to a "Moved" verb.
pub(crate) fn track_kind_str(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Ins => "ins",
        TrackKind::Del => "del",
        TrackKind::Fmt => "fmt",
        TrackKind::MoveFrom => "movefrom",
        TrackKind::MoveTo => "moveto",
    }
}

pub(crate) fn track_colour(author: &str, kind: TrackKind) -> [u8; 3] {
    // Cool palette for insertions, warm for deletions; same index per author so a reviewer's
    // insertions + deletions are a matched cool/warm pair.
    const INS: [[u8; 3]; 4] = [
        [0x1F, 0x66, 0xB0], // blue
        [0x00, 0x83, 0x8F], // teal
        [0x3F, 0x51, 0xB5], // indigo
        [0x2E, 0x7D, 0x32], // green
    ];
    const DEL: [[u8; 3]; 4] = [
        [0xC0, 0x30, 0x2E], // red
        [0xB0, 0x5A, 0x00], // orange
        [0xA0, 0x1E, 0x7A], // magenta
        [0x8A, 0x3A, 0x2B], // brick
    ];
    // Moves get their own green family (Word's move hue), distinct from the cool insertion palette at
    // every index so a single author's move reads apart from their insertions + deletions.
    const MOVE: [[u8; 3]; 4] = [
        [0x2E, 0x7D, 0x32], // green
        [0x55, 0x6B, 0x2F], // olive
        [0x00, 0x77, 0x55], // jade
        [0x6B, 0x8E, 0x23], // yellow-green
    ];
    let mut h: u32 = 0x811c_9dc5;
    for b in author.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    let i = (h as usize) % INS.len();
    match kind {
        // A formatting change isn't an insertion, but it shares the cool palette (it's not struck/
        // underlined, so it stays distinguishable) - a recolour cue alongside the margin change-bar.
        TrackKind::Ins | TrackKind::Fmt => INS[i],
        TrackKind::Del => DEL[i],
        // Both halves of a move share one hue so source + destination read as a matched pair.
        TrackKind::MoveFrom | TrackKind::MoveTo => MOVE[i],
    }
}

pub(crate) fn parse_hex(s: &str) -> [u8; 3] {
    let s = s.trim().trim_start_matches('#');
    match u32::from_str_radix(s, 16) {
        Ok(n) if s.len() == 6 => [((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8, (n & 0xff) as u8],
        _ => [0x1a, 0x1a, 0x1a],
    }
}

/// Build the painter's [`scriptor_layout::BlockBorders`] from the compact `edge=val,sz,space,color`
/// string [`scriptor_crdt::ParaProps`]`::border` stores (`t|l|b|r` keys). `pt_to_px` converts points
/// to device px at the render scale: `w:sz` is eighths-of-a-point line weight, `w:space` is whole
/// points of text-to-line gap, `w:color` is RGB hex (or `auto` = black). `None` when nothing draws.
pub(crate) fn parse_pbdr(compact: Option<&str>, pt_to_px: f32) -> Option<scriptor_layout::BlockBorders> {
    let compact = compact?;
    let mut b = scriptor_layout::BlockBorders::default();
    let mut any = false;
    for tok in compact.split('|').filter(|t| !t.is_empty()) {
        let Some((edge, rest)) = tok.split_once('=') else { continue };
        let mut it = rest.split(',');
        let _val = it.next(); // line style (single/double/...) - all painted as a solid line for now
        let sz: f32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(4.0);
        let space: f32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let color = it.next().unwrap_or("auto");
        let rgb = if color.eq_ignore_ascii_case("auto") { [0, 0, 0] } else { parse_hex(color) };
        let line = scriptor_layout::BorderLine {
            width_px: (sz / 8.0 * pt_to_px).max(1.0),
            space_px: space * pt_to_px,
            rgb,
        };
        match edge {
            "t" => b.top = Some(line),
            "l" => b.left = Some(line),
            "b" => b.bottom = Some(line),
            "r" => b.right = Some(line),
            _ => continue,
        }
        any = true;
    }
    if any { Some(b) } else { None }
}
