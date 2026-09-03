//! The change manifest: a deterministic, machine-readable description of every difference, derived
//! from the same pass that emits the redline (so it is provably consistent with what a reviewer
//! sees). This is the surface an agent consumes and reasons over - and the anchor the semantic
//! overlay cites. It never contains judgment; it is ground truth.

use serde::{Deserialize, Serialize};

/// The nature of one change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    /// Text inserted within an existing paragraph.
    Insert,
    /// Text deleted from an existing paragraph.
    Delete,
    /// Text replaced (a deletion and an insertion at the same spot).
    Replace,
    /// A whole paragraph inserted (present only in the revised document).
    ParaInsert,
    /// A whole paragraph deleted (present only in the original document).
    ParaDelete,
    /// Run formatting changed over unchanged text (`w:rPrChange`).
    Format,
    /// Paragraph properties / style changed (`w:pPrChange`).
    ParaFormat,
    /// A whole table row inserted (`w:trPr/w:ins`).
    TableRowInsert,
    /// A whole table row deleted (`w:trPr/w:del`).
    TableRowDelete,
    /// A whole table column deleted (`w:tcPr/w:cellDel` on every cell of the column).
    TableColumnDelete,
    /// A paragraph relocated - deleted here, inserted elsewhere (`w:moveFrom`/`w:moveTo`).
    Move,
}

/// One entry in the manifest. `before`/`after` are the affected text (empty where a side does not
/// apply). `para` is the paragraph's index in the *original* document (canonical addressing); a
/// paragraph inserted in the revised document reports the original-side index it follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The tracked-change revision id stamped on the emitted `w:ins`/`w:del`/… (ties the manifest
    /// entry to the exact revision in the document).
    pub id: u64,
    pub kind: ChangeKind,
    pub para: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub before: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub after: String,
}

impl Change {
    pub(crate) fn new(id: u64, kind: ChangeKind, para: usize) -> Self {
        Self { id, kind, para, before: String::new(), after: String::new() }
    }
    pub(crate) fn before(mut self, s: impl Into<String>) -> Self {
        self.before = s.into();
        self
    }
    pub(crate) fn after(mut self, s: impl Into<String>) -> Self {
        self.after = s.into();
        self
    }
}

/// How one aligned block relates its original and revised sides - the driver of the side-by-side
/// view's semantic scroll-lock + per-paragraph highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AlignKind {
    /// The paragraph is present and textually identical on both sides (`a` ↔ `b`).
    Equal,
    /// The paragraph is present on both sides but its text differs (an edited pair; the inline diff
    /// applies). Both `a` and `b` are set.
    Edited,
    /// Present only in the original (a whole-paragraph deletion). Only `a` is set.
    Delete,
    /// Present only in the revised document (a whole-paragraph insertion). Only `b` is set.
    Insert,
}

/// One block of the original↔revised paragraph correspondence, in document order. `a` is the original
/// paragraph index, `b` the revised paragraph index; a side is absent for a pure insert / delete. The
/// side-by-side view scroll-locks on the `Equal`/`Edited` anchors (both indices present) and highlights
/// per `kind`. A proportional fallback is used when a comparison emits no alignment (see the doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<usize>,
    pub kind: AlignKind,
}

/// The full manifest for one comparison, ordered by document position (top to bottom).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub changes: Vec<Change>,
    /// The paragraph-level original↔revised correspondence, for the side-by-side view. Empty when the
    /// comparison could not produce one (the view then falls back to proportional scroll-sync).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alignment: Vec<AlignEntry>,
}

impl Manifest {
    /// Pretty-printed JSON (stable field order via the struct definition).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("Manifest serializes")
    }

    /// Counts per kind, for a one-line summary.
    pub fn summary(&self) -> String {
        use ChangeKind::*;
        let count = |k: ChangeKind| self.changes.iter().filter(|c| c.kind == k).count();
        format!(
            "{} change(s): {} ins, {} del, {} replace, {} \u{00b6}+, {} \u{00b6}-, {} fmt, {} \u{00b6}fmt, {} row+, {} row-, {} col-, {} move",
            self.changes.len(),
            count(Insert),
            count(Delete),
            count(Replace),
            count(ParaInsert),
            count(ParaDelete),
            count(Format),
            count(ParaFormat),
            count(TableRowInsert),
            count(TableRowDelete),
            count(TableColumnDelete),
            count(Move),
        )
    }
}
