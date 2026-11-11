//! File status from git, by running the git command. That handles every repository format,
//! worktree and config option for free, which no reimplementation would.

use anyhow::{Context as _, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitState {
    Added,
    Untracked,
    Renamed,
    Modified,
    Deleted,
    Conflicted,
}

/// Which files differ from HEAD. Folders report the most pressing state of anything inside them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitStatus {
    states: HashMap<PathBuf, GitState>,
}

impl GitStatus {
    pub fn state(&self, path: &Path) -> Option<GitState> {
        self.states.get(path).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    fn insert(&mut self, path: PathBuf, state: GitState, stop_at: &Path) {
        for ancestor in path.ancestors() {
            if ancestor == stop_at || !ancestor.starts_with(stop_at) {
                break;
            }
            let existing = self.states.entry(ancestor.to_path_buf()).or_insert(state);
            *existing = (*existing).max(state);
        }
    }
}

/// Reads the status of the repository containing `root`. A folder that isn't in a repository, or
/// a machine without git, gives an empty status rather than an error, since that's not a failure.
pub fn read_status(root: &Path) -> Result<GitStatus> {
    let Ok(toplevel) = run_git(root, &["rev-parse", "--show-toplevel"]) else {
        return Ok(GitStatus::default());
    };
    let toplevel = PathBuf::from(toplevel.trim_end_matches(['\n', '\r']));
    let output = run_git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Ok(parse_status(&output, &toplevel))
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        // Don't let this background refresh take the index lock that the user's commands want.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("couldn't run git")?;
    if !output.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses `git status --porcelain=v1 -z`: records of `XY path`, NUL-separated. Renames and copies
/// are followed by an extra record holding the old path.
fn parse_status(output: &str, toplevel: &Path) -> GitStatus {
    let mut status = GitStatus::default();
    let mut records = output.split('\0');
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let (code, path) = record.split_at(3);
        let bytes = code.as_bytes();
        let (index, worktree) = (bytes[0], bytes[1]);
        if matches!(index, b'R' | b'C') {
            records.next();
        }
        let state = match (index, worktree) {
            (b'?', b'?') => GitState::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => GitState::Conflicted,
            (_, b'D') | (b'D', _) => GitState::Deleted,
            (_, b'M') | (b'M', _) | (_, b'T') | (b'T', _) => GitState::Modified,
            (b'R', _) | (b'C', _) => GitState::Renamed,
            (b'A', _) => GitState::Added,
            _ => continue,
        };
        status.insert(toplevel.join(path), state, toplevel);
    }
    status
}

/// A diff as runs, with positions counted from the lengths of the runs before: `similar`'s own
/// positions around a deletion aren't always where the run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Equal {
        old: usize,
        new: usize,
        len: usize,
    },
    Changed {
        old: usize,
        old_len: usize,
        new: usize,
        new_len: usize,
    },
}

fn runs<T: PartialEq + std::hash::Hash + Eq + Ord>(old: &[T], new: &[T]) -> Vec<Run> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let (mut o, mut n) = (0, 0);
    let mut out: Vec<Run> = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, old, new) {
        let (old_len, new_len, equal) = match op {
            DiffOp::Equal { len, .. } => (len, len, true),
            DiffOp::Delete { old_len, .. } => (old_len, 0, false),
            DiffOp::Insert { new_len, .. } => (0, new_len, false),
            DiffOp::Replace {
                old_len, new_len, ..
            } => (old_len, new_len, false),
        };
        let run = if equal {
            Run::Equal {
                old: o,
                new: n,
                len: old_len,
            }
        } else {
            // A deletion beside an insertion is one change.
            if let Some(Run::Changed {
                old_len: ol,
                new_len: nl,
                ..
            }) = out.last_mut()
            {
                *ol += old_len;
                *nl += new_len;
                o += old_len;
                n += new_len;
                continue;
            }
            Run::Changed {
                old: o,
                old_len,
                new: n,
                new_len,
            }
        };
        out.push(run);
        o += old_len;
        n += new_len;
    }
    out
}

/// How a run of lines differs from the last commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChangeKind {
    Added,
    Modified,
    /// Lines removed just before `lines.start`, which is then empty.
    Deleted,
}

