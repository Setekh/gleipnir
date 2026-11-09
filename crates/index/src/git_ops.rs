//! What the Git tool windows do, by running git: list the changes, commit some of them, push and
//! pull, settle conflicts, and read history. Each returns git's own message when it refuses, as
//! that says best what's wrong.

use crate::git::GitState;
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A file that differs from the last commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    /// As git names it, from the repository's top.
    pub relative: String,
    pub state: GitState,
}

/// A commit in the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// How long ago, as git says it: `3 days ago`.
    pub when: String,
    pub subject: String,
    /// Branches and tags pointing at it, as git shows them: `HEAD -> main`, `tag: v1`.
    pub refs: Vec<String>,
}

/// A stash, newest first: `stash@{0}`, and the message it was saved with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    pub name: String,
    pub message: String,
}

/// Runs git in a folder: its output, or what it said when it failed.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("couldn't run git")?;
    if !output.status.success() {
        let mut message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if message.is_empty() {
            message = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        bail!("{}", message.lines().collect::<Vec<_>>().join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The top of the repository a folder is in.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(
        git(dir, &["rev-parse", "--show-toplevel"])?.trim_end(),
    ))
}

/// Every file that differs from the last commit, staged or not, untracked ones included, in
/// the order git lists them, with conflicts first.
pub fn changed_files(dir: &Path) -> Result<Vec<ChangedFile>> {
    let top = toplevel(dir)?;
    let output = git(
        &top,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut files = Vec::new();
    let mut records = output.split('\0');
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let (code, relative) = record.split_at(3);
        let bytes = code.as_bytes();
        let (index, worktree) = (bytes[0], bytes[1]);
        if matches!(index, b'R' | b'C') {
            records.next();
        }
        let state = match (index, worktree) {
            (b'?', b'?') => GitState::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => GitState::Conflicted,
            (_, b'D') | (b'D', _) => GitState::Deleted,
            (b'R', _) | (b'C', _) => GitState::Renamed,
            (b'A', _) => GitState::Added,
            _ => GitState::Modified,
        };
        files.push(ChangedFile {
            path: top.join(relative),
            relative: relative.to_string(),
            state,
        });
    }
    files.sort_by_key(|f| f.state != GitState::Conflicted);
    Ok(files)
}

/// Commits these files as they are now, and only these, whatever else is staged: new ones are
/// added first. With `amend`, the last commit is redone with them added.
pub fn commit(dir: &Path, files: &[ChangedFile], message: &str, amend: bool) -> Result<String> {
    if files.is_empty() && !amend {
        bail!("No files are chosen to commit");
    }
    if message.trim().is_empty() {
        bail!("The commit message is empty");
    }
    let top = toplevel(dir)?;
    let untracked: Vec<&str> = files
        .iter()
        .filter(|f| f.state == GitState::Untracked)
        .map(|f| f.relative.as_str())
        .collect();
    if !untracked.is_empty() {
        let mut args = vec!["add", "--"];
        args.extend(untracked);
        git(&top, &args)?;
    }
    let mut args = vec!["commit", "-m", message];
    if amend {
        args.push("--amend");
    }
    if !files.is_empty() {
        args.push("--");
        args.extend(files.iter().map(|f| f.relative.as_str()));
    }
    git(&top, &args)?;
    Ok(git(&top, &["log", "-1", "--format=%h %s"])?
        .trim()
        .to_string())
}

/// The last commit's message, for amending it.
pub fn last_message(dir: &Path) -> Result<String> {
    Ok(git(dir, &["log", "-1", "--format=%B"])?
        .trim_end()
        .to_string())
}

/// Puts files back as they were in the last commit; new ones are deleted.
pub fn rollback(dir: &Path, files: &[ChangedFile]) -> Result<()> {
    let top = toplevel(dir)?;
    let (new, tracked): (Vec<&ChangedFile>, Vec<&ChangedFile>) = files
        .iter()
        .partition(|f| matches!(f.state, GitState::Untracked | GitState::Added));
    if !tracked.is_empty() {
        let mut args = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
        args.extend(tracked.iter().map(|f| f.relative.as_str()));
        git(&top, &args)?;
    }
    for file in new {
        if file.state == GitState::Added {
            git(&top, &["rm", "--cached", "-q", "--", &file.relative])?;
        }
        std::fs::remove_file(&file.path).ok();
    }
    Ok(())
}

/// Settles a conflicted file with one side, `ours` or `theirs`, and stages it.
pub fn take_side(dir: &Path, file: &ChangedFile, ours: bool) -> Result<()> {
    let top = toplevel(dir)?;
    let side = if ours { "--ours" } else { "--theirs" };
    git(&top, &["checkout", side, "--", &file.relative])?;
    git(&top, &["add", "--", &file.relative])?;
    Ok(())
}

/// A conflicted file's three versions, as git keeps them while merging: the common base (empty
/// if both sides added it), ours and theirs.
pub fn conflict_versions(dir: &Path, file: &ChangedFile) -> Result<(String, String, String)> {
    let top = toplevel(dir)?;
    let stage = |n: u8| git(&top, &["show", &format!(":{n}:{}", file.relative)]);
    Ok((stage(1).unwrap_or_default(), stage(2)?, stage(3)?))
}

/// Settles a conflict with merged text: writes it and stages it.
pub fn resolve_with(dir: &Path, file: &ChangedFile, text: &str) -> Result<()> {
    std::fs::write(&file.path, text)?;
    mark_resolved(dir, file)
}

/// Marks a conflicted file resolved, as it is now, after it's been edited by hand.
pub fn mark_resolved(dir: &Path, file: &ChangedFile) -> Result<()> {
    git(&toplevel(dir)?, &["add", "--", &file.relative])?;
    Ok(())
}

/// Pushes the current branch, setting where it goes on its first push.
pub fn push(dir: &Path) -> Result<String> {
    let has_upstream = git(dir, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok();
    if has_upstream {
        git(dir, &["push", "--follow-tags"])?;
    } else {
        let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let branch = branch.trim();
        let remote = git(dir, &["remote"])?;
        let remote = remote
            .lines()
            .next()
            .context("there's no remote to push to")?;
        git(dir, &["push", "--follow-tags", "-u", remote, branch])?;
    }
    Ok("Pushed".to_string())
}

/// Brings the current branch up to date with where it's pulled from, as git is set up to.
pub fn update(dir: &Path) -> Result<String> {
    let output = git(dir, &["pull"])?;
    Ok(if output.contains("Already up to date") {
        "Already up to date".to_string()
    } else {
        "Updated".to_string()
    })
}

/// The latest commits, newest first: of every branch and tag with `all`, else of the current
/// branch.
pub fn history(dir: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
    history_of(dir, limit, false)
}

pub fn history_of(dir: &Path, limit: usize, all: bool) -> Result<Vec<CommitInfo>> {
    let count = format!("-{limit}");
    let mut args = vec![
        "log",
        count.as_str(),
        // Children before their parents, whatever the times say, as a graph reads.
        "--topo-order",
        "--format=%H%x1f%h%x1f%an%x1f%ar%x1f%s%x1f%D",
    ];
    if all {
        args.push("--all");
    }
    let output = git(dir, &args)?;
    Ok(output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\u{1f}');
            Some(CommitInfo {
                hash: parts.next()?.to_string(),
                short: parts.next()?.to_string(),
                author: parts.next()?.to_string(),
                when: parts.next()?.to_string(),
                subject: parts.next()?.to_string(),
                refs: parts
                    .next()
                    .unwrap_or("")
                    .split(", ")
                    .filter(|r| !r.is_empty())
                    .map(String::from)
                    .collect(),
            })
        })
        .collect())
}

