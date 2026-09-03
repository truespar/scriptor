//! `numbering.xml`: list definitions and the runtime-synthesized lists.
//! 
//! Maps a paragraph's `numId`/`ilvl` to a concrete marker format, indent and start
//! value, and synthesizes definitions for lists created in the editor.

use super::*;

// ── numbering (numbering.xml -> list markers) ────────────────────────────────

/// One numbering level: format (`decimal`/`bullet`/...), the level text template (`%1.`), the start
/// value, and the level's indent (twips).
#[derive(Debug, Clone, Default)]
pub struct NumLevel {
    pub fmt: String,
    pub text: String,
    pub start: i32,
    pub ind_left: i32,
    pub ind_hanging: i32,
}

/// The marker style of a synthesized list: the editor's **Bullets** button (one bullet kind) and the
/// **Numbering** button + its format picker (`1.` / `a.` / `A.` / `i.` / `I.`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    Bullet,
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
}

impl ListFormat {
    /// The OOXML `w:numFmt` token this kind uses at level 0 - what list reuse + the toolbar's
    /// active-format check key on (a list's identity is its level-0 format).
    pub fn level0_numfmt(self) -> &'static str {
        match self {
            ListFormat::Bullet => "bullet",
            ListFormat::Decimal => "decimal",
            ListFormat::LowerLetter => "lowerLetter",
            ListFormat::UpperLetter => "upperLetter",
            ListFormat::LowerRoman => "lowerRoman",
            ListFormat::UpperRoman => "upperRoman",
        }
    }

    /// Parse a `w:numFmt` token (a toolbar choice, or a paragraph's resolved level-0 format) back to a
    /// kind. `bullet` -> Bullet; unknown / `decimal` -> Decimal.
    pub fn from_numfmt(s: &str) -> ListFormat {
        match s {
            "bullet" => ListFormat::Bullet,
            "lowerLetter" => ListFormat::LowerLetter,
            "upperLetter" => ListFormat::UpperLetter,
            "lowerRoman" => ListFormat::LowerRoman,
            "upperRoman" => ListFormat::UpperRoman,
            _ => ListFormat::Decimal,
        }
    }
}

/// A numbering definition created at runtime (the editor's "Bullets" / "Numbering" buttons on a doc
/// that lacks a suitable list), tracked so [`Numbering::synth_xml`] can write it into `numbering.xml`.
#[derive(Debug, Clone, Copy)]
pub struct SynthList {
    pub num_id: i32,
    pub abstract_id: i32,
    pub bullet: bool,
}

/// Resolved `word/numbering.xml`: numId -> abstractNumId, and abstractNumId -> levels.
#[derive(Debug, Clone, Default)]
pub struct Numbering {
    num_to_abstract: std::collections::HashMap<i32, i32>,
    abstracts: std::collections::HashMap<i32, std::collections::HashMap<i32, NumLevel>>,
    /// Definitions synthesized this session (not from the imported part) - written into `numbering.xml`
    /// on save (the imported definitions are preserved verbatim; these are appended).
    synth: Vec<SynthList>,
}

impl Numbering {
    pub fn is_empty(&self) -> bool {
        self.num_to_abstract.is_empty()
    }
    /// The abstract numbering id a `numId` maps to.
    pub fn abstract_id(&self, num_id: i32) -> Option<i32> {
        self.num_to_abstract.get(&num_id).copied()
    }
    /// The level definition for `(num_id, ilvl)`.
    pub fn level(&self, num_id: i32, ilvl: i32) -> Option<&NumLevel> {
        let aid = self.num_to_abstract.get(&num_id)?;
        self.abstracts.get(aid)?.get(&ilvl)
    }

    /// The `numId` for a list of `fmt`: an existing definition whose level-0 format matches (so applying
    /// a list reuses the document's own style of that kind, including ones synthesized earlier this
    /// session so repeated toggles don't pile up definitions), else a freshly synthesized definition
    /// (recorded for save). Backs the editor's Bullets / Numbering buttons + the numbering format picker.
    pub fn ensure_list(&mut self, fmt: ListFormat) -> i32 {
        let want = fmt.level0_numfmt();
        let mut reusable: Vec<i32> = self
            .num_to_abstract
            .iter()
            .filter(|(_, aid)| {
                self.abstracts.get(aid).and_then(|m| m.get(&0)).is_some_and(|l0| l0.fmt == want)
            })
            .map(|(&num_id, _)| num_id)
            .collect();
        if !reusable.is_empty() {
            reusable.sort_unstable(); // deterministic pick
            return reusable[0];
        }
        let abstract_id = self.abstracts.keys().copied().max().map(|m| m + 1).unwrap_or(0);
        let num_id = self.num_to_abstract.keys().copied().max().map(|m| m + 1).unwrap_or(1).max(1);
        self.abstracts.insert(abstract_id, build_list_levels(fmt));
        self.num_to_abstract.insert(num_id, abstract_id);
        self.synth.push(SynthList { num_id, abstract_id, bullet: matches!(fmt, ListFormat::Bullet) });
        num_id
    }

