//! The single diff core: an O(ND) Myers diff parameterized by an *index* equality predicate.
//!
//! Everything in the engine reduces to this one function - inline word diff (equality = token text
//! equal), block backbone (equality = paragraph signature equal), and fuzzy block pairing within a
//! gap (equality = similarity >= tau). Keeping one, well-tested core is deliberate: a diff bug is a
//! correctness bug, and there is exactly one place for it to live.
//!
//! Reference: Eugene W. Myers, "An O(ND) Difference Algorithm and Its Variations" (1986).

/// One element of an edit script, in document order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `a[ai]` matches `b[bi]` (predicate held).
    Equal(usize, usize),
    /// `a[ai]` is present only in A.
    Delete(usize),
    /// `b[bi]` is present only in B.
    Insert(usize),
}

/// The Myers frontier is capped at this edit distance. Beyond it two sequences are "too different"
/// to align affordably - the diff of the differing middle degrades to delete-all + insert-all
/// (correct, just not minimal). This bounds the cost at O(prefix+suffix + MAX_D^2): without it, two
/// wholesale-different documents are O(N^2), which - synchronously on the browser's main thread -
/// freezes the tab. A real edit's distance is far below the cap; a genuine rewrite *is* all-changed.
const MAX_D: usize = 2048;

/// Diff two sequences of lengths `n` (A) and `m` (B) using `eq(i, j)` to decide whether `a[i]`
/// equals `b[j]`. Returns an ordered edit script - minimal (shortest) unless the sequences differ by
/// more than [`MAX_D`], in which case the differing middle is marked wholly changed. Deterministic.
/// A common prefix / suffix is matched in O(n) first (the usual case for two document versions), so
/// the quadratic Myers core only ever runs on the genuinely-differing middle.
pub fn diff_by<F: Fn(usize, usize) -> bool>(n: usize, m: usize, eq: F) -> Vec<Op> {
    // Match the common prefix, then the common suffix, then diff only the middle.
    let mut p = 0;
    while p < n && p < m && eq(p, p) {
        p += 1;
    }
    let mut s = 0;
    while s < n - p && s < m - p && eq(n - 1 - s, m - 1 - s) {
        s += 1;
    }
    let (mn, mm) = (n - p - s, m - p - s);

    let mut ops = Vec::with_capacity(p + mn.max(mm) + s);
    for i in 0..p {
        ops.push(Op::Equal(i, i));
    }
    match myers(mn, mm, |i, j| eq(p + i, p + j)) {
        Some(mid) => {
            for op in mid {
                ops.push(match op {
                    Op::Equal(i, j) => Op::Equal(p + i, p + j),
                    Op::Delete(i) => Op::Delete(p + i),
                    Op::Insert(j) => Op::Insert(p + j),
                });
            }
        }
        // Over the cap: the whole middle is treated as changed (delete all of A, insert all of B).
        None => {
            ops.extend((0..mn).map(|i| Op::Delete(p + i)));
            ops.extend((0..mm).map(|j| Op::Insert(p + j)));
        }
    }
    for k in 0..s {
        ops.push(Op::Equal(n - s + k, m - s + k));
    }
    ops
}

