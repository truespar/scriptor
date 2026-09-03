//! A lightweight performance guard. Comparison runs synchronously in wasm on the browser's main
//! thread, so a slow compare freezes the tab; this pins a floor on the common case (two similar
//! versions of a large document) and exercises the wholesale-different path that the diff cap bounds.

use std::time::Instant;

use scriptor_compare::{compare, CompareOptions};
use scriptor_crdt::{CollabDoc, Run};

fn run(text: &str) -> Run {
    let mut r = Run::plain(text);
    r.font = Some("Calibri".into()); // Word stamps rFonts on almost every run
    r.size = Some(22);
    r
}

fn doc(n: usize, edit_every: usize, prefix: &str) -> Vec<u8> {
    let d = CollabDoc::new();
    for i in 0..n {
        let suffix = if edit_every > 0 && i % edit_every == 0 { " (revised)" } else { "" };
        let text = format!(
            "{prefix} clause {i}. The Supplier shall provide the Services in Schedule A{suffix}, and \
             the Buyer shall pay the fees set out therein within thirty days of each invoice."
        );
        d.append_paragraph(&[run(&text)], None).unwrap();
    }
    d.to_docx_bytes().unwrap()
}

#[test]
#[ignore = "perf bench - run with --ignored"]
fn compare_large_similar_documents() {
    let n = 1500;
    let a = doc(n, 0, "A");
    let b = doc(n, 25, "A"); // ~4% of paragraphs edited, mostly-equal
    let t = Instant::now();
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    let ms = t.elapsed().as_millis();
    eprintln!("similar {n}p: {ms} ms, {} changes", result.manifest.changes.len());
    assert!(ms < 8000, "compare took {ms} ms");
}

/// The legal-redline worst case: a large document where *most* paragraphs are edited (a word or two
/// changed each). Every edited paragraph emits several `suggest_*` ops, so this exercises the
/// emission cost (each op walks the block sequence + commits), which is what spins on a real doc.
#[test]
#[ignore = "perf bench - run with --ignored"]
fn compare_large_heavily_edited() {
    let n = 2000;
    let a = doc(n, 0, "A");
    let b = doc(n, 2, "A"); // ~50% of paragraphs edited
    let t = Instant::now();
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    let ms = t.elapsed().as_millis();
    eprintln!("heavy {n}p: {ms} ms, {} changes", result.manifest.changes.len());
    assert!(!result.redline.is_empty());
    // With the bulk-emission batch this is ~0.4s; the pre-batch O(N*changes) emission was ~16s. A
    // generous ceiling (room for slow CI) that still trips if the per-op O(N) rescans regress.
    assert!(ms < 5000, "heavy compare took {ms} ms - the O(N) per-op rescan may have regressed");
}

/// A wholesale-different comparison would be O(N^2) in the diff without the `MAX_D` cap; assert it
/// completes (the cap makes the differing middle degrade to delete-all + insert-all).
#[test]
#[ignore = "perf bench - run with --ignored"]
fn compare_wholesale_different_is_bounded() {
    let n = 600;
    let a = doc(n, 0, "Alpha unrelated preamble");
    let b = doc(n, 0, "Omega distinct heading text");
    let t = Instant::now();
    let result = compare(&a, &b, &CompareOptions::default()).unwrap();
    eprintln!("wholesale {n}p: {} ms, {} changes", t.elapsed().as_millis(), result.manifest.changes.len());
    assert!(!result.redline.is_empty());
}
