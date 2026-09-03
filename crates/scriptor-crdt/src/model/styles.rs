//! `styles.xml`: definitions, resolution and round-trip.
//! 
//! Resolves the ECMA-376 inheritance chain (document defaults, then table style, then
//! numbering, then paragraph style, then direct formatting) and carries Word's
//! built-in gallery for documents that omit it. Export patches an imported
//! `styles.xml` in place rather than regenerating it, so properties this model does
//! not understand survive a round-trip.

use super::*;

// ── styles.xml (paragraph/character style resolution) ─────────────────────────

/// Formatting a style (or the document default) contributes. Run-level fields are half-points /
/// hex like [`Run`]; paragraph spacing is in twips (1/20 pt) as OOXML stores it. `None` = unset
/// (inherit). Word puts heading / title sizing here, not inline on the runs, so rendering the
/// document hierarchy needs this table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyleProps {
    pub size: Option<u16>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub color: Option<String>,
    /// Highlight colour name (`w:highlight w:val`, e.g. "cyan") from the style's rPr. A run without
    /// its own highlight inherits it - Word's paragraph/character styles can carry a highlight.
    pub highlight: Option<String>,
    /// Font family from the style's / docDefaults' `w:rFonts` (ascii). Drives metric-compatible
    /// substitution so a run without its own `rFonts` still uses the document's font.
    pub font: Option<String>,
    /// Line spacing value (`w:spacing w:line`); a 240ths multiplier with `line_rule` auto, else twips.
    pub line_spacing: Option<u16>,
    /// Line-spacing rule (`None` = auto). Resolved through `basedOn` alongside `line_spacing`.
    pub line_rule: Option<LineRule>,
    pub space_before: Option<u32>,
    pub space_after: Option<u32>,
    /// List membership from the style's `w:pPr/w:numPr` (`w:numId` + `w:ilvl`). Word's outline
    /// headings (Heading1/Rubrik1 -> a list at level 0, Heading2 -> level 1, ...) carry their
    /// numbering here, not on the paragraph - so a heading's "1." / "1.1" marker resolves through the
    /// style chain. `num_id` inherits via `basedOn` (Heading2 sets only the level, keeps Heading1's id).
    pub num_id: Option<i32>,
    pub num_ilvl: Option<i32>,
    /// Paragraph border box from the style's `w:pPr/w:pBdr`, in [`ParaProps::border`]'s compact
    /// `edge=val,sz,space,color` form. Word's built-in `Header` style draws its horizontal rule
    /// through exactly this - a bottom border on the style, not the paragraph.
    pub border: Option<String>,
    /// Indents from the style's `w:pPr/w:ind` (twips, signed - Word's Header styles use NEGATIVE
    /// indents to widen the header box, and its rule, into both page margins).
    pub indent_left: Option<i32>,
    pub indent_right: Option<i32>,
    /// Tab stops from the style's `w:pPr/w:tabs` (twips; `tab_kinds` parallel, as in
    /// [`ParaProps::tab_stops`]). Word's Header styles right-align their page number by tabbing to
    /// a style-defined right stop - the paragraph itself carries only the tab characters.
    pub tab_stops: Vec<u32>,
    pub tab_kinds: Vec<u8>,
    /// Positions this style CLEARS from inherited stops (`w:tab w:val="clear"`) - consumed by
    /// [`StyleProps::overlay`] so a derived style can retract its parent's stops (Word's
    /// landscape header clears the portrait stops and adds its own further right).
    pub tab_clears: Vec<u32>,
    /// `w:keepNext` from the style's `w:pPr` - Word's heading styles set this so a heading stays on
    /// the same page as the body that follows it. Resolved through the `basedOn` chain like the rest.
    pub keep_next: Option<bool>,
    /// `w:contextualSpacing` from the style - suppress space between adjacent same-style paragraphs
    /// (list / body styles set it). Resolved through `basedOn`.
    pub contextual_spacing: Option<bool>,
    /// Paragraph alignment (`w:jc`) from the style - Title/Heading styles are often centred, Quote
    /// right-aligned. A paragraph with no direct `w:jc` inherits the style's (resolved through
    /// `basedOn`); without this a style-centred paragraph rendered left.
    pub align: Option<Align>,
    /// `w:pageBreakBefore` from the style - a "page break before" style (e.g. a `NewPageBreak` /
    /// `Title` style) forces every paragraph using it onto a new page. A paragraph with no direct
    /// `w:pageBreakBefore` inherits the style's (resolved through `basedOn`); without this a
    /// style-break paragraph never broke (tdf89377: 1 page vs Word's 3).
    pub page_break_before: Option<bool>,
}

impl StyleProps {
    /// Overlay `other`'s set fields onto `self` (other wins where present).
    fn overlay(&mut self, other: &StyleProps) {
        if other.size.is_some() {
            self.size = other.size;
        }
        if other.bold.is_some() {
            self.bold = other.bold;
        }
        if other.italic.is_some() {
            self.italic = other.italic;
        }
        if other.color.is_some() {
            self.color = other.color.clone();
        }
        if other.highlight.is_some() {
            self.highlight = other.highlight.clone();
        }
        if other.font.is_some() {
            self.font = other.font.clone();
        }
        if other.line_spacing.is_some() {
            self.line_spacing = other.line_spacing;
            self.line_rule = other.line_rule; // the rule travels with its value
        }
        if other.space_before.is_some() {
            self.space_before = other.space_before;
        }
        if other.space_after.is_some() {
            self.space_after = other.space_after;
        }
        if other.border.is_some() {
            self.border = other.border.clone();
        }
        if other.indent_left.is_some() {
            self.indent_left = other.indent_left;
        }
        if other.indent_right.is_some() {
            self.indent_right = other.indent_right;
        }
        // Tab lists MERGE through the chain (Word semantics): the derived style's clears remove
        // inherited positions, its stops insert (replacing a same-position stop's kind).
        if !other.tab_clears.is_empty() || !other.tab_stops.is_empty() {
            while self.tab_kinds.len() < self.tab_stops.len() {
                self.tab_kinds.push(0);
            }
            for c in &other.tab_clears {
                if let Some(i) = self.tab_stops.iter().position(|p| p == c) {
                    self.tab_stops.remove(i);
                    self.tab_kinds.remove(i);
                }
            }
            for (i, pos) in other.tab_stops.iter().enumerate() {
                let kind = other.tab_kinds.get(i).copied().unwrap_or(0);
                match self.tab_stops.iter().position(|p| p >= pos) {
                    Some(j) if self.tab_stops[j] == *pos => self.tab_kinds[j] = kind,
                    Some(j) => {
                        self.tab_stops.insert(j, *pos);
                        self.tab_kinds.insert(j, kind);
                    }
                    None => {
                        self.tab_stops.push(*pos);
                        self.tab_kinds.push(kind);
                    }
                }
            }
        }
        // Numbering inherits field-by-field: a derived style (Heading2) that sets only the level keeps
        // its ancestor's (Heading1's) num_id.
        if other.num_id.is_some() {
            self.num_id = other.num_id;
        }
        if other.num_ilvl.is_some() {
            self.num_ilvl = other.num_ilvl;
        }
        if other.keep_next.is_some() {
            self.keep_next = other.keep_next;
        }
        if other.contextual_spacing.is_some() {
            self.contextual_spacing = other.contextual_spacing;
        }
        if other.align.is_some() {
            self.align = other.align;
        }
        if other.page_break_before.is_some() {
            self.page_break_before = other.page_break_before;
        }
    }
}

