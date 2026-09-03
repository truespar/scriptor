//! The semantic overlay: a *separable* layer of judgment on top of the deterministic change
//! manifest. An agent / LLM reads the manifest (ground truth) and returns annotations - materiality,
//! category, a natural-language summary, risk flags - each citing a specific change. This module owns
//! the annotation **schema** and, crucially, the **trust boundary**: [`Manifest::annotate`] validates
//! that every annotation references a real change, so the overlay can *describe* the redline but never
//! invent or alter it. The LLM call itself is the integrator's (it lives outside this crate); this is
//! the deterministic scaffolding around it.
//!
//! The seam: send [`Manifest::to_json`] to the model, get back a `Vec<Annotation>` as JSON,
//! deserialize it, and call [`Manifest::annotate`]. A hallucinated or stale citation is rejected, not
//! silently trusted.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::manifest::{Change, Manifest};

/// How much a change matters - the headline filter for a reviewer. `Trivial` is cosmetic (whitespace,
/// a typo, renumbering, a formatting-only change); `Substantive` changes meaning, an obligation, or a
/// legally-operative value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Materiality {
    Trivial,
    Substantive,
}

/// One semantic annotation over a deterministic change. It **cites** a change (by its index in the
/// manifest, document order) and layers judgment on it - it never alters the redline. `category` is a
/// free-form label the model assigns (e.g. "obligation", "money", "date", "party", "definition",
/// "clause-removed"); `risks` are human-readable flags (e.g. "'shall' -> 'may' weakens the
/// obligation").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    /// Index of the annotated change in the manifest's `changes` (document order).
    pub change: usize,
    pub materiality: Materiality,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
}

/// A deterministic manifest with a **validated** semantic overlay: every annotation references a real
/// change and at most one annotation exists per change. The manifest (ground truth) is preserved
/// verbatim; annotations are a separate, index-keyed layer - so accepting the overlay never changes
/// what the redline says, only how it is described.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotatedManifest {
    pub manifest: Manifest,
    pub annotations: Vec<Annotation>,
}

impl Manifest {
    /// Attach a semantic overlay, enforcing the trust boundary: every annotation must cite a change
    /// that exists in this manifest (a hallucinated or stale index is rejected) and no change may be
    /// annotated twice. The deterministic manifest is moved in unchanged.
    pub fn annotate(self, annotations: Vec<Annotation>) -> Result<AnnotatedManifest> {
        let n = self.changes.len();
        let mut seen = vec![false; n];
        for a in &annotations {
            if a.change >= n {
                bail!("annotation cites change #{}, but the manifest has {} change(s)", a.change, n);
            }
            if seen[a.change] {
                bail!("change #{} is annotated more than once", a.change);
            }
            seen[a.change] = true;
        }
        Ok(AnnotatedManifest { manifest: self, annotations })
    }
}

impl AnnotatedManifest {
    /// The annotation for the change at `index`, if any.
    pub fn annotation_for(&self, index: usize) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.change == index)
    }

    /// Every change paired with its annotation (annotated changes only), in document order.
    pub fn annotated(&self) -> Vec<(&Change, &Annotation)> {
        let mut pairs: Vec<(&Change, &Annotation)> = self
            .annotations
            .iter()
            .filter_map(|a| self.manifest.changes.get(a.change).map(|c| (c, a)))
            .collect();
        pairs.sort_by_key(|(_, a)| a.change);
        pairs
    }

    /// The substantive changes (materiality `Substantive`) paired with their annotation - the short
    /// list a reviewer actually cares about.
    pub fn substantive(&self) -> Vec<(&Change, &Annotation)> {
        self.annotated().into_iter().filter(|(_, a)| a.materiality == Materiality::Substantive).collect()
    }

    /// Every risk flag raised across all annotations, in document order.
    pub fn risks(&self) -> Vec<&str> {
        self.annotated().iter().flat_map(|(_, a)| a.risks.iter().map(String::as_str)).collect()
    }

    /// Pretty-printed JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("AnnotatedManifest serializes")
    }

    /// A one-line summary: total changes, how many are annotated / substantive, and the risk count.
    pub fn summary(&self) -> String {
        let substantive = self.annotations.iter().filter(|a| a.materiality == Materiality::Substantive).count();
        format!(
            "{} change(s): {} annotated ({} substantive), {} risk flag(s)",
            self.manifest.changes.len(),
            self.annotations.len(),
            substantive,
            self.risks().len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Change, ChangeKind};

    fn manifest(n: usize) -> Manifest {
        Manifest {
            changes: (0..n).map(|i| Change::new(i as u64 + 1, ChangeKind::Replace, i)).collect(),
            ..Default::default()
        }
    }

    fn ann(change: usize, m: Materiality) -> Annotation {
        Annotation { change, materiality: m, category: String::new(), summary: String::new(), risks: Vec::new() }
    }

    #[test]
    fn valid_overlay_attaches() {
        let m = manifest(3);
        let a = m.annotate(vec![ann(0, Materiality::Substantive), ann(2, Materiality::Trivial)]).unwrap();
        assert_eq!(a.annotations.len(), 2);
        assert_eq!(a.substantive().len(), 1);
    }

    #[test]
    fn hallucinated_citation_is_rejected() {
        let m = manifest(2);
        let err = m.annotate(vec![ann(5, Materiality::Substantive)]).unwrap_err();
        assert!(err.to_string().contains("cites change #5"), "{err}");
    }

    #[test]
    fn double_annotation_is_rejected() {
        let m = manifest(2);
        let err = m.annotate(vec![ann(1, Materiality::Trivial), ann(1, Materiality::Substantive)]).unwrap_err();
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn risks_and_summary_aggregate() {
        let m = manifest(2);
        let mut a0 = ann(0, Materiality::Substantive);
        a0.risks = vec!["'shall' -> 'may' weakens the obligation".into()];
        a0.category = "obligation".into();
        let annotated = m.annotate(vec![a0, ann(1, Materiality::Trivial)]).unwrap();
        assert_eq!(annotated.risks().len(), 1);
        assert!(annotated.summary().contains("2 change(s)"));
        assert!(annotated.summary().contains("1 substantive"));
    }

    #[test]
    fn round_trips_through_json() {
        let m = manifest(2);
        let annotated = m.annotate(vec![ann(0, Materiality::Substantive)]).unwrap();
        let json = annotated.to_json();
        let back: AnnotatedManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, annotated);
    }

    #[test]
    fn empty_overlay_is_valid() {
        let m = manifest(2);
        let a = m.annotate(vec![]).unwrap();
        assert_eq!(a.annotations.len(), 0);
        assert!(a.substantive().is_empty());
    }
}
