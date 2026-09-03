//! Scriptor's font subsystem - the layer that *determines layout fidelity*, owned by us rather than
//! delegated to cosmic-text/fontdb. Shaping + rasterization ride cosmic-text/rustybuzz/swash
//! (in `scriptor-layout`); this crate owns the policy on top of them:
//!
//! - **Metric-compatible substitution**: a document names proprietary MS fonts we cannot ship
//!   (Calibri, Cambria, Georgia, Arial, ...). We map each to an open clone with *matching metrics*
//!   so glyph advances + line heights match Word and line breaks land where Word puts them. The
//!   clones are the Google / tyPoland "Croscore + C-fonts" set (all Apache-2.0 / OFL):
//!   Calibri -> Carlito, Cambria -> Caladea, Georgia -> Gelasio,
//!   Arial -> Arimo, Times New Roman -> Tinos, Courier New -> Cousine.
//!   Same trick LibreOffice + ONLYOFFICE use. (Without this, every font rendered as the one bundled
//!   clone, so a Georgia document was laid out with Calibri metrics - pagination was meaningless.)
//! - **Pluggable provider** (planned): ship clones by default, but accept the *real* MS fonts when a
//!   deployment is licensed for them - then layout is glyph-identical to Word.
//! - **OOXML embedded fonts** (planned): load + deobfuscate `word/fonts/*.odttf` from the package.

/// Metric-compatible substitute family for a requested (often proprietary MS) font, or `None` to let
/// the caller fall back to its own matching. Case-insensitive, whitespace-trimmed. The returned name
/// is the clone's *actual* family name, i.e. what the shaping db registers it under.
pub fn substitute_family(requested: &str) -> Option<&'static str> {
    match requested.trim().to_ascii_lowercase().as_str() {
        "calibri" => Some("Carlito"),
        "cambria" | "cambria math" => Some("Caladea"),
        "georgia" => Some("Gelasio"),
        "arial" | "arial nova" | "helvetica" | "liberation sans" => Some("Arimo"),
        // Arial Narrow is a CONDENSED face (~82% of Arial width); substituting full-width Arimo
        // over-wraps every line. Liberation Sans Narrow is the metric-compatible narrow clone
        // (GPLv2 + font-embedding exception - see NOTICES.md). Its own family name is registered.
        "arial narrow" => Some("Liberation Sans Narrow"),
        "times new roman" | "times" | "liberation serif" => Some("Tinos"),
        "courier new" | "courier" | "consolas" | "liberation mono" => Some("Cousine"),
        // Legal / business serifs the MS core set doesn't cover, mapped to the GUST TeX Gyre clones
        // (metric-compatible with the URW base-35 fonts; LPPL/GUST Font License - see NOTICES.md).
        // Century Schoolbook is mandated by the US Supreme Court + several federal circuits; Book
        // Antiqua / Palatino and Bookman are common in briefs. These match Word's letterforms + are
        // advance-close (the base-35 lineage), a large improvement over the previous arbitrary-sans
        // fallback; their line height is Word-tuned in `line_height_factor` (not the clone's own).
        "century schoolbook" | "century schoolbook std" | "centuryschoolbook"
        | "new century schoolbook" => Some("TeX Gyre Schola"),
        "book antiqua" | "bookantiqua" | "palatino" | "palatino linotype" | "palatinolinotype"
        | "palladio" => Some("TeX Gyre Pagella"),
        "bookman" | "bookman old style" | "bookmanoldstyle" | "bookman old style std" => {
            Some("TeX Gyre Bonum")
        }
        _ => None,
    }
}

/// Resolve a requested family to the family we will actually shape with: the metric-compatible
/// substitute if we have one, else the request unchanged (an unmapped family the db may still hold,
/// otherwise the shaper falls back to its default).
pub fn resolve_family(requested: &str) -> &str {
    substitute_family(requested).unwrap_or(requested)
}

/// The family the shaper falls back to when a document specifies no font at all - Word's default is
/// Calibri, whose metric clone is Carlito.
pub const DEFAULT_FAMILY: &str = "Carlito";

/// Line-height factor for a family we have no metrics for - the long-standing ~1.15x leading that
/// happens to fit the serif/sans clones (Tinos/Arimo/Caladea). Used for any unmapped passthrough
/// family (a system font we don't substitute).
pub const FALLBACK_LINE_HEIGHT: f32 = 1.15;