/// Resolved style information from `word/styles.xml`: the document defaults plus each named style's
/// own properties + each style's `w:basedOn` parent so inheritance chains resolve (e.g.
/// TableParagraph -> Normal -> docDefaults).
#[derive(Debug, Clone, Default)]
pub struct StyleTable {
    pub default: StyleProps,
    /// The id of the document's DEFAULT paragraph style (`w:style w:type="paragraph" w:default="1"`,
    /// conventionally "Normal"). A paragraph with no `w:pStyle` inherits this style's properties over
    /// docDefaults - so when "Normal" overrides docDefaults (e.g. `after=0 line=240` vs docDefaults'
    /// `after=200 line=276`), a bare body paragraph must follow Normal, not docDefaults. `None` if the
    /// styles part declares no default paragraph style.
    pub default_para_style: Option<String>,
    pub by_id: std::collections::HashMap<String, StyleProps>,
    /// `w:basedOn` parent of each style id (for inheritance resolution).
    pub based_on: std::collections::HashMap<String, String>,
    /// Each table style's own `w:tblPr/w:tblBorders` (the style's *base* borders, not its conditional
    /// `w:tblStylePr` bands). Lets a table that sets no direct `w:tblBorders` inherit them from its
    /// `w:tblStyle` - e.g. the ubiquitous `TableGrid`, whose grid lines live only in the style.
    /// Resolved through `basedOn` like the rest. See [`StyleTable::resolve_table_borders`].
    pub table_borders: std::collections::HashMap<String, EdgeBorders>,
    /// Each table style's own `w:tblPr/w:tblCellMar` (base, not conditional) - per side. This is
    /// how the default table style's padding (TableNormal: 108 twips left/right) reaches a table
    /// whose direct `w:tblCellMar` sets only some sides. See
    /// [`StyleTable::resolve_table_cell_margins`].
    pub table_cell_margins: std::collections::HashMap<String, CellMargins>,
    /// Each table style's first-row conditional shading (`w:tblStylePr w:type="firstRow"` >
    /// `w:tcPr/w:shd`), reduced to an effective RGB hex (pattern shades like `pct10` blended over
    /// the fill). Applied to row 0 when the table's `w:tblLook` enables first-row formatting.
    pub table_first_row_shd: std::collections::HashMap<String, String>,
    /// Human-facing name (`w:name w:val`) of each style id, e.g. `Heading1` -> "Heading 1".
    pub names: std::collections::HashMap<String, String>,
    /// Paragraph style ids the Styles gallery should offer, in document order: those flagged
    /// `w:qFormat` (Word's "quick styles") of `w:type="paragraph"` - Title / Subtitle / Heading N /
    /// Normal / Quote etc. - so the dropdown is the curated set, not every internal style.
    pub gallery: Vec<String>,
}

impl StyleTable {
    /// The `basedOn` chain for `style`, most-derived first (the style itself, then its parent, ...).
    /// The returned ids borrow from `self` (the `by_id` keys), not from the `style` argument.
    fn chain(&self, style: Option<&str>) -> Vec<&str> {
        let mut chain: Vec<&str> = Vec::new();
        let mut cur = style;
        let mut guard = 0;
        while let Some(id) = cur {
            // Take the key ref from `self` so the chain outlives the (possibly borrowed) argument.
            let Some((key, _)) = self.by_id.get_key_value(id) else { break };
            if chain.contains(&key.as_str()) {
                break;
            }
            chain.push(key.as_str());
            cur = self.based_on.get(key.as_str()).map(|s| s.as_str());
            guard += 1;
            if guard > 32 {
                break;
            }
        }
        chain
    }

    /// Effective base properties for a paragraph carrying `style`: document defaults, then each
    /// ancestor from the root of the `basedOn` chain down to `style` (most-derived wins). Inline run
    /// formatting (on [`Run`]) overrides this at render time.
    pub fn resolve(&self, style: Option<&str>) -> StyleProps {
        // A paragraph with no explicit style still inherits the DEFAULT paragraph style ("Normal"),
        // not bare docDefaults - so a Normal that overrides docDefaults (single spacing, no
        // space-after) is honoured instead of leaking docDefaults' 1.15-line / 200-twip body spacing
        // (which over-paginated such docs, e.g. FDO76248). No-op when Normal sets nothing (the common
        // case): its empty overlay leaves docDefaults unchanged.
        let style = style.or(self.default_para_style.as_deref());
        let mut p = self.default.clone();
        for id in self.chain(style).iter().rev() {
            if let Some(s) = self.by_id.get(*id) {
                p.overlay(s);
            }
        }
        p
    }

    /// Like [`resolve`](Self::resolve) but for a paragraph inside a table cell: the table style's
    /// paragraph properties sit between document defaults and the paragraph style, matching Word's
    /// inheritance (docDefaults < table style < paragraph style < direct). This is what makes a
    /// `TableGrid`-styled table's cells single-spaced with no space-after (its `pPr` sets
    /// `after=0 line=240`), instead of inheriting docDefaults' body spacing - which otherwise inflates
    /// every row and over-paginates dense tables.
    pub fn resolve_in_table(&self, style: Option<&str>, table_style: Option<&str>) -> StyleProps {
        // An unstyled cell paragraph still carries the DEFAULT paragraph style ("Normal"), and per
        // the ECMA-376 hierarchy paragraph styles sit ABOVE table styles - so a Normal that sets
        // `after=120` overrides a table style's `after=240` (Word confirms: 6pt gaps in such
        // cells). Without this substitution the table style silently won for every unstyled cell
        // paragraph, inflating dense legal tables row by row.
        let style = style.or(self.default_para_style.as_deref());
        let mut p = self.default.clone();
        for id in self.chain(table_style).iter().rev() {
            if let Some(s) = self.by_id.get(*id) {
                p.overlay(s);
            }
        }
        for id in self.chain(style).iter().rev() {
            if let Some(s) = self.by_id.get(*id) {
                p.overlay(s);
            }
        }
        p
    }

    /// The table borders contributed by `style` (its `w:tblPr/w:tblBorders`) resolved through the
    /// `basedOn` chain, most-derived edge winning. Empty when the chain defines none. This is the
    /// fallback a table uses for any edge it doesn't set directly - so a `TableGrid`-styled table
    /// with no direct `w:tblBorders` still draws its grid.
    pub fn resolve_table_borders(&self, style: Option<&str>) -> EdgeBorders {
        let mut e = EdgeBorders::default();
        for id in self.chain(style).iter().rev() {
            if let Some(b) = self.table_borders.get(*id) {
                overlay_edges(&mut e, b);
            }
        }
        e
    }

    /// The cell margins contributed by `style` (its `w:tblPr/w:tblCellMar`) resolved through the
    /// `basedOn` chain, most-derived side winning. A table falls back here for any side its direct
    /// `w:tblCellMar` doesn't set - which is how TableNormal's 108-twip left/right padding reaches
    /// every styled table.
    pub fn resolve_table_cell_margins(&self, style: Option<&str>) -> CellMargins {
        let mut m = CellMargins::default();
        for id in self.chain(style).iter().rev() {
            if let Some(s) = self.table_cell_margins.get(*id) {
                m.top = s.top.or(m.top);
                m.left = s.left.or(m.left);
                m.bottom = s.bottom.or(m.bottom);
                m.right = s.right.or(m.right);
            }
        }
        m
    }

