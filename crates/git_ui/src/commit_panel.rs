//! IntelliJ's Commit tool window: the files that differ from the last commit, ticked to commit,
//! conflicts first with a side to take, and a message to commit them with. Double-clicking a file
//! shows its changes.

use gleipnir_core::{BufferModel, ProjectEvent};
use gleipnir_editor::EditorView;
use gleipnir_index::GitState;
use gleipnir_index::git_ops::{self, ChangedFile, Operation};
use gleipnir_ui::{FileIcon, Icon, Theme, file_icon};
use gleipnir_workspace::{DockPosition, Panel, Workspace};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    SharedString, Subscription, Task, WeakEntity, Window, div, prelude::*, px, svg,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const COMMIT_PANEL: &str = "commit";

pub struct CommitPanel {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    workspace: WeakEntity<Workspace>,
    files: Vec<ChangedFile>,
    /// Files left out of the next commit; the rest are in.
    unticked: HashSet<PathBuf>,
    selected: Option<usize>,
    message: Entity<EditorView>,
    amend: bool,
    /// What's running, like a commit, so it isn't started twice.
    busy: Option<&'static str>,
    /// A rebase or merge stopped partway, to continue or abort once its conflicts are settled.
    operation: Option<Operation>,
    refresh_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl CommitPanel {
    pub fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| BufferModel::new("", None, cx));
        let message = cx.new(|cx| {
            let mut editor = EditorView::new(buffer, window, cx);
            editor.set_plain(true, cx);
            editor
        });
        let mut subscriptions = Vec::new();
        if let Some(project) = workspace.project() {
            subscriptions.push(cx.subscribe(project, |this, _, event, cx| {
                if matches!(
                    event,
                    ProjectEvent::GitStatusChanged | ProjectEvent::FilesChanged(_)
                ) {
                    this.refresh(cx);
                }
            }));
        }
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            root: workspace.root().map(Path::to_path_buf),
            workspace: workspace.weak_handle(),
            files: Vec::new(),
            unticked: HashSet::new(),
            selected: None,
            message,
            amend: false,
            busy: None,
            operation: None,
            refresh_task: None,
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    pub fn files(&self) -> &[ChangedFile] {
        &self.files
    }

