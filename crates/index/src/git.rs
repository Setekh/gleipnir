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
    use similar::{DiffOp, TextDiff};
    // Line endings aren't changes worth marking.
    let base = base.replace("\r\n", "\n");
    let current = current.replace("\r\n", "\n");
    let diff = TextDiff::from_lines(&base, &current);
    let mut changes: Vec<LineChange> = Vec::new();
    for op in diff.ops() {
        let change = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => LineChange {
                kind: LineChangeKind::Added,
                lines: new_index..new_index + new_len,
                base_lines: old_index..old_index,
            },
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => LineChange {
                kind: LineChangeKind::Deleted,
                lines: new_index..new_index,
                base_lines: old_index..old_index + old_len,
            },
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => LineChange {
                kind: LineChangeKind::Modified,
                lines: new_index..new_index + new_len,
                base_lines: old_index..old_index + old_len,
            },
        };
        // A deletion beside an insertion, as some diffs give a replacement, is one change.
        if let Some(last) = changes.last_mut()
            && last.lines.end == change.lines.start
            && last.base_lines.end == change.base_lines.start
            && last.kind != change.kind
            && (last.kind == LineChangeKind::Deleted || change.kind == LineChangeKind::Deleted)
        {
            last.kind = LineChangeKind::Modified;
            last.lines.end = change.lines.end;
            last.base_lines.end = change.base_lines.end;
            continue;
        }
        changes.push(change);
    }
    changes
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

/// Two texts lined up for showing side by side: unchanged lines together, a changed run's old
/// and new lines paired, with blanks where one side has more.
pub fn side_by_side(old: &str, new: &str) -> Vec<DiffRow> {
    use similar::{DiffOp, TextDiff};
    let old = old.replace("\r\n", "\n");
    let new = new.replace("\r\n", "\n");
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let line =
        |lines: &[&str], i: usize| Some((i, lines.get(i).copied().unwrap_or("").to_string()));
    let mut rows = Vec::new();
    for op in TextDiff::from_lines(&old, &new).ops() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => rows.extend((0..len).map(|i| DiffRow {
                left: line(&old_lines, old_index + i),
                right: line(&new_lines, new_index + i),
                kind: DiffRowKind::Same,
            })),
            DiffOp::Insert {
                new_index, new_len, ..
            } => rows.extend((0..new_len).map(|i| DiffRow {
                left: None,
                right: line(&new_lines, new_index + i),
                kind: DiffRowKind::Added,
            })),
            DiffOp::Delete {
                old_index, old_len, ..
            } => rows.extend((0..old_len).map(|i| DiffRow {
                left: line(&old_lines, old_index + i),
                right: None,
                kind: DiffRowKind::Deleted,
            })),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => rows.extend((0..old_len.max(new_len)).map(|i| {
                DiffRow {
                    left: (i < old_len)
                        .then(|| line(&old_lines, old_index + i))
                        .flatten(),
                    right: (i < new_len)
                        .then(|| line(&new_lines, new_index + i))
                        .flatten(),
                    kind: DiffRowKind::Changed,
                }
            })),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