/// Font-natural single-spacing line-height as a multiple of the em, for `lineRule="auto"` layout.
/// Word takes single line spacing from the font's own vertical metrics (`hhea` ascent - descent +
/// lineGap, over unitsPerEm), so the factor differs per family - a single constant can only match
/// one. 1.15 fits Tinos/Arimo/Caladea (~1.15) but is 6% short for Carlito (Calibri, 1.221) and 10%
/// short for Gelasio (Georgia, 1.270), so a tall single-font body drifted ~1 line per 16 and
/// mis-paginated. These are the bundled clones' actual `hhea` factors; the
/// `line_height_factor_matches_bundled_hhea` test re-derives them from the font bytes so they can't
/// silently drift. `family` is the already-resolved clone name (see [`resolve_family`]); an unmapped
/// family falls back to [`FALLBACK_LINE_HEIGHT`].
pub fn line_height_factor(family: &str) -> f32 {
    match family.trim() {
        "Carlito" => 1.2207,  // Calibri
        "Caladea" => 1.1500,  // Cambria
        "Gelasio" => 1.2695,  // Georgia
        "Arimo" => 1.1499,    // Arial
        "Liberation Sans Narrow" => 1.1475, // Arial Narrow (hhea 1916 - -434 + 0 over 2048)
        "Tinos" => 1.1499,    // Times New Roman
        "Cousine" => 1.1328,  // Courier New
        // The TeX Gyre legal serifs (see `substitute_family`). UNLIKE the Croscore clones - whose
        // hhea is metric-matched to the MS font, so their factor IS the font's hhea - TeX Gyre's own
        // vertical metrics track the URW base-35 fonts (hhea 1.007-1.47, all over the map) and do NOT
        // match Word's rendering of Century Schoolbook / Book Antiqua / Bookman. So these are
        // ESTIMATES in Word's typical body-serif range (~1.2); the corpus docs that use these fonts
        // (n779642, n592908-*, tableCurrupt, tdf117504, fdo76316) paginate to Word's page count with
        // them. Refine against a heavy-usage brief via `visual-diff.ps1 -Reference word` when one is
        // available - these docs use the fonts only partially, so they don't tightly pin the factor.
        // The `line_height_factor_matches_bundled_hhea` test exempts these (checks a sane range).
        "TeX Gyre Schola" => 1.2100,  // Century Schoolbook (Word-estimated)
        "TeX Gyre Pagella" => 1.1800, // Book Antiqua / Palatino (Word-estimated)
        "TeX Gyre Bonum" => 1.2200,   // Bookman Old Style (Word-estimated)
        "DejaVu Sans" => 1.1641,      // broad-Unicode fallback face (its own hhea; never a primary)
        _ => FALLBACK_LINE_HEIGHT,
    }
}

/// A bundled clone font: its bytes + the MS family it stands in for (+ style), for documentation /
/// future provider logic. The shaping db registers it under the font's own family name (the name in
/// `substitute_family`'s output), so only `data` is needed to load it.
pub struct CloneFont {
    pub substitutes: &'static str,
    pub bold: bool,
    pub italic: bool,
    pub data: &'static [u8],
}

macro_rules! family {
    ($ms:literal, $dir:literal) => {
        family!($ms, $dir, "ttf")
    };
    // `$ext` lets a family ship as `.otf` (CFF outlines - the TeX Gyre clones) instead of `.ttf`;
    // fontdb + swash load either from the raw bytes, so only the filename differs.
    ($ms:literal, $dir:literal, $ext:literal) => {
        [
            CloneFont { substitutes: $ms, bold: false, italic: false,
                data: include_bytes!(concat!("../fonts/", $dir, "-Regular.", $ext)) },
            CloneFont { substitutes: $ms, bold: true, italic: false,
                data: include_bytes!(concat!("../fonts/", $dir, "-Bold.", $ext)) },
            CloneFont { substitutes: $ms, bold: false, italic: true,
                data: include_bytes!(concat!("../fonts/", $dir, "-Italic.", $ext)) },
            CloneFont { substitutes: $ms, bold: true, italic: true,
                data: include_bytes!(concat!("../fonts/", $dir, "-BoldItalic.", $ext)) },
        ]
    };
}