/// Applies a commit's changes on top of the current branch.
pub fn cherry_pick(dir: &Path, hash: &str) -> Result<String> {
    git(dir, &["cherry-pick", hash])?;
    Ok(format!("Cherry-picked {}", short(hash)))
}

/// Makes a commit undoing another.
pub fn revert(dir: &Path, hash: &str) -> Result<String> {
    git(dir, &["revert", "--no-edit", hash])?;
    Ok(format!("Reverted {}", short(hash)))
}

/// Tags a commit, with an annotated tag so a push takes it along.
pub fn tag(dir: &Path, name: &str, hash: &str) -> Result<String> {
    git(dir, &["tag", "-a", name, "-m", name, hash])?;
    Ok(format!("Tagged {} as {name}", short(hash)))
}

/// Puts uncommitted changes, new files included, aside.
pub fn stash(dir: &Path, message: &str) -> Result<String> {
    let mut args = vec!["stash", "push", "--include-untracked"];
    if !message.trim().is_empty() {
        args.extend(["-m", message]);
    }
    let output = git(dir, &args)?;
    if output.contains("No local changes") {
        bail!("There are no changes to stash");
    }
    Ok("Stashed the changes".to_string())
}

/// The stashes there are, newest first.
pub fn stashes(dir: &Path) -> Result<Vec<Stash>> {
    Ok(git(dir, &["stash", "list", "--format=%gd%x1f%s"])?
        .lines()
        .filter_map(|line| {
            let (name, message) = line.split_once('\u{1f}')?;
            Some(Stash {
                name: name.to_string(),
                message: message.to_string(),
            })
        })
        .collect())
}

