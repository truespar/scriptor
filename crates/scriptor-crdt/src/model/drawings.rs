//! Picture discovery: DrawingML (`w:drawing`) and legacy VML (`w:pict`).
//! 
//! Walks a body part for pictures, resolving inline and anchored placement, crop,
//! extent and wrap mode into a common [`DrawImage`]. The `vml_*` helpers below
//! implement VML's own unit and `style=` attribute grammar, which only this scan uses.

use super::*;

// ── images (w:drawing -> blip + extent + anchor) ────────────────────────────

/// A picture found in a `w:drawing`: the blip relationship id, its size (EMU), and - for anchored
/// (floating) pictures - the position offsets + what they are relative to.
#[derive(Debug, Clone)]
pub struct DrawImage {
    pub embed: String,
    pub w_emu: i64,
    pub h_emu: i64,
    pub anchored: bool,
    /// `behindDoc="1"` - the picture sits behind the text (painted before it), not over it.
    pub behind: bool,
    pub x_emu: i64,
    pub y_emu: i64,
    pub h_from: String,
    pub v_from: String,
    /// `wp:align` (left/center/right or top/bottom/center) when used instead of a `posOffset`.
    pub h_align: String,
    pub v_align: String,
    /// `<a:srcRect>` crop (thousandths of a percent, 0..100000), l/t/r/b.
    pub crop_l: i64,
    pub crop_t: i64,
    pub crop_r: i64,
    pub crop_b: i64,
    /// Wrap type for a floating picture (`square` / `tight` / `through` / `topAndBottom` / `none`).
    pub wrap: String,
    /// 0-based index of the `w:p` the drawing sits in (for body flow positioning).
    pub para_index: usize,
    /// The tracked-change wrapper around the drawing's run (`w:ins`/`w:del`), if any - so a picture
    /// inserted or deleted under Track Changes round-trips its redline like a text run does.
    pub track: Option<Track>,
    /// This picture sits inside a `<w:txbxContent>` - it belongs to a text box, not to the body flow.
    ///
    /// It is still collected, because the renderer paints text-box pictures. But it must not get a
    /// body placeholder run, and it must not make its enclosing run look like an ordinary picture
    /// run to [`parse_passthrough`]. Both of those happened: a text box holding a picture was
    /// declined for verbatim capture because a picture *was* found inside it, and the modeled image
    /// path then emitted that picture on its own at body level - hoisting the picture out and
    /// dropping every word in the box. 11 corpus documents lost text that way, `fdo76591.docx` all
    /// 666 characters of it.
    pub in_textbox: bool,
}


/// Parse a VML / CSS length (`27.3pt`, `1.5in`, `36px`, `1cm`) to EMU. A bare number is treated as
/// points (VML `style` lengths default to pt). Returns 0 for anything it can't read.
fn vml_len_emu(s: &str) -> i64 {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+')).unwrap_or(s.len());
    let Ok(v) = s[..split].parse::<f64>() else { return 0 };
    let per_unit = match s[split..].trim() {
        "in" => 914_400.0,
        "cm" => 360_000.0,
        "mm" => 36_000.0,
        "px" => 9525.0,    // 1px = 1/96in
        "pc" => 152_400.0, // 1 pica = 12pt
        _ => 12_700.0,     // pt, and bare numbers
    };
    (v * per_unit) as i64
}

/// Look a key out of a VML `style` attribute (`position:absolute;width:27pt;margin-left:5pt`).
fn vml_style_get(style: &str, key: &str) -> Option<String> {
    style.split(';').find_map(|kv| {
        let (k, v) = kv.split_once(':')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_string())
    })
}

/// An open `<v:group>` scope while scanning for pictures: the group's resolved page box (EMU) plus
/// the scale from its child-coordinate units to EMU. Children of a group express their geometry in
/// the group's `coordsize` units (bare numbers, NOT pt) - reading them as absolute lengths inflates
/// a 10pt logo thousands-fold (the NOBA footer icons asked for a 171k-px resize). The group also
/// owns the anchor: its children are positioned within it, so `position`/`mso-position-*` come from
/// the outermost group.
#[derive(Clone)]
struct VmlGroup {
    x_emu: i64,
    y_emu: i64,
    /// EMU per horizontal / vertical child-coordinate unit.
    sx: f64,
    sy: f64,
    anchored: bool,
    h_from: String,
    v_from: String,
    h_align: String,
    v_align: String,
}