    /// Reads the changed files again, off the UI thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let read = cx.background_executor().spawn(async move {
            (
                git_ops::changed_files(&root),
                git_ops::operation_in_progress(&root),
            )
        });
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let (files, operation) = read.await;
            let files = files.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.operation = operation;
                let present: HashSet<&PathBuf> = files.iter().map(|f| &f.path).collect();
                this.unticked.retain(|p| present.contains(p));
                this.selected = this.selected.filter(|&i| i < files.len());
                this.files = files;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn message_text(&self, cx: &App) -> String {
        self.message.read(cx).buffer().read(cx).buffer().text()
    }

    pub fn set_message(&mut self, text: &str, cx: &mut Context<Self>) {
        let buffer = self.message.read(cx).buffer().clone();
        buffer.update(cx, |buffer, cx| {
            let len = buffer.buffer().len();
            buffer.apply_external_edits(vec![gleipnir_text_edit(0..len, text)], cx);
        });
    }

    /// Where focus goes to write the message.
    pub fn message_focus(&self, cx: &App) -> FocusHandle {
        self.message.read(cx).focus_handle(cx)
    }

    pub fn set_ticked(&mut self, path: &Path, ticked: bool, cx: &mut Context<Self>) {
        if ticked {
            self.unticked.remove(path);
        } else {
            self.unticked.insert(path.to_path_buf());
        }
        cx.notify();
    }

    fn ticked(&self) -> Vec<ChangedFile> {
        self.files
            .iter()
            .filter(|f| !self.unticked.contains(&f.path) && f.state != GitState::Conflicted)
            .cloned()
            .collect()
    }

    /// Amending starts from the last commit's message, as IntelliJ does.
    pub fn set_amend(&mut self, amend: bool, cx: &mut Context<Self>) {
        self.amend = amend;
        if amend
            && self.message_text(cx).trim().is_empty()
            && let Some(root) = &self.root
            && let Ok(last) = git_ops::last_message(root)
        {
            self.set_message(&last, cx);
        }
        cx.notify();
    }

    /// Commits the ticked files with the message, then pushes if asked.
    pub fn commit(&mut self, push: bool, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let files = self.ticked();
        let message = self.message_text(cx);
        let amend = self.amend;
        self.busy = Some(if push {
            "Committing and pushing…"
        } else {
            "Committing…"
        });
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let made = git_ops::commit(&root, &files, &message, amend)?;
            if push {
                git_ops::push(&root)?;
                return Ok(format!("Committed and pushed {made}"));
            }
            anyhow::Ok(format!("Committed {made}"))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                let worked = result.is_ok();
                if worked {
                    this.set_message("", cx);
                    this.amend = false;
                }
                this.after_git(result, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Puts the chosen file, or every ticked one, back as committed.
    pub fn rollback(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let files: Vec<ChangedFile> = match self.selected.and_then(|i| self.files.get(i)) {
            Some(file) => vec![file.clone()],
            None => self.ticked(),
        };
        if files.is_empty() {
            return;
        }
        let count = files.len();
        let work = cx.background_executor().spawn(async move {
            git_ops::rollback(&root, &files)?;
            anyhow::Ok(format!(
                "Rolled back {count} file{}",
                if count == 1 { "" } else { "s" }
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| this.after_git(result, cx)).ok();
        })
        .detach();
    }

    /// Settles a conflicted file with one side, or marks it resolved as edited.
    pub fn resolve(&mut self, index: usize, side: Option<bool>, cx: &mut Context<Self>) {
        let (Some(root), Some(file)) = (self.root.clone(), self.files.get(index).cloned()) else {
            return;
        };
        let work = cx.background_executor().spawn(async move {
            match side {
                Some(ours) => git_ops::take_side(&root, &file, ours)?,
                None => git_ops::mark_resolved(&root, &file)?,
            }
            anyhow::Ok(format!("Resolved {}", file.relative))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| this.after_git(result, cx)).ok();
        })
        .detach();
    }

    /// Says how a git command went, and brings everything that shows git's state up to date.
    fn after_git(&mut self, result: anyhow::Result<String>, cx: &mut Context<Self>) {
        self.refresh(cx);
        self.workspace
            .update(cx, |workspace, cx| {
                match result {
                    Ok(message) => workspace.show_message(message, cx),
                    Err(error) => workspace.show_error(format!("{error:#}"), cx),
                }
                refresh_git_state(workspace, cx);
            })
            .ok();
        cx.notify();
    }

    /// Shows a file's changes against the last commit, or for a conflict, the merge window.
    pub fn show_diff(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(index).cloned() else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| {
                if file.state == GitState::Conflicted {
                    crate::open_merge(workspace, file, window, cx)
                } else {
                    crate::show_file_diff(workspace, &file.path, window, cx)
                }
            })
            .ok();
    }

    fn render_file(
        &self,
        index: usize,
        file: &ChangedFile,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ticked = !self.unticked.contains(&file.path);
        let selected = self.selected == Some(index);
        let path = file.path.clone();
        let (name, folder) = match file.relative.rsplit_once('/') {
            Some((folder, name)) => (name.to_string(), folder.to_string()),
            None => (file.relative.clone(), String::new()),
        };
        let color = match file.state {
            GitState::Added | GitState::Untracked => theme.git_added,
            GitState::Deleted => theme.git_deleted,
            GitState::Conflicted => theme.git_conflicted,
            _ => theme.git_modified,
        };
        let conflicted = file.state == GitState::Conflicted;
        let side_button = |id: &'static str, label: &'static str, side: Option<bool>| {
            div()
                .id(SharedString::from(format!("{id}-{index}")))
                .debug_selector(move || format!("{id}-{index}"))
                .px(px(6.))
                .rounded(px(4.))
                .border_1()
                .border_color(theme.border)
                .text_size(px(11.))
                .cursor_pointer()
                .hover(|s| s.bg(theme.hover))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.resolve(index, side, cx)))
        };
        div()
            .id(("commit-file", index))
            .debug_selector(move || format!("commit-file-{index}"))
            .h(px(24.))
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .when(selected, |d| d.bg(theme.selection))
            .when(!selected, |d| d.hover(|s| s.bg(theme.hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.selected = Some(index);
                if event.click_count() >= 2 {
                    this.show_diff(index, window, cx);
                }
                cx.notify();
            }))
            .child(
                div()
                    .id(("commit-tick", index))
                    .debug_selector(move || format!("commit-tick-{index}"))
                    .size(px(14.))
                    .flex_none()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(if ticked && !conflicted {
                        theme.accent
                    } else {
                        theme.border
                    })
                    .when(ticked && !conflicted, |d| d.bg(theme.accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .text_color(theme.background)
                    .child(if ticked && !conflicted { "✓" } else { "" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_ticked(&path, !ticked, cx);
                    })),
            )
            .child(match file_icon(Path::new(&name)) {
                FileIcon::Badge(text, color) => div()
                    .w(px(16.))
                    .flex_none()
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(color)
                    .child(text)
                    .into_any_element(),
                FileIcon::Plain => svg()
                    .path(Icon::File.path())
                    .size(px(14.))
                    .flex_none()
                    .text_color(theme.text_muted)
                    .into_any_element(),
            })
            .child(div().flex_none().text_color(color).child(name))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme.text_faint)
                    .child(folder),
            )
            .when(conflicted, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("merge-{index}")))
                        .debug_selector(move || format!("merge-{index}"))
                        .px(px(6.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme.accent)
                        .text_color(theme.accent)
                        .text_size(px(11.))
                        .cursor_pointer()
                        .child("Merge…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.show_diff(index, window, cx)
                        })),
                )
                .child(side_button("take-yours", "Yours", Some(true)))
                .child(side_button("take-theirs", "Theirs", Some(false)))
                .child(side_button("mark-resolved", "Resolved", None))
            })
            .into_any_element()
    }
}