    /// The first-row conditional shading (effective RGB hex) of `style`, through the `basedOn`
    /// chain (most-derived wins). `None` when no style in the chain shades its header band.
    pub fn resolve_table_first_row_shd(&self, style: Option<&str>) -> Option<String> {
        for id in self.chain(style) {
            if let Some(s) = self.table_first_row_shd.get(id) {
                return Some(s.clone());
            }
        }
        None
    }

    /// Fold runtime style-definition edits (the [`STYLE_OVERRIDES`] map, read via
    /// [`read_style_overrides`]) into this table: each override's *set* fields win over the parsed
    /// base for that style id, the rest untouched (so `basedOn` inheritance still applies on resolve).
    /// Apply to a FRESH clone of the parsed base each reconcile (not in place repeatedly), so removing
    /// an override - e.g. undo - correctly reverts to the base rather than leaving a field baked in.
    /// An override on an id absent from the base seeds a new (partial) style entry.
    pub fn apply_overrides(&mut self, overrides: &std::collections::HashMap<String, StyleProps>) {
        for (id, ov) in overrides {
            self.by_id.entry(id.clone()).or_default().overlay(ov);
        }
    }

    /// Fold runtime-added paragraph styles (the [`STYLE_ADDED`] map) into this table: register each
    /// one's name + `basedOn`, ensure a `by_id` entry exists so it resolves (its formatting came in via
    /// [`apply_overrides`]), and surface it in the gallery so the Styles list offers it. Apply AFTER
    /// `apply_overrides` (which seeds `by_id`). Idempotent.
    pub fn apply_added_styles(&mut self, added: &std::collections::HashMap<String, AddedStyle>) {
        for (id, a) in added {
            self.by_id.entry(id.clone()).or_default();
            self.names.insert(id.clone(), a.name.clone());
            match &a.based_on {
                Some(b) => {
                    self.based_on.insert(id.clone(), b.clone());
                }
                None => {
                    self.based_on.remove(id);
                }
            }
            if !self.gallery.iter().any(|g| g == id) {
                self.gallery.push(id.clone());
            }
        }
    }
}

/// Overlay `other`'s present edges onto `into` (other wins where it sets an edge). Used to fold a
/// style's table borders down its `basedOn` chain.
fn overlay_edges(into: &mut EdgeBorders, other: &EdgeBorders) {
    if other.top.is_some() {
        into.top = other.top.clone();
    }
    if other.left.is_some() {
        into.left = other.left.clone();
    }
    if other.bottom.is_some() {
        into.bottom = other.bottom.clone();
    }
    if other.right.is_some() {
        into.right = other.right.clone();
    }
    if other.inside_h.is_some() {
        into.inside_h = other.inside_h.clone();
    }
    if other.inside_v.is_some() {
        into.inside_v = other.inside_v.clone();
    }
}

/// The heading level (1-9) of a paragraph style id (`Heading1`..`Heading9`, case-insensitive on the
/// `heading` prefix), or `None` for a non-heading style. Used to collect a document's headings for a
/// table of contents.
pub fn heading_level(style: &str) -> Option<u8> {
    let n = style.strip_prefix("Heading").or_else(|| style.strip_prefix("heading"))?;
    match n.parse::<u8>() {
        Ok(l) if (1..=9).contains(&l) => Some(l),
        _ => None,
    }
}

/// Parse `word/settings.xml` for the paragraph-spacing compatibility mode. Returns `true` when the
/// document opts into LEGACY spacing - adjacent space-after + space-before SUM instead of Word's
/// modern max-consolidation ("HTML paragraph auto spacing"): either
/// `w:compat/w:doNotUseHTMLParagraphAutoSpacing` is set (tdf145716), or the document's
/// `compatibilityMode` compat-setting is Word 2003 or older (<= 11, tdf153964). Modern documents
/// (mode 12/14/15, or no setting at all - Word treats those as mode 12) consolidate.
pub fn settings_legacy_spacing(xml: &[u8]) -> bool {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:doNotUseHTMLParagraphAutoSpacing" if toggle_on(&e) => return true,
                b"w:compatSetting" => {
                    if attr(&e, b"w:name").as_deref() == Some("compatibilityMode")
                        && let Some(v) = attr(&e, b"w:val").and_then(|s| s.parse::<u32>().ok())
                        && v <= 11
                    {
                        return true;
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
        buf.clear();
    }
}

/// Parse the document-level page background: `<w:document>`'s `<w:background w:color="RRGGBB">`
/// (the first child, before `<w:body>`). Returns the solid fill colour as hex; a gradient / bitmap
/// fill (VML inside the element) degrades to its base colour, `auto` to none. Whether Word actually
/// DISPLAYS it is a separate settings flag ([`settings_display_background`]).
pub fn parse_background(xml: &[u8]) -> Option<String> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:background" => {
                    return attr(&e, b"w:color").filter(|c| !c.is_empty() && c != "auto");
                }
                // The background precedes the body in the schema - no point scanning further.
                b"w:body" => return None,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// Whether `word/settings.xml` opts into displaying the page background
/// (`<w:displayBackgroundShape/>`): Word paints `w:background` only when this is set.
pub fn settings_display_background(xml: &[u8]) -> bool {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if e.name().as_ref() == b"w:displayBackgroundShape" =>
            {
                return toggle_on(&e);
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
        buf.clear();
    }
}

/// Parse `word/styles.xml` into a [`StyleTable`]: `w:docDefaults` -> [`StyleTable::default`], and
/// Reduce a `w:shd` to an effective RGB hex: `pctNN` pattern shades blend the pattern colour
/// (auto = black) over the fill (auto = white) at NN% - Word's `pct10` header grey comes out
/// `E6E6E6`; `solid` shows the pattern colour; `clear` shows the fill. `None` when nothing
/// visible results (e.g. `clear` over an auto fill).
fn shd_effective_hex(val: Option<&str>, color: Option<&str>, fill: Option<&str>) -> Option<String> {
    let rgb = |s: Option<&str>, dflt: [u8; 3]| -> [u8; 3] {
        match s {
            Some(h) if h.len() == 6 => u32::from_str_radix(h, 16)
                .map(|n| [(n >> 16) as u8, (n >> 8) as u8, n as u8])
                .unwrap_or(dflt),
            _ => dflt,
        }
    };
    let hex = |c: [u8; 3]| format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
    let val = val.unwrap_or("clear");
    if let Some(pct) = val.strip_prefix("pct").and_then(|p| p.parse::<u32>().ok()) {
        let c = rgb(color, [0, 0, 0]);
        let f = rgb(fill, [0xFF, 0xFF, 0xFF]);
        let t = pct.min(100) as f32 / 100.0;
        let mix = |a: u8, b: u8| (a as f32 * t + b as f32 * (1.0 - t)).round() as u8;
        return Some(hex([mix(c[0], f[0]), mix(c[1], f[1]), mix(c[2], f[2])]));
    }
    match val {
        "solid" => Some(hex(rgb(color, [0, 0, 0]))),
        _ => fill.filter(|f| f.len() == 6).map(|f| f.to_ascii_uppercase()),
    }
}