/// A run of changed lines: where it is now, from 0, and what it was in the committed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineChange {
    pub kind: LineChangeKind,
    pub lines: std::ops::Range<usize>,
    pub base_lines: std::ops::Range<usize>,
}

/// A file's text as of the last commit: none if it isn't in one, like a new file, or isn't in a
/// repository at all.
pub fn committed_text(path: &Path) -> Result<Option<String>> {
    let Some(dir) = path.parent() else {
        return Ok(None);
    };
    let Ok(toplevel) = run_git(dir, &["rev-parse", "--show-toplevel"]) else {
        return Ok(None);
    };
    let toplevel = PathBuf::from(toplevel.trim_end_matches(['\n', '\r']));
    // The repository's own path for the file, which may differ from ours by symlinks.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let toplevel = std::fs::canonicalize(&toplevel).unwrap_or(toplevel);
    let Ok(relative) = real.strip_prefix(&toplevel) else {
        return Ok(None);
    };
    let spec = format!("HEAD:{}", relative.to_string_lossy().replace('\\', "/"));
    Ok(run_git(&toplevel, &["show", &spec]).ok())
}

/// The runs of lines that differ between the committed text and the text now, in order.
pub fn line_changes(base: &str, current: &str) -> Vec<LineChange> {
    // Line endings aren't changes worth marking.
    let base = base.replace("\r\n", "\n");
    let current = current.replace("\r\n", "\n");
    let old: Vec<&str> = base.split_inclusive('\n').collect();
    let new: Vec<&str> = current.split_inclusive('\n').collect();
    runs(&old, &new)
        .into_iter()
        .filter_map(|run| match run {
            Run::Equal { .. } => None,
            Run::Changed {
                old,
                old_len,
                new,
                new_len,
            } => Some(LineChange {
                kind: match (old_len, new_len) {
                    (0, _) => LineChangeKind::Added,
                    (_, 0) => LineChangeKind::Deleted,
                    _ => LineChangeKind::Modified,
                },
                lines: new..new + new_len,
                base_lines: old..old + old_len,
            }),
        })
        .collect()
}

/// One row of a side-by-side diff: a line on either side, or both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    /// The line in the older text, from 0, and its text.
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub kind: DiffRowKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    Same,
    Added,
    Deleted,
    Changed,
}

/// What differences in whitespace a diff passes over, as IntelliJ's diff offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Whitespace {
    #[default]
    Compare,
    /// Space at the start and end of lines doesn't count.
    Trim,
    /// No whitespace counts.
    Ignore,
    /// Nor do blank lines added or taken out.
    IgnoreWithBlankLines,
}

impl Whitespace {
    /// A line as it's compared.
    fn key(self, line: &str) -> String {
        match self {
            Self::Compare => line.to_string(),
            Self::Trim => line.trim().to_string(),
            Self::Ignore | Self::IgnoreWithBlankLines => {
                line.chars().filter(|c| !c.is_whitespace()).collect()
            }
        }
    }
}

/// Two texts lined up for showing side by side: unchanged lines together, a changed run's old
/// and new lines paired, with blanks where one side has more.
pub fn side_by_side(old: &str, new: &str) -> Vec<DiffRow> {
    side_by_side_with(old, new, Whitespace::Compare)
}