/// After a commit or the like: the project's file status, and what open files are compared to.
pub(crate) fn refresh_git_state(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    if let Some(project) = workspace.project().cloned() {
        project.update(cx, |project, cx| project.refresh_git(cx));
    }
    let store = workspace.store().clone();
    store.update(cx, |store, cx| store.refresh_committed_texts(cx));
}

fn gleipnir_text_edit(range: std::ops::Range<usize>, text: &str) -> gleipnir_text::TextEdit {
    gleipnir_text::TextEdit::new(range, text.to_string())
}

impl Focusable for CommitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for CommitPanel {
    fn id(&self) -> &'static str {
        COMMIT_PANEL
    }
    fn title(&self) -> &'static str {
        "Commit"
    }
    fn icon(&self) -> Icon {
        Icon::Commit
    }
    fn position(&self) -> DockPosition {
        DockPosition::Left
    }
    fn debug_state(&self, cx: &App) -> serde_json::Value {
        serde_json::json!({
            "files": self.files.iter().map(|f| {
                let ticked = !self.unticked.contains(&f.path);
                format!("{} {:?} {}", if ticked { "[x]" } else { "[ ]" }, f.state, f.relative)
            }).collect::<Vec<_>>(),
            "message": self.message_text(cx),
            "amend": self.amend,
            "busy": self.busy,
            "operation": self.operation.map(Operation::doing),
        })
    }
}

impl Render for CommitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let rows: Vec<AnyElement> = self
            .files
            .clone()
            .iter()
            .enumerate()
            .map(|(i, f)| self.render_file(i, f, &theme, cx))
            .collect();
        let ticked = self.ticked().len();
        let can_commit = self.busy.is_none() && (ticked > 0 || self.amend);
        let button = |id: &'static str, label: String, primary: bool, enabled: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(10.))
                .py(px(4.))
                .rounded(px(5.))
                .border_1()
                .border_color(if primary { theme.accent } else { theme.border })
                .when(primary, |d| d.bg(theme.accent).text_color(theme.background))
                .when(!enabled, |d| d.opacity(0.5))
                .when(enabled, |d| d.cursor_pointer())
                .child(label)
        };
        let toolbar_button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.bg(theme.hover))
                .child(label)
        };
        div()
            .key_context("CommitPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_color(theme.text)
            .on_action(cx.listener(|this, _: &crate::CommitAndKeep, _, cx| this.commit(false, cx)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .h(px(32.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Commit"),
                    )
                    .child(
                        toolbar_button("commit-refresh", "Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(toolbar_button("commit-diff", "Diff").on_click(cx.listener(
                        |this, _, window, cx| {
                            if let Some(index) = this.selected {
                                this.show_diff(index, window, cx);
                            }
                        },
                    )))
                    .child(
                        toolbar_button("commit-rollback", "Rollback")
                            .on_click(cx.listener(|this, _, _, cx| this.rollback(cx))),
                    ),
            )
            // What's stopped partway, with how to carry on, as IntelliJ shows it.
            .children(self.operation.map(|operation| {
                div()
                    .px(px(8.))
                    .py(px(5.))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .bg(theme.git_conflicted.opacity(0.12))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .child(format!("{} stopped", operation.doing())),
                    )
                    .child(
                        toolbar_button("operation-continue", "Continue")
                            .text_color(theme.accent)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.workspace.update(cx, crate::continue_operation).ok();
                            })),
                    )
                    .child(
                        toolbar_button("operation-abort", "Abort").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.workspace.update(cx, crate::abort_operation).ok();
                            },
                        )),
                    )
            }))
            .child(
                div()
                    .id("commit-files")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py(px(4.))
                    .when(rows.is_empty(), |d| {
                        d.child(div().p(px(12.)).text_color(theme.text_muted).child(
                            if self.root.is_some() {
                                "No changes"
                            } else {
                                "Open a project to commit"
                            },
                        ))
                    })
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(8.))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .h(px(96.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(theme.border)
                            .overflow_hidden()
                            .child(self.message.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("commit-amend")
                                    .debug_selector(|| "commit-amend".into())
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.))
                                    .cursor_pointer()
                                    .text_size(px(12.))
                                    .child(if self.amend { "☑" } else { "☐" })
                                    .child("Amend")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let amend = !this.amend;
                                        this.set_amend(amend, cx);
                                    })),
                            )
                            .child(div().flex_1())
                            .children(self.busy.map(|b| {
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.text_muted)
                                    .child(b)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .justify_end()
                            .gap(px(6.))
                            .child(
                                button(
                                    "commit-push-button",
                                    "Commit and Push".into(),
                                    false,
                                    can_commit,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if can_commit {
                                            this.commit(true, cx)
                                        }
                                    },
                                )),
                            )
                            .child(
                                button("commit-button", "Commit".into(), true, can_commit)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if can_commit {
                                            this.commit(false, cx)
                                        }
                                    })),
                            ),
                    ),
            )
    }
}
