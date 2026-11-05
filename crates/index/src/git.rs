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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
