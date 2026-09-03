//! Section properties: page geometry, columns, and header/footer references.
//! 
//! Reads `w:sectPr` out of a document body - the page size and margins every layout
//! pass needs, the per-section column count, the verbatim `sectPr` blobs the exporter
//! re-emits, and which header/footer parts each section references.

use super::*;

// ── page geometry (w:sectPr) ─────────────────────────────────────────────────

/// Page size + margins from the document section properties (`w:sectPr`), in **twips** (1/20 pt) as
/// OOXML stores them. Defaults to A4 with 1-inch margins when the document omits them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageGeometry {
    pub width: u32,
    pub height: u32,
    // Margins are SIGNED twips: Word allows negative page margins (content bleeds into / past the
    // physical margin, enlarging the usable area). A u32 would fail to parse "-1440" and silently fall
    // back to the 1-inch default, shrinking the page and over-paginating (tdf105490, tdf143384).
    pub margin_top: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    /// Distance from the page top edge to the header, and from the bottom edge to the footer (twips).
    pub header_dist: u32,
    pub footer_dist: u32,
    /// Whether ANY section declares more than one newspaper column (`w:cols w:num >= 2`). We don't lay
    /// out columns, but this gates whether a manual column break is treated as a page break: only when
    /// the whole document is single-column (then a column break has no next column = a page break).
    pub multi_column: bool,
}

impl Default for PageGeometry {
    fn default() -> Self {
        // A4 (210x297mm) with 1-inch (1440 twip) margins; 0.5" header/footer distance.
        Self {
            width: 11906,
            height: 16838,
            margin_top: 1440,
            margin_bottom: 1440,
            margin_left: 1440,
            margin_right: 1440,
            header_dist: 720,
            footer_dist: 720,
            multi_column: false,
        }
    }
}

/// A section's newspaper-column geometry (`w:sectPr/w:cols`). One per `<w:sectPr>` in document order
/// (each in-paragraph `sectPr` terminates a section; the body-final `sectPr` is the last section), so
/// the Nth entry is the Nth section. Layout flows a multi-column section's content through `count`
/// columns before advancing the page. See [`parse_sections`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCols {
    /// Newspaper column count (`w:num`); 1 = single column (the default).
    pub count: u16,
    /// Default gap between columns in twips (`w:space`); used when columns are equal-width.
    pub space: u32,
    /// `w:equalWidth` (default true). When false, `widths` holds each column's explicit geometry.
    pub equal_width: bool,
    /// Per-column `(width, trailing-space)` in twips from `<w:col w:w=.. w:space=..>`, when not
    /// equal-width. Empty for the equal-width case (use `count` + `space`).
    pub widths: Vec<(u32, u32)>,
}

impl Default for SectionCols {
    fn default() -> Self {
        Self { count: 1, space: 720, equal_width: true, widths: Vec::new() }
    }
}

/// Read page size (`w:pgSz`) + margins (`w:pgMar`) from `word/document.xml`'s section properties.
/// Missing values keep their [`PageGeometry::default`] (A4 / 1-inch).
pub fn parse_page_geometry(xml: &[u8]) -> PageGeometry {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut g = PageGeometry::default();
    let (mut got_sz, mut got_mar) = (false, false);

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Empty(e) | Event::Start(e) => match e.name().as_ref() {
                b"w:pgSz" => {
                    if let Some(v) = attr(&e, b"w:w").and_then(|s| s.parse().ok()) {
                        g.width = v;
                    }
                    if let Some(v) = attr(&e, b"w:h").and_then(|s| s.parse().ok()) {
                        g.height = v;
                    }
                    got_sz = true;
                }
                b"w:pgMar" => {
                    if let Some(v) = attr(&e, b"w:top").and_then(|s| s.parse().ok()) {
                        g.margin_top = v;
                    }
                    if let Some(v) = attr(&e, b"w:bottom").and_then(|s| s.parse().ok()) {
                        g.margin_bottom = v;
                    }
                    if let Some(v) = attr(&e, b"w:left").and_then(|s| s.parse().ok()) {
                        g.margin_left = v;
                    }
                    if let Some(v) = attr(&e, b"w:right").and_then(|s| s.parse().ok()) {
                        g.margin_right = v;
                    }
                    if let Some(v) = attr(&e, b"w:header").and_then(|s| s.parse().ok()) {
                        g.header_dist = v;
                    }
                    if let Some(v) = attr(&e, b"w:footer").and_then(|s| s.parse().ok()) {
                        g.footer_dist = v;
                    }
                    got_mar = true;
                }
                _ => {}
            },
            _ => {}
        }
        if got_sz && got_mar {
            break;
        }
        buf.clear();
    }

    // Separate pass for multi-column detection: any section with `w:cols w:num >= 2`. Done apart from
    // the geometry loop (which stops at the first section) since columns can appear in any section.
    let mut cols_reader = quick_xml::Reader::from_reader(xml);
    let mut cols_buf = Vec::new();
    while let Ok(ev) = cols_reader.read_event_into(&mut cols_buf) {
        match ev {
            Event::Eof => break,
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w:cols"
                && attr(&e, b"w:num").and_then(|s| s.parse::<u32>().ok()).is_some_and(|n| n >= 2) => {
                    g.multi_column = true;
                    break;
                }
            _ => {}
        }
        cols_buf.clear();
    }

    g
}