/// As `side_by_side`, passing over the differences in whitespace that `whitespace` says to. Lines
/// that differ only in those are shown as the same.
pub fn side_by_side_with(old: &str, new: &str, whitespace: Whitespace) -> Vec<DiffRow> {
    let old = old.replace("\r\n", "\n");
    let new = new.replace("\r\n", "\n");
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let old_keys: Vec<String> = old_lines.iter().map(|l| whitespace.key(l)).collect();
    let new_keys: Vec<String> = new_lines.iter().map(|l| whitespace.key(l)).collect();
    let line =
        |lines: &[&str], i: usize| Some((i, lines.get(i).copied().unwrap_or("").to_string()));
    let mut rows = Vec::new();
    for run in runs(&old_keys, &new_keys) {
        // A change of nothing but blank lines, when those don't count.
        let run = match run {
            Run::Changed {
                old,
                old_len,
                new,
                new_len,
            } if whitespace == Whitespace::IgnoreWithBlankLines
                && old_keys[old..old + old_len]
                    .iter()
                    .filter(|k| !k.is_empty())
                    .eq(new_keys[new..new + new_len]
                        .iter()
                        .filter(|k| !k.is_empty())) =>
            {
                rows.extend((0..old_len.max(new_len)).map(|i| DiffRow {
                    left: (i < old_len).then(|| line(&old_lines, old + i)).flatten(),
                    right: (i < new_len).then(|| line(&new_lines, new + i)).flatten(),
                    kind: DiffRowKind::Same,
                }));
                continue;
            }
            run => run,
        };
        match run {
            Run::Equal { old, new, len } => rows.extend((0..len).map(|i| DiffRow {
                left: line(&old_lines, old + i),
                right: line(&new_lines, new + i),
                kind: DiffRowKind::Same,
            })),
            Run::Changed {
                old,
                old_len,
                new,
                new_len,
            } => {
                let kind = match (old_len, new_len) {
                    (0, _) => DiffRowKind::Added,
                    (_, 0) => DiffRowKind::Deleted,
                    _ => DiffRowKind::Changed,
                };
                rows.extend((0..old_len.max(new_len)).map(|i| DiffRow {
                    left: (i < old_len).then(|| line(&old_lines, old + i)).flatten(),
                    right: (i < new_len).then(|| line(&new_lines, new + i)).flatten(),
                    kind,
                }));
            }
        }
    }
    rows
}

/// Where two versions of a changed run of lines differ within the lines, as byte ranges in each,
/// by words: runs of letters, digits and `_`, runs of spaces, and single other characters. With
/// `ignore_space`, differences in spacing alone aren't marked.
pub fn word_changes(
    old: &str,
    new: &str,
    ignore_space: bool,
) -> (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) {
    fn tokens(text: &str, ignore_space: bool) -> Vec<(std::ops::Range<usize>, &str)> {
        let mut out = Vec::new();
        let mut chars = text.char_indices().peekable();
        while let Some((start, c)) = chars.next() {
            let class = |c: char| {
                if c.is_alphanumeric() || c == '_' {
                    1
                } else if c.is_whitespace() {
                    2
                } else {
                    0
                }
            };
            let kind = class(c);
            let mut end = start + c.len_utf8();
            if kind != 0 {
                while let Some(&(i, next)) = chars.peek() {
                    if class(next) != kind {
                        break;
                    }
                    end = i + next.len_utf8();
                    chars.next();
                }
            }
            if !(ignore_space && kind == 2) {
                out.push((start..end, &text[start..end]));
            }
        }
        out
    }
    let old_tokens = tokens(old, ignore_space);
    let new_tokens = tokens(new, ignore_space);
    let old_words: Vec<&str> = old_tokens.iter().map(|t| t.1).collect();
    let new_words: Vec<&str> = new_tokens.iter().map(|t| t.1).collect();
    let (mut old_out, mut new_out): (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) =
        (Vec::new(), Vec::new());
    // Adjacent changed words are marked as one, with what's between them.
    let push = |out: &mut Vec<std::ops::Range<usize>>, range: std::ops::Range<usize>| match out
        .last_mut()
    {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => out.push(range),
    };
    for run in runs(&old_words, &new_words) {
        if let Run::Changed {
            old,
            old_len,
            new,
            new_len,
        } = run
        {
            for token in &old_tokens[old..old + old_len] {
                push(&mut old_out, token.0.clone());
            }
            for token in &new_tokens[new..new + new_len] {
                push(&mut new_out, token.0.clone());
            }
        }
    }
    (old_out, new_out)
}

/// How a region of a three-way merge came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeKind {
    /// Neither side changed it.
    Same,
    /// Only ours changed it, so ours is taken.
    Ours,
    /// Only theirs changed it, so theirs is taken.
    Theirs,
    /// Both changed it the same way.
    Both,
    /// Both changed it differently: someone has to choose.
    Conflict,
}

/// A region of a three-way merge, with each version's lines, line breaks kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeChunk {
    pub kind: MergeKind,
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}

impl MergeChunk {
    /// What the merge takes here, unless it's a conflict.
    pub fn merged(&self) -> Option<&[String]> {
        match self.kind {
            MergeKind::Same => Some(&self.base),
            MergeKind::Ours | MergeKind::Both => Some(&self.ours),
            MergeKind::Theirs => Some(&self.theirs),
            MergeKind::Conflict => None,
        }
    }
}