    /// Whether any definition was synthesized this session (so save must patch `numbering.xml`).
    pub fn has_synth(&self) -> bool {
        !self.synth.is_empty()
    }

    /// The `numId` of an existing definition whose level-0 format matches `fmt` (so a runtime list of
    /// this kind reuses the document's own def of that kind - imported OR synthesized), lowest id first
    /// for determinism. `None` when no such def exists yet. The loro-backed [`CollabDoc::ensure_list`]
    /// uses this to dedupe before synthesizing a fresh def.
    pub fn reusable_num_id(&self, fmt: ListFormat) -> Option<i32> {
        let want = fmt.level0_numfmt();
        self.num_to_abstract
            .iter()
            .filter(|(_, aid)| {
                self.abstracts.get(aid).and_then(|m| m.get(&0)).is_some_and(|l0| l0.fmt == want)
            })
            .map(|(&num_id, _)| num_id)
            .min()
    }

    /// Insert a runtime-synthesized list definition with an EXPLICIT `num_id` (the loro-backed flow
    /// assigns the id, persists it in the [`NUM_SYNTH`] map, then mirrors it here). Idempotent: a
    /// `num_id` already present (already reconciled, or imported) is left untouched, so repeated
    /// reconciliation after a merge never piles up duplicate abstracts or `synth` entries. Allocates a
    /// fresh abstract id (above any existing) and builds the standard nine levels for `fmt`.
    pub fn insert_synth(&mut self, num_id: i32, fmt: ListFormat) {
        if self.num_to_abstract.contains_key(&num_id) {
            return; // already known (reconciled earlier, or an imported def shares the id)
        }
        let abstract_id = self.abstracts.keys().copied().max().map(|m| m + 1).unwrap_or(0);
        self.abstracts.insert(abstract_id, build_list_levels(fmt));
        self.num_to_abstract.insert(num_id, abstract_id);
        self.synth.push(SynthList { num_id, abstract_id, bullet: matches!(fmt, ListFormat::Bullet) });
    }

    /// Rebuild the in-memory synthesized definitions from the loro [`NUM_SYNTH`] map (`numId -> numFmt
    /// token`). This is how a runtime list survives a reopen / arrives from a peer: the def's identity
    /// lives in loro, and on load the full level table is regenerated here so `level()`, the marker
    /// computation, and `synth_xml()` all see it. Idempotent (see [`insert_synth`]); safe to call after
    /// every `merge` / construct.
    pub fn reconcile_synth(&mut self, synth: &std::collections::HashMap<i32, String>) {
        // Deterministic order so abstract ids assigned during rebuild are reproducible across peers.
        let mut ids: Vec<i32> = synth.keys().copied().collect();
        ids.sort_unstable();
        for num_id in ids {
            let fmt = ListFormat::from_numfmt(&synth[&num_id]);
            self.insert_synth(num_id, fmt);
        }
    }

    /// The next free `numId` for a freshly synthesized runtime list: above every existing synth id and at
    /// least [`SYNTH_NUM_BASE`], so it never collides with an imported `numbering.xml` id.
    pub fn next_synth_num_id(&self) -> i32 {
        let max_synth = self.synth.iter().map(|d| d.num_id).max().unwrap_or(0);
        (max_synth + 1).max(SYNTH_NUM_BASE)
    }

    /// The `<w:abstractNum>` + `<w:num>` XML for every synthesized definition (schema order: all
    /// abstracts, then all nums), for injection into `numbering.xml` on save.
    pub fn synth_xml(&self) -> String {
        let mut s = String::new();
        for d in &self.synth {
            s.push_str(&format!("<w:abstractNum w:abstractNumId=\"{}\">", d.abstract_id));
            if let Some(levels) = self.abstracts.get(&d.abstract_id) {
                for lvl in 0..9 {
                    if let Some(l) = levels.get(&lvl) {
                        s.push_str(&format!(
                            "<w:lvl w:ilvl=\"{lvl}\"><w:start w:val=\"{}\"/><w:numFmt w:val=\"{}\"/>\
<w:lvlText w:val=\"{}\"/><w:lvlJc w:val=\"left\"/>\
<w:pPr><w:ind w:left=\"{}\" w:hanging=\"{}\"/></w:pPr></w:lvl>",
                            l.start,
                            xml_escape(&l.fmt),
                            xml_escape(&l.text),
                            l.ind_left,
                            l.ind_hanging
                        ));
                    }
                }
            }
            s.push_str("</w:abstractNum>");
        }
        for d in &self.synth {
            s.push_str(&format!(
                "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{}\"/></w:num>",
                d.num_id, d.abstract_id
            ));
        }
        s
    }
}