/// Parse every section's newspaper-column geometry (`w:sectPr/w:cols`) from `document.xml`, in
/// document order: each in-paragraph `sectPr` plus the body-final one, so entry N is section N. This
/// aligns with the `section_terminator` paragraphs the layout walks (there are N-1 of them for N
/// sections; the last section has no terminator). A `w:sectPrChange` (tracked section change) holds an
/// OLD nested `sectPr` we must NOT count - a depth guard skips everything inside it. A section with no
/// `w:cols` (or `w:num<=1`) stays single-column ([`SectionCols::default`]).
pub fn parse_sections(xml: &[u8]) -> Vec<SectionCols> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out: Vec<SectionCols> = Vec::new();
    let mut in_sectpr = 0u32; // depth inside a real (non-change) w:sectPr
    let mut in_change = 0u32; // depth inside w:sectPrChange - skip its nested sectPr
    let mut cur = SectionCols::default();

    let read_cols = |e: &quick_xml::events::BytesStart, c: &mut SectionCols| {
        if let Some(n) = attr(e, b"w:num").and_then(|s| s.parse().ok()) {
            c.count = n;
        }
        if let Some(s) = attr(e, b"w:space").and_then(|s| s.parse().ok()) {
            c.space = s;
        }
        // `w:equalWidth` defaults to true; only "0"/"false" turn it off.
        c.equal_width = attr(e, b"w:equalWidth").is_none_or(|v| v != "0" && v != "false");
    };

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                b"w:sectPrChange" => in_change += 1,
                b"w:sectPr" if in_change == 0 => {
                    in_sectpr += 1;
                    cur = SectionCols::default();
                }
                b"w:cols" if in_sectpr > 0 && in_change == 0 => read_cols(&e, &mut cur),
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"w:cols" if in_sectpr > 0 && in_change == 0 => read_cols(&e, &mut cur),
                b"w:col" if in_sectpr > 0 && in_change == 0 => {
                    let w = attr(&e, b"w:w").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let sp = attr(&e, b"w:space").and_then(|s| s.parse().ok()).unwrap_or(0);
                    cur.widths.push((w, sp));
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"w:sectPrChange" if in_change > 0 => in_change -= 1,
                b"w:sectPr" if in_sectpr > 0 && in_change == 0 => {
                    in_sectpr -= 1;
                    out.push(std::mem::take(&mut cur));
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Byte-slice every real `<w:sectPr>…</w:sectPr>` element **verbatim** (attributes + children) from
/// `document.xml`, in document order: each in-paragraph section break followed by the body-final one,
/// so entry N is the Nth section's properties. A `w:sectPrChange` (tracked section change) and a
/// `w:pPrChange` (tracked paragraph-property change) each hold an OLD nested `sectPr` that must NOT be
/// captured - a depth guard skips everything inside them. This preserves per-section page geometry /
/// columns / header-footer refs / page borders / line numbering that the single synthesized final
/// `sectPr` collapsed. Slicing is against the ORIGINAL bytes, so the BOM is stripped first (quick-xml
/// skips a BOM without counting it in `buffer_position` - the same shift that truncated passthrough
/// captures, see [`strip_bom`]).
pub fn parse_section_props(xml: &[u8]) -> Vec<String> {
    use quick_xml::events::Event;
    let xml = strip_bom(xml);
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut skip = 0u32; // depth inside a w:sectPrChange / w:pPrChange - its nested sectPr is OLD
    let mut start: Option<usize> = None; // byte offset of the open `<w:sectPr>` we are capturing
    loop {
        let before = reader.buffer_position() as usize;
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                b"w:sectPrChange" | b"w:pPrChange" => skip += 1,
                b"w:sectPr" if skip == 0 && start.is_none() => start = Some(before),
                _ => {}
            },
            // A self-closing `<w:sectPr/>` (an empty section) is one whole element.
            Event::Empty(e) if e.name().as_ref() == b"w:sectPr" && skip == 0 => {
                if let Ok(s) = std::str::from_utf8(&xml[before..reader.buffer_position() as usize]) {
                    out.push(s.to_string());
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:sectPrChange" | b"w:pPrChange" if skip > 0 => skip -= 1,
                b"w:sectPr" if skip == 0 => {
                    if let Some(s) = start.take()
                        && let Ok(slice) = std::str::from_utf8(&xml[s..reader.buffer_position() as usize])
                    {
                        out.push(slice.to_string());
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

// ── headers / footers (w:sectPr references -> header/footer parts) ────────────

/// A reference from the section properties to a header / footer part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfRef {
    pub is_header: bool,
    /// `default` | `first` | `even`.
    pub kind: String,
    /// The relationship id (resolve via `word/_rels/document.xml.rels`).
    pub r_id: String,
}

/// Read `w:headerReference` / `w:footerReference` elements from `word/document.xml`.
pub fn header_footer_refs(document_xml: &[u8]) -> Vec<HfRef> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(document_xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Empty(e) | Event::Start(e) => {
                let is_header = e.name().as_ref() == b"w:headerReference";
                let is_footer = e.name().as_ref() == b"w:footerReference";
                if is_header || is_footer {
                    let kind = attr(&e, b"w:type").unwrap_or_else(|| "default".into());
                    if let Some(r_id) = attr(&e, b"r:id") {
                        out.push(HfRef { is_header, kind, r_id });
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Whether the section has `<w:titlePg/>` - Word's "Different First Page", so page 1 uses the
/// `first` header/footer variant instead of the `default` one.
pub fn title_page(document_xml: &[u8]) -> bool {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(document_xml);
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w:titlePg" => return true,
            _ => {}
        }
        buf.clear();
    }
    false
}