/// A VML length that may be inside an open `<v:group>`: a BARE number there is in the group's
/// coordinate units (scaled by the group's on-page box); a value with an explicit unit, or one
/// outside any group, is an absolute CSS length ([`vml_len_emu`]).
fn vml_scoped_len_emu(s: &str, scope: Option<&VmlGroup>, horiz: bool) -> i64 {
    let t = s.trim();
    let bare = !t.is_empty()
        && t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+');
    match (bare, scope) {
        (true, Some(g)) => {
            let v: f64 = t.parse().unwrap_or(0.0);
            (v * if horiz { g.sx } else { g.sy }) as i64
        }
        _ => vml_len_emu(s),
    }
}

/// Parse a `coordsize="cw,ch"` attribute; the VML default (absent / unparsable axis) is 1000.
fn vml_coordsize(s: &str) -> (i64, i64) {
    let mut it = s.split(',').map(|p| p.trim().parse::<i64>().unwrap_or(0));
    let cw = it.next().unwrap_or(0);
    let ch = it.next().unwrap_or(0);
    (if cw > 0 { cw } else { 1000 }, if ch > 0 { ch } else { 1000 })
}

// Map a VML `mso-position-*-relative` value to the DrawingML `relativeFrom` the placer understands:
/// `page`/`margin` pass through; everything else (column / text / char / paragraph / line) is left
/// empty, which [`place_float`] reads as column-left / anchor-paragraph-relative.
fn vml_rel(v: Option<&str>) -> String {
    match v.unwrap_or("") {
        "page" => "page".to_string(),
        "margin" => "margin".to_string(),
        _ => String::new(),
    }
}


