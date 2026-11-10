//! Interactive rebase without a terminal or an editor. The window prepares the whole plan, and
//! git is given it: `GIT_SEQUENCE_EDITOR` copies the todo list we wrote over git's own, and new
//! messages are put in place by `exec git commit --amend -F <file>` lines right after the commits
//! they belong to, so no editor is ever opened, even when a conflict stops the rebase and it's
//! continued later. Squashes are done as fixups followed by such a line, with the messages joined.

use anyhow::{Result, bail};
use gleipnir_index::git_ops::{self, Operation};
use std::path::{Path, PathBuf};

/// What to do with a commit, as git's todo list names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebaseAction {
    Pick,
    /// Keep the changes, with a new message.
    Reword,
    /// Fold into the commit before, adding this one's message to its.
    Squash,
    /// Fold into the commit before, dropping this one's message.
    Fixup,
    Drop,
}

impl RebaseAction {
    pub const ALL: [RebaseAction; 5] = [
        RebaseAction::Pick,
        RebaseAction::Reword,
        RebaseAction::Squash,
        RebaseAction::Fixup,
        RebaseAction::Drop,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RebaseAction::Pick => "pick",
            RebaseAction::Reword => "reword",
            RebaseAction::Squash => "squash",
            RebaseAction::Fixup => "fixup",
            RebaseAction::Drop => "drop",
        }
    }

    /// Whether the commit's message can be edited for this.
    pub fn takes_message(self) -> bool {
        matches!(self, RebaseAction::Reword | RebaseAction::Squash)
    }
}

/// One commit in the rebase, oldest first, as the todo list orders them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseEntry {
    pub hash: String,
    pub short: String,
    pub subject: String,
    /// The whole message, as committed.
    pub message: String,
    pub action: RebaseAction,
    /// A message to use instead, for a reword or what a squash adds.
    pub new_message: Option<String>,
}

impl RebaseEntry {
    pub fn message(&self) -> &str {
        self.new_message.as_deref().unwrap_or(&self.message)
    }
}

/// The todo list for git, and the messages to write for its `exec` lines, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebasePlan {
    pub todo: String,
    pub messages: Vec<String>,
}

/// A commit kept, and what's folded into it so far.
struct Group {
    message: String,
    /// Whether the message differs from what git would leave, so must be set.
    set_message: bool,
}

/// Works out the todo list for the entries, oldest first. `message_file` says where the `i`th
/// message will be written, for the line that uses it.
pub fn plan(
    entries: &[RebaseEntry],
    message_file: impl Fn(usize) -> PathBuf,
) -> Result<RebasePlan> {
    if entries.is_empty() {
        bail!("There are no commits to rebase");
    }
    let mut todo = String::new();
    let mut messages = Vec::new();
    let mut group: Option<Group> = None;
    let mut end_group = |group: Option<Group>, todo: &mut String| {
        if let Some(group) = group
            && group.set_message
        {
            let file = message_file(messages.len());
            messages.push(format!("{}\n", group.message.trim_end()));
            todo.push_str(&format!(
                "exec git commit --amend --allow-empty --no-verify --quiet -F {}\n",
                shell_quote(&file.to_string_lossy())
            ));
        }
    };
    for entry in entries {
        let line = |verb: &str| format!("{verb} {} {}\n", entry.hash, entry.subject);
        match entry.action {
            RebaseAction::Drop => todo.push_str(&line("drop")),
            RebaseAction::Pick | RebaseAction::Reword => {
                end_group(group.take(), &mut todo);
                todo.push_str(&line("pick"));
                let reword = entry.action == RebaseAction::Reword;
                if reword && entry.message().trim().is_empty() {
                    bail!("{}'s new message is empty", entry.short);
                }
                group = Some(Group {
                    message: if reword {
                        entry.message().to_string()
                    } else {
                        entry.message.clone()
                    },
                    set_message: reword && entry.message().trim_end() != entry.message.trim_end(),
                });
            }
            RebaseAction::Squash | RebaseAction::Fixup => {
                let Some(into) = group.as_mut() else {
                    bail!(
                        "{} can't be folded into the commit before it, as that isn't being kept",
                        entry.short
                    );
                };
                todo.push_str(&line("fixup"));
                if entry.action == RebaseAction::Squash {
                    into.message = format!(
                        "{}\n\n{}",
                        into.message.trim_end(),
                        entry.message().trim_end()
                    );
                    into.set_message = true;
                }
            }
        }
    }
    end_group(group.take(), &mut todo);
    Ok(RebasePlan { todo, messages })
}

