//! Which lines changed between two versions of a text: for reloading a file that changed on disk
//! as small edits rather than one replacement, and for carrying line positions, like
//! breakpoints, across a change made outside the editor.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// Old lines replaced by new ones. Either range may be empty, for an insertion or a deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old: Range<usize>,
    pub new: Range<usize>,
}

/// Past this many differences, the middle is treated as one replaced block: two unrelated files
/// would otherwise cost time for an answer nobody needs in detail.
const MAX_EDITS: usize = 2_000;

/// Lines with their line endings, so a missing final newline counts as a difference.
pub fn split_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The changes from `old` to `new`, by line, in order.
pub fn diff_lines(old: &str, new: &str) -> Vec<Hunk> {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    let hash = |line: &&str| {
        let mut hasher = DefaultHasher::new();
        line.hash(&mut hasher);
        hasher.finish()
    };
    let a: Vec<u64> = old_lines.iter().map(hash).collect();
    let b: Vec<u64> = new_lines.iter().map(hash).collect();
    let equal = |i: usize, j: usize| a[i] == b[j] && old_lines[i] == new_lines[j];

    // Common start and end, which is most of any real change.
    let mut start = 0;
    while start < a.len() && start < b.len() && equal(start, start) {
        start += 1;
    }
    let (mut old_end, mut new_end) = (a.len(), b.len());
    while old_end > start && new_end > start && equal(old_end - 1, new_end - 1) {
        old_end -= 1;
        new_end -= 1;
    }
    if start == old_end && start == new_end {
        return Vec::new();
    }
    match myers(start..old_end, start..new_end, &equal) {
        Some(hunks) => hunks,
        None => vec![Hunk {
            old: start..old_end,
            new: start..new_end,
        }],
    }
}

/// Myers' greedy diff over the ranges, with a trace per step for walking back. `None` if it would
/// take more than `MAX_EDITS` differences.
fn myers(
    old: Range<usize>,
    new: Range<usize>,
    equal: &impl Fn(usize, usize) -> bool,
) -> Option<Vec<Hunk>> {
    let n = old.len() as isize;
    let m = new.len() as isize;
    let max = ((n + m) as usize).min(MAX_EDITS) as isize;
    let offset = max + 1;
    let mut v = vec![0isize; (2 * offset + 1) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'outer: for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && equal(old.start + x as usize, new.start + y as usize) {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let d_final = found?;

    // Walk back from the end, collecting each step as an inserted or deleted line.
    let (mut x, mut y) = (n, m);
    let mut steps: Vec<(bool, usize)> = Vec::new(); // (is_insert, index in its side)
    for d in (1..=d_final).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let index = (k + offset) as usize;
        let prev_k = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            steps.push((true, (y - 1) as usize));
        } else {
            steps.push((false, (x - 1) as usize));
        }
        x = prev_x;
        y = prev_y;
    }
    steps.reverse();

    // Group adjacent steps into hunks, using the positions on both sides.
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut ox, mut ny) = (0usize, 0usize);
    let mut step = steps.into_iter().peekable();
    while let Some((insert, at)) = step.next() {
        // Equal lines before this step.
        let skip = if insert { at - ny } else { at - ox };
        ox += skip;
        ny += skip;
        let mut hunk = Hunk {
            old: old.start + ox..old.start + ox,
            new: new.start + ny..new.start + ny,
        };
        let apply = |insert: bool, hunk: &mut Hunk, ox: &mut usize, ny: &mut usize| {
            if insert {
                hunk.new.end += 1;
                *ny += 1;
            } else {
                hunk.old.end += 1;
                *ox += 1;
            }
        };
        apply(insert, &mut hunk, &mut ox, &mut ny);
        while let Some(&(insert, at)) = step.peek() {
            let contiguous = if insert { at == ny } else { at == ox };
            if !contiguous {
                break;
            }
            step.next();
            apply(insert, &mut hunk, &mut ox, &mut ny);
        }
        hunks.push(hunk);
    }
    Some(hunks)
}

/// Where an old line is after the change: shifted past the hunks before it, the same place within
/// a hunk that replaced as many lines as it removed, or the start of a hunk that didn't.
pub fn map_line(hunks: &[Hunk], line: usize) -> usize {
    let mut delta: isize = 0;
    for hunk in hunks {
        if line < hunk.old.start {
            break;
        }
        if line < hunk.old.end {
            return if hunk.old.len() == hunk.new.len() {
                hunk.new.start + (line - hunk.old.start)
            } else {
                hunk.new.start
            };
        }
        delta += hunk.new.len() as isize - hunk.old.len() as isize;
    }
    (line as isize + delta).max(0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Applies hunks to the old lines, which must give the new ones.
    fn apply(old: &str, new: &str, hunks: &[Hunk]) -> String {
        let old_lines = split_lines(old);
        let new_lines = split_lines(new);
        let mut out = String::new();
        let mut at = 0;
        for hunk in hunks {
            for line in &old_lines[at..hunk.old.start] {
                out.push_str(line);
            }
            for line in &new_lines[hunk.new.clone()] {
                out.push_str(line);
            }
            at = hunk.old.end;
        }
        for line in &old_lines[at..] {
            out.push_str(line);
        }
        out
    }

    #[test]
    fn separate_changes_are_separate_hunks() {
        let old = "a\nb\nc\nd\ne\nf\n";
        let new = "a\nB\nc\nd\ne\nnew\nf\n";
        let hunks = diff_lines(old, new);
        assert_eq!(
            hunks,
            [
                Hunk {
                    old: 1..2,
                    new: 1..2
                },
                Hunk {
                    old: 5..5,
                    new: 5..6
                },
            ]
        );
        assert_eq!(apply(old, new, &hunks), new);
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        assert!(diff_lines("x\ny", "x\ny").is_empty());
    }

    #[test]
    fn lines_map_past_and_through_changes() {
        // Line 3 ("d") follows an inserted line, then a removed one.
        let old = "a\nb\nc\nd\n";
        let new = "a\nnew\nb\nd\n";
        let hunks = diff_lines(old, new);
        assert_eq!(map_line(&hunks, 0), 0);
        assert_eq!(map_line(&hunks, 1), 2);
        assert_eq!(map_line(&hunks, 3), 3);
        // A removed line goes to where its hunk lands.
        assert_eq!(map_line(&hunks, 2), 3);
    }

    proptest! {
        #[test]
        fn hunks_turn_the_old_text_into_the_new(
            old in proptest::collection::vec(0u8..5, 0..40),
            new in proptest::collection::vec(0u8..5, 0..40),
        ) {
            let text = |lines: &[u8]| lines.iter().map(|l| format!("{l}\n")).collect::<String>();
            let (old, new) = (text(&old), text(&new));
            let hunks = diff_lines(&old, &new);
            prop_assert_eq!(apply(&old, &new, &hunks), new.clone());
            // Unchanged lines map to an equal line.
            let old_lines = split_lines(&old);
            let new_lines = split_lines(&new);
            let mut changed = vec![false; old_lines.len()];
            for hunk in &hunks {
                for i in hunk.old.clone() {
                    changed[i] = true;
                }
            }
            for (i, line) in old_lines.iter().enumerate() {
                if !changed[i] {
                    prop_assert_eq!(new_lines[map_line(&hunks, i)], *line);
                }
            }
        }
    }
}
