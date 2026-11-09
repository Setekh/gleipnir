//! Git, as IntelliJ has it: the Commit tool window, the Git log, diffs, push and update, and
//! rolling back changed lines from the editor.

mod commit_panel;
mod diff_view;
mod log_panel;
mod merge_view;

pub use commit_panel::{COMMIT_PANEL, CommitPanel};
pub use diff_view::{DiffView, Revertible};
pub use log_panel::{LOG_PANEL, LogPanel};
pub use merge_view::{Choice, MergeView};

use gleipnir_index::git_ops;
use gleipnir_workspace::Workspace;
use gpui::{App, AppContext as _, Context, KeyBinding, Window, actions, px};
use std::path::Path;

actions!(
    git,
    [
        /// Opens the Commit tool window with the cursor in the message.
        Commit,
        /// Commits from the message box.
        CommitAndKeep,
        Push,
        UpdateProject,
        ShowLog,
        ToggleCommitWindow,
        /// The current file's changes against the last commit.
        ShowDiff,
        /// Puts the changed lines at the cursor back as committed.
        RollbackLines,
        /// Puts uncommitted changes aside, with a message.
        StashChanges,
        /// Brings stashed changes back, or drops them.
        UnstashChanges,
    ]
);

pub fn bind_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-k", Commit, None),
        KeyBinding::new("cmd-shift-k", Push, None),
        KeyBinding::new("cmd-t", UpdateProject, None),
        KeyBinding::new("cmd-9", ShowLog, None),
        KeyBinding::new("cmd-0", ToggleCommitWindow, None),
        KeyBinding::new("cmd-alt-z", RollbackLines, Some("Editor")),
        KeyBinding::new("cmd-enter", CommitAndKeep, Some("CommitPanel")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-k", Commit, None),
        KeyBinding::new("ctrl-shift-k", Push, None),
        KeyBinding::new("ctrl-t", UpdateProject, None),
        KeyBinding::new("alt-9", ShowLog, None),
        KeyBinding::new("alt-0", ToggleCommitWindow, None),
        KeyBinding::new("ctrl-alt-z", RollbackLines, Some("Editor")),
        KeyBinding::new("ctrl-enter", CommitAndKeep, Some("CommitPanel")),
    ]);
    diff_view::bind_keys(cx);
    merge_view::bind_keys(cx);
}

pub fn add_to_workspace(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.root().is_none() {
        return;
    }
    let commit = cx.new(|cx| CommitPanel::new(workspace, window, cx));
    workspace.add_panel(commit, cx);
    let log = cx.new(|cx| LogPanel::new(workspace, cx));
    workspace.add_panel(log, cx);

    workspace.register_action(|workspace, _: &Commit, window, cx| {
        workspace.show_panel(COMMIT_PANEL, cx);
        if let Some(panel) = workspace.panel::<CommitPanel>(cx) {
            panel.update(cx, |panel, cx| panel.refresh(cx));
            let message = panel.read(cx).message_focus(cx);
            window.focus(&message, cx);
        }
    });
    workspace.register_action(|workspace, _: &ToggleCommitWindow, window, cx| {
        workspace.toggle_panel(COMMIT_PANEL, window, cx)
    });
    workspace.register_action(|workspace, _: &ShowLog, window, cx| {
        workspace.toggle_panel(LOG_PANEL, window, cx)
    });
    workspace.register_action(|workspace, _: &Push, _, cx| {
        run_in_background(workspace, "Pushing…", git_ops::push, cx)
    });
    workspace.register_action(|workspace, _: &UpdateProject, _, cx| {
        run_in_background(workspace, "Updating…", git_ops::update, cx)
    });
    workspace.register_action(|workspace, _: &ShowDiff, window, cx| {
        if let Some(path) = workspace.active_file(cx) {
            show_file_diff(workspace, &path, window, cx);
        }
    });
    workspace.register_action(|workspace, _: &RollbackLines, _, cx| rollback_lines(workspace, cx));
    workspace.register_action(|workspace, _: &StashChanges, window, cx| {
        workspace.ask_for_name(
            "Stash changes",
            "Message (optional)",
            |message, workspace, cx| {
                run_git_task(
                    workspace,
                    "Stashing…",
                    move |root| git_ops::stash(root, &message),
                    cx,
                )
            },
            window,
            cx,
        )
    });
    workspace.register_action(|workspace, _: &UnstashChanges, window, cx| {
        open_stashes(workspace, window, cx)
    });
}

/// Runs a git command that takes a while, off the UI thread, saying it's running and then how
/// it went, and brings what shows git's state up to date.
pub fn run_git_task(
    workspace: &mut Workspace,
    running: &'static str,
    command: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static,
    cx: &mut Context<Workspace>,
) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        return;
    };
    workspace.show_message(running, cx);
    let work = cx
        .background_executor()
        .spawn(async move { command(&root) });
    cx.spawn(async move |this, cx| {
        let result = work.await;
        this.update(cx, |workspace, cx| {
            match result {
                Ok(message) => workspace.show_message(message, cx),
                Err(error) => workspace.show_error(format!("{error:#}"), cx),
            }
            commit_panel::refresh_git_state(workspace, cx);
            if let Some(log) = workspace.panel::<LogPanel>(cx) {
                log.update(cx, |log, cx| log.refresh(cx));
            }
        })
        .ok();
    })
    .detach();
}

