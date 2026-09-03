//! Windows Metafile (WMF) + Enhanced Metafile (EMF) decoding to a raster bitmap.
//!
//! Word embeds a lot of clip-art / diagrams / pasted bitmaps as metafiles, which the `image` crate
//! can't read (they're GDI record streams, not pixel formats). This module walks the record stream
//! and renders it to an `image::RgbaImage` that the page compositor draws like any other picture.
//!
//! This file implements the **embedded-bitmap** path. A metafile that just blits a DIB (`StretchDIBits`
//! / `SetDIBitsToDevice` / `BitBlt` and friends - ~10% of the corpus, but every "pasted screenshot")
//! carries a full device-independent bitmap inside one record. We locate the largest such DIB, wrap it
//! with a 14-byte `BITMAPFILEHEADER` (a DIB is exactly a BMP minus that header), and hand it to the
//! `image` BMP decoder - so all the bit-depth / palette / RLE / bitfield handling is reused, pixel
//! exact. A pure-vector metafile (the other ~90%) finds no DIB here and is left to a future
//! vector interpreter.

use std::io::Cursor;

/// One text run a metafile asks to draw, in the rendered image's pixel space (the GDI device point
/// already mapped through the bounds + world transform). tiny-skia can't draw text, so the decoder
/// collects these and the renderer - which owns the font system - rasterizes them onto the image.
pub struct MetaText {
    pub x: f32,
    pub y: f32, // top of the text cell (GDI TA_TOP) in image pixels
    pub size_px: f32,
    pub rgb: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub family: Option<String>,
    pub text: String,
}

/// Decode a WMF or EMF byte stream to (RGBA image, text runs to draw on it), or `None` if it isn't a
/// metafile we can render. The image holds the geometry/bitmap; `texts` (EMF only for now) are
/// rasterized by the caller's font system.
pub fn decode(bytes: &[u8]) -> Option<(image::RgbaImage, Vec<MetaText>)> {
    if is_emf(bytes) {
        // An embedded bitmap (a pasted screenshot) decodes pixel-exact; otherwise play the GDI
        // records as vector art (+ collected text).
        if let Some(img) = decode_emf(bytes) {
            return Some((img, Vec::new()));
        }
        return emf_vector(bytes);
    }
    if let Some(body) = wmf_body(bytes) {
        if let Some(img) = decode_wmf(body) {
            return Some((img, Vec::new())); // embedded bitmap
        }
        return wmf_vector(bytes); // vector records (needs the full bytes for the placeable frame)
    }
    None
}

/// Whether `bytes` looks like a metafile this module recognizes (WMF placeable/standard, or EMF) -
/// used by the caller to route bytes here before falling back to the `image` decoders.
pub fn is_metafile(bytes: &[u8]) -> bool {
    is_emf(bytes) || wmf_body(bytes).is_some()
}

// ── EMF ──────────────────────────────────────────────────────────────────────

const EMR_BITBLT: u32 = 76;
const EMR_STRETCHBLT: u32 = 77;
const EMR_SETDIBITSTODEVICE: u32 = 80;
const EMR_STRETCHDIBITS: u32 = 81;
const EMR_EOF: u32 = 14;

fn is_emf(b: &[u8]) -> bool {
    // EMR_HEADER: iType == 1, and the ASCII signature " EMF" sits at byte 40 of the header record.
    b.len() >= 44 && rd_u32(b, 0) == Some(1) && &b[40..44] == b" EMF"
}

/// The (`offBmiSrc`, `offBitsSrc`) field positions, relative to the record start, for each EMF
/// bitmap record - everything before them is fixed-size, so the offsets land at known places.
fn emf_dib_field_pos(rtype: u32) -> Option<(usize, usize)> {
    // Layout after iType(4)+nSize(4): then the record-specific fields. offBmiSrc/cbBmiSrc/
    // offBitsSrc/cbBitsSrc appear together; we give the byte offset of offBmiSrc.
    match rtype {
        // Bounds(16) xDest yDest xSrc ySrc cxSrc cySrc (each i32) -> offBmi at 8+16+24 = 48.
        EMR_STRETCHDIBITS => Some((48, 56)),
        // Bounds(16) xDest yDest cxDest cyDest dwRop xSrc ySrc XformSrc(24) BkColorSrc iUsageSrc ->
        // offBmiSrc at 8+16+(7*4)+24+4+4 = 8+16+28+32 = 84; offBitsSrc at 92.
        EMR_BITBLT | EMR_STRETCHBLT => Some((84, 92)),
        // Bounds(16) xDest yDest xSrc ySrc cxSrc cySrc(each i32, 6) -> offBmi at 8+16+24 = 48.
        EMR_SETDIBITSTODEVICE => Some((48, 56)),
        _ => None,
    }
}