/// Every bundled clone (6 families x 4 styles). On wasm there is no system font source, so these ARE
/// the document's fonts - the renderer loads them all into the shaping db, and `substitute_family`
/// maps the document's requested family to the matching clone's registered name.
pub fn bundled_fonts() -> Vec<CloneFont> {
    let mut v = Vec::with_capacity(40);
    v.extend(family!("Calibri", "Carlito"));
    v.extend(family!("Cambria", "Caladea"));
    v.extend(family!("Georgia", "Gelasio"));
    v.extend(family!("Arial", "Arimo"));
    v.extend(family!("Arial Narrow", "LiberationSansNarrow"));
    v.extend(family!("Times New Roman", "Tinos"));
    v.extend(family!("Courier New", "Cousine"));
    // The GUST TeX Gyre legal serifs (`.otf` / CFF; LPPL/GUST Font License - see NOTICES.md).
    v.extend(family!("Century Schoolbook", "TeXGyreSchola", "otf"));
    v.extend(family!("Book Antiqua", "TeXGyrePagella", "otf"));
    v.extend(family!("Bookman Old Style", "TeXGyreBonum", "otf"));
    // DejaVu Sans (regular only) - a broad-Unicode FALLBACK face, NOT a metric substitute for any MS
    // font (`substitute_family` never returns it). cosmic-text falls back to it PER GLYPH for a
    // character the metric clones lack - e.g. the U+05C0 Hebrew paseq some templates use as a `|`
    // separator, which otherwise renders as a tofu box. Bitstream Vera / DejaVu permissive license
    // (see NOTICES.md). Preserves the model text (a fallback glyph, not a character substitution), so
    // caret byte-offsets are unaffected.
    v.push(CloneFont {
        substitutes: "DejaVu Sans",
        bold: false,
        italic: false,
        data: include_bytes!("../fonts/DejaVuSans.ttf"),
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_proprietary_ms_fonts_to_metric_clones() {
        assert_eq!(substitute_family("Calibri"), Some("Carlito"));
        assert_eq!(substitute_family("CAMBRIA"), Some("Caladea"));
        assert_eq!(substitute_family("Georgia"), Some("Gelasio"));
        assert_eq!(substitute_family(" arial "), Some("Arimo"));
        assert_eq!(substitute_family("Arial Nova"), Some("Arimo"));
        // Arial Narrow is condensed - its own narrow clone, NOT full-width Arimo.
        assert_eq!(substitute_family("Arial Narrow"), Some("Liberation Sans Narrow"));
        assert_eq!(substitute_family(" arial narrow "), Some("Liberation Sans Narrow"));
        assert_eq!(substitute_family("Times New Roman"), Some("Tinos"));
        // Legal serifs -> the TeX Gyre clones (case / spacing variants).
        assert_eq!(substitute_family("Century Schoolbook"), Some("TeX Gyre Schola"));
        assert_eq!(substitute_family("book antiqua"), Some("TeX Gyre Pagella"));
        assert_eq!(substitute_family("Palatino Linotype"), Some("TeX Gyre Pagella"));
        assert_eq!(substitute_family("Bookman Old Style"), Some("TeX Gyre Bonum"));
    }

    #[test]
    fn passes_through_unknown_families() {
        assert_eq!(substitute_family("Inter"), None);
        assert_eq!(resolve_family("Inter"), "Inter");
    }

    #[test]
    fn bundles_all_families() {
        // 7 MS-core clones + 3 TeX Gyre legal serifs (4 styles each) + 1 DejaVu fallback face.
        assert_eq!(bundled_fonts().len(), 41);
    }

    /// `line_height_factor`'s constants are the bundled clones' real `hhea` metrics. Re-derive each
    /// from the font bytes (a minimal big-endian sfnt table walk) so a swapped font file can't let
    /// the hardcoded factor silently drift away from what the renderer actually shapes with.
    #[test]
    fn line_height_factor_matches_bundled_hhea() {
        fn hhea_factor(d: &[u8]) -> f32 {
            let be16 = |o: usize| u16::from_be_bytes([d[o], d[o + 1]]);
            let be16i = |o: usize| i16::from_be_bytes([d[o], d[o + 1]]) as f32;
            let be32 = |o: usize| u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize;
            let (mut head, mut hhea) = (0usize, 0usize);
            for i in 0..be16(4) as usize {
                let off = 12 + i * 16;
                match &d[off..off + 4] {
                    b"head" => head = be32(off + 8),
                    b"hhea" => hhea = be32(off + 8),
                    _ => {}
                }
            }
            let upm = be16(head + 18) as f32;
            (be16i(hhea + 4) - be16i(hhea + 6) + be16i(hhea + 8)) / upm
        }
        for f in bundled_fonts().iter().filter(|f| !f.bold && !f.italic) {
            let family = resolve_family(f.substitutes);
            let declared = line_height_factor(family);
            // The TeX Gyre legal serifs are advance-compatible with the URW base-35 fonts, but their
            // OWN hhea (1.007-1.47) does not match Word's rendering of the MS fonts they stand in for
            // - so their factor is deliberately Word-tuned, not hhea-derived. Guard it stays a sane
            // serif value instead (the drift-prevention the hhea check gives the Croscore clones).
            if family.starts_with("TeX Gyre") {
                assert!(
                    (1.10..=1.30).contains(&declared),
                    "{family}: Word-tuned factor {declared} out of the sane serif range"
                );
                continue;
            }
            let derived = hhea_factor(f.data);
            assert!(
                (derived - declared).abs() < 0.001,
                "{family}: declared {declared} but font hhea gives {derived}"
            );
        }
    }
}