/// Brings a stash's changes back: `pop` drops it after, `apply` keeps it.
pub fn unstash(dir: &Path, name: &str, pop: bool) -> Result<String> {
    git(dir, &["stash", if pop { "pop" } else { "apply" }, name])?;
    Ok(format!("Unstashed {name}"))
}

pub fn drop_stash(dir: &Path, name: &str) -> Result<String> {
    git(dir, &["stash", "drop", name])?;
    Ok(format!("Dropped {name}"))
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(7)]
}

/// The files a commit changed, with how.
pub fn commit_files(dir: &Path, hash: &str) -> Result<Vec<ChangedFile>> {
    let top = toplevel(dir)?;
    let output = git(&top, &["show", "--format=", "--name-status", "-z", hash])?;
    let mut files = Vec::new();
    let mut parts = output.split('\0').filter(|p| !p.is_empty());
    while let Some(status) = parts.next() {
        let state = match status.chars().next() {
            Some('A') => GitState::Added,
            Some('D') => GitState::Deleted,
            Some('R') | Some('C') => {
                parts.next();
                GitState::Renamed
            }
            _ => GitState::Modified,
        };
        let Some(relative) = parts.next() else { break };
        files.push(ChangedFile {
            path: top.join(relative),
            relative: relative.to_string(),
            state,
        });
    }
    Ok(files)
}