/// each `w:style` (by `w:styleId`) -> its direct run/paragraph properties.
pub fn parse_styles(xml: &[u8]) -> StyleTable {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut table = StyleTable::default();

    let mut cur = StyleProps::default(); // accumulates the active docDefaults / w:style
    let mut cur_id: Option<String> = None; // Some while inside a w:style
    let mut cur_based_on: Option<String> = None;
    let mut cur_type: Option<String> = None; // w:style w:type (paragraph / character / table / ...)
    let mut cur_default: Option<String> = None; // w:style w:default ("1" = the type's default style)
    let mut cur_name: Option<String> = None; // w:name w:val (the human style name)
    let mut cur_qformat = false; // w:qFormat present -> a "quick style" for the gallery
    let mut in_rpr = false;
    let mut in_ppr = false;
    let mut in_cond = false; // inside w:tblStylePr (a conditional band) - skip its borders
    let mut cur_cond_type: Option<String> = None; // the band's w:type (firstRow / band1Horz / ...)
    let mut in_style_tbl_borders = false; // inside a table style's base w:tblBorders
    let mut in_style_tbl_cellmar = false; // inside a table style's base w:tblCellMar
    let mut in_style_pbdr = false; // inside a paragraph style's w:pPr/w:pBdr
    let mut in_style_tabs = false; // inside a paragraph style's w:pPr/w:tabs
    let mut cur_tbl_borders = EdgeBorders::default();
    let mut cur_tbl_cellmar = CellMargins::default();
    let mut cur_first_row_shd: Option<String> = None;
    let mut cur_pbdr: Vec<String> = Vec::new();

    // w:b / w:i appear as Empty (<w:b/>) or Start (<w:b w:val=".."/>); handle both via this.
    let on_rpr_toggle = |e: &quick_xml::events::BytesStart, cur: &mut StyleProps| {
        match e.name().as_ref() {
            b"w:b" => cur.bold = Some(toggle_on(e)),
            b"w:i" => cur.italic = Some(toggle_on(e)),
            b"w:sz" => {
                if let Some(v) = attr(e, b"w:val").and_then(|s| s.parse().ok()) {
                    cur.size = Some(v);
                }
            }
            b"w:color" => cur.color = attr(e, b"w:val").filter(|c| c != "auto"),
            // Keep "none": a character/paragraph style's `highlight="none"` cancels an inherited
            // highlight (resolved below the run), it isn't "unset". `highlight_rgb("none")` is no-op.
            b"w:highlight" => cur.highlight = attr(e, b"w:val").filter(|c| !c.is_empty()),
            b"w:rFonts" => {
                if let Some(f) = attr(e, b"w:ascii") {
                    cur.font = Some(f);
                }
            }
            b"w:spacing" => {
                cur.space_before = attr(e, b"w:before").and_then(|s| s.parse().ok());
                cur.space_after = attr(e, b"w:after").and_then(|s| s.parse().ok());
                // `w:line` is a 240ths multiplier with lineRule auto/absent, and absolute twips with
                // exact. We model auto + exact from styles; `atLeast` from a style stays unmodelled
                // (capturing it and applying it as a multiplier over-spaces docs like mixednumberings),
                // so it falls through to docDefaults / natural as before.
                let rule = attr(e, b"w:lineRule").and_then(|s| LineRule::from_ooxml(&s));
                if !matches!(rule, Some(LineRule::AtLeast)) {
                    cur.line_spacing = attr(e, b"w:line").and_then(|s| s.parse().ok());
                    cur.line_rule = rule;
                }
            }
            _ => {}
        }
    };

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                b"w:docDefaults" => {
                    cur = StyleProps::default();
                }
                b"w:style" => {
                    cur_id = attr(&e, b"w:styleId");
                    cur_based_on = None;
                    cur_type = attr(&e, b"w:type");
                    cur_default = attr(&e, b"w:default");
                    cur_name = None;
                    cur_qformat = false;
                    cur = StyleProps::default();
                    cur_tbl_borders = EdgeBorders::default();
                    cur_tbl_cellmar = CellMargins::default();
                    cur_first_row_shd = None;
                    in_cond = false;
                    cur_cond_type = None;
                    in_style_tbl_borders = false;
                    in_style_tbl_cellmar = false;
                }
                b"w:rPr" => in_rpr = true,
                b"w:pPr" => in_ppr = true,
                // A table style's base borders/margins live in its top-level `w:tblPr`; the
                // conditional `w:tblStylePr` bands (firstRow / band1 / ...) carry their own borders
                // we don't model - skip those so a band can't masquerade as the table's base grid.
                // The one band property we do capture is the firstRow shading (below).
                b"w:tblStylePr" => {
                    in_cond = true;
                    cur_cond_type = attr(&e, b"w:type");
                }
                b"w:tblBorders" if cur_id.is_some() && !in_cond => in_style_tbl_borders = true,
                b"w:tblCellMar" if cur_id.is_some() && !in_cond => in_style_tbl_cellmar = true,
                b"w:pBdr" if in_ppr && !in_cond => {
                    in_style_pbdr = true;
                    cur_pbdr.clear();
                }
                b"w:tabs" if in_ppr && !in_cond => in_style_tabs = true,
                _ => {
                    if in_rpr && matches!(e.name().as_ref(), b"w:b" | b"w:i") {
                        on_rpr_toggle(&e, &mut cur);
                    }
                }
            },
            Event::Empty(e) => {
                if (in_rpr
                    && matches!(
                        e.name().as_ref(),
                        b"w:b" | b"w:i" | b"w:sz" | b"w:color" | b"w:highlight" | b"w:rFonts"
                    ))
                    || (in_ppr && e.name().as_ref() == b"w:spacing")
                {
                    on_rpr_toggle(&e, &mut cur);
                } else if in_ppr && e.name().as_ref() == b"w:numId" {
                    // Style numbering (`w:pPr/w:numPr/w:numId`): an outline heading's list id.
                    cur.num_id = attr(&e, b"w:val").and_then(|s| s.parse().ok());
                } else if in_ppr && e.name().as_ref() == b"w:ilvl" {
                    cur.num_ilvl = attr(&e, b"w:val").and_then(|s| s.parse().ok());
                } else if in_ppr && e.name().as_ref() == b"w:jc" {
                    // Style alignment (Title/Heading centred, Quote right): a paragraph with no direct
                    // `w:jc` inherits it.
                    cur.align = attr(&e, b"w:val").as_deref().and_then(Align::from_ooxml);
                } else if in_ppr && e.name().as_ref() == b"w:keepNext" {
                    // Heading styles set keepNext so a heading isn't orphaned at a page foot.
                    cur.keep_next = Some(toggle_on(&e));
                } else if in_ppr && e.name().as_ref() == b"w:pageBreakBefore" {
                    // A "page break before" style forces its paragraphs onto a new page.
                    cur.page_break_before = Some(toggle_on(&e));
                } else if in_ppr && e.name().as_ref() == b"w:contextualSpacing" {
                    cur.contextual_spacing = Some(toggle_on(&e));
                } else if in_ppr && e.name().as_ref() == b"w:ind" {
                    // Style indents (signed twips): Word's Header styles widen their box into the
                    // margins with negative left/right.
                    let side = |a: &[u8], b: &[u8]| {
                        attr(&e, a).or_else(|| attr(&e, b)).and_then(|s| s.parse::<i32>().ok())
                    };
                    if let Some(v) = side(b"w:left", b"w:start") {
                        cur.indent_left = Some(v);
                    }
                    if let Some(v) = side(b"w:right", b"w:end") {
                        cur.indent_right = Some(v);
                    }
                } else if in_style_tabs && e.name().as_ref() == b"w:tab" {
                    // A style-defined tab stop (or a `clear` retracting an inherited one).
                    let val = attr(&e, b"w:val").unwrap_or_default();
                    if let Some(pos) = attr(&e, b"w:pos").and_then(|s| s.parse::<i64>().ok()) {
                        let pos = pos.max(0) as u32;
                        if val == "clear" {
                            cur.tab_clears.push(pos);
                        } else {
                            let kind = match val.as_str() {
                                "center" => 1u8,
                                "right" | "end" => 2,
                                "decimal" => 3,
                                "bar" => 255, // a vertical rule, not an alignment stop
                                _ => 0,
                            };
                            if kind != 255 {
                                cur.tab_stops.push(pos);
                                cur.tab_kinds.push(kind);
                            }
                        }
                    }
                } else if in_style_pbdr
                    && matches!(e.name().as_ref(), b"w:top" | b"w:left" | b"w:bottom" | b"w:right")
                {
                    // A style's paragraph-border edge, in ParaProps::border's compact form (the
                    // exact builder the direct w:pPr/w:pBdr parse uses).
                    let val = attr(&e, b"w:val").unwrap_or_default();
                    if !val.is_empty() && val != "none" && val != "nil" {
                        let sz = attr(&e, b"w:sz").unwrap_or_else(|| "4".into());
                        let space = attr(&e, b"w:space").unwrap_or_else(|| "0".into());
                        let color = attr(&e, b"w:color").unwrap_or_else(|| "auto".into());
                        let edge = match e.name().as_ref() {
                            b"w:top" => "t",
                            b"w:left" => "l",
                            b"w:bottom" => "b",
                            _ => "r",
                        };
                        cur_pbdr.push(format!("{edge}={val},{sz},{space},{color}"));
                    }
                } else if in_style_tbl_cellmar
                    && matches!(e.name().as_ref(), b"w:top" | b"w:left" | b"w:bottom" | b"w:right")
                {
                    let w = attr(&e, b"w:w").and_then(|s| s.parse().ok());
                    match e.name().as_ref() {
                        b"w:top" => cur_tbl_cellmar.top = w,
                        b"w:left" => cur_tbl_cellmar.left = w,
                        b"w:bottom" => cur_tbl_cellmar.bottom = w,
                        b"w:right" => cur_tbl_cellmar.right = w,
                        _ => {}
                    }
                } else if in_cond
                    && !in_rpr
                    && !in_ppr
                    && cur_cond_type.as_deref() == Some("firstRow")
                    && e.name().as_ref() == b"w:shd"
                {
                    // The header band's cell shading (`tblStylePr/tcPr/shd`) - the grey Word paints
                    // on a styled table's first row. rPr/pPr shd inside the band is not cell fill.
                    if let Some(hex) = shd_effective_hex(
                        attr(&e, b"w:val").as_deref(),
                        attr(&e, b"w:color").as_deref(),
                        attr(&e, b"w:fill").as_deref(),
                    ) {
                        cur_first_row_shd = Some(hex);
                    }
                } else if in_style_tbl_borders
                    && matches!(
                        e.name().as_ref(),
                        b"w:top" | b"w:left" | b"w:bottom" | b"w:right" | b"w:insideH" | b"w:insideV"
                    )
                {
                    let b = parse_border(&e);
                    match e.name().as_ref() {
                        b"w:top" => cur_tbl_borders.top = b,
                        b"w:left" => cur_tbl_borders.left = b,
                        b"w:bottom" => cur_tbl_borders.bottom = b,
                        b"w:right" => cur_tbl_borders.right = b,
                        b"w:insideH" => cur_tbl_borders.inside_h = b,
                        b"w:insideV" => cur_tbl_borders.inside_v = b,
                        _ => {}
                    }
                } else if cur_id.is_some() && e.name().as_ref() == b"w:basedOn" {
                    cur_based_on = attr(&e, b"w:val");
                } else if cur_id.is_some() && e.name().as_ref() == b"w:name" {
                    cur_name = attr(&e, b"w:val");
                } else if cur_id.is_some() && e.name().as_ref() == b"w:qFormat" {
                    cur_qformat = true;
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:rPr" => in_rpr = false,
                b"w:pPr" => in_ppr = false,
                b"w:tblStylePr" => {
                    in_cond = false;
                    cur_cond_type = None;
                }
                b"w:tblBorders" => in_style_tbl_borders = false,
                b"w:tblCellMar" => in_style_tbl_cellmar = false,
                b"w:pBdr" => {
                    in_style_pbdr = false;
                    if !cur_pbdr.is_empty() {
                        cur.border = Some(std::mem::take(&mut cur_pbdr).join("|"));
                    }
                }
                b"w:tabs" => in_style_tabs = false,
                b"w:docDefaults" => {
                    table.default = std::mem::take(&mut cur);
                }
                b"w:style" => {
                    if let Some(id) = cur_id.take() {
                        if let Some(parent) = cur_based_on.take() {
                            table.based_on.insert(id.clone(), parent);
                        }
                        let tbl_b = std::mem::take(&mut cur_tbl_borders);
                        if tbl_b.top.is_some()
                            || tbl_b.left.is_some()
                            || tbl_b.bottom.is_some()
                            || tbl_b.right.is_some()
                            || tbl_b.inside_h.is_some()
                            || tbl_b.inside_v.is_some()
                        {
                            table.table_borders.insert(id.clone(), tbl_b);
                        }
                        let tbl_m = std::mem::take(&mut cur_tbl_cellmar);
                        if tbl_m != CellMargins::default() {
                            table.table_cell_margins.insert(id.clone(), tbl_m);
                        }
                        if let Some(shd) = cur_first_row_shd.take() {
                            table.table_first_row_shd.insert(id.clone(), shd);
                        }
                        if let Some(name) = cur_name.take() {
                            table.names.insert(id.clone(), name);
                        }
                        // A paragraph style (the default when w:type is absent) flagged qFormat is a
                        // gallery candidate, in document order.
                        let is_para = cur_type.as_deref().unwrap_or("paragraph") == "paragraph";
                        if is_para && cur_qformat {
                            table.gallery.push(id.clone());
                        }
                        // The default paragraph style (`w:default="1"`) is what a style-less paragraph
                        // inherits - record it so `resolve(None)` layers it over docDefaults.
                        if is_para && matches!(cur_default.as_deref(), Some("1") | Some("true") | Some("on")) {
                            table.default_para_style = Some(id.clone());
                        }
                        cur_type = None;
                        cur_default = None;
                        cur_qformat = false;
                        table.by_id.insert(id, std::mem::take(&mut cur));
                    }
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }

    table
}

// ── built-in quick styles (Word's default gallery) ──────────────────────────

/// The canonical Word quick-style ids, in gallery order. A blank Word document ships these latent
/// styles so the Styles gallery is populated before any get used; Scriptor seeds the same set.
const QUICK_STYLE_IDS: [&str; 10] = [
    "Normal", "NoSpacing", "Heading1", "Heading2", "Heading3", "Title", "Subtitle", "Quote",
    "IntenseQuote", "ListParagraph",
];

impl StyleTable {
    /// Word's built-in quick-style set with default-theme formatting (Calibri / Calibri Light, accent
    /// `#2E74B5`), so a from-scratch document's Styles gallery matches Word and applied styles render +
    /// export correctly. `docDefaults` = Calibri 11pt / 1.08 line / 8pt after - Word's Normal.dotm base.
    pub fn word_default() -> StyleTable {
        let mut t = StyleTable {
            default: StyleProps {
                font: Some("Calibri".into()),
                size: Some(22),
                line_spacing: Some(259),
                space_after: Some(160),
                ..StyleProps::default()
            },
            ..StyleTable::default()
        };
        t.merge_defaults();
        t
    }

    /// Add any canonical quick-style this table lacks - the document's own definitions always win, so
    /// this only *fills gaps*. Keeps the gallery Word-complete (and an applied built-in from dangling)
    /// whether the doc shipped a full styles.xml, a sparse one, or none. Idempotent.
    pub fn merge_defaults(&mut self) {
        for (id, name, based_on, props, _) in canonical_styles() {
            self.names.entry(id.to_string()).or_insert_with(|| name.to_string());
            if let Some(parent) = based_on {
                self.based_on.entry(id.to_string()).or_insert_with(|| parent.to_string());
            }
            self.by_id.entry(id.to_string()).or_insert(props);
        }
        // Gallery: canonical order first (those now present), then the doc's own extra quick styles.
        let mut gallery: Vec<String> = QUICK_STYLE_IDS
            .iter()
            .filter(|id| self.by_id.contains_key(**id))
            .map(|id| id.to_string())
            .collect();
        for id in &self.gallery {
            if !gallery.iter().any(|g| g == id) {
                gallery.push(id.clone());
            }
        }
        self.gallery = gallery;
    }
}

/// `(styleId, display name, basedOn, props, qFormat)` for each canonical quick style. Sizes are
/// half-points; spacing is twips; colour is hex (no `#`). Approximates Word's default-theme styles.
fn canonical_styles() -> Vec<(&'static str, &'static str, Option<&'static str>, StyleProps, bool)> {
    // size, bold, italic, color, font, before, after, line
    let p = |size: Option<u16>,
             bold: Option<bool>,
             italic: Option<bool>,
             color: Option<&str>,
             font: Option<&str>,
             before: Option<u32>,
             after: Option<u32>,
             line: Option<u16>| StyleProps {
        size,
        bold,
        italic,
        color: color.map(String::from),
        highlight: None, // built-in quick styles carry no highlight
        font: font.map(String::from),
        line_spacing: line,
        line_rule: None, // built-in quick styles use auto line spacing
        space_before: before,
        space_after: after,
        num_id: None, // built-in quick styles carry no numbering (added when a user applies a list)
        num_ilvl: None,
        border: None, // built-in quick styles carry no paragraph border
        indent_left: None,
        indent_right: None,
        tab_stops: Vec::new(),
        tab_kinds: Vec::new(),
        tab_clears: Vec::new(),
        keep_next: None, // imported heading styles carry their own keepNext; new-doc ones don't yet
        contextual_spacing: None,
        align: None, // built-in quick-style alignment comes from an imported styles.xml, not the seed
        page_break_before: None,
    };
    vec![
        ("Normal", "Normal", None, StyleProps::default(), true),
        ("NoSpacing", "No Spacing", Some("Normal"),
            p(None, None, None, None, None, Some(0), Some(0), Some(240)), true),
        ("Heading1", "Heading 1", Some("Normal"),
            p(Some(32), None, None, Some("2E74B5"), Some("Calibri Light"), Some(240), Some(0), None), true),
        ("Heading2", "Heading 2", Some("Normal"),
            p(Some(26), None, None, Some("2E74B5"), Some("Calibri Light"), Some(40), Some(0), None), true),
        ("Heading3", "Heading 3", Some("Normal"),
            p(Some(24), None, None, Some("1F4D78"), Some("Calibri Light"), Some(40), Some(0), None), true),
        ("Title", "Title", Some("Normal"),
            p(Some(56), None, None, None, Some("Calibri Light"), None, Some(80), Some(240)), true),
        ("Subtitle", "Subtitle", Some("Normal"),
            p(Some(28), None, None, Some("5A5A5A"), Some("Calibri Light"), None, Some(160), None), true),
        ("Quote", "Quote", Some("Normal"),
            p(None, None, Some(true), Some("404040"), None, Some(200), Some(200), None), true),
        ("IntenseQuote", "Intense Quote", Some("Normal"),
            p(None, None, Some(true), Some("2E74B5"), None, Some(200), Some(200), None), true),
        ("ListParagraph", "List Paragraph", Some("Normal"), StyleProps::default(), true),
    ]
}

// ── styles.xml serialization (export) ────────────────────────────────────────

const STYLES_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// The `<w:rPr>` inner XML (rFonts / b / i / sz / color) for `props`; `""` when nothing is set.
fn style_rpr_inner(p: &StyleProps) -> String {
    let mut s = String::new();
    if let Some(f) = &p.font {
        let f = xml_escape(f);
        s.push_str(&format!("<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:cs=\"{f}\"/>"));
    }
    if p.bold == Some(true) {
        s.push_str("<w:b/>");
    }
    if p.italic == Some(true) {
        s.push_str("<w:i/>");
    }
    if let Some(sz) = p.size {
        s.push_str(&format!("<w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/>"));
    }
    if let Some(c) = &p.color {
        s.push_str(&format!("<w:color w:val=\"{}\"/>", xml_escape(c)));
    }
    s
}

/// The `<w:pPr>` inner XML for `props`' paragraph formatting (`w:pageBreakBefore` + `w:spacing` +
/// `w:jc`); `""` when none is set. Child order follows CT_PPr (pageBreakBefore, spacing, jc).
fn style_ppr_inner(p: &StyleProps) -> String {
    let mut s = String::new();
    if p.page_break_before == Some(true) {
        s.push_str("<w:pageBreakBefore/>");
    }
    if p.space_before.is_some() || p.space_after.is_some() || p.line_spacing.is_some() {
        s.push_str("<w:spacing");
        if let Some(b) = p.space_before {
            s.push_str(&format!(" w:before=\"{b}\""));
        }
        if let Some(a) = p.space_after {
            s.push_str(&format!(" w:after=\"{a}\""));
        }
        if let Some(l) = p.line_spacing {
            let rule = p.line_rule.map(|r| r.as_str()).unwrap_or("auto");
            s.push_str(&format!(" w:line=\"{l}\" w:lineRule=\"{rule}\""));
        }
        s.push_str("/>");
    }
    if let Some(a) = p.align {
        s.push_str(&format!("<w:jc w:val=\"{}\"/>", a.to_ooxml()));
    }
    s
}

/// A `<w:style w:type="paragraph">` block for paragraph style `id`.
fn style_block(id: &str, name: &str, based_on: Option<&str>, props: &StyleProps, qformat: bool) -> String {
    let mut s = format!("<w:style w:type=\"paragraph\" w:styleId=\"{}\">", xml_escape(id));
    s.push_str(&format!("<w:name w:val=\"{}\"/>", xml_escape(name)));
    if let Some(parent) = based_on {
        s.push_str(&format!("<w:basedOn w:val=\"{}\"/>", xml_escape(parent)));
    }
    if qformat {
        s.push_str("<w:qFormat/>");
    }
    let ppr = style_ppr_inner(props);
    if !ppr.is_empty() {
        s.push_str(&format!("<w:pPr>{ppr}</w:pPr>"));
    }
    let rpr = style_rpr_inner(props);
    if !rpr.is_empty() {
        s.push_str(&format!("<w:rPr>{rpr}</w:rPr>"));
    }
    s.push_str("</w:style>");
    s
}

/// Serialize a full `word/styles.xml` from `table` (docDefaults + every style). Used when a
/// from-scratch document is saved (it has no source styles.xml to preserve).
pub fn export_styles_xml(table: &StyleTable) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n");
    s.push_str(&format!("<w:styles xmlns:w=\"{STYLES_NS}\">"));
    s.push_str("<w:docDefaults><w:rPrDefault>");
    let rpr = style_rpr_inner(&table.default);
    if !rpr.is_empty() {
        s.push_str(&format!("<w:rPr>{rpr}</w:rPr>"));
    }
    s.push_str("</w:rPrDefault><w:pPrDefault>");
    let ppr = style_ppr_inner(&table.default);
    if !ppr.is_empty() {
        s.push_str(&format!("<w:pPr>{ppr}</w:pPr>"));
    }
    s.push_str("</w:pPrDefault></w:docDefaults>");
    // Styles: gallery order first, then any others (sorted for deterministic output).
    let mut ids: Vec<String> = table.gallery.clone();
    let mut rest: Vec<String> =
        table.by_id.keys().filter(|k| !ids.contains(k)).cloned().collect();
    rest.sort();
    ids.extend(rest);
    for id in &ids {
        if let Some(props) = table.by_id.get(id) {
            let name = table.names.get(id).map(String::as_str).unwrap_or(id);
            let based = table.based_on.get(id).map(String::as_str);
            let q = table.gallery.iter().any(|g| g == id);
            s.push_str(&style_block(id, name, based, props, q));
        }
    }
    s.push_str("</w:styles>");
    s
}