/// Quotes a path for the shell git runs editors and `exec` lines with.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Where a rebase starts from and goes up to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseRange {
    /// HEAD when the commits were listed, to make sure nothing's been committed meanwhile.
    pub head: String,
    /// The commit the rebased ones go on, or none to rebase from the very first.
    pub onto: Option<String>,
}

/// The commits from `from` up to HEAD, oldest first, each to be picked, and the range they
/// span.
pub fn rebase_entries(root: &Path, from: &str) -> Result<(Vec<RebaseEntry>, RebaseRange)> {
    if let Some(operation) = git_ops::operation_in_progress(root) {
        bail!(
            "{} isn't finished: continue or abort it first",
            operation.doing()
        );
    }
    if git_ops::git(root, &["merge-base", "--is-ancestor", from, "HEAD"]).is_err() {
        bail!("Only commits on the current branch can be rebased from");
    }
    let head = git_ops::git(root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let onto = parent(root, from);
    let range = match &onto {
        Some(parent) => format!("{parent}..HEAD"),
        None => "HEAD".to_string(),
    };
    if !git_ops::git(root, &["rev-list", "--merges", &range])?
        .trim()
        .is_empty()
    {
        bail!("There are merges since this commit, which an interactive rebase would flatten");
    }
    let output = git_ops::git(
        root,
        &[
            "log",
            "--reverse",
            "--topo-order",
            "--format=%H%x1f%h%x1f%s%x1f%B%x1e",
            &range,
        ],
    )?;
    let entries = output
        .split('\u{1e}')
        .filter_map(|record| {
            let mut parts = record.trim_start().splitn(4, '\u{1f}');
            Some(RebaseEntry {
                hash: parts.next()?.to_string(),
                short: parts.next()?.to_string(),
                subject: parts.next()?.to_string(),
                message: parts.next()?.trim_end().to_string(),
                action: RebaseAction::Pick,
                new_message: None,
            })
        })
        .collect();
    Ok((entries, RebaseRange { head, onto }))
}

fn parent(root: &Path, hash: &str) -> Option<String> {
    git_ops::git(
        root,
        &["rev-parse", "--verify", "--quiet", &format!("{hash}^")],
    )
    .ok()
    .map(|p| p.trim().to_string())
}

/// Where the plan's files are kept while the rebase runs, which may outlast a restart.
fn plan_folder(root: &Path) -> Result<PathBuf> {
    Ok(git_ops::git_dir(root)?.join("gleipnir-rebase"))
}

/// Runs the rebase. Returns what happened: finished, or stopped at a conflict to settle.
pub fn start(root: &Path, range: &RebaseRange, entries: &[RebaseEntry]) -> Result<String> {
    let now = git_ops::git(root, &["rev-parse", "HEAD"])?;
    if now.trim() != range.head {
        bail!("The branch has changed since; open the rebase again");
    }
    let folder = plan_folder(root)?;
    std::fs::create_dir_all(&folder)?;
    let plan = plan(entries, |i| folder.join(format!("message-{i}.txt")))?;
    for (i, message) in plan.messages.iter().enumerate() {
        std::fs::write(folder.join(format!("message-{i}.txt")), message)?;
    }
    let todo = folder.join("todo");
    std::fs::write(&todo, &plan.todo)?;
    let sequence_editor = format!("cp {}", shell_quote(&todo.to_string_lossy()));
    let mut args = vec![
        // Commits left out of our list would be lost; with this, git refuses instead.
        "-c",
        "rebase.missingCommitsCheck=error",
        "rebase",
        "--interactive",
        "--no-autosquash",
    ];
    match &range.onto {
        Some(onto) => args.push(onto),
        None => args.push("--root"),
    }
    let result = git_ops::git_with(
        root,
        &args,
        &[
            ("GIT_SEQUENCE_EDITOR", &sequence_editor),
            ("GIT_EDITOR", "true"),
        ],
        None,
    );
    after(root, result, entries.len())
}

/// Says how the rebase went, and tidies up once it's over.
fn after(root: &Path, result: Result<String>, count: usize) -> Result<String> {
    if git_ops::operation_in_progress(root) == Some(Operation::Rebase) {
        let stopped = match &result {
            Err(error) => format!("{error:#}"),
            Ok(_) => String::new(),
        };
        let at = stopped
            .split("could not apply ")
            .nth(1)
            .map(|s| s.split("...").next().unwrap_or(s).trim().to_string());
        return Ok(match at {
            Some(at) => format!(
                "The rebase stopped at a conflict in {at}: settle it in the Commit window, then Continue"
            ),
            None => "The rebase stopped: settle it in the Commit window, then Continue".to_string(),
        });
    }
    finished(root);
    result?;
    Ok(format!(
        "Rebased {count} commit{}",
        if count == 1 { "" } else { "s" }
    ))
}

/// Removes the plan's files once no rebase is running.
pub fn finished(root: &Path) {
    if git_ops::operation_in_progress(root) != Some(Operation::Rebase)
        && let Ok(folder) = plan_folder(root)
    {
        std::fs::remove_dir_all(folder).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str, action: RebaseAction) -> RebaseEntry {
        RebaseEntry {
            hash: hash.to_string(),
            short: hash.to_string(),
            subject: format!("{hash} subject"),
            message: format!("{hash} subject\n\n{hash} body"),
            action,
            new_message: None,
        }
    }

    fn file(i: usize) -> PathBuf {
        PathBuf::from(format!("/tmp/it's/m{i}"))
    }

    #[test]
    fn picks_and_drops_go_through_as_they_are() {
        use RebaseAction::*;
        let plan = plan(
            &[entry("a", Pick), entry("b", Drop), entry("c", Pick)],
            file,
        )
        .unwrap();
        assert_eq!(
            plan.todo,
            "pick a a subject\ndrop b b subject\npick c c subject\n"
        );
        assert!(plan.messages.is_empty());
    }

    #[test]
    fn a_reword_amends_with_its_new_message_right_after_the_pick() {
        use RebaseAction::*;
        let mut reworded = entry("b", Reword);
        reworded.new_message = Some("Better words\n".into());
        let plan = plan(&[entry("a", Pick), reworded, entry("c", Pick)], file).unwrap();
        assert_eq!(
            plan.todo,
            "pick a a subject\n\
             pick b b subject\n\
             exec git commit --amend --allow-empty --no-verify --quiet -F '/tmp/it'\\''s/m0'\n\
             pick c c subject\n"
        );
        assert_eq!(plan.messages, ["Better words\n"]);
        // Reworded to the same message: nothing to do.
        let unchanged = plan_for(&[entry("a", Reword)]);
        assert_eq!(unchanged.todo, "pick a a subject\n");
    }

    fn plan_for(entries: &[RebaseEntry]) -> RebasePlan {
        plan(entries, file).unwrap()
    }

    #[test]
    fn squashes_join_messages_and_fixups_keep_the_first() {
        use RebaseAction::*;
        let mut squash = entry("c", Squash);
        squash.new_message = Some("c, said better".into());
        let plan = plan_for(&[
            entry("a", Pick),
            entry("b", Fixup),
            entry("x", Drop),
            squash,
            entry("d", Pick),
            entry("e", Fixup),
        ]);
        assert_eq!(
            plan.todo,
            "pick a a subject\n\
             fixup b b subject\n\
             drop x x subject\n\
             fixup c c subject\n\
             exec git commit --amend --allow-empty --no-verify --quiet -F '/tmp/it'\\''s/m0'\n\
             pick d d subject\n\
             fixup e e subject\n"
        );
        assert_eq!(plan.messages, ["a subject\n\na body\n\nc, said better\n"]);
    }

    #[test]
    fn a_squash_into_a_reword_uses_the_new_message() {
        use RebaseAction::*;
        let mut reword = entry("a", Reword);
        reword.new_message = Some("New a".into());
        let plan = plan_for(&[reword, entry("b", Squash)]);
        // One amend for the two, at the end of the group.
        assert_eq!(plan.todo.matches("exec").count(), 1);
        assert!(plan.todo.ends_with("'\n") && plan.todo.contains("fixup b"));
        assert_eq!(plan.messages, ["New a\n\nb subject\n\nb body\n"]);
    }

    #[test]
    fn nothing_to_fold_into_and_empty_messages_are_refused() {
        use RebaseAction::*;
        assert!(plan(&[entry("a", Squash)], file).is_err());
        assert!(plan(&[entry("a", Drop), entry("b", Fixup)], file).is_err());
        let mut empty = entry("a", Reword);
        empty.new_message = Some("  \n".into());
        assert!(plan(&[empty], file).is_err());
        assert!(plan(&[], file).is_err());
    }
}