/// Build the nine standard levels (Word uses 0-8) of a synthesized list. A **bullet** list cycles the
/// glyph `• o ▪` by depth. The **decimal** list is Word's default multilevel outline: the number format
/// cycles `decimal -> lowerLetter -> lowerRoman` (1. a. i.) by depth, so demoting turns `1.` into `a.`
/// then `i.`. A **picked** number format (`a.` / `A.` / `i.` / `I.`) applies *uniformly* at every level
/// (deeper levels just indent further) - matching what choosing a Numbering-library format does. Each
/// level shows only its own counter (`%n.`, not the cumulative `1.a.i.` legal form) and is indented a
/// further 0.25" with a 0.25" hanging indent (Word's default list geometry).
fn build_list_levels(fmt: ListFormat) -> std::collections::HashMap<i32, NumLevel> {
    const BULLETS: [&str; 3] = ["\u{2022}", "o", "\u{25AA}"];
    const CASCADE: [&str; 3] = ["decimal", "lowerLetter", "lowerRoman"];
    let mut m = std::collections::HashMap::new();
    for lvl in 0..9 {
        let (numfmt, text): (&str, String) = match fmt {
            ListFormat::Bullet => ("bullet", BULLETS[(lvl % 3) as usize].to_string()),
            ListFormat::Decimal => (CASCADE[(lvl % 3) as usize], format!("%{}.", lvl + 1)),
            _ => (fmt.level0_numfmt(), format!("%{}.", lvl + 1)),
        };
        m.insert(
            lvl,
            NumLevel { fmt: numfmt.to_string(), text, start: 1, ind_left: 720 * (lvl + 1), ind_hanging: 360 },
        );
    }
    m
}

/// Parse `word/numbering.xml`: the `w:abstractNum` level definitions + the `w:num` -> abstract map.
pub fn parse_numbering(xml: &[u8]) -> Numbering {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Numbering::default();

    let mut cur_abstract: Option<i32> = None;
    let mut cur_ilvl: Option<i32> = None;
    let mut cur_level = NumLevel::default();
    let mut cur_num: Option<i32> = None; // inside <w:num>

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"w:abstractNum" => {
                    cur_abstract = attr(&e, b"w:abstractNumId").and_then(|s| s.parse().ok());
                }
                b"w:lvl" if cur_abstract.is_some() && cur_num.is_none() => {
                    cur_ilvl = attr(&e, b"w:ilvl").and_then(|s| s.parse().ok());
                    cur_level = NumLevel::default();
                }
                b"w:start" if cur_ilvl.is_some() => {
                    cur_level.start = attr(&e, b"w:val").and_then(|s| s.parse().ok()).unwrap_or(1);
                }
                b"w:numFmt" if cur_ilvl.is_some() => {
                    cur_level.fmt = attr(&e, b"w:val").unwrap_or_default();
                }
                b"w:lvlText" if cur_ilvl.is_some() => {
                    cur_level.text = attr(&e, b"w:val").unwrap_or_default();
                }
                b"w:ind" if cur_ilvl.is_some() => {
                    cur_level.ind_left = attr(&e, b"w:left").and_then(|s| s.parse().ok()).unwrap_or(0);
                    cur_level.ind_hanging =
                        attr(&e, b"w:hanging").and_then(|s| s.parse().ok()).unwrap_or(0);
                }
                b"w:num" => {
                    cur_num = attr(&e, b"w:numId").and_then(|s| s.parse().ok());
                }
                b"w:abstractNumId" if cur_num.is_some() => {
                    if let (Some(n), Some(a)) =
                        (cur_num, attr(&e, b"w:val").and_then(|s| s.parse::<i32>().ok()))
                    {
                        out.num_to_abstract.insert(n, a);
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"w:lvl" => {
                    if let (Some(a), Some(l)) = (cur_abstract, cur_ilvl.take()) {
                        out.abstracts.entry(a).or_default().insert(l, std::mem::take(&mut cur_level));
                    }
                }
                b"w:abstractNum" => cur_abstract = None,
                b"w:num" => cur_num = None,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}