/// Reconcile an imported `styles.xml` with the live table for export: (1) **patch** the definition of
/// every edited style (`overrides`) the source already defines - replacing only the modeled child
/// elements (font / bold / italic / size / colour in `w:rPr`; spacing / keepNext / contextualSpacing
/// in `w:pPr`) and leaving every other child (`w:basedOn`, `w:next`, `w:link`, `w:outlineLvl`, ...) and
/// every unedited spacing attribute byte-for-byte intact; (2) **append** any canonical quick style the
/// doc applied but never defined (the effective `table` carries an edit to such a style); (3) **append**
/// any runtime-**added** style (a custom style in the table's gallery the source never defined). A style
/// the source defines and the patch can't find is otherwise skipped. The from-scratch path
/// ([`export_styles_xml`]) stays lossless and already emits overrides + added styles.
pub fn merge_styles_into_xml(
    src: &str,
    table: &StyleTable,
    overrides: &std::collections::HashMap<String, StyleProps>,
) -> String {
    // (1) Patch the blocks of edited styles the source already defines.
    let patched = patch_overridden_style_blocks(src, overrides);
    let src = patched.as_str();

    let Some(pos) = src.rfind("</w:styles>") else {
        return src.to_string();
    };
    let defines = |id: &str| src.contains(&format!("w:styleId=\"{id}\""));
    // (2) Append any canonical quick style missing entirely from the source.
    let mut add = String::new();
    let canonical: std::collections::HashSet<&str> = canonical_styles().iter().map(|t| t.0).collect();
    for (id, name, based_on, props, q) in canonical_styles() {
        if defines(id) {
            continue;
        }
        let props = table.by_id.get(id).cloned().unwrap_or(props);
        add.push_str(&style_block(id, name, based_on, &props, q));
    }
    // (3) Append any added custom style (in the gallery, not canonical, not defined by the source).
    for id in &table.gallery {
        if canonical.contains(id.as_str()) || defines(id) {
            continue;
        }
        if let Some(props) = table.by_id.get(id) {
            let name = table.names.get(id).map(String::as_str).unwrap_or(id);
            let based = table.based_on.get(id).map(String::as_str);
            add.push_str(&style_block(id, name, based, props, true));
        }
    }
    if add.is_empty() {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len() + add.len());
    out.push_str(&src[..pos]);
    out.push_str(&add);
    out.push_str(&src[pos..]);
    out
}