/// A file's text in a commit, or before it with `parent`; empty where it didn't exist.
pub fn text_at(dir: &Path, hash: &str, relative: &str, parent: bool) -> String {
    let spec = if parent {
        format!("{hash}^:{relative}")
    } else {
        format!("{hash}:{relative}")
    };
    git(dir, &["show", &spec]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        run(&root, &["init", "-q", "-b", "main"]);
        run(&root, &["config", "user.name", "t"]);
        run(&root, &["config", "user.email", "t@t"]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join("b.txt"), "bee\n").unwrap();
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "first"]);
        (dir, root)
    }

    fn run(root: &Path, args: &[&str]) {
        git(root, args).unwrap();
    }

    fn states(root: &Path) -> Vec<(String, GitState)> {
        changed_files(root)
            .unwrap()
            .into_iter()
            .map(|f| (f.relative, f.state))
            .collect()
    }

    #[test]
    fn only_the_chosen_files_are_committed() {
        let (_dir, root) = repo();
        fs::write(root.join("a.txt"), "two\n").unwrap();
        fs::write(root.join("b.txt"), "changed too\n").unwrap();
        fs::write(root.join("new.txt"), "new\n").unwrap();
        let files = changed_files(&root).unwrap();
        assert_eq!(
            states(&root),
            [
                ("a.txt".into(), GitState::Modified),
                ("b.txt".into(), GitState::Modified),
                ("new.txt".into(), GitState::Untracked)
            ]
        );
        let chosen: Vec<ChangedFile> = files
            .into_iter()
            .filter(|f| f.relative != "b.txt")
            .collect();
        let made = commit(&root, &chosen, "second", false).unwrap();
        assert!(made.ends_with(" second"), "{made}");
        assert_eq!(states(&root), [("b.txt".into(), GitState::Modified)]);
        // Amending adds to the last commit, with its message kept unless changed.
        assert_eq!(last_message(&root).unwrap(), "second");
        let b = changed_files(&root).unwrap();
        commit(&root, &b, "second, with b", true).unwrap();
        assert!(states(&root).is_empty());
        assert_eq!(history(&root, 10).unwrap().len(), 2);
        assert!(commit(&root, &[], "nothing", false).is_err());
    }

    #[test]
    fn rollback_restores_the_commit_and_removes_new_files() {
        let (_dir, root) = repo();
        fs::write(root.join("a.txt"), "two\n").unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        fs::write(root.join("new.txt"), "new\n").unwrap();
        rollback(&root, &changed_files(&root).unwrap()).unwrap();
        assert!(states(&root).is_empty());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one\n");
        assert!(root.join("b.txt").exists() && !root.join("new.txt").exists());
    }

    #[test]
    fn a_conflict_is_settled_with_either_side() {
        let (_dir, root) = repo();
        run(&root, &["switch", "-q", "-c", "other"]);
        fs::write(root.join("a.txt"), "theirs\n").unwrap();
        run(&root, &["commit", "-q", "-am", "theirs"]);
        run(&root, &["switch", "-q", "main"]);
        fs::write(root.join("a.txt"), "ours\n").unwrap();
        run(&root, &["commit", "-q", "-am", "ours"]);
        let merged = git(&root, &["merge", "other"]);
        assert!(merged.is_err());
        let files = changed_files(&root).unwrap();
        assert_eq!(files[0].state, GitState::Conflicted);
        let (base, ours, theirs) = conflict_versions(&root, &files[0]).unwrap();
        assert_eq!(
            (base.as_str(), ours.as_str(), theirs.as_str()),
            ("one\n", "ours\n", "theirs\n")
        );
        take_side(&root, &files[0], false).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "theirs\n");
        assert_eq!(states(&root), [("a.txt".into(), GitState::Modified)]);
    }

    #[test]
    fn history_lists_commits_and_what_each_changed() {
        let (_dir, root) = repo();
        fs::write(root.join("a.txt"), "two\n").unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        run(&root, &["commit", "-q", "-am", "change a, drop b"]);
        let log = history(&root, 10).unwrap();
        assert_eq!(
            log.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(),
            ["change a, drop b", "first"]
        );
        let files = commit_files(&root, &log[0].hash).unwrap();
        let summary: Vec<(&str, GitState)> = files
            .iter()
            .map(|f| (f.relative.as_str(), f.state))
            .collect();
        assert_eq!(
            summary,
            [("a.txt", GitState::Modified), ("b.txt", GitState::Deleted)]
        );
        assert_eq!(text_at(&root, &log[0].hash, "a.txt", true), "one\n");
        assert_eq!(text_at(&root, &log[0].hash, "a.txt", false), "two\n");
        assert_eq!(text_at(&root, &log[0].hash, "b.txt", false), "");
    }

    #[test]
    fn cherry_picks_reverts_tags_and_the_log_of_every_branch() {
        let (_dir, root) = repo();
        run(&root, &["switch", "-q", "-c", "side"]);
        fs::write(root.join("c.txt"), "side\n").unwrap();
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "on side"]);
        run(&root, &["switch", "-q", "main"]);
        // The current branch's log hasn't it; every branch's has, labelled.
        assert_eq!(history(&root, 10).unwrap().len(), 1);
        let all = history_of(&root, 10, true).unwrap();
        assert_eq!(all[0].subject, "on side");
        assert_eq!(all[0].refs, ["side"]);
        assert_eq!(all[1].refs, ["HEAD -> main"]);
        cherry_pick(&root, &all[0].hash).unwrap();
        assert_eq!(fs::read_to_string(root.join("c.txt")).unwrap(), "side\n");
        let picked = history(&root, 1).unwrap().remove(0);
        revert(&root, &picked.hash).unwrap();
        assert!(!root.join("c.txt").exists());
        tag(&root, "v1", &picked.hash).unwrap();
        let tagged = history_of(&root, 10, true).unwrap();
        assert!(
            tagged
                .iter()
                .any(|c| c.refs.contains(&"tag: v1".to_string()))
        );
    }

    #[test]
    fn stashes_put_changes_aside_and_bring_them_back() {
        let (_dir, root) = repo();
        fs::write(root.join("a.txt"), "changed\n").unwrap();
        fs::write(root.join("new.txt"), "new\n").unwrap();
        stash(&root, "wip").unwrap();
        assert!(states(&root).is_empty());
        let list = stashes(&root).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "stash@{0}");
        assert!(list[0].message.ends_with("wip"), "{:?}", list[0]);
        unstash(&root, &list[0].name, false).unwrap();
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "changed\n");
        assert!(root.join("new.txt").exists());
        // Applied, so it's still there to drop.
        drop_stash(&root, "stash@{0}").unwrap();
        assert!(stashes(&root).unwrap().is_empty());
        rollback(&root, &changed_files(&root).unwrap()).unwrap();
        assert!(stash(&root, "").is_err(), "nothing to stash");
    }

    #[test]
    fn push_sets_the_upstream_the_first_time() {
        let (_dir, root) = repo();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare"]).unwrap();
        run(
            &root,
            &["remote", "add", "origin", &remote.path().to_string_lossy()],
        );
        assert_eq!(push(&root).unwrap(), "Pushed");
        assert!(git(&root, &["rev-parse", "@{upstream}"]).is_ok());
        assert_eq!(update(&root).unwrap(), "Already up to date");
    }
}