/// Scan a WordprocessingML body for pictures (`w:drawing` -> `pic:blipFill` + `wp:extent` + anchor),
/// plus legacy VML pictures (`w:pict` -> `v:shape` style + `v:imagedata`), so both render.
pub fn parse_images(xml: &[u8]) -> Vec<DrawImage> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut para_index = 0usize;
    let mut cur: Option<DrawImage> = None;
    // A VML picture shape (`<v:shape style=...>`) awaiting its `<v:imagedata r:id>` - geometry comes
    // from the shape's CSS `style`, the media ref from the imagedata child, so it is finalized there.
    let mut vml_pending: Option<DrawImage> = None;
    // Open `<v:group>` scopes (innermost last): children express geometry in the group's coordinate
    // units and inherit the group's anchor - see [`VmlGroup`].
    let mut vml_groups: Vec<VmlGroup> = Vec::new();
    // Depth inside a `<w:txbxContent>` (a shape / VML text box). Its `<w:p>`s are NOT body paragraphs,
    // so they must not advance `para_index` - otherwise a picture that sits in the same paragraph as a
    // text box anchors past the real block count and the whole import aborts (090716_*.docx: a header
    // logo beside a text-box banner anchored at index 2 of a 2-block header).
    let mut txbx_depth = 0usize;
    // NOTE: there is no matching table-depth guard here, unlike `parse_passthrough`. A picture inside
    // a NESTED table would be collected AND carried inside that table's verbatim capture, emitting it
    // twice - the same duplicate-id defect a `w:pict` in a nested table produced. No corpus document
    // has one, so this is latent rather than active; add the guard alongside a document that proves
    // it rather than on speculation.
    // The tracked-change wrapper currently open around runs (a `w:ins`/`w:del` in the paragraph body),
    // inherited by any `w:drawing` inside it - so a tracked picture keeps its redline on import.
    let mut pending_track: Option<Track> = None;
    let (mut in_posh, mut in_posv, mut in_posoffset, mut in_align) = (false, false, false, false);
    // Depth inside an `<a:blipFill>` - a shape's picture *fill*, where a blip is a background rather
    // than a picture. A real picture's blip sits in `<pic:blipFill>`, a different element.
    let mut fill_depth = 0usize;

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            // Word wraps a modern drawing as `<mc:AlternateContent>`: a DrawingML `<mc:Choice>` AND
            // a legacy VML `<mc:Fallback>` of the SAME picture. We read the Choice, so the whole
            // Fallback subtree must be skipped - ingesting both renders every such image twice, at
            // subtly different anchors (the NOBA checklist icons doubled across headings).
            Event::Start(e) if e.name().as_ref() == b"mc:Fallback" => {
                let name = e.name();
                let mut skip = Vec::new();
                let _ = reader.read_to_end_into(name, &mut skip);
            }
            // An OLE object / ActiveX control is preserved wholesale by `parse_passthrough` (verbatim
            // run capture), so its VML preview (`<v:shape><v:imagedata>`) must NOT also be ingested as
            // a picture here - that would double-emit the object's image on export.
            Event::Start(e) if matches!(e.name().as_ref(), b"w:object" | b"w:control") => {
                let name = e.name();
                let mut skip = Vec::new();
                let _ = reader.read_to_end_into(name, &mut skip);
            }
            // A `<v:group>` establishes a child coordinate system: its own box is a CSS length (or
            // parent-group units when nested); its children's bare lengths are `coordsize` units
            // mapped onto that box. Word wraps footer/logo image shapes in groups, so a group-blind
            // read of a child's `width:128587` as points asks for a 45-metre image (the resize then
            // overflows on wasm32 and stalls natively). Start-only: an Empty group has no children
            // (and no matching End to pop).
            Event::Start(e) if e.name().as_ref() == b"v:group" => {
                let style = attr(&e, b"style").unwrap_or_default();
                let parent = vml_groups.last().cloned();
                let len = |k: &str, horiz: bool| {
                    vml_style_get(&style, k)
                        .map(|s| vml_scoped_len_emu(&s, parent.as_ref(), horiz))
                        .unwrap_or(0)
                };
                let (w, h) = (len("width", true), len("height", false));
                let (cw, ch) =
                    attr(&e, b"coordsize").map(|s| vml_coordsize(&s)).unwrap_or((1000, 1000));
                vml_groups.push(match parent.clone() {
                    // Nested group: position within the parent, inherit the parent's anchor.
                    Some(g) => VmlGroup {
                        x_emu: g.x_emu + len("margin-left", true) + len("left", true),
                        y_emu: g.y_emu + len("margin-top", false) + len("top", false),
                        sx: w as f64 / cw as f64,
                        sy: h as f64 / ch as f64,
                        ..g
                    },
                    // Outermost group: it owns the anchor its children inherit.
                    None => VmlGroup {
                        x_emu: len("margin-left", true),
                        y_emu: len("margin-top", false),
                        sx: w as f64 / cw as f64,
                        sy: h as f64 / ch as f64,
                        anchored: vml_style_get(&style, "position").as_deref() == Some("absolute"),
                        h_from: vml_rel(vml_style_get(&style, "mso-position-horizontal-relative").as_deref()),
                        v_from: vml_rel(vml_style_get(&style, "mso-position-vertical-relative").as_deref()),
                        h_align: vml_style_get(&style, "mso-position-horizontal").unwrap_or_default(),
                        v_align: vml_style_get(&style, "mso-position-vertical").unwrap_or_default(),
                    },
                });
            }
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"w:ins" => pending_track = revision_track(&e, TrackKind::Ins),
                b"w:del" => pending_track = revision_track(&e, TrackKind::Del),
                b"w:txbxContent" => txbx_depth += 1,
                b"w:drawing" => {
                    cur = Some(DrawImage {
                        embed: String::new(),
                        w_emu: 0,
                        h_emu: 0,
                        anchored: false,
                        behind: false,
                        x_emu: 0,
                        y_emu: 0,
                        h_from: String::new(),
                        v_from: String::new(),
                        h_align: String::new(),
                        v_align: String::new(),
                        crop_l: 0,
                        crop_t: 0,
                        crop_r: 0,
                        crop_b: 0,
                        wrap: String::new(),
                        para_index,
                        track: pending_track.clone(),
                        in_textbox: txbx_depth > 0,
                    });
                }
                b"a:srcRect" => {
                    if let Some(c) = cur.as_mut() {
                        let g = |k: &[u8]| attr(&e, k).and_then(|s| s.parse().ok()).unwrap_or(0);
                        c.crop_l = g(b"l");
                        c.crop_t = g(b"t");
                        c.crop_r = g(b"r");
                        c.crop_b = g(b"b");
                    }
                }
                b"wp:wrapSquare" => {
                    if let Some(c) = cur.as_mut() {
                        c.wrap = "square".into();
                    }
                }
                b"wp:wrapTight" => {
                    if let Some(c) = cur.as_mut() {
                        c.wrap = "tight".into();
                    }
                }
                b"wp:wrapThrough" => {
                    if let Some(c) = cur.as_mut() {
                        c.wrap = "through".into();
                    }
                }
                b"wp:wrapTopAndBottom" => {
                    if let Some(c) = cur.as_mut() {
                        c.wrap = "topAndBottom".into();
                    }
                }
                b"wp:wrapNone" => {
                    if let Some(c) = cur.as_mut() {
                        c.wrap = "none".into();
                    }
                }
                b"wp:anchor" => {
                    if let Some(c) = cur.as_mut() {
                        c.anchored = true;
                        c.behind = attr(&e, b"behindDoc").as_deref() == Some("1");
                    }
                }
                b"wp:extent" => {
                    if let Some(c) = cur.as_mut() {
                        c.w_emu = attr(&e, b"cx").and_then(|s| s.parse().ok()).unwrap_or(0);
                        c.h_emu = attr(&e, b"cy").and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
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
                b"a:blipFill" => fill_depth += 1,
                b"wp:posOffset" => in_posoffset = true,
                b"wp:align" => in_align = true,
                // A blip inside `<a:blipFill>` is a shape's *background fill*, not a picture. A real
                // picture's blip sits in `<pic:blipFill>`, a different element. Counting the fill as
                // a picture made a text box with a picture fill look like a picture run, so the box
                // was never captured verbatim: its text was dropped and the fill was hoisted to body
                // level as a standalone image.
                b"a:blip" if fill_depth == 0 => {
                    if let Some(c) = cur.as_mut()
                        && let Some(id) = attr(&e, b"r:embed") {
                            c.embed = id;
                        }
                }
                // Legacy VML picture: a `<v:shape>` (etc.) carries the geometry in its CSS `style`; its
                // `<v:imagedata r:id>` child carries the media ref + finalizes it. `mso-position-*`
                // drive the anchor like a `w:drawing`'s wp:positionH/V do. Inside a `<v:group>` the
                // lengths are group-coordinate units and the GROUP owns the anchor.
                b"v:shape" | b"v:rect" | b"v:roundrect" | b"v:oval" => {
                    if let Some(style) = attr(&e, b"style") {
                        let g = vml_groups.last();
                        let len = |k: &str, horiz: bool| {
                            vml_style_get(&style, k)
                                .map(|s| vml_scoped_len_emu(&s, g, horiz))
                                .unwrap_or(0)
                        };
                        let abs = match g {
                            Some(gr) => gr.anchored,
                            None => vml_style_get(&style, "position").as_deref() == Some("absolute"),
                        };
                        vml_pending = Some(DrawImage {
                            embed: String::new(),
                            w_emu: len("width", true),
                            h_emu: len("height", false),
                            anchored: abs,
                            behind: false,
                            x_emu: g.map_or(0, |gr| gr.x_emu) + len("margin-left", true) + len("left", true),
                            y_emu: g.map_or(0, |gr| gr.y_emu) + len("margin-top", false) + len("top", false),
                            h_from: match g {
                                Some(gr) => gr.h_from.clone(),
                                None => vml_rel(vml_style_get(&style, "mso-position-horizontal-relative").as_deref()),
                            },
                            v_from: match g {
                                Some(gr) => gr.v_from.clone(),
                                None => vml_rel(vml_style_get(&style, "mso-position-vertical-relative").as_deref()),
                            },
                            h_align: match g {
                                Some(gr) => gr.h_align.clone(),
                                None => vml_style_get(&style, "mso-position-horizontal").unwrap_or_default(),
                            },
                            v_align: match g {
                                Some(gr) => gr.v_align.clone(),
                                None => vml_style_get(&style, "mso-position-vertical").unwrap_or_default(),
                            },
                            crop_l: 0,
                            crop_t: 0,
                            crop_r: 0,
                            crop_b: 0,
                            // A floating VML shape sits in its own layer (no body reflow); inline keeps default.
                            wrap: if abs { "none".into() } else { String::new() },
                            para_index,
                            track: pending_track.clone(),
                            in_textbox: txbx_depth > 0,
                        });
                    }
                }
                b"v:imagedata" => {
                    if let Some(id) = attr(&e, b"r:id")
                        && let Some(mut c) = vml_pending.take()
                    {
                        c.embed = id;
                        if c.w_emu > 0 && c.h_emu > 0 {
                            out.push(c);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_posoffset => {
                if let Ok(v) = t.decode().unwrap_or_default().trim().parse::<i64>()
                    && let Some(c) = cur.as_mut() {
                        if in_posh {
                            c.x_emu = v;
                        } else if in_posv {
                            c.y_emu = v;
                        }
                    }
            }
            Event::Text(t) if in_align => {
                let v = t.decode().unwrap_or_default().trim().to_string();
                if let Some(c) = cur.as_mut() {
                    if in_posh {
                        c.h_align = v;
                    } else if in_posv {
                        c.v_align = v;
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"wp:posOffset" => in_posoffset = false,
                b"wp:align" => in_align = false,
                b"wp:positionH" => in_posh = false,
                b"wp:positionV" => in_posv = false,
                b"w:ins" | b"w:del" => pending_track = None,
                b"w:txbxContent" => txbx_depth = txbx_depth.saturating_sub(1),
                b"a:blipFill" => fill_depth = fill_depth.saturating_sub(1),
                b"v:group" => {
                    vml_groups.pop();
                }
                b"w:drawing" => {
                    if let Some(c) = cur.take()
                        && !c.embed.is_empty() {
                            out.push(c);
                        }
                }
                // Only body paragraphs advance the anchor index; text-box paragraphs do not.
                b"w:p" if txbx_depth == 0 => para_index += 1,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}