/// Patch each edited style's `<w:style>` block in place (Modify-Style export). For every id in
/// `overrides` the source defines, only the modeled child elements change; everything else is
/// preserved. Ids the source doesn't define are left for the canonical-append / add-style paths.
fn patch_overridden_style_blocks(
    src: &str,
    overrides: &std::collections::HashMap<String, StyleProps>,
) -> String {
    let mut out = src.to_string();
    for (id, ov) in overrides {
        if let Some(patched) = patch_one_style_block(&out, id, ov) {
            out = patched;
        }
    }
    out
}

/// The byte span of `<w:style ... w:styleId="id"> ... </w:style>` in `src`, or `None` when undefined.
/// `<w:style ` (trailing space) is the element open, so it won't match `<w:styleLink `.
fn locate_style_block(src: &str, id: &str) -> Option<(usize, usize)> {
    let attr_at = src.find(&format!("w:styleId=\"{id}\""))?;
    let start = src[..attr_at].rfind("<w:style ")?;
    let end = src[start..].find("</w:style>")? + start + "</w:style>".len();
    Some((start, end))
}

/// Patch style `id`'s block from `src` using the sparse per-field override `ov` (only its `Some`
/// fields change), or `None` when `src` defines no such style.
fn patch_one_style_block(src: &str, id: &str, ov: &StyleProps) -> Option<String> {
    let (start, end) = locate_style_block(src, id)?;
    let mut block = src[start..end].to_string();
    if ov.font.is_some()
        || ov.bold.is_some()
        || ov.italic.is_some()
        || ov.size.is_some()
        || ov.color.is_some()
        || ov.highlight.is_some()
    {
        block = patch_style_section(&block, "rPr", |inner| patch_style_rpr_inner(inner, ov));
    }
    if ov.space_before.is_some()
        || ov.space_after.is_some()
        || ov.line_spacing.is_some()
        || ov.keep_next.is_some()
        || ov.contextual_spacing.is_some()
        || ov.align.is_some()
        || ov.page_break_before.is_some()
    {
        block = patch_style_section(&block, "pPr", |inner| patch_style_ppr_inner(inner, ov));
    }
    let mut out = String::with_capacity(src.len() - (end - start) + block.len());
    out.push_str(&src[..start]);
    out.push_str(&block);
    out.push_str(&src[end..]);
    Some(out)
}