/// The Myers core over an (already prefix/suffix-trimmed) middle. Returns `None` if the edit distance
/// exceeds [`MAX_D`] (the caller then treats the middle as wholly changed).
fn myers<F: Fn(usize, usize) -> bool>(n: usize, m: usize, eq: F) -> Option<Vec<Op>> {
    if n == 0 && m == 0 {
        return Some(Vec::new());
    }
    if n == 0 {
        return Some((0..m).map(Op::Insert).collect());
    }
    if m == 0 {
        return Some((0..n).map(Op::Delete).collect());
    }

    let max = (n + m).min(MAX_D);
    let offset = max as isize; // shift k (in -max..=max) into a 0-based array index
    let mut v = vec![0isize; 2 * max + 1];
    // Snapshot of the frontier taken *before* each round d, so backtracking can replay it.
    let mut trace: Vec<Vec<isize>> = Vec::with_capacity(max + 1);

    let mut found_d = None;
    'outer: for d in 0..=(max as isize) {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + offset) as usize;
            // Choose the incoming move: down (insert, advance in B) or right (delete, advance in A).
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1] // down: came from k+1, x unchanged
            } else {
                v[idx - 1] + 1 // right: came from k-1, x advances
            };
            let mut y = x - k;
            // Extend the diagonal (the "snake") across equal elements.
            while (x as usize) < n && (y as usize) < m && eq(x as usize, y as usize) {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x as usize >= n && y as usize >= m {
                found_d = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }

    // The endpoint was not reached within the cap: signal "too different" to the caller.
    let d_final = found_d?;

    let mut ops = Vec::new();
    let mut x = n as isize;
    let mut y = m as isize;
    for d in (0..=d_final).rev() {
        let vd = &trace[d as usize];
        let k = x - y;
        let idx = (k + offset) as usize;
        // Which neighbour did round d descend from? Mirror the forward choice exactly.
        let down = k == -d || (k != d && vd[idx - 1] < vd[idx + 1]);
        let prev_k = if down { k + 1 } else { k - 1 };
        let prev_idx = (prev_k + offset) as usize;
        let prev_x = vd[prev_idx];
        let prev_y = prev_x - prev_k;

        // Walk the snake back (diagonal matches).
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal((x - 1) as usize, (y - 1) as usize));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if down {
                ops.push(Op::Insert((y - 1) as usize));
            } else {
                ops.push(Op::Delete((x - 1) as usize));
            }
            x = prev_x;
            y = prev_y;
        }
    }
    ops.reverse();
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reconstruct B from A by applying an edit script; the property every Myers result must satisfy.
    fn apply(a: &[char], b: &[char], ops: &[Op]) -> Vec<char> {
        let mut out = Vec::new();
        for op in ops {
            match *op {
                Op::Equal(ai, bi) => {
                    assert_eq!(a[ai], b[bi], "Equal must align identical elements");
                    out.push(a[ai]);
                }
                Op::Delete(_) => {}
                Op::Insert(bi) => out.push(b[bi]),
            }
        }
        out
    }

    fn check(a: &str, b: &str) {
        let av: Vec<char> = a.chars().collect();
        let bv: Vec<char> = b.chars().collect();
        let ops = diff_by(av.len(), bv.len(), |i, j| av[i] == bv[j]);
        assert_eq!(apply(&av, &bv, &ops), bv, "A={a:?} B={b:?} ops={ops:?}");
    }

    #[test]
    fn reconstructs_b() {
        check("", "");
        check("abc", "abc");
        check("", "abc");
        check("abc", "");
        check("abcabba", "cbabac");
        check("the quick brown fox", "the slow brown cat");
        check("shall indemnify", "may indemnify");
        check("aaaa", "aa");
        check("Party A shall pay", "Party B shall not pay");
    }

    #[test]
    fn minimal_for_pure_insert() {
        let a: Vec<char> = "abc".chars().collect();
        let b: Vec<char> = "abXc".chars().collect();
        let ops = diff_by(a.len(), b.len(), |i, j| a[i] == b[j]);
        let inserts = ops.iter().filter(|o| matches!(o, Op::Insert(_))).count();
        let deletes = ops.iter().filter(|o| matches!(o, Op::Delete(_))).count();
        assert_eq!((inserts, deletes), (1, 0));
    }

    #[test]
    fn determinism() {
        let a: Vec<char> = "abcabba".chars().collect();
        let b: Vec<char> = "cbabac".chars().collect();
        let first = diff_by(a.len(), b.len(), |i, j| a[i] == b[j]);
        for _ in 0..8 {
            assert_eq!(diff_by(a.len(), b.len(), |i, j| a[i] == b[j]), first);
        }
    }
}