/// A side's changes to the base, as (base lines, its lines), in order.
#[allow(clippy::type_complexity)]
fn hunks(base: &[&str], other: &[&str]) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    runs(base, other)
        .into_iter()
        .filter_map(|run| match run {
            Run::Equal { .. } => None,
            Run::Changed {
                old,
                old_len,
                new,
                new_len,
            } => Some((old..old + old_len, new..new + new_len)),
        })
        .collect()
}

/// Merges two versions of a text that each came from `base`, line by line, as git does: what
/// only one side changed is taken from it, and what both changed differently is a conflict.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Vec<MergeChunk> {
    let split = |text: &str| -> Vec<String> {
        text.replace("\r\n", "\n")
            .split_inclusive('\n')
            .map(String::from)
            .collect()
    };
    let (base, ours, theirs) = (split(base), split(ours), split(theirs));
    fn as_str(lines: &[String]) -> Vec<&str> {
        lines.iter().map(String::as_str).collect()
    }
    let (b, o, t) = (as_str(&base), as_str(&ours), as_str(&theirs));
    let ours_hunks = hunks(&b, &o);
    let theirs_hunks = hunks(&b, &t);

    // Every change, in base order, as (base range, side: true for ours, index).
    let mut all: Vec<(std::ops::Range<usize>, bool, usize)> = ours_hunks
        .iter()
        .enumerate()
        .map(|(i, h)| (h.0.clone(), true, i))
        .chain(
            theirs_hunks
                .iter()
                .enumerate()
                .map(|(i, h)| (h.0.clone(), false, i)),
        )
        .collect();
    all.sort_by_key(|(range, ours, _)| (range.start, range.end, !*ours));

    // Changes that touch are one region: overlapping, or meeting where one inserts.
    let mut regions: Vec<(std::ops::Range<usize>, Vec<usize>, Vec<usize>)> = Vec::new();
    for (range, is_ours, index) in all {
        let joins = regions.last().is_some_and(|(r, _, _)| {
            range.start < r.end
                || (range.start == r.end && (range.is_empty() || r.is_empty() || r.start == r.end))
                || (range.start == r.start)
        });
        if !joins {
            regions.push((range.clone(), Vec::new(), Vec::new()));
        }
        let (r, ours_in, theirs_in) = regions.last_mut().unwrap();
        r.start = r.start.min(range.start);
        r.end = r.end.max(range.end);
        if is_ours {
            ours_in.push(index);
        } else {
            theirs_in.push(index);
        }
    }

    // A side's lines for a base region: base positions moved by the side's changes before it.
    let side_range = |hunks: &[(std::ops::Range<usize>, std::ops::Range<usize>)],
                      inside: &[usize],
                      region: &std::ops::Range<usize>| {
        let shift = |at: usize| -> isize {
            hunks
                .iter()
                .filter(|(b, _)| b.end <= at && !(b.is_empty() && b.start == at))
                .map(|(b, n)| n.len() as isize - b.len() as isize)
                .sum()
        };
        if inside.is_empty() {
            let start = (region.start as isize + shift(region.start)) as usize;
            return start..start + region.len();
        }
        let first = &hunks[inside[0]];
        let last = &hunks[*inside.last().unwrap()];
        let start = first.1.start - (first.0.start - region.start);
        let end = last.1.end + (region.end - last.0.end);
        start..end
    };

    let mut chunks = Vec::new();
    let mut at = 0;
    let push_same = |chunks: &mut Vec<MergeChunk>, range: std::ops::Range<usize>| {
        if !range.is_empty() {
            let lines = base[range].to_vec();
            chunks.push(MergeChunk {
                kind: MergeKind::Same,
                base: lines.clone(),
                ours: lines.clone(),
                theirs: lines,
            });
        }
    };
    for (region, ours_in, theirs_in) in &regions {
        push_same(&mut chunks, at..region.start);
        let o_range = side_range(&ours_hunks, ours_in, region);
        let t_range = side_range(&theirs_hunks, theirs_in, region);
        let chunk_ours = ours[o_range].to_vec();
        let chunk_theirs = theirs[t_range].to_vec();
        let kind = match (ours_in.is_empty(), theirs_in.is_empty()) {
            (false, true) => MergeKind::Ours,
            (true, false) => MergeKind::Theirs,
            _ if chunk_ours == chunk_theirs => MergeKind::Both,
            _ => MergeKind::Conflict,
        };
        chunks.push(MergeChunk {
            kind,
            base: base[region.clone()].to_vec(),
            ours: chunk_ours,
            theirs: chunk_theirs,
        });
        at = region.end;
    }
    push_same(&mut chunks, at..base.len());
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn kinds(chunks: &[MergeChunk]) -> Vec<(MergeKind, String, String)> {
        chunks
            .iter()
            .map(|c| (c.kind, c.ours.concat(), c.theirs.concat()))
            .collect()
    }

    #[test]
    fn a_merge_takes_one_sided_changes_and_finds_conflicts() {
        let base = "a\nb\nc\nd\ne\n";
        let ours = "A\nb\nc\nD-ours\ne\n";
        let theirs = "a\nb\nC\nD-theirs\ne\nf\n";
        let chunks = merge3(base, ours, theirs);
        assert_eq!(
            kinds(&chunks),
            [
                (MergeKind::Ours, "A\n".into(), "a\n".into()),
                (MergeKind::Same, "b\n".into(), "b\n".into()),
                (
                    MergeKind::Conflict,
                    "c\nD-ours\n".into(),
                    "C\nD-theirs\n".into()
                ),
                (MergeKind::Same, "e\n".into(), "e\n".into()),
                (MergeKind::Theirs, "".into(), "f\n".into()),
            ]
        );
        assert_eq!(chunks[0].merged().unwrap().concat(), "A\n");
        assert_eq!(chunks[4].merged().unwrap().concat(), "f\n");
        assert_eq!(chunks[2].merged(), None);
    }

    #[test]
    fn the_same_change_on_both_sides_is_no_conflict() {
        let chunks = merge3("a\nb\n", "a\nB\n", "a\nB\n");
        assert_eq!(chunks[1].kind, MergeKind::Both);
        // Two insertions at the same place are a conflict, unless they're the same.
        let chunks = merge3("a\nz\n", "a\nours\nz\n", "a\ntheirs\nz\n");
        assert_eq!(
            kinds(&chunks)[1],
            (MergeKind::Conflict, "ours\n".into(), "theirs\n".into())
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 200, ..Default::default() })]

        /// With one side unchanged, the merge is the other side, conflict-free, whatever it did.
        #[test]
        fn merging_with_an_unchanged_side_gives_the_other(
            base in proptest::collection::vec("[a-d]", 0..12),
            edits in proptest::collection::vec((0usize..12, 0usize..3, "[a-z]{0,2}"), 0..5),
        ) {
            let base: String = base.iter().map(|l| format!("{l}\n")).collect();
            let mut lines: Vec<String> = base.lines().map(|l| format!("{l}\n")).collect();
            for (at, remove, insert) in edits {
                let at = at.min(lines.len());
                let end = (at + remove).min(lines.len());
                let new: Vec<String> = insert.chars().map(|c| format!("{c}\n")).collect();
                lines.splice(at..end, new);
            }
            let changed: String = lines.concat();
            for (ours, theirs) in [(&changed, &base), (&base, &changed)] {
                let chunks = merge3(&base, ours, theirs);
                let merged: String = chunks
                    .iter()
                    .map(|c| c.merged().expect("no conflict").concat())
                    .collect();
                proptest::prop_assert_eq!(&merged, &changed);
            }
        }
    }

    #[test]
    fn a_merge_without_conflicts_gives_both_sides_changes() {
        let chunks = merge3(
            "1\n2\n3\n4\n5\n6\n",
            "one\n2\n3\n4\n5\n6\n",
            "1\n2\n3\n4\n5\nsix\n",
        );
        let merged: String = chunks
            .iter()
            .map(|c| c.merged().unwrap().concat())
            .collect();
        assert_eq!(merged, "one\n2\n3\n4\n5\nsix\n");
    }

    #[test]
    fn side_by_side_pairs_changed_lines_and_pads_the_shorter_side() {
        let rows = side_by_side("a\nb\nc\nd\n", "a\nB\nB2\nd\ne\n");
        let summary: Vec<(Option<usize>, Option<usize>, DiffRowKind)> = rows
            .iter()
            .map(|r| {
                (
                    r.left.as_ref().map(|l| l.0),
                    r.right.as_ref().map(|l| l.0),
                    r.kind,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (Some(0), Some(0), DiffRowKind::Same),
                (Some(1), Some(1), DiffRowKind::Changed),
                (Some(2), Some(2), DiffRowKind::Changed),
                (Some(3), Some(3), DiffRowKind::Same),
                (None, Some(4), DiffRowKind::Added),
            ]
        );
        assert_eq!(rows[1].right.as_ref().unwrap().1, "B");
    }

    #[test]
    fn whitespace_can_be_passed_over() {
        let kinds = |rows: Vec<DiffRow>| rows.iter().map(|r| r.kind).collect::<Vec<_>>();
        let (old, new) = ("a\n  b\nc d\n", "a\nb  \ncd\n");
        use DiffRowKind::*;
        assert_eq!(kinds(side_by_side(old, new)), [Same, Changed, Changed]);
        assert_eq!(
            kinds(side_by_side_with(old, new, Whitespace::Trim)),
            [Same, Same, Changed]
        );
        assert_eq!(
            kinds(side_by_side_with(old, new, Whitespace::Ignore)),
            [Same, Same, Same]
        );
        // The lines shown are still the real ones.
        let rows = side_by_side_with(old, new, Whitespace::Ignore);
        assert_eq!(rows[1].left.as_ref().unwrap().1, "  b");
        assert_eq!(rows[1].right.as_ref().unwrap().1, "b  ");

        let (old, new) = ("a\nb\n", "a\n\n\nb\nc\n");
        assert_eq!(
            kinds(side_by_side_with(old, new, Whitespace::Ignore)),
            [Same, Added, Added, Same, Added]
        );
        assert_eq!(
            kinds(side_by_side_with(
                old,
                new,
                Whitespace::IgnoreWithBlankLines
            )),
            [Same, Same, Same, Same, Added]
        );
    }

    #[test]
    fn word_changes_mark_only_the_words_that_differ() {
        let old = "let total = price * count;";
        let new = "let sum = price * amount;";
        let (o, n) = word_changes(old, new, false);
        let words = |text: &str, ranges: &[std::ops::Range<usize>]| {
            ranges
                .iter()
                .map(|r| text[r.clone()].to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(words(old, &o), ["total", "count"]);
        assert_eq!(words(new, &n), ["sum", "amount"]);

        // Spacing alone is a change unless it's ignored.
        let (o, n) = word_changes("f(a, b)", "f(a,b)", false);
        assert_eq!((o.len(), n.len()), (1, 0));
        let (o, n) = word_changes("f(a, b)", "f(a,b)", true);
        assert!(o.is_empty() && n.is_empty());

        // Neighbouring changed words are one mark, and multi-byte text is cut on characters.
        let (o, n) = word_changes("un café noir", "un thé chaud noir", false);
        assert_eq!(words("un café noir", &o), ["café"]);
        assert_eq!(words("un thé chaud noir", &n), ["thé chaud"]);
    }

    #[test]
    fn line_changes_mark_added_changed_and_deleted_runs() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let current = "zero\none\nTWO\nthree\nfive\n";
        let changes = line_changes(base, current);
        let kinds: Vec<(
            LineChangeKind,
            std::ops::Range<usize>,
            std::ops::Range<usize>,
        )> = changes
            .into_iter()
            .map(|c| (c.kind, c.lines, c.base_lines))
            .collect();
        assert_eq!(
            kinds,
            [
                (LineChangeKind::Added, 0..1, 0..0),
                (LineChangeKind::Modified, 2..3, 1..2),
                (LineChangeKind::Deleted, 4..4, 3..4),
            ]
        );
        assert!(line_changes("a\r\nb\r\n", "a\nb\n").is_empty());
        // A line taken out from a run of equal ones is marked where it was.
        let removed = line_changes("a\na\na\na\na\nd\n", "a\na\na\na\nd\n");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].kind, LineChangeKind::Deleted);
        assert_eq!(removed[0].lines, 4..4);
    }

    #[test]
    fn committed_text_is_the_file_at_head() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "{args:?}");
        };
        git(&["init", "-q"]);
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/a.rs"), "committed\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        fs::write(dir.path().join("src/a.rs"), "changed\n").unwrap();
        fs::write(dir.path().join("src/new.rs"), "new\n").unwrap();
        assert_eq!(
            committed_text(&dir.path().join("src/a.rs"))
                .unwrap()
                .as_deref(),
            Some("committed\n")
        );
        assert_eq!(
            committed_text(&dir.path().join("src/new.rs")).unwrap(),
            None
        );
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("b.rs"), "").unwrap();
        assert_eq!(committed_text(&outside.path().join("b.rs")).unwrap(), None);
    }

    fn parse(output: &str) -> GitStatus {
        parse_status(output, Path::new("/repo"))
    }

    #[test]
    fn each_status_code_maps_to_a_state() {
        let status = parse(
            " M mod.rs\0M  staged.rs\0A  new.rs\0?? untracked.rs\0 D gone.rs\0D  staged-gone.rs\0UU conflict.rs\0AA both.rs\0",
        );
        let state = |p: &str| status.state(&Path::new("/repo").join(p));
        assert_eq!(state("mod.rs"), Some(GitState::Modified));
        assert_eq!(state("staged.rs"), Some(GitState::Modified));
        assert_eq!(state("new.rs"), Some(GitState::Added));
        assert_eq!(state("untracked.rs"), Some(GitState::Untracked));
        assert_eq!(state("gone.rs"), Some(GitState::Deleted));
        assert_eq!(state("staged-gone.rs"), Some(GitState::Deleted));
        assert_eq!(state("conflict.rs"), Some(GitState::Conflicted));
        assert_eq!(state("both.rs"), Some(GitState::Conflicted));
        assert_eq!(state("clean.rs"), None);
    }

    #[test]
    fn a_rename_consumes_its_old_path_record() {
        let status = parse("R  new-name.rs\0old-name.rs\0 M after.rs\0");
        let state = |p: &str| status.state(&Path::new("/repo").join(p));
        assert_eq!(state("new-name.rs"), Some(GitState::Renamed));
        assert_eq!(state("old-name.rs"), None);
        assert_eq!(state("after.rs"), Some(GitState::Modified));
    }

    #[test]
    fn paths_with_spaces_and_unicode_survive() {
        let status = parse(" M dir with space/é 😀.rs\0");
        assert_eq!(
            status.state(Path::new("/repo/dir with space/é 😀.rs")),
            Some(GitState::Modified)
        );
    }

    #[test]
    fn folders_take_the_most_pressing_state_inside_them() {
        let status = parse("?? src/a/new.rs\0 M src/b.rs\0UU other/c.rs\0");
        assert_eq!(
            status.state(Path::new("/repo/src/a")),
            Some(GitState::Untracked)
        );
        assert_eq!(
            status.state(Path::new("/repo/src")),
            Some(GitState::Modified)
        );
        assert_eq!(
            status.state(Path::new("/repo/other")),
            Some(GitState::Conflicted)
        );
        assert_eq!(
            status.state(Path::new("/repo")),
            None,
            "the root itself isn't colored"
        );
    }

    #[test]
    fn empty_or_garbage_output_is_an_empty_status() {
        assert!(parse("").is_empty());
        assert!(parse("x\0\0").is_empty());
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    #[test]
    fn reads_a_real_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        fs::write(root.join("tracked.txt"), "one").unwrap();
        fs::write(root.join("clean.txt"), "same").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "initial"]);

        fs::write(root.join("tracked.txt"), "two").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/new.txt"), "").unwrap();

        let status = read_status(&root).unwrap();
        assert_eq!(
            status.state(&root.join("tracked.txt")),
            Some(GitState::Modified)
        );
        assert_eq!(
            status.state(&root.join("sub/new.txt")),
            Some(GitState::Untracked)
        );
        assert_eq!(status.state(&root.join("sub")), Some(GitState::Untracked));
        assert_eq!(status.state(&root.join("clean.txt")), None);
    }

    #[test]
    fn opening_a_subfolder_of_a_repository_still_finds_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        git(&root, &["init", "-q"]);
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/new.txt"), "").unwrap();
        let status = read_status(&root.join("sub")).unwrap();
        assert_eq!(
            status.state(&root.join("sub/new.txt")),
            Some(GitState::Untracked)
        );
    }

    #[test]
    fn a_folder_outside_any_repository_is_just_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_status(dir.path()).unwrap().is_empty());
    }
}