/// Apply `patch` to the inner XML of `block`'s `<w:{tag}>` section (`tag` = `pPr` / `rPr`), creating
/// the section when absent (pPr before rPr; both before `</w:style>`) and expanding a self-closing
/// `<w:{tag}/>`. A patch that yields empty inner on an absent section is a no-op.
fn patch_style_section(block: &str, tag: &str, patch: impl Fn(&str) -> String) -> String {
    let open = format!("<w:{tag}>");
    let close = format!("</w:{tag}>");
    if let Some(s) = block.find(&open) {
        let inner_start = s + open.len();
        if let Some(rel) = block[inner_start..].find(&close) {
            let inner_end = inner_start + rel;
            let new_inner = patch(&block[inner_start..inner_end]);
            return format!("{}{}{}", &block[..inner_start], new_inner, &block[inner_end..]);
        }
    }
    let empty = format!("<w:{tag}/>");
    if let Some(s) = block.find(&empty) {
        let new_inner = patch("");
        if new_inner.is_empty() {
            return block.to_string();
        }
        return format!("{}<w:{tag}>{new_inner}</w:{tag}>{}", &block[..s], &block[s + empty.len()..]);
    }
    let new_inner = patch("");
    if new_inner.is_empty() {
        return block.to_string();
    }
    // Insert a fresh section: pPr goes before rPr (or before </w:style>); rPr goes before </w:style>.
    let at = if tag == "pPr" {
        block.find("<w:rPr>").or_else(|| block.find("<w:rPr/>"))
    } else {
        None
    }
    .or_else(|| block.rfind("</w:style>"))
    .unwrap_or(block.len());
    format!("{}<w:{tag}>{new_inner}</w:{tag}>{}", &block[..at], &block[at..])
}

