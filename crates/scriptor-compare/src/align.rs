//! Two-tier block alignment - the single biggest lever on redline *noise*.
//!
//! Plain exact-match LCS is what makes naive diffs unusable: if every paragraph changed one word it
//! reports "deleted N paragraphs, inserted N" instead of "N edited paragraphs." So:
//!
//! 1. **Exact backbone.** Diff the paragraph *signatures* (style + text) - unique exact matches are
//!    high-confidence anchors (the patience-diff intuition).
//! 2. **Similarity gap-fill.** Between anchors, re-diff the leftover A-only and B-only blocks with a
//!    *fuzzy* equality predicate (Sorensen-Dice over word bags >= tau). A matched pair is an *edited*
//!    paragraph (the inline diff handles it); the rest are whole-block insert / delete.
//!
//! Both tiers are the same `diff_by` core - only the predicate changes.

use std::collections::HashMap;

use crate::diff::{diff_by, Op};

/// A bag (multiset) of lowercased word tokens, for similarity scoring.
#[derive(Debug, Clone, Default)]
pub struct WordBag {
    counts: HashMap<String, u32>,
    total: u32,
}

/// Build the word bag of a paragraph's text: lowercased maximal alphanumeric runs. Punctuation and
/// whitespace are ignored for similarity (they are noise at the block-matching level; the inline
/// diff still sees them).
pub fn word_bag(text: &str) -> WordBag {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut total = 0u32;
    let mut cur = String::new();
    let flush = |cur: &mut String, counts: &mut HashMap<String, u32>, total: &mut u32| {
        if !cur.is_empty() {
            *counts.entry(std::mem::take(cur)).or_insert(0) += 1;
            *total += 1;
        }
    };
    for c in text.chars() {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else {
            flush(&mut cur, &mut counts, &mut total);
        }
    }
    flush(&mut cur, &mut counts, &mut total);
    WordBag { counts, total }
}

/// Sorensen-Dice coefficient over two word bags: `2 * |A intersect B| / (|A| + |B|)`, in `[0, 1]`.
/// Two empty paragraphs score 1.0 (they pair as an edited/format-only change rather than a
/// delete+insert); one empty against a non-empty scores 0.0.
pub fn dice(a: &WordBag, b: &WordBag) -> f64 {
    if a.total == 0 && b.total == 0 {
        return 1.0;
    }
    let mut inter = 0u32;
    // Iterate the smaller bag for a touch less work; correctness is symmetric.
    let (small, large) = if a.counts.len() <= b.counts.len() { (a, b) } else { (b, a) };
    for (w, &ca) in &small.counts {
        if let Some(&cb) = large.counts.get(w) {
            inter += ca.min(cb);
        }
    }
    2.0 * inter as f64 / (a.total + b.total) as f64
}

/// One block-level alignment entry, in document order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// A-block `a` corresponds to B-block `b` (identical, or edited - the inline diff decides).
    Equal { a: usize, b: usize },
    /// A-block `a` is present only in the original (a whole-paragraph deletion).
    Delete { a: usize },
    /// B-block `b` is present only in the revised document (a whole-paragraph insertion).
    Insert { b: usize },
}