fn decode_emf(b: &[u8]) -> Option<image::RgbaImage> {
    let mut best: Option<image::RgbaImage> = None;
    let mut best_px = 0u64;
    let mut off = 0usize;
    let mut guard = 0u32;
    while off + 8 <= b.len() {
        let rtype = rd_u32(b, off)?;
        let size = rd_u32(b, off + 4)? as usize;
        // `size` comes straight from the file. On wasm32 `usize` is 32-bit, so `off + size` wraps
        // for a crafted size and would pass a plain `> b.len()` check, then panic on the slice.
        let Some(end) = off.checked_add(size) else { break };
        if size < 8 || end > b.len() {
            break;
        }
        if let Some((p_bmi, p_bits)) = emf_dib_field_pos(rtype)
            && let Some(img) = emf_extract_dib(&b[off..end], p_bmi, p_bits)
        {
            let px = img.width() as u64 * img.height() as u64;
            if px > best_px {
                best_px = px;
                best = Some(img);
            }
        }
        if rtype == EMR_EOF {
            break;
        }
        off += size;
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
    best
}

/// Pull the DIB out of one EMF bitmap record: `offBmiSrc`/`cbBmiSrc` (the BITMAPINFO = header +
/// palette) and `offBitsSrc`/`cbBitsSrc` (the pixels), each relative to the record start. They aren't
/// necessarily contiguous, so we concatenate BMI then bits and decode that.
fn emf_extract_dib(rec: &[u8], p_bmi: usize, p_bits: usize) -> Option<image::RgbaImage> {
    let off_bmi = rd_u32(rec, p_bmi)? as usize;
    let cb_bmi = rd_u32(rec, p_bmi + 4)? as usize;
    let off_bits = rd_u32(rec, p_bits)? as usize;
    let cb_bits = rd_u32(rec, p_bits + 4)? as usize;
    // Every one of these four is a raw u32 from the record: sum them with `checked_add` so a crafted
    // pair cannot wrap on wasm32 and turn the bounds checks below into panicking slices.
    let end_bmi = off_bmi.checked_add(cb_bmi)?;
    let end_bits = off_bits.checked_add(cb_bits)?;
    if cb_bmi < 12 || end_bmi > rec.len() || end_bits > rec.len() || cb_bits == 0 {
        return None;
    }
    let mut dib = Vec::with_capacity(cb_bmi.checked_add(cb_bits)?);
    dib.extend_from_slice(&rec[off_bmi..end_bmi]);
    dib.extend_from_slice(&rec[off_bits..end_bits]);
    // The bits begin right after the BITMAPINFO we just appended.
    decode_dib(&dib, 14 + cb_bmi as u32)
}

// ── WMF ──────────────────────────────────────────────────────────────────────

const META_DIBBITBLT: u16 = 0x0940;
const META_DIBSTRETCHBLT: u16 = 0x0B41;
const META_STRETCHDIB: u16 = 0x0F43;
const META_SETDIBTODEV: u16 = 0x0D33;

/// Returns the slice starting at the WMF header (skipping a 22-byte placeable header if present),
/// or `None` if `bytes` isn't a WMF. Validates the standard `META_HEADER` (type 1/2, 9-word header).
fn wmf_body(bytes: &[u8]) -> Option<&[u8]> {
    let start = if bytes.len() >= 22 && bytes[..4] == [0xD7, 0xCD, 0xC6, 0x9A] { 22 } else { 0 };
    let b = bytes.get(start..)?;
    let mtype = rd_u16(b, 0)?;
    let header_words = rd_u16(b, 2)?;
    if (mtype == 1 || mtype == 2) && header_words == 9 {
        Some(b)
    } else {
        None
    }
}

fn decode_wmf(b: &[u8]) -> Option<image::RgbaImage> {
    let mut best: Option<image::RgbaImage> = None;
    let mut best_px = 0u64;
    let mut off = 18usize; // past the 9-word META_HEADER
    let mut guard = 0u32;
    while off + 6 <= b.len() {
        let size_words = rd_u32(b, off)? as usize; // record size in 16-bit words, incl. this field
        let func = rd_u16(b, off + 4)?;
        // Attacker-controlled: the *2 can overflow on wasm32 before the addition even runs.
        let Some(end) = size_words.checked_mul(2).and_then(|n| off.checked_add(n)) else { break };
        if size_words < 3 || end > b.len() {
            break;
        }
        let params = &b[off + 6..end]; // record params (the DIB lives in here)
        if let Some(dib) = wmf_dib_in_params(func, params)
            && let Some(img) = decode_dib(dib, 14 + dib_pixel_offset(dib))
        {
            let px = img.width() as u64 * img.height() as u64;
            if px > best_px {
                best_px = px;
                best = Some(img);
            }
        }
        if func == 0x0000 {
            break; // META_EOF
        }
        off = end;
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
    best
}

/// The DIB bytes inside a WMF bitmap record's params, by record function. The fixed fields before the
/// DIB differ per record (raster op, src/dest rects); the DIB (BITMAPINFOHEADER onward) is the tail.
fn wmf_dib_in_params(func: u16, p: &[u8]) -> Option<&[u8]> {
    let dib_at = match func {
        // RasterOp(4) ColorUsage(2) SrcHeight SrcWidth YSrc XSrc DestHeight DestWidth YDest XDest -> 22
        META_STRETCHDIB => 22,
        // RasterOp(4) YSrc XSrc Height Width YDest XDest -> 16 (the in-record-DIB form)
        META_DIBBITBLT => 16,
        // RasterOp(4) SrcHeight SrcWidth YSrc XSrc DestHeight DestWidth YDest XDest -> 20
        META_DIBSTRETCHBLT => 20,
        // ColorUsage(2) ScanCount(2) StartScan(2) YDib XDib Height Width YDest XDest -> 18
        META_SETDIBTODEV => 18,
        _ => return None,
    };
    p.get(dib_at..).filter(|d| d.len() > 40)
}

// ── DIB -> RGBA (via the image BMP decoder, by prepending a BITMAPFILEHEADER) ──

/// Byte offset of the pixel data within a packed DIB (BITMAPINFOHEADER + palette): header size +
/// the colour table. Used to set `bfOffBits` for the synthesized BMP header.
fn dib_pixel_offset(dib: &[u8]) -> u32 {
    let bi_size = rd_u32(dib, 0).unwrap_or(40);
    let bit_count = rd_u16(dib, 14).unwrap_or(0) as u32;
    let compression = rd_u32(dib, 16).unwrap_or(0);
    let clr_used = rd_u32(dib, 32).unwrap_or(0);
    let palette = if bit_count <= 8 {
        let n = if clr_used != 0 { clr_used } else { 1u32 << bit_count };
        n * 4 // BITMAPINFOHEADER colour entries are 4 bytes (RGBQUAD)
    } else if compression == 3 {
        12 // BI_BITFIELDS: three 4-byte channel masks precede the pixels
    } else {
        0
    };
    bi_size + palette
}

/// Wrap a packed DIB (`pixel_off` = byte offset of pixels within it) with a 14-byte BITMAPFILEHEADER
/// and decode it through the `image` BMP decoder - reusing its bit-depth / palette / RLE / bitfields
/// support. `None` if it isn't a DIB we can read.
fn decode_dib(dib: &[u8], pixel_off: u32) -> Option<image::RgbaImage> {
    let bi_size = rd_u32(dib, 0)?;
    // A BITMAPINFOHEADER (40) / V4 (108) / V5 (124). Reject a BITMAPCOREHEADER (12) + junk.
    if !(bi_size == 40 || bi_size == 52 || bi_size == 56 || bi_size == 108 || bi_size == 124) {
        return None;
    }
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes()); // bfSize
    bmp.extend_from_slice(&0u32.to_le_bytes()); // bfReserved
    bmp.extend_from_slice(&pixel_off.to_le_bytes()); // bfOffBits
    bmp.extend_from_slice(dib);
    image::ImageReader::with_format(Cursor::new(bmp), image::ImageFormat::Bmp)
        .decode()
        .ok()
        .map(|i| i.to_rgba8())
}

// ── GDI vector playback (shared by EMF + WMF) ────────────────────────────────

use tiny_skia::{Color, FillRule, Mask, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// The cap on the rendered pixmap's longest side - metafiles carry logical bounds that can be huge or
/// tiny; we render at the bounds resolution scaled into this, then the compositor fits it to the
/// picture's docx box. High enough to stay crisp when scaled up in the page.
const MAX_DIM: f32 = 1600.0;

#[derive(Clone, Copy)]
struct Pen {
    color: Color,
    width: f32, // logical units; 0 = a 1-device-pixel cosmetic pen
    visible: bool,
}
#[derive(Clone, Copy)]
struct Brush {
    color: Color,
    filled: bool,      // false = hollow / null brush
    hatch: Option<u8>, // Some(style) for BS_HATCHED: 0=horiz 1=vert 2=fdiag 3=bdiag 4=cross 5=diagcross
}

#[derive(Clone)]
struct MetaFont {
    height: f32, // logical units (absolute); device px = height * base scale
    bold: bool,
    italic: bool,
    family: Option<String>,
}

enum GdiObj {
    Pen(Pen),
    Brush(Brush),
    Font(MetaFont),
}


/// A minimal GDI device: a tiny-skia canvas plus the device-context state the drawing records read
/// (current pen / brush, current position, fill rule, the world transform) and an object table that
/// `SelectObject` indexes. Logical coordinates are mapped to device pixels by `base` (from the
/// metafile bounds) composed with `world` (the record-set transform).
struct Gdi {
    pm: Pixmap,
    base: Transform,
    world: Transform,
    objs: Vec<Option<GdiObj>>,
    pen: Pen,
    brush: Brush,
    pos: (f32, f32),
    fill_rule: FillRule,
    px_per_logical: f32, // approximate device px per logical unit, for cosmetic pen width + font size
    text_color: Color,
    font: MetaFont,
    ta_baseline: bool, // SETTEXTALIGN selected TA_BASELINE (reference y is the baseline, not the top)
    texts: Vec<MetaText>,
    // WMF window mapping (logical -> the placeable frame): SetWindowOrg/Ext drive `base`.
    win_org: (f32, f32),
    win_ext: (f32, f32),
    // GDI path-bracket state: between BeginPath and EndPath, geometry records accumulate into `pb`
    // (in logical coords) instead of drawing; FillPath/StrokePath/StrokeAndFillPath then render it.
    path_rec: bool,
    pb: PathBuilder,
    pb_started: bool, // whether `pb` has a current subpath point (so a bare LineTo gets a leading move)
}

impl Gdi {
    fn new(pm: Pixmap, base: Transform, px_per_logical: f32) -> Self {
        Gdi {
            pm,
            base,
            world: Transform::identity(),
            objs: Vec::new(),
            pen: Pen { color: Color::BLACK, width: 0.0, visible: true },
            brush: Brush { color: Color::WHITE, filled: false, hatch: None }, // no fill until selected
            pos: (0.0, 0.0),
            fill_rule: FillRule::Winding,
            px_per_logical,
            text_color: Color::BLACK,
            font: MetaFont { height: 0.0, bold: false, italic: false, family: None },
            ta_baseline: false,
            texts: Vec::new(),
            win_org: (0.0, 0.0),
            win_ext: (0.0, 0.0),
            path_rec: false,
            pb: PathBuilder::new(),
            pb_started: false,
        }
    }

    /// Add an object to the first free table slot - WMF's `CreateXIndirect` has no explicit index
    /// (unlike EMF), so it fills the lowest available handle.
    fn add_obj(&mut self, obj: GdiObj) {
        if let Some(slot) = self.objs.iter_mut().find(|o| o.is_none()) {
            *slot = Some(obj);
        } else {
            self.objs.push(Some(obj));
        }
    }

    /// Recompute the WMF logical->device transform from the current window origin/extent (mapped onto
    /// the full pixmap, which was sized from the placeable frame).
    fn wmf_set_base(&mut self) {
        let (ex, ey) = self.win_ext;
        if ex.abs() < 0.5 || ey.abs() < 0.5 {
            return;
        }
        let (pw, ph) = (self.pm.width() as f32, self.pm.height() as f32);
        let (sx, sy) = (pw / ex, ph / ey);
        self.base = Transform::from_row(sx, 0.0, 0.0, sy, -self.win_org.0 * sx, -self.win_org.1 * sy);
        self.px_per_logical = sy.abs().max(0.0001);
    }

    /// The composed logical->device transform (apply the world transform, then the bounds map).
    fn xform(&self) -> Transform {
        self.base.pre_concat(self.world)
    }

    fn put_obj(&mut self, idx: usize, obj: GdiObj) {
        if idx >= self.objs.len() {
            self.objs.resize_with(idx + 1, || None);
        }
        self.objs[idx] = Some(obj);
    }

    fn select(&mut self, handle: u32) {
        if handle & 0x8000_0000 != 0 {
            // Stock object: the ones that actually change the look (null pen/brush, black/white).
            match handle & 0x7fff_ffff {
                0 | 1 => self.brush = Brush { color: Color::WHITE, filled: true, hatch: None }, // WHITE/LTGRAY
                4 => self.brush = Brush { color: Color::BLACK, filled: true, hatch: None },     // BLACK_BRUSH
                5 => self.brush.filled = false,                                    // NULL_BRUSH
                7 => self.pen = Pen { color: Color::BLACK, width: 0.0, visible: true }, // BLACK_PEN
                8 => self.pen.visible = false,                                     // NULL_PEN
                _ => {}
            }
            return;
        }
        match self.objs.get(handle as usize).and_then(|o| o.as_ref()) {
            Some(GdiObj::Pen(p)) => self.pen = *p,
            Some(GdiObj::Brush(b)) => self.brush = *b,
            Some(GdiObj::Font(f)) => self.font = f.clone(),
            None => {}
        }
    }

    fn stroke_width_dev(&self) -> f32 {
        // tiny-skia scales the stroke by the transform, so give it a logical width; a 0-width
        // (cosmetic) pen becomes ~1 device px.
        if self.pen.width > 0.0 {
            self.pen.width
        } else {
            (1.0 / self.px_per_logical.max(0.0001)).max(0.01)
        }
    }

    fn stroke(&mut self, path: &tiny_skia::Path) {
        if !self.pen.visible {
            return;
        }
        let mut paint = Paint::default();
        paint.set_color(self.pen.color);
        paint.anti_alias = true;
        let stroke = Stroke { width: self.stroke_width_dev(), ..Default::default() };
        let t = self.xform();
        self.pm.stroke_path(path, &paint, &stroke, t, None);
    }

    fn fill(&mut self, path: &tiny_skia::Path) {
        if !self.brush.filled {
            return;
        }
        if let Some(style) = self.brush.hatch {
            self.fill_hatch(path, style);
            return;
        }
        let mut paint = Paint::default();
        paint.set_color(self.brush.color);
        paint.anti_alias = true;
        let t = self.xform();
        self.pm.fill_path(path, &paint, self.fill_rule, t, None);
    }

    /// Fill a shape with a GDI hatch pattern (BS_HATCHED) instead of a solid colour: rasterize the
    /// shape into a clip mask, then stroke a grid of evenly-spaced device-space lines through it.
    /// Styles: 0 horizontal, 1 vertical, 2 fdiagonal, 3 bdiagonal, 4 cross, 5 diagcross.
    fn fill_hatch(&mut self, path: &tiny_skia::Path, style: u8) {
        let t = self.xform();
        let dev = match path.clone().transform(t) {
            Some(p) => p.bounds(),
            None => return,
        };
        let (pw, ph) = (self.pm.width() as f32, self.pm.height() as f32);
        let (l, top) = (dev.left().max(0.0), dev.top().max(0.0));
        let (r, bot) = (dev.right().min(pw), dev.bottom().min(ph));
        if r <= l || bot <= top {
            return;
        }
        let mut mask = match Mask::new(self.pm.width(), self.pm.height()) {
            Some(m) => m,
            None => return,
        };
        mask.fill_path(path, self.fill_rule, true, t);

        const GAP: f32 = 6.0;
        let mut pb = PathBuilder::new();
        let (horiz, vert) = (style == 0 || style == 4, style == 1 || style == 4);
        let (fdiag, bdiag) = (style == 2 || style == 5, style == 3 || style == 5);
        if horiz {
            let mut y = top;
            while y <= bot {
                pb.move_to(l, y);
                pb.line_to(r, y);
                y += GAP;
            }
        }
        if vert {
            let mut x = l;
            while x <= r {
                pb.move_to(x, top);
                pb.line_to(x, bot);
                x += GAP;
            }
        }
        let h = bot - top;
        if fdiag {
            // "\" lines (slope +1): sweep the top x-intercept across the box plus its height.
            let mut x0 = l - h;
            while x0 <= r {
                pb.move_to(x0, top);
                pb.line_to(x0 + h, bot);
                x0 += GAP;
            }
        }
        if bdiag {
            // "/" lines (slope -1).
            let mut x0 = l;
            while x0 <= r + h {
                pb.move_to(x0, top);
                pb.line_to(x0 - h, bot);
                x0 += GAP;
            }
        }
        if let Some(hpath) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(self.brush.color);
            paint.anti_alias = true;
            let stroke = Stroke { width: 1.0, ..Default::default() };
            self.pm.stroke_path(&hpath, &paint, &stroke, Transform::identity(), Some(&mask));
        }
    }

    /// Fill (brush) then stroke (pen) a closed shape - GDI paints the interior, then the outline.
    fn fill_and_stroke(&mut self, path: &tiny_skia::Path) {
        self.fill(path);
        self.stroke(path);
    }

    /// Start a path bracket (`BeginPath`): subsequent geometry accumulates into `pb` instead of drawing.
    fn begin_path(&mut self) {
        self.path_rec = true;
        self.pb = PathBuilder::new();
        self.pb_started = false;
    }

    fn pb_move(&mut self, x: f32, y: f32) {
        self.pb.move_to(x, y);
        self.pb_started = true;
        self.pos = (x, y);
    }

    fn pb_line(&mut self, x: f32, y: f32) {
        if !self.pb_started {
            self.pb.move_to(self.pos.0, self.pos.1);
            self.pb_started = true;
        }
        self.pb.line_to(x, y);
        self.pos = (x, y);
    }

    fn pb_cubic(&mut self, c1: (f32, f32), c2: (f32, f32), end: (f32, f32)) {
        if !self.pb_started {
            self.pb.move_to(self.pos.0, self.pos.1);
            self.pb_started = true;
        }
        self.pb.cubic_to(c1.0, c1.1, c2.0, c2.1, end.0, end.1);
        self.pos = end;
    }

    /// Close the current subpath (`CloseFigure`).
    fn close_figure(&mut self) {
        if self.path_rec && self.pb_started {
            self.pb.close();
            self.pb_started = false;
        }
    }

    /// Finish the accumulated path (`EndPath` then a paint command): take the builder and return the
    /// path in logical coords (the caller fills/strokes it, applying the world transform as usual).
    fn finish_path(&mut self) -> Option<tiny_skia::Path> {
        let pb = std::mem::replace(&mut self.pb, PathBuilder::new());
        self.pb_started = false;
        self.path_rec = false;
        pb.finish()
    }
}

/// A COLORREF (`0x00BBGGRR`, little-endian bytes R,G,B,_) to a tiny-skia colour.
fn colorref(b: &[u8], at: usize) -> Color {
    let r = b.get(at).copied().unwrap_or(0);
    let g = b.get(at + 1).copied().unwrap_or(0);
    let bl = b.get(at + 2).copied().unwrap_or(0);
    Color::from_rgba8(r, g, bl, 255)
}

fn path_from_points(pts: &[(f32, f32)], close: bool) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut it = pts.iter();
    let first = it.next()?;
    pb.move_to(first.0, first.1);
    for p in it {
        pb.line_to(p.0, p.1);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

// ── EMF vector interpreter ───────────────────────────────────────────────────

const EMR_POLYBEZIER: u32 = 2;
const EMR_POLYGON: u32 = 3;
const EMR_POLYLINE: u32 = 4;
const EMR_POLYBEZIERTO: u32 = 5;
const EMR_POLYLINETO: u32 = 6;
const EMR_POLYPOLYGON: u32 = 8;
const EMR_SETWORLDTRANSFORM: u32 = 35;
const EMR_MODIFYWORLDTRANSFORM: u32 = 36;
const EMR_BEGINPATH: u32 = 59;
const EMR_ENDPATH: u32 = 60;
const EMR_CLOSEFIGURE: u32 = 61;
const EMR_FILLPATH: u32 = 62;
const EMR_STROKEANDFILLPATH: u32 = 63;
const EMR_STROKEPATH: u32 = 64;
const EMR_SELECTOBJECT: u32 = 37;
const EMR_CREATEPEN: u32 = 38;
const EMR_CREATEBRUSHINDIRECT: u32 = 39;
const EMR_DELETEOBJECT: u32 = 40;
const EMR_ELLIPSE: u32 = 42;
const EMR_RECTANGLE: u32 = 43;
const EMR_SETPOLYFILLMODE: u32 = 19;
const EMR_MOVETOEX: u32 = 27;
const EMR_LINETO: u32 = 54;
const EMR_POLYBEZIER16: u32 = 85;
const EMR_POLYGON16: u32 = 86;
const EMR_POLYLINE16: u32 = 87;
const EMR_POLYBEZIERTO16: u32 = 88;
const EMR_POLYLINETO16: u32 = 89;
const EMR_POLYPOLYGON16: u32 = 91;
const EMR_EXTCREATEPEN: u32 = 95;
const EMR_SETTEXTALIGN: u32 = 22;
const EMR_SETTEXTCOLOR: u32 = 24;
const EMR_EXTCREATEFONTINDIRECTW: u32 = 82;
const EMR_EXTTEXTOUTA: u32 = 83;
const EMR_EXTTEXTOUTW: u32 = 84;

/// Render an EMF as vector art (the ~90% of metafiles that aren't a wrapped bitmap). Walks the GDI
/// records onto a tiny-skia canvas sized from the metafile bounds, collecting text runs the caller
/// rasterizes. `None` if it produced nothing.
/// The metafile frame (`rclFrame`, in 0.01 mm at header bytes 24..40) converted to device pixels via
/// the recording resolution (`szlDevice` at 72/76, `szlMillimeters` at 80/84). Returns
/// `(left, top, width, height)` in device pixels, or `None` if the resolution fields are unusable.
fn emf_frame_device(b: &[u8]) -> Option<(f32, f32, f32, f32)> {
    let (fl, ft, fr, fb) = (rd_i32(b, 24)?, rd_i32(b, 28)?, rd_i32(b, 32)?, rd_i32(b, 36)?);
    let (sdx, sdy) = (rd_i32(b, 72)?, rd_i32(b, 76)?);
    let (smx, smy) = (rd_i32(b, 80)?, rd_i32(b, 84)?);
    if sdx <= 0 || sdy <= 0 || smx <= 0 || smy <= 0 {
        return None;
    }
    let px = |v: i32, sd: i32, sm: i32| v as f32 / 100.0 * sd as f32 / sm as f32;
    Some((px(fl, sdx, smx), px(ft, sdy, smy), px(fr - fl, sdx, smx), px(fb - ft, sdy, smy)))
}

fn emf_vector(b: &[u8]) -> Option<(image::RgbaImage, Vec<MetaText>)> {
    if b.len() < 88 {
        return None;
    }
    // EMR_HEADER: rclBounds (inclusive device rect) at byte 8.
    let (l, t, r, bot) = (rd_i32(b, 8)?, rd_i32(b, 12)?, rd_i32(b, 16)?, rd_i32(b, 20)?);
    let (bw, bh) = ((r - l).max(1) as f32, (bot - t).max(1) as f32);
    // The drawing rect to rasterize defaults to the content bounds. But when the metafile frame
    // (rclFrame) is meaningfully larger than the content, size to the frame instead: the content then
    // keeps its position + scale inside it, and the pixmap aspect matches the picture box rather than
    // the content bbox. Without this, art drawn small in a wide frame (e.g. WordArt) gets stretched to
    // fill the box. A guarded no-op when frame ~= bounds, which is the common case.
    let (mut dl, mut dt, mut dw, mut dh) = (l as f32, t as f32, bw, bh);
    if let Some((fl, ft, fw, fh)) = emf_frame_device(b)
        && fw > 0.0
        && fh > 0.0
        && (fw > bw * 1.3 || fh > bh * 1.3)
        && fw <= bw * 64.0
        && fh <= bh * 64.0
        && l as f32 >= fl - 1.0
        && t as f32 >= ft - 1.0
    {
        (dl, dt, dw, dh) = (fl, ft, fw, fh);
    }
    let s = (MAX_DIM / dw.max(dh)).clamp(0.05, 8.0);
    let (pw, ph) = ((dw * s).round().max(1.0) as u32, (dh * s).round().max(1.0) as u32);
    if pw > 4096 || ph > 4096 {
        return None;
    }
    let base = Transform::from_row(s, 0.0, 0.0, s, -dl * s, -dt * s);
    let mut g = Gdi::new(Pixmap::new(pw, ph)?, base, s);

    let mut off = 0usize;
    let mut guard = 0u32;
    let mut drew = false;
    while off + 8 <= b.len() {
        let rt = rd_u32(b, off)?;
        let size = rd_u32(b, off + 4)? as usize;
        // See `decode_emf`: `size` is from the file and wraps a plain add on wasm32.
        let Some(end) = off.checked_add(size) else { break };
        if size < 8 || end > b.len() {
            break;
        }
        let rec = &b[off..end];
        drew |= emf_play(&mut g, rt, rec);
        if rt == EMR_EOF {
            break;
        }
        off = end;
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
    if !drew {
        return None;
    }
    gdi_finish(&mut g)
}

/// Finish a GDI playback: un-premultiply the tiny-skia pixmap (it stores premultiplied RGBA; the page
/// compositor blends straight alpha) and pair it with the collected text runs.
fn gdi_finish(g: &mut Gdi) -> Option<(image::RgbaImage, Vec<MetaText>)> {
    let texts = std::mem::take(&mut g.texts);
    let (w, h) = (g.pm.width(), g.pm.height());
    let mut data = g.pm.data().to_vec();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u16;
        if a > 0 && a < 255 {
            px[0] = (px[0] as u16 * 255 / a) as u8;
            px[1] = (px[1] as u16 * 255 / a) as u8;
            px[2] = (px[2] as u16 * 255 / a) as u8;
        }
    }
    image::RgbaImage::from_raw(w, h, data).map(|img| (img, texts))
}

/// Map a logical text reference point + the current font/colour into a [`MetaText`] in image pixels
/// (shared by EMF + WMF text records). Returns whether a non-empty run was queued.
fn push_meta_text(g: &mut Gdi, lx: f32, ly: f32, text: String) -> bool {
    let text = text.trim_end_matches('\0').to_string();
    if text.trim().is_empty() {
        return false;
    }
    let mut pts = [tiny_skia::Point::from_xy(lx, ly)];
    g.xform().map_points(&mut pts);
    let size_px = (g.font.height * g.px_per_logical).max(1.0);
    let top = if g.ta_baseline { pts[0].y - size_px * 0.8 } else { pts[0].y };
    let rgb = [
        (g.text_color.red() * 255.0).round() as u8,
        (g.text_color.green() * 255.0).round() as u8,
        (g.text_color.blue() * 255.0).round() as u8,
    ];
    g.texts.push(MetaText {
        x: pts[0].x,
        y: top,
        size_px,
        rgb,
        bold: g.font.bold,
        italic: g.font.italic,
        family: g.font.family.clone(),
        text,
    });
    true
}

/// Play one EMF record onto the device. Returns whether it drew anything (so a metafile that only
/// set state, with no geometry, isn't mistaken for a rendered image).
fn emf_play(g: &mut Gdi, rt: u32, rec: &[u8]) -> bool {
    match rt {
        EMR_SETWORLDTRANSFORM => {
            if let Some(x) = read_xform(rec, 8) {
                g.world = x;
            }
        }
        EMR_MODIFYWORLDTRANSFORM => {
            // data at 8: XFORM(24), then iMode(4). Mode 4 = MWT_SET (replace); else pre/post-multiply.
            if let Some(x) = read_xform(rec, 8) {
                let mode = rd_u32(rec, 32).unwrap_or(4);
                g.world = match mode {
                    1 => g.world.pre_concat(x),  // LEFTMULTIPLY
                    2 => g.world.post_concat(x), // RIGHTMULTIPLY
                    _ => x,                       // SET
                };
            }
        }
        EMR_BEGINPATH => g.begin_path(),
        EMR_ENDPATH => g.path_rec = false,
        EMR_CLOSEFIGURE => g.close_figure(),
        EMR_FILLPATH => {
            if let Some(p) = g.finish_path() {
                g.fill(&p);
                return true;
            }
        }
        EMR_STROKEANDFILLPATH => {
            if let Some(p) = g.finish_path() {
                g.fill_and_stroke(&p);
                return true;
            }
        }
        EMR_STROKEPATH => {
            if let Some(p) = g.finish_path() {
                g.stroke(&p);
                return true;
            }
        }
        EMR_SETPOLYFILLMODE => {
            g.fill_rule = if rd_u32(rec, 8) == Some(2) { FillRule::EvenOdd } else { FillRule::Winding };
        }
        EMR_CREATEPEN => {
            // ihPen(8), LOGPEN: penStyle(12), width.x(16), color(24).
            let idx = rd_u32(rec, 8).unwrap_or(0) as usize;
            let style = rd_u32(rec, 12).unwrap_or(0);
            let width = rd_i32(rec, 16).unwrap_or(0) as f32;
            let pen = Pen { color: colorref(rec, 24), width, visible: style & 0xff != 5 };
            g.put_obj(idx, GdiObj::Pen(pen));
        }
        EMR_EXTCREATEPEN => {
            // ihPen(8), off/cb x4 (12..28), ELEXTLOGPEN: penStyle(28), width(32), brushStyle(36), color(40).
            let idx = rd_u32(rec, 8).unwrap_or(0) as usize;
            let style = rd_u32(rec, 28).unwrap_or(0);
            let width = rd_u32(rec, 32).unwrap_or(0) as f32;
            let pen = Pen { color: colorref(rec, 40), width, visible: style & 0xff != 5 };
            g.put_obj(idx, GdiObj::Pen(pen));
        }
        EMR_CREATEBRUSHINDIRECT => {
            // ihBrush(8), LOGBRUSH: style(12), color(16), hatch(20). style 1 = BS_NULL, 2 = BS_HATCHED.
            let idx = rd_u32(rec, 8).unwrap_or(0) as usize;
            let style = rd_u32(rec, 12).unwrap_or(0);
            let hatch = (style == 2).then(|| rd_u32(rec, 20).unwrap_or(0) as u8);
            let brush = Brush { color: colorref(rec, 16), filled: style != 1, hatch };
            g.put_obj(idx, GdiObj::Brush(brush));
        }
        EMR_SELECTOBJECT => {
            if let Some(h) = rd_u32(rec, 8) {
                g.select(h);
            }
        }
        EMR_DELETEOBJECT => {
            let idx = rd_u32(rec, 8).unwrap_or(0) as usize;
            if idx < g.objs.len() {
                g.objs[idx] = None;
            }
        }
        EMR_MOVETOEX => {
            let to = (rd_i32(rec, 8).unwrap_or(0) as f32, rd_i32(rec, 12).unwrap_or(0) as f32);
            if g.path_rec {
                g.pb_move(to.0, to.1);
            } else {
                g.pos = to;
            }
        }
        EMR_LINETO => {
            let to = (rd_i32(rec, 8).unwrap_or(0) as f32, rd_i32(rec, 12).unwrap_or(0) as f32);
            if g.path_rec {
                g.pb_line(to.0, to.1);
                return false;
            }
            if let Some(p) = path_from_points(&[g.pos, to], false) {
                g.stroke(&p);
            }
            g.pos = to;
            return true;
        }
        EMR_POLYLINE | EMR_POLYGON | EMR_POLYLINETO => {
            let pts = emf_points(rec, false);
            return draw_poly(g, &pts, rt == EMR_POLYGON, rt == EMR_POLYLINETO);
        }
        EMR_POLYLINE16 | EMR_POLYGON16 | EMR_POLYLINETO16 => {
            let pts = emf_points(rec, true);
            return draw_poly(g, &pts, rt == EMR_POLYGON16, rt == EMR_POLYLINETO16);
        }
        EMR_POLYBEZIER | EMR_POLYBEZIERTO => {
            let pts = emf_points(rec, false);
            return draw_bezier(g, &pts, rt == EMR_POLYBEZIERTO);
        }
        EMR_POLYBEZIER16 | EMR_POLYBEZIERTO16 => {
            let pts = emf_points(rec, true);
            return draw_bezier(g, &pts, rt == EMR_POLYBEZIERTO16);
        }
        EMR_POLYPOLYGON | EMR_POLYPOLYGON16 => {
            return emf_polypolygon(g, rec, rt == EMR_POLYPOLYGON16);
        }
        EMR_SETTEXTCOLOR => {
            g.text_color = colorref(rec, 8);
        }
        EMR_SETTEXTALIGN => {
            // TA_BASELINE (24) in the vertical bits means the reference point is the baseline.
            g.ta_baseline = rd_u32(rec, 8).unwrap_or(0) & 24 == 24;
        }
        EMR_EXTCREATEFONTINDIRECTW => {
            // ihFont(8), LOGFONT: lfHeight(12), lfWeight(28), lfItalic u8(32), faceName UTF-16(40, 64B).
            let idx = rd_u32(rec, 8).unwrap_or(0) as usize;
            let height = rd_i32(rec, 12).unwrap_or(0).unsigned_abs() as f32;
            let bold = rd_i32(rec, 28).unwrap_or(0) >= 700;
            let italic = rec.get(32).copied().unwrap_or(0) != 0;
            let family = utf16_string(rec, 40, 32).filter(|s| !s.is_empty());
            g.put_obj(idx, GdiObj::Font(MetaFont { height, bold, italic, family }));
        }
        EMR_EXTTEXTOUTW | EMR_EXTTEXTOUTA => {
            return emf_text_out(g, rec, rt == EMR_EXTTEXTOUTW);
        }
        EMR_BITBLT => {
            // A BitBlt with no source bitmap (cbBitsSrc == 0) is a PatBlt-style solid fill of the
            // destination rect with the current brush (how spreadsheet cell shading is drawn).
            let cb_bits = rd_u32(rec, 96).unwrap_or(0);
            if cb_bits == 0 {
                let (x, y, cx, cy) = (rd_i32(rec, 24), rd_i32(rec, 28), rd_i32(rec, 32), rd_i32(rec, 36));
                if let (Some(x), Some(y), Some(cx), Some(cy)) = (x, y, cx, cy)
                    && let Some(rr) = tiny_skia::Rect::from_xywh(x as f32, y as f32, cx as f32, cy as f32)
                {
                    let path = PathBuilder::from_rect(rr);
                    // PATCOPY paints the brush regardless of its NULL flag.
                    let saved = g.brush.filled;
                    g.brush.filled = true;
                    g.fill(&path);
                    g.brush.filled = saved;
                    return true;
                }
            }
        }
        EMR_RECTANGLE | EMR_ELLIPSE => {
            // data: rclBox (l,t,r,b i32) at 8.
            let (l, t, r, bo) = (rd_i32(rec, 8), rd_i32(rec, 12), rd_i32(rec, 16), rd_i32(rec, 20));
            if let (Some(l), Some(t), Some(r), Some(bo)) = (l, t, r, bo) {
                let rectf = tiny_skia::Rect::from_ltrb(l as f32, t as f32, r as f32, bo as f32);
                let path = rectf.and_then(|rr| {
                    if rt == EMR_ELLIPSE {
                        PathBuilder::from_oval(rr)
                    } else {
                        Some(PathBuilder::from_rect(rr))
                    }
                });
                if let Some(p) = path {
                    if g.path_rec {
                        g.pb.push_path(&p);
                        g.pb_started = false; // the pushed subpath is complete
                        return false;
                    }
                    g.fill_and_stroke(&p);
                    return true;
                }
            }
        }
        _ => {}
    }
    false
}

/// Stroke (and optionally fill) a point list, or - inside a path bracket - append it to the path.
/// `close` marks a polygon (filled, closed); `continues` marks a *To variant that extends from the
/// current point (vs starting a fresh subpath).
fn draw_poly(g: &mut Gdi, pts: &[(f32, f32)], close: bool, continues: bool) -> bool {
    if pts.is_empty() {
        return false;
    }
    if g.path_rec {
        if continues {
            for &(x, y) in pts {
                g.pb_line(x, y);
            }
        } else {
            let mut it = pts.iter();
            if let Some(&(x, y)) = it.next() {
                g.pb_move(x, y);
            }
            for &(x, y) in it {
                g.pb_line(x, y);
            }
            if close {
                g.close_figure();
            }
        }
        return false; // recording draws nothing yet
    }
    if pts.len() < 2 {
        return false;
    }
    if let Some(p) = path_from_points(pts, close) {
        if close {
            g.fill_and_stroke(&p);
        } else {
            g.stroke(&p);
        }
        return true;
    }
    false
}

/// Stroke a cubic Bézier path with the current pen. `PolyBezier` gives the start point then triples
/// (ctrl1, ctrl2, end); `PolyBezierTo` (`bezier_to`) starts at the current position. Updates the
/// current position to the final end point, matching GDI.
fn draw_bezier(g: &mut Gdi, pts: &[(f32, f32)], bezier_to: bool) -> bool {
    let (start, rest) = if bezier_to {
        (g.pos, pts)
    } else {
        match pts.split_first() {
            Some((f, r)) => (*f, r),
            None => return false,
        }
    };
    if rest.len() < 3 {
        return false;
    }
    if g.path_rec {
        if !bezier_to {
            g.pb_move(start.0, start.1);
        }
        let mut i = 0;
        while i + 2 < rest.len() {
            g.pb_cubic(rest[i], rest[i + 1], rest[i + 2]);
            i += 3;
        }
        return false; // recording draws nothing yet
    }
    let mut pb = PathBuilder::new();
    pb.move_to(start.0, start.1);
    let mut last = start;
    let mut i = 0;
    while i + 2 < rest.len() {
        let (c1, c2, end) = (rest[i], rest[i + 1], rest[i + 2]);
        pb.cubic_to(c1.0, c1.1, c2.0, c2.1, end.0, end.1);
        last = end;
        i += 3;
    }
    g.pos = last;
    if let Some(path) = pb.finish() {
        g.stroke(&path);
        return true;
    }
    false
}

/// Points of a single-array EMF poly record: Bounds(16) Count(4) then Count points (POINTL 8 bytes,
/// or POINTS 4 bytes for the *16 variants). Record data starts at byte 8.
fn emf_points(rec: &[u8], small: bool) -> Vec<(f32, f32)> {
    let count = rd_u32(rec, 24).unwrap_or(0) as usize;
    let mut out = Vec::with_capacity(count.min(100_000));
    let stride = if small { 4 } else { 8 };
    let mut p = 28;
    for _ in 0..count.min(200_000) {
        let (x, y) = if small {
            (rd_i16(rec, p).map(|v| v as i32), rd_i16(rec, p + 2).map(|v| v as i32))
        } else {
            (rd_i32(rec, p), rd_i32(rec, p + 4))
        };
        match (x, y) {
            (Some(x), Some(y)) => out.push((x as f32, y as f32)),
            _ => break,
        }
        p += stride;
    }
    out
}

/// EMR_POLYPOLYGON[16]: Bounds(16) NumPolys(4) TotalCount(4) then NumPolys u32 counts, then points.
fn emf_polypolygon(g: &mut Gdi, rec: &[u8], small: bool) -> bool {
    let npoly = rd_u32(rec, 24).unwrap_or(0) as usize;
    if npoly == 0 || npoly > 100_000 {
        return false;
    }
    let mut counts = Vec::with_capacity(npoly);
    let mut p = 32; // after Bounds(16)@8 + NumPolys@24 + TotalCount@28
    for _ in 0..npoly {
        counts.push(rd_u32(rec, p).unwrap_or(0) as usize);
        p += 4;
    }
    let stride = if small { 4 } else { 8 };
    let read = |p: usize| -> (Option<i32>, Option<i32>) {
        if small {
            (rd_i16(rec, p).map(|v| v as i32), rd_i16(rec, p + 2).map(|v| v as i32))
        } else {
            (rd_i32(rec, p), rd_i32(rec, p + 4))
        }
    };
    if g.path_rec {
        for c in counts {
            for i in 0..c {
                let (x, y) = read(p);
                p += stride;
                if let (Some(x), Some(y)) = (x, y) {
                    if i == 0 {
                        g.pb_move(x as f32, y as f32);
                    } else {
                        g.pb_line(x as f32, y as f32);
                    }
                }
            }
            g.close_figure();
        }
        return false; // recording draws nothing yet
    }
    let mut pb = PathBuilder::new();
    let mut drew = false;
    for c in counts {
        for i in 0..c {
            let (x, y) = read(p);
            p += stride;
            if let (Some(x), Some(y)) = (x, y) {
                if i == 0 {
                    pb.move_to(x as f32, y as f32);
                } else {
                    pb.line_to(x as f32, y as f32);
                }
            }
        }
        pb.close();
        drew = true;
    }
    if let Some(path) = pb.finish() {
        g.fill_and_stroke(&path);
        return drew;
    }
    false
}

/// An EMF `XFORM` (six f32: eM11 eM12 eM21 eM22 eDx eDy) to a tiny-skia transform.
fn read_xform(rec: &[u8], at: usize) -> Option<Transform> {
    let f = |o: usize| rd_f32(rec, at + o);
    Some(Transform::from_row(f(0)?, f(4)?, f(8)?, f(12)?, f(16)?, f(20)?))
}

/// EMR_EXTTEXTOUT[W/A]: after the bounds/scale fields, the EMRTEXT struct at byte 36 - Reference
/// POINTL (x@36, y@40), Chars@44, offString@48 (relative to the record). Collects a [`MetaText`] in
/// the rendered image's pixel space for the caller's font system to draw.
fn emf_text_out(g: &mut Gdi, rec: &[u8], wide: bool) -> bool {
    let rx = rd_i32(rec, 36).unwrap_or(0) as f32;
    let ry = rd_i32(rec, 40).unwrap_or(0) as f32;
    let nchars = rd_u32(rec, 44).unwrap_or(0) as usize;
    let off = rd_u32(rec, 48).unwrap_or(0) as usize;
    if nchars == 0 || nchars > 100_000 {
        return false;
    }
    let text = if wide {
        utf16_string(rec, off, nchars).unwrap_or_default()
    } else {
        rec.get(off..off + nchars).map(|s| s.iter().map(|&c| c as char).collect()).unwrap_or_default()
    };
    push_meta_text(g, rx, ry, text)
}

/// Up to `max_chars` UTF-16LE units at byte `at`, stopping at the first NUL.
fn utf16_string(b: &[u8], at: usize, max_chars: usize) -> Option<String> {
    let mut units = Vec::new();
    for i in 0..max_chars {
        let u = rd_u16(b, at + i * 2)?;
        if u == 0 {
            break;
        }
        units.push(u);
    }
    Some(String::from_utf16_lossy(&units))
}

// ── WMF vector interpreter ───────────────────────────────────────────────────

const META_SETWINDOWORG: u16 = 0x020B;
const META_SETWINDOWEXT: u16 = 0x020C;
const META_SETPOLYFILLMODE_W: u16 = 0x0106;
const META_SETTEXTCOLOR_W: u16 = 0x0209;
const META_SETTEXTALIGN_W: u16 = 0x012E;
const META_LINETO: u16 = 0x0213;
const META_MOVETO: u16 = 0x0214;
const META_POLYGON: u16 = 0x0324;
const META_POLYLINE: u16 = 0x0325;
const META_POLYPOLYGON_W: u16 = 0x0538;
const META_RECTANGLE: u16 = 0x041B;
const META_ELLIPSE: u16 = 0x0418;
const META_ROUNDRECT: u16 = 0x061C;
// Some producers (seen in fdo76316 / tdf119809) emit RoundRect with the `0x061D` function code; treat
// it identically. Either way the record's first two params are the corner-ellipse size and the rect
// is the LAST four (Bottom, Right, Top, Left) - unlike Rectangle/Ellipse, whose rect is the first four.
const META_ROUNDRECT_ALT: u16 = 0x061D;
const META_TEXTOUT: u16 = 0x0521;
const META_EXTTEXTOUT_W: u16 = 0x0A32;
const META_SELECTOBJECT_W: u16 = 0x012D;
const META_DELETEOBJECT_W: u16 = 0x01F0;
const META_CREATEPENINDIRECT: u16 = 0x02FA;
const META_CREATEBRUSHINDIRECT_W: u16 = 0x02FC;
const META_CREATEFONTINDIRECT: u16 = 0x02FB;

/// Render a WMF as vector art. WMF is the 16-bit GDI record format: logical coordinates map to the
/// placeable frame via the window origin/extent, objects fill the first free table slot (no explicit
/// handle), and scalar point records store Y before X. Drives the shared tiny-skia GDI engine.
fn wmf_vector(full: &[u8]) -> Option<(image::RgbaImage, Vec<MetaText>)> {
    let body = wmf_body(full)?;
    let placeable = full.len() >= 22 && full[..4] == [0xD7, 0xCD, 0xC6, 0x9A];
    let (org_x, org_y, ext_w, ext_h) = if placeable {
        let l = rd_i16(full, 6)? as f32;
        let t = rd_i16(full, 8)? as f32;
        let r = rd_i16(full, 10)? as f32;
        let bo = rd_i16(full, 12)? as f32;
        (l, t, (r - l).abs().max(1.0), (bo - t).abs().max(1.0))
    } else {
        let (w, h) = wmf_prescan_winext(body).unwrap_or((1000.0, 1000.0));
        (0.0, 0.0, w, h)
    };
    let s = (MAX_DIM / ext_w.max(ext_h)).clamp(0.05, 8.0);
    let (pw, ph) = ((ext_w * s).round().max(1.0) as u32, (ext_h * s).round().max(1.0) as u32);
    if pw > 4096 || ph > 4096 {
        return None;
    }
    let mut g = Gdi::new(Pixmap::new(pw, ph)?, Transform::identity(), 1.0);
    g.win_org = (org_x, org_y);
    g.win_ext = (ext_w, ext_h);
    g.wmf_set_base();

    let mut off = 18usize; // past the META_HEADER
    let mut guard = 0u32;
    let mut drew = false;
    while off + 6 <= body.len() {
        let size = rd_u32(body, off)? as usize; // record size in 16-bit words
        let func = rd_u16(body, off + 4)?;
        // See `decode_wmf`: both the *2 and the add are attacker-reachable on wasm32.
        let Some(end) = size.checked_mul(2).and_then(|n| off.checked_add(n)) else { break };
        if size < 3 || end > body.len() {
            break;
        }
        let p = &body[off + 6..end];
        drew |= wmf_play(&mut g, func, p);
        if func == 0 {
            break; // META_EOF
        }
        off = end;
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
    if !drew {
        return None;
    }
    gdi_finish(&mut g)
}

/// Play one WMF record onto the device. Returns whether it drew geometry / text.
fn wmf_play(g: &mut Gdi, func: u16, p: &[u8]) -> bool {
    match func {
        META_SETWINDOWORG => {
            // Y then X.
            let y = rd_i16(p, 0).unwrap_or(0) as f32;
            let x = rd_i16(p, 2).unwrap_or(0) as f32;
            g.win_org = (x, y);
            g.wmf_set_base();
        }
        META_SETWINDOWEXT => {
            // Height (Y) then Width (X).
            let h = rd_i16(p, 0).unwrap_or(0) as f32;
            let w = rd_i16(p, 2).unwrap_or(0) as f32;
            g.win_ext = (w, h);
            g.wmf_set_base();
        }
        META_SETPOLYFILLMODE_W => {
            g.fill_rule = if rd_u16(p, 0) == Some(2) { FillRule::Winding } else { FillRule::EvenOdd };
        }
        META_SETTEXTCOLOR_W => g.text_color = colorref(p, 0),
        META_SETTEXTALIGN_W => g.ta_baseline = rd_u16(p, 0).unwrap_or(0) & 24 == 24,
        META_MOVETO => {
            let y = rd_i16(p, 0).unwrap_or(0) as f32;
            let x = rd_i16(p, 2).unwrap_or(0) as f32;
            g.pos = (x, y);
        }
        META_LINETO => {
            let y = rd_i16(p, 0).unwrap_or(0) as f32;
            let x = rd_i16(p, 2).unwrap_or(0) as f32;
            if let Some(path) = path_from_points(&[g.pos, (x, y)], false) {
                g.stroke(&path);
            }
            g.pos = (x, y);
            return true;
        }
        META_POLYLINE | META_POLYGON => {
            let pts = wmf_points(p);
            // WMF has no path brackets here, so `continues` is always false (a fresh subpath).
            return draw_poly(g, &pts, func == META_POLYGON, false);
        }
        META_POLYPOLYGON_W => return wmf_polypolygon(g, p),
        META_RECTANGLE | META_ELLIPSE | META_ROUNDRECT | META_ROUNDRECT_ALT => {
            // params: Bottom, Right, Top, Left (Y/X reversed). RoundRect prefixes two corner-size
            // params, so its rect is the LAST four; Rectangle/Ellipse use the first four.
            let base = if func == META_ROUNDRECT || func == META_ROUNDRECT_ALT { 4 } else { 0 };
            let (bo, r, t, l) =
                (rd_i16(p, base), rd_i16(p, base + 2), rd_i16(p, base + 4), rd_i16(p, base + 6));
            if let (Some(bo), Some(r), Some(t), Some(l)) = (bo, r, t, l) {
                // Normalize: a producer may store left>right / top>bottom (these files do).
                let (x0, x1) = (l.min(r) as f32, l.max(r) as f32);
                let (y0, y1) = (t.min(bo) as f32, t.max(bo) as f32);
                if let Some(rr) = tiny_skia::Rect::from_ltrb(x0, y0, x1.max(x0 + 1.0), y1.max(y0 + 1.0)) {
                    let path = if func == META_ELLIPSE {
                        PathBuilder::from_oval(rr)
                    } else {
                        Some(PathBuilder::from_rect(rr))
                    };
                    if let Some(path) = path {
                        g.fill_and_stroke(&path);
                        return true;
                    }
                }
            }
        }
        META_CREATEPENINDIRECT => {
            // LogPen: Style(u16), Width(PointS: x@2, y@4), Color@6.
            let style = rd_u16(p, 0).unwrap_or(0);
            let width = rd_i16(p, 2).unwrap_or(0) as f32;
            g.add_obj(GdiObj::Pen(Pen { color: colorref(p, 6), width, visible: style & 0xff != 5 }));
        }
        META_CREATEBRUSHINDIRECT_W => {
            // LogBrush: Style(u16)@0, Color@2, Hatch(u16)@6. Style 1 = BS_NULL, 2 = BS_HATCHED.
            let style = rd_u16(p, 0).unwrap_or(0);
            let hatch = (style == 2).then(|| rd_u16(p, 6).unwrap_or(0) as u8);
            g.add_obj(GdiObj::Brush(Brush { color: colorref(p, 2), filled: style != 1, hatch }));
        }
        META_CREATEFONTINDIRECT => {
            // LogFont: Height@0, Width@2, Esc@4, Orient@6, Weight@8, Italic u8@10, FaceName@18 (32 ANSI).
            let height = rd_i16(p, 0).unwrap_or(0).unsigned_abs() as f32;
            let bold = rd_i16(p, 8).unwrap_or(0) >= 700;
            let italic = p.get(10).copied().unwrap_or(0) != 0;
            let family = ansi_string(p, 18, 32);
            g.add_obj(GdiObj::Font(MetaFont { height, bold, italic, family }));
        }
        META_SELECTOBJECT_W => {
            if let Some(i) = rd_u16(p, 0) {
                g.select(i as u32);
            }
        }
        META_DELETEOBJECT_W => {
            if let Some(i) = rd_u16(p, 0) {
                let i = i as usize;
                if i < g.objs.len() {
                    g.objs[i] = None;
                }
            }
        }
        META_TEXTOUT => return wmf_text_out(g, p),
        META_EXTTEXTOUT_W => return wmf_ext_text_out(g, p),
        _ => {}
    }
    false
}

/// Points of a WMF poly record: Count(u16) then `Count` PointS (x@,y@, each i16). Note: point ARRAYS
/// store x-then-y, unlike the scalar point records (MoveTo/LineTo) which store y-then-x.
fn wmf_points(p: &[u8]) -> Vec<(f32, f32)> {
    let n = rd_u16(p, 0).unwrap_or(0) as usize;
    let mut out = Vec::with_capacity(n.min(100_000));
    let mut o = 2;
    for _ in 0..n.min(200_000) {
        match (rd_i16(p, o), rd_i16(p, o + 2)) {
            (Some(x), Some(y)) => out.push((x as f32, y as f32)),
            _ => break,
        }
        o += 4;
    }
    out
}

/// META_POLYPOLYGON: NumberOfPolygons(u16), per-polygon point counts (u16), then all PointS.
fn wmf_polypolygon(g: &mut Gdi, p: &[u8]) -> bool {
    let npoly = rd_u16(p, 0).unwrap_or(0) as usize;
    if npoly == 0 || npoly > 100_000 {
        return false;
    }
    let mut counts = Vec::with_capacity(npoly);
    let mut o = 2;
    for _ in 0..npoly {
        counts.push(rd_u16(p, o).unwrap_or(0) as usize);
        o += 2;
    }
    let mut pb = PathBuilder::new();
    let mut drew = false;
    for c in counts {
        for i in 0..c {
            if let (Some(x), Some(y)) = (rd_i16(p, o), rd_i16(p, o + 2)) {
                if i == 0 {
                    pb.move_to(x as f32, y as f32);
                } else {
                    pb.line_to(x as f32, y as f32);
                }
            }
            o += 4;
        }
        pb.close();
        drew = true;
    }
    if let Some(path) = pb.finish() {
        g.fill_and_stroke(&path);
        return drew;
    }
    false
}

/// META_TEXTOUT: StringLength(u16), String(ANSI, padded to even), then Y, X.
fn wmf_text_out(g: &mut Gdi, p: &[u8]) -> bool {
    let len = rd_u16(p, 0).unwrap_or(0) as usize;
    if len == 0 || len > 100_000 {
        return false;
    }
    let text: String =
        p.get(2..2 + len).map(|s| s.iter().map(|&c| c as char).collect()).unwrap_or_default();
    let padded = len + (len & 1);
    let y = rd_i16(p, 2 + padded).unwrap_or(0) as f32;
    let x = rd_i16(p, 4 + padded).unwrap_or(0) as f32;
    push_meta_text(g, x, y, text)
}

/// META_EXTTEXTOUT: Y, X, StringLength, fwOpts, [Rectangle if ETO_OPAQUE/CLIPPED], String(ANSI).
fn wmf_ext_text_out(g: &mut Gdi, p: &[u8]) -> bool {
    let y = rd_i16(p, 0).unwrap_or(0) as f32;
    let x = rd_i16(p, 2).unwrap_or(0) as f32;
    let len = rd_u16(p, 4).unwrap_or(0) as usize;
    let opts = rd_u16(p, 6).unwrap_or(0);
    if len == 0 || len > 100_000 {
        return false;
    }
    let str_off = 8 + if opts & 0x0006 != 0 { 8 } else { 0 };
    let text: String = p
        .get(str_off..str_off + len)
        .map(|s| s.iter().map(|&c| c as char).collect())
        .unwrap_or_default();
    push_meta_text(g, x, y, text)
}

/// Up to `max` ANSI bytes at `at`, stopping at NUL, decoded as Latin-1. `None` if empty.
fn ansi_string(b: &[u8], at: usize, max: usize) -> Option<String> {
    let s = b.get(at..)?;
    let out: String = s.iter().take(max).take_while(|&&c| c != 0).map(|&c| c as char).collect();
    if out.is_empty() { None } else { Some(out) }
}

/// Pre-scan a non-placeable WMF for META_SETWINDOWEXT to size the canvas.
fn wmf_prescan_winext(body: &[u8]) -> Option<(f32, f32)> {
    let mut off = 18usize;
    let mut guard = 0u32;
    while off + 6 <= body.len() {
        let size = rd_u32(body, off)? as usize;
        let func = rd_u16(body, off + 4)?;
        let Some(end) = size.checked_mul(2).and_then(|n| off.checked_add(n)) else { break };
        if size < 3 || end > body.len() {
            break;
        }
        if func == META_SETWINDOWEXT {
            let p = &body[off + 6..end];
            let h = rd_i16(p, 0)? as f32;
            let w = rd_i16(p, 2)? as f32;
            return Some((w.abs().max(1.0), h.abs().max(1.0)));
        }
        if func == 0 {
            break;
        }
        off = end;
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
    None
}

// ── little-endian readers (bounds-checked) ───────────────────────────────────

fn rd_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn rd_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn rd_i16(b: &[u8], at: usize) -> Option<i16> {
    b.get(at..at + 2).map(|s| i16::from_le_bytes([s[0], s[1]]))
}
fn rd_i32(b: &[u8], at: usize) -> Option<i32> {
    b.get(at..at + 4).map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn rd_f32(b: &[u8], at: usize) -> Option<f32> {
    b.get(at..at + 4).map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packed 2x2 24-bit BI_RGB DIB (bottom-up rows, 4-byte padded): bottom row = red,green;
    /// top row = blue,white. Returns (dib bytes, pixel offset).
    fn dib_2x2() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes()); // biSize
        d.extend_from_slice(&2i32.to_le_bytes()); // biWidth
        d.extend_from_slice(&2i32.to_le_bytes()); // biHeight (bottom-up)
        d.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        d.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
        d.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
        d.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
        d.extend_from_slice(&[0u8; 16]); // ppm x/y, clrUsed, clrImportant
        // Pixels are BGR, bottom row first, each row padded to a 4-byte boundary (6 -> 8).
        d.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // bottom: red, green, pad
        d.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // top: blue, white, pad
        d
    }

    #[test]
    fn decodes_a_packed_dib() {
        let img = decode_dib(&dib_2x2(), 14 + 40).expect("decode 2x2 dib");
        assert_eq!((img.width(), img.height()), (2, 2));
        // Top-left is blue (the BMP decoder flips the bottom-up rows).
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 255, 255], "top-left blue");
        assert_eq!(img.get_pixel(1, 1).0, [0, 255, 0, 255], "bottom-right green");
    }

    #[test]
    fn decodes_an_emf_wrapping_a_dib() {
        let dib = dib_2x2();
        let mut emf = Vec::new();
        // EMR_HEADER (88 bytes): iType=1, nSize=88, then bounds/frame/signature " EMF" at byte 40.
        let mut hdr = vec![0u8; 88];
        hdr[0..4].copy_from_slice(&1u32.to_le_bytes());
        hdr[4..8].copy_from_slice(&88u32.to_le_bytes());
        hdr[40..44].copy_from_slice(b" EMF");
        emf.extend_from_slice(&hdr);
        // EMR_STRETCHDIBITS (81): header up to offBmiSrc at byte 48, then the offsets + the DIB.
        let bmi_len = 40u32; // header only (24-bit BI_RGB has no palette)
        let bits_len = (dib.len() - 40) as u32;
        let rec_size = 80u32 + dib.len() as u32; // fixed STRETCHDIBITS fields (80) + the DIB tail
        let mut rec = vec![0u8; 80];
        rec[0..4].copy_from_slice(&81u32.to_le_bytes());
        rec[4..8].copy_from_slice(&rec_size.to_le_bytes());
        rec[48..52].copy_from_slice(&80u32.to_le_bytes()); // offBmiSrc (DIB starts at byte 80)
        rec[52..56].copy_from_slice(&bmi_len.to_le_bytes());
        rec[56..60].copy_from_slice(&(80u32 + bmi_len).to_le_bytes()); // offBitsSrc
        rec[60..64].copy_from_slice(&bits_len.to_le_bytes());
        rec.extend_from_slice(&dib);
        emf.extend_from_slice(&rec);
        // EMR_EOF (14).
        let mut eof = vec![0u8; 20];
        eof[0..4].copy_from_slice(&14u32.to_le_bytes());
        eof[4..8].copy_from_slice(&20u32.to_le_bytes());
        emf.extend_from_slice(&eof);

        assert!(is_emf(&emf), "detected as EMF");
        let (img, _texts) = decode(&emf).expect("decode EMF-wrapped DIB");
        assert_eq!((img.width(), img.height()), (2, 2));
        assert_eq!(img.get_pixel(0, 0).0, [0, 0, 255, 255], "blue pixel survives the round trip");
    }

    #[test]
    fn hatched_brush_leaves_gaps() {
        // A cross-hatch fill should ink the shape but leave transparent gaps - not a solid block.
        let pm = Pixmap::new(40, 40).unwrap();
        let mut g = Gdi::new(pm, Transform::identity(), 1.0);
        g.brush = Brush { color: Color::BLACK, filled: true, hatch: Some(4) }; // HS_CROSS
        let rect = tiny_skia::Rect::from_ltrb(2.0, 2.0, 38.0, 38.0).unwrap();
        g.fill(&PathBuilder::from_rect(rect));
        let data = g.pm.data();
        let any_ink = data.as_chunks::<4>().0.iter().filter(|px| px[3] > 0).count();
        assert!(any_ink > 0, "hatch laid down ink");
        // The interior (away from the box edges) should stay mostly transparent - a solid fill would
        // ink nearly all of these cells; a 6px hatch grid leaves the spaces between lines clear.
        let mut clear_interior = 0u32;
        for y in 4..36 {
            for x in 4..36 {
                if data[(y * 40 + x) * 4 + 3] == 0 {
                    clear_interior += 1;
                }
            }
        }
        // A solid fill would leave ~0 of these 1024 cells clear; a hatch leaves a large fraction.
        assert!(clear_interior > 300, "hatch leaves the interior largely clear (got {clear_interior})");
    }

    #[test]
    fn bezier_strokes_a_curve() {
        let pm = Pixmap::new(40, 40).unwrap();
        let mut g = Gdi::new(pm, Transform::identity(), 1.0);
        g.pen = Pen { color: Color::BLACK, width: 1.0, visible: true };
        // start, then one (ctrl, ctrl, end) triple.
        let pts = [(2.0, 20.0), (12.0, 2.0), (28.0, 38.0), (38.0, 20.0)];
        assert!(draw_bezier(&mut g, &pts, false), "bezier reported drawing");
        let inked = g.pm.data().as_chunks::<4>().0.iter().filter(|px| px[3] > 0).count();
        assert!(inked > 0, "bezier curve put ink on the canvas");
        assert_eq!(g.pos, (38.0, 20.0), "current position advanced to the final end point");
    }

    #[test]
    fn path_bracket_fills_a_polygon() {
        // BeginPath..(geometry)..CloseFigure..EndPath..FillPath should fill the accumulated shape -
        // the mechanism that renders filled curved EMF art (the records draw nothing until FillPath).
        let pm = Pixmap::new(40, 40).unwrap();
        let mut g = Gdi::new(pm, Transform::identity(), 1.0);
        g.brush = Brush { color: Color::BLACK, filled: true, hatch: None };
        g.begin_path();
        g.pb_move(5.0, 5.0);
        g.pb_line(35.0, 5.0);
        g.pb_line(35.0, 35.0);
        g.pb_line(5.0, 35.0);
        g.close_figure();
        let path = g.finish_path().expect("accumulated path");
        assert!(!g.path_rec, "finish_path ended recording");
        g.fill(&path);
        let data = g.pm.data();
        let center = (20 * 40 + 20) * 4;
        assert!(data[center + 3] > 200, "path interior filled (alpha {})", data[center + 3]);
    }

    /// `0x061D` is a RoundRect variant (seen in fdo76316 / tdf119809): two corner-size params then
    /// Bottom, Right, Top, Left - and the producer may store left>right, so the rect must normalize.
    /// Before the fix these files drew nothing (the only other record was empty text) and rendered
    /// blank.
    #[test]
    fn roundrect_0x061d_variant_fills_the_normalized_rect() {
        let pm = Pixmap::new(100, 30).unwrap();
        let mut g = Gdi::new(pm, Transform::identity(), 1.0);
        g.brush = Brush { color: Color::BLACK, filled: true, hatch: None };
        // params (i16 LE): cornerH=8, cornerW=8, Bottom=24, Right=1, Top=0, Left=95 (left>right).
        let mut p = Vec::new();
        for v in [8i16, 8, 24, 1, 0, 95] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        assert!(wmf_play(&mut g, 0x061D, &p), "0x061D drew (recognised as RoundRect)");
        let data = g.pm.data();
        let idx = (12 * 100 + 50) * 4; // (50,12): inside the normalized 1..95 x 0..24 box
        assert!(data[idx + 3] > 200, "roundrect interior filled (alpha {})", data[idx + 3]);
    }

    #[test]
    fn emf_frame_to_device_pixels() {
        // rclFrame 16510 x 898 (0.01mm) at 4816px/204mm => ~3898 x ~212 device px.
        let mut h = vec![0u8; 88];
        h[32..36].copy_from_slice(&16510i32.to_le_bytes()); // frame right
        h[36..40].copy_from_slice(&898i32.to_le_bytes()); // frame bottom
        h[72..76].copy_from_slice(&4816i32.to_le_bytes()); // szlDevice cx
        h[76..80].copy_from_slice(&6400i32.to_le_bytes()); // szlDevice cy
        h[80..84].copy_from_slice(&204i32.to_le_bytes()); // szlMillimeters cx
        h[84..88].copy_from_slice(&271i32.to_le_bytes()); // szlMillimeters cy
        let (fl, ft, fw, fh) = emf_frame_device(&h).expect("frame device");
        assert_eq!((fl, ft), (0.0, 0.0));
        assert!((fw - 3897.6).abs() < 2.0, "frame width ~3898 px, got {fw}");
        assert!((fh - 212.0).abs() < 2.0, "frame height ~212 px, got {fh}");
        // A zeroed resolution field is unusable => None (caller keeps bounds-based sizing).
        let mut bad = h.clone();
        bad[80..84].copy_from_slice(&0i32.to_le_bytes());
        assert!(emf_frame_device(&bad).is_none());
    }

    #[test]
    fn detects_wmf_and_rejects_junk() {
        // A standard (non-placeable) WMF header: type=1, headerSize=9 words.
        let mut wmf = vec![0u8; 18];
        wmf[0..2].copy_from_slice(&1u16.to_le_bytes());
        wmf[2..4].copy_from_slice(&9u16.to_le_bytes());
        assert!(wmf_body(&wmf).is_some(), "valid WMF header");
        assert!(is_metafile(&wmf));
        assert!(!is_metafile(b"not a metafile at all, just text"));
        assert!(decode(b"\x89PNG\r\n\x1a\n....").is_none(), "a PNG is not a metafile");
    }

    /// A record whose declared size is `u32::MAX` must be rejected, not sliced. The record walks
    /// bound-check `off + size` against the buffer, and on wasm32 (32-bit `usize`) that addition
    /// WRAPS for a size this large: it lands below `b.len()`, passes the check, and the following
    /// `&b[off..off + size]` panics, which aborts the wasm instance and takes the editor down.
    /// The walks use `checked_add`/`checked_mul` so the record is dropped instead. On 64-bit the
    /// same input exercises the ordinary out-of-range path.
    #[test]
    fn hostile_record_sizes_are_rejected_not_sliced() {
        // EMF: valid EMR_HEADER (iType=1, " EMF" at 40), then a record claiming u32::MAX bytes.
        let mut emf = vec![0u8; 44];
        emf[0..4].copy_from_slice(&1u32.to_le_bytes());
        emf[4..8].copy_from_slice(&44u32.to_le_bytes());
        emf[40..44].copy_from_slice(b" EMF");
        emf.extend_from_slice(&EMR_STRETCHDIBITS.to_le_bytes());
        emf.extend_from_slice(&u32::MAX.to_le_bytes());
        emf.resize(emf.len() + 64, 0);
        assert!(is_emf(&emf), "the header itself is well-formed");
        assert!(decode_emf(&emf).is_none(), "hostile EMF size must not panic");
        let _ = decode(&emf); // the vector path walks the same records

        // WMF: valid META_HEADER, then a record whose word count overflows when doubled.
        let mut wmf = vec![0u8; 18];
        wmf[0..2].copy_from_slice(&1u16.to_le_bytes());
        wmf[2..4].copy_from_slice(&9u16.to_le_bytes());
        wmf.extend_from_slice(&u32::MAX.to_le_bytes()); // size in 16-bit words
        wmf.extend_from_slice(&META_STRETCHDIB.to_le_bytes());
        wmf.resize(wmf.len() + 64, 0);
        let body = wmf_body(&wmf).expect("valid WMF header");
        assert!(decode_wmf(body).is_none(), "hostile WMF size must not panic");
        assert!(wmf_prescan_winext(body).is_none(), "prescan must not panic either");
        let _ = decode(&wmf);
    }

    /// The DIB extractor reads four raw u32 offsets/lengths from a record and sums them pairwise.
    /// `u32::MAX` pairs must be refused rather than wrapping into an in-range slice on wasm32.
    #[test]
    fn hostile_dib_field_offsets_are_rejected() {
        let mut rec = vec![0u8; 128];
        // p_bmi = 48 for EMR_STRETCHDIBITS: offBmiSrc, cbBmiSrc, then offBitsSrc, cbBitsSrc.
        rec[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
        rec[52..56].copy_from_slice(&u32::MAX.to_le_bytes());
        rec[56..60].copy_from_slice(&u32::MAX.to_le_bytes());
        rec[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(emf_extract_dib(&rec, 48, 56).is_none(), "must not wrap into a valid range");
    }
}