/// The stashes, each opening what can be done with it: pop, apply or drop.
fn open_stashes(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        return;
    };
    let stashes = git_ops::stashes(&root).unwrap_or_default();
    if stashes.is_empty() {
        workspace.show_message("There are no stashes", cx);
        return;
    }
    let position = gpui::point(window.viewport_size().width / 2. - px(160.), px(120.));
    let handle = cx.weak_entity();
    let menu = cx.new(|cx| {
        stashes
            .into_iter()
            .fold(gleipnir_ui::ContextMenu::new(cx), |menu, stash| {
                let handle = handle.clone();
                let label = format!("{}: {}", stash.name, stash.message);
                menu.item(label, None, move |window, cx| {
                    let name = stash.name.clone();
                    handle
                        .update(cx, |workspace, cx| {
                            stash_actions(workspace, name, position, window, cx)
                        })
                        .ok();
                })
            })
    });
    workspace.show_menu(menu, position, window, cx);
}

fn stash_actions(
    workspace: &mut Workspace,
    name: String,
    position: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let handle = cx.weak_entity();
    let action = |label: &'static str, pop: Option<bool>| {
        let (handle, name) = (handle.clone(), name.clone());
        move |_: &mut Window, cx: &mut App| {
            let name = name.clone();
            handle
                .update(cx, |workspace, cx| {
                    run_git_task(
                        workspace,
                        label,
                        move |root| match pop {
                            Some(pop) => git_ops::unstash(root, &name, pop),
                            None => git_ops::drop_stash(root, &name),
                        },
                        cx,
                    )
                })
                .ok();
        }
    };
    let menu = cx.new(|cx| {
        gleipnir_ui::ContextMenu::new(cx)
            .item("Pop", None, action("Unstashing…", Some(true)))
            .item("Apply", None, action("Unstashing…", Some(false)))
            .separator()
            .item("Drop", None, action("Dropping…", None))
    });
    workspace.show_menu(menu, position, window, cx);
}

/// Runs a git command that takes a while, saying it's running and then how it went.
fn run_in_background(
    workspace: &mut Workspace,
    running: &'static str,
    command: fn(&Path) -> anyhow::Result<String>,
    cx: &mut Context<Workspace>,
) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        return;
    };
    workspace.show_message(running, cx);
    let work = cx
        .background_executor()
        .spawn(async move { command(&root) });
    cx.spawn(async move |this, cx| {
        let result = work.await;
        this.update(cx, |workspace, cx| {
            match result {
                Ok(message) => workspace.show_message(message, cx),
                Err(error) => workspace.show_error(format!("{error:#}"), cx),
            }
            commit_panel::refresh_git_state(workspace, cx);
        })
        .ok();
    })
    .detach();
}

/// The merge window for a conflicted file.
pub fn open_merge(
    workspace: &mut Workspace,
    file: gleipnir_index::git_ops::ChangedFile,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        return;
    };
    let versions = match git_ops::conflict_versions(&root, &file) {
        Ok(versions) => versions,
        Err(error) => {
            workspace.show_error(format!("{error:#}"), cx);
            return;
        }
    };
    let handle = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |_, cx| {
        MergeView::new(root, file, versions, handle, cx)
    });
}

/// A file's changes against the last commit: the open buffer's text, with unsaved edits, or the
/// file on disk.
pub fn show_file_diff(
    workspace: &mut Workspace,
    path: &Path,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let buffer = workspace.store().read(cx).get(path);
    let committed = match &buffer {
        Some(buffer) => buffer.read(cx).committed_text().map(|t| t.to_string()),
        None => None,
    }
    .or_else(|| gleipnir_index::git::committed_text(path).ok().flatten())
    .unwrap_or_default();
    let now = match &buffer {
        Some(buffer) => buffer.read(cx).buffer().text(),
        None => std::fs::read_to_string(path).unwrap_or_default(),
    };
    let shown = workspace
        .root()
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .display()
        .to_string();
    let target = match buffer {
        Some(buffer) => Revertible::Buffer(buffer),
        None => Revertible::File(path.to_path_buf()),
    };
    workspace.toggle_modal(window, cx, move |_, cx| {
        DiffView::new(shown, ("Last commit", &committed), ("Now", &now), cx).revertible(target)
    });
}

/// IntelliJ's Rollback Lines: the change around the cursor goes back to what was committed, as
/// one step that undo takes back.
fn rollback_lines(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(editor) = workspace.active_editor(cx) else {
        return;
    };
    let buffer = editor.read(cx).buffer().clone();
    let model = buffer.read(cx);
    let text = model.buffer();
    let line = text
        .offset_to_point(editor.read(cx).selections().primary().head)
        .line;
    let Some(committed) = model.committed_text().cloned() else {
        workspace.show_message("This file isn't in a commit to roll back to", cx);
        return;
    };
    let changes = model.line_changes().clone();
    let Some(change) = changes
        .iter()
        .find(|c| c.lines.contains(&line) || (c.lines.is_empty() && c.lines.start == line))
    else {
        workspace.show_message("Nothing changed here since the last commit", cx);
        return;
    };
    let committed = committed.replace("\r\n", "\n");
    let base: Vec<&str> = committed.split_inclusive('\n').collect();
    let mut replacement: String = base
        .get(change.base_lines.clone())
        .map(|lines| lines.concat())
        .unwrap_or_default();
    let line_count = text.line_count();
    let start = text.line_start(change.lines.start.min(line_count));
    let end = if change.lines.end >= line_count {
        text.len()
    } else {
        text.line_start(change.lines.end)
    };
    // At the very end, without a final line break, the committed lines may have one to drop.
    if end == text.len() && !text.slice(start..end).ends_with('\n') && replacement.ends_with('\n') {
        replacement.pop();
    }
    buffer.update(cx, |buffer, cx| {
        buffer.apply_external_edits(
            vec![gleipnir_text::TextEdit::new(start..end, replacement)],
            cx,
        )
    });
    workspace.show_message("Rolled back the change", cx);
}

#[cfg(test)]
mod tests;