/// Align A's blocks to B's blocks. `sigs_*` are exact-match signatures (style + text); `bags_*` are
/// the word bags for fuzzy pairing; `tau` is the similarity threshold above which two unmatched
/// blocks are treated as an edited pair rather than delete + insert.
pub fn align(
    sigs_a: &[String],
    sigs_b: &[String],
    bags_a: &[WordBag],
    bags_b: &[WordBag],
    tau: f64,
) -> Vec<Align> {
    let backbone = diff_by(sigs_a.len(), sigs_b.len(), |i, j| sigs_a[i] == sigs_b[j]);

    let mut out = Vec::new();
    let mut dels: Vec<usize> = Vec::new();
    let mut ins: Vec<usize> = Vec::new();

    for op in backbone {
        match op {
            Op::Equal(a, b) => {
                resolve_gap(&mut out, &dels, &ins, bags_a, bags_b, tau);
                dels.clear();
                ins.clear();
                out.push(Align::Equal { a, b });
            }
            Op::Delete(a) => dels.push(a),
            Op::Insert(b) => ins.push(b),
        }
    }
    resolve_gap(&mut out, &dels, &ins, bags_a, bags_b, tau);

    // Word never deletes a document's final paragraph mark. If the alignment would delete A's last
    // paragraph *and* insert B's last, force them into an edited pair instead: the final paragraph is
    // replaced in place (content redlined) rather than structurally removed - which both matches Word
    // and sidesteps the "can't delete the final ¶" case for a wholly-replaced body.
    if let (Some(al), Some(bl)) = (sigs_a.len().checked_sub(1), sigs_b.len().checked_sub(1)) {
        let has_del = out.iter().any(|e| matches!(e, Align::Delete { a } if *a == al));
        let has_ins = out.iter().any(|e| matches!(e, Align::Insert { b } if *b == bl));
        if has_del && has_ins {
            out.retain(|e| {
                !matches!(e, Align::Delete { a } if *a == al)
                    && !matches!(e, Align::Insert { b } if *b == bl)
            });
            out.push(Align::Equal { a: al, b: bl });
        }
    }
    out
}

/// Pair the A-only (`dels`) and B-only (`ins`) blocks of one gap by similarity, order-preserving.
fn resolve_gap(
    out: &mut Vec<Align>,
    dels: &[usize],
    ins: &[usize],
    bags_a: &[WordBag],
    bags_b: &[WordBag],
    tau: f64,
) {
    if dels.is_empty() && ins.is_empty() {
        return;
    }
    // Fuzzy-pair within the gap by similarity. The `diff_by` core caps its Myers frontier, so a gap
    // whose paragraphs are genuinely unrelated degrades to plain delete-all + insert-all rather than
    // running away quadratically - while a gap of lightly-edited paragraphs (the common case) still
    // pairs them as edits.
    let inner = diff_by(dels.len(), ins.len(), |x, y| dice(&bags_a[dels[x]], &bags_b[ins[y]]) >= tau);
    for op in inner {
        match op {
            Op::Equal(x, y) => out.push(Align::Equal { a: dels[x], b: ins[y] }),
            Op::Delete(x) => out.push(Align::Delete { a: dels[x] }),
            Op::Insert(y) => out.push(Align::Insert { b: ins[y] }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sigs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    fn bags(v: &[&str]) -> Vec<WordBag> {
        v.iter().map(|s| word_bag(s)).collect()
    }

    #[test]
    fn identical_all_equal() {
        let a = ["one", "two", "three"];
        let al = align(&sigs(&a), &sigs(&a), &bags(&a), &bags(&a), 0.5);
        assert!(al.iter().all(|e| matches!(e, Align::Equal { .. })));
        assert_eq!(al.len(), 3);
    }

    #[test]
    fn edited_paragraph_is_paired_not_replaced() {
        // Middle paragraph changed a word: must be one Equal (edited), not a Delete + Insert.
        let a = ["the header", "Party A shall pay the sum", "the footer"];
        let b = ["the header", "Party A shall pay the amount", "the footer"];
        let al = align(&sigs(&a), &sigs(&b), &bags(&a), &bags(&b), 0.5);
        assert_eq!(al.len(), 3, "{al:?}");
        assert!(al.iter().all(|e| matches!(e, Align::Equal { .. })), "{al:?}");
    }

    #[test]
    fn unrelated_paragraph_is_delete_plus_insert() {
        let a = ["shared", "wholly different original text here", "tail"];
        let b = ["shared", "an utterly unrelated brand new clause", "tail"];
        let al = align(&sigs(&a), &sigs(&b), &bags(&a), &bags(&b), 0.5);
        assert!(al.iter().any(|e| matches!(e, Align::Delete { .. })), "{al:?}");
        assert!(al.iter().any(|e| matches!(e, Align::Insert { .. })), "{al:?}");
    }

    #[test]
    fn pure_insertion_and_deletion() {
        let a = ["a", "b"];
        let b = ["a", "x", "b"];
        let al = align(&sigs(&a), &sigs(&b), &bags(&a), &bags(&b), 0.5);
        assert!(al.iter().filter(|e| matches!(e, Align::Insert { .. })).count() == 1, "{al:?}");
    }
}