/// Drop every `<w:{tag} .../>` (self-closing) or `<w:{tag}>...</w:{tag}>` element from `inner`. The
/// match requires the char after the tag name to be space / `/` / `>` so `b` doesn't hit `bCs`/`bdr`,
/// `sz` doesn't hit `szCs`, etc.
fn drop_style_element(inner: &str, tag: &str) -> String {
    let open = format!("<w:{tag}");
    let mut out = String::with_capacity(inner.len());
    let mut rest = inner;
    while let Some(pos) = rest.find(&open) {
        let after = rest[pos + open.len()..].chars().next();
        if !matches!(after, Some(' ') | Some('/') | Some('>')) {
            out.push_str(&rest[..pos + open.len()]); // a longer tag (bCs, szCs, ...) - keep, move on
            rest = &rest[pos + open.len()..];
            continue;
        }
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let gt = tail.find('>').map(|g| g + 1).unwrap_or(tail.len());
        if tail[..gt].ends_with("/>") {
            rest = &tail[gt..]; // self-closing
        } else if let Some(c) = tail.find(&format!("</w:{tag}>")) {
            rest = &tail[c + format!("</w:{tag}>").len()..]; // paired
        } else {
            rest = &tail[gt..];
        }
    }
    out.push_str(rest);
    out
}

/// Patch the modeled `w:rPr` children in `inner` from `ov` (only its set fields), replacing any
/// existing element of the same kind and appending when absent. Sibling children we don't model are
/// untouched.
fn patch_style_rpr_inner(inner: &str, ov: &StyleProps) -> String {
    let mut s = inner.to_string();
    if let Some(f) = &ov.font {
        s = drop_style_element(&s, "rFonts");
        let f = xml_escape(f);
        s.push_str(&format!("<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:cs=\"{f}\"/>"));
    }
    if let Some(b) = ov.bold {
        s = drop_style_element(&s, "b");
        s.push_str(if b { "<w:b/>" } else { "<w:b w:val=\"false\"/>" });
    }
    if let Some(i) = ov.italic {
        s = drop_style_element(&s, "i");
        s.push_str(if i { "<w:i/>" } else { "<w:i w:val=\"false\"/>" });
    }
    if let Some(sz) = ov.size {
        s = drop_style_element(&s, "sz");
        s = drop_style_element(&s, "szCs");
        s.push_str(&format!("<w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/>"));
    }
    if let Some(c) = &ov.color {
        s = drop_style_element(&s, "color");
        s.push_str(&format!("<w:color w:val=\"{}\"/>", xml_escape(c)));
    }
    if let Some(h) = &ov.highlight {
        s = drop_style_element(&s, "highlight");
        s.push_str(&format!("<w:highlight w:val=\"{}\"/>", xml_escape(h)));
    }
    s
}

/// Patch the modeled `w:pPr` children in `inner` from `ov`. Spacing is patched at the **attribute**
/// level (an edit to one of before / after / line keeps the others), so a partial spacing change
/// doesn't drop the rest.
fn patch_style_ppr_inner(inner: &str, ov: &StyleProps) -> String {
    let mut s = inner.to_string();
    if ov.space_before.is_some() || ov.space_after.is_some() || ov.line_spacing.is_some() {
        s = upsert_spacing(&s, ov.space_before, ov.space_after, ov.line_spacing, ov.line_rule);
    }
    if let Some(k) = ov.keep_next {
        s = drop_style_element(&s, "keepNext");
        if k {
            s.push_str("<w:keepNext/>");
        }
    }
    if let Some(c) = ov.contextual_spacing {
        s = drop_style_element(&s, "contextualSpacing");
        if c {
            s.push_str("<w:contextualSpacing/>");
        }
    }
    if let Some(a) = ov.align {
        s = drop_style_element(&s, "jc");
        s.push_str(&format!("<w:jc w:val=\"{}\"/>", a.to_ooxml()));
    }
    if let Some(pb) = ov.page_break_before {
        s = drop_style_element(&s, "pageBreakBefore");
        if pb {
            s.push_str("<w:pageBreakBefore/>");
        }
    }
    s
}

/// Set / add the `w:before` / `w:after` / `w:line` (+ `w:lineRule`) attributes of `inner`'s
/// `<w:spacing/>` for the fields that are `Some`, preserving the rest; create the element when absent.
fn upsert_spacing(
    inner: &str,
    before: Option<u32>,
    after: Option<u32>,
    line: Option<u16>,
    rule: Option<LineRule>,
) -> String {
    let with_attrs = |elem: &str| -> String {
        let mut e = elem.to_string();
        if let Some(b) = before {
            e = set_xml_attr(&e, "w:before", &b.to_string());
        }
        if let Some(a) = after {
            e = set_xml_attr(&e, "w:after", &a.to_string());
        }
        if let Some(l) = line {
            e = set_xml_attr(&e, "w:line", &l.to_string());
            e = set_xml_attr(&e, "w:lineRule", rule.map(|r| r.as_str()).unwrap_or("auto"));
        }
        e
    };
    if let Some(s) = inner.find("<w:spacing") {
        let tail = &inner[s..];
        let gt = tail.find('>').map(|g| s + g + 1).unwrap_or(inner.len());
        let new_elem = with_attrs(&inner[s..gt]);
        return format!("{}{}{}", &inner[..s], new_elem, &inner[gt..]);
    }
    let mut out = inner.to_string();
    out.push_str(&with_attrs("<w:spacing/>"));
    out
}

/// Set attribute `name` on the single XML element `elem` to `val` (replacing its current value, or
/// inserting the attribute before the closing `/>` / `>` when absent).
fn set_xml_attr(elem: &str, name: &str, val: &str) -> String {
    let needle = format!("{name}=\"");
    if let Some(p) = elem.find(&needle) {
        let vstart = p + needle.len();
        let vend = elem[vstart..].find('"').map(|e| vstart + e).unwrap_or(vstart);
        return format!("{}{}{}", &elem[..vstart], xml_escape(val), &elem[vend..]);
    }
    let ins = elem.rfind("/>").unwrap_or_else(|| elem.rfind('>').unwrap_or(elem.len()));
    format!("{} {name}=\"{}\"{}", elem[..ins].trim_end(), xml_escape(val), &elem[ins..])
}



/// Parse one border edge element (`<w:top w:val=".." w:sz=".." w:color=".."/>`). Returns `None` for
/// `w:val="none"/"nil"` (an explicit no-line edge) so it overrides an inherited border.
pub(crate) fn parse_border(e: &quick_xml::events::BytesStart) -> Option<Border> {
    match attr(e, b"w:val").as_deref() {
        Some("none") | Some("nil") | None => None,
        _ => Some(Border {
            size_eighths: attr(e, b"w:sz").and_then(|s| s.parse().ok()).unwrap_or(4),
            color: attr(e, b"w:color")
                .filter(|c| c != "auto")
                .unwrap_or_else(|| "000000".to_string()),
        }),
    }
}
