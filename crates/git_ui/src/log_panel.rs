//! IntelliJ's Git tool window, its Log: the latest commits, and for the one chosen, the files it
//! changed; double-clicking a file shows what the commit did to it.

use gleipnir_core::ProjectEvent;
use gleipnir_index::GitState;
use gleipnir_index::git_ops::{self, ChangedFile, CommitInfo};
use gleipnir_ui::{Icon, Theme};
use gleipnir_workspace::{DockPosition, Panel, Workspace};
use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, Focusable, Subscription, Task, WeakEntity,
    Window, div, prelude::*, px,
};
use std::path::{Path, PathBuf};

pub const LOG_PANEL: &str = "git-log";
/// How many commits the log reads.
const LOG_LENGTH: usize = 300;

pub struct LogPanel {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    workspace: WeakEntity<Workspace>,
    commits: Vec<CommitInfo>,
    selected: Option<usize>,
    files: Vec<ChangedFile>,
    task: Option<Task<()>>,
    files_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LogPanel {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        if let Some(project) = workspace.project() {
            subscriptions.push(cx.subscribe(project, |this, _, event, cx| {
                if matches!(event, ProjectEvent::GitStatusChanged) {
                    this.refresh(cx);
                }
            }));
        }
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            root: workspace.root().map(Path::to_path_buf),
            workspace: workspace.weak_handle(),
            commits: Vec::new(),
            selected: None,
            files: Vec::new(),
            task: None,
            files_task: None,
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let read = cx
            .background_executor()
            .spawn(async move { git_ops::history(&root, LOG_LENGTH) });
        self.task = Some(cx.spawn(async move |this, cx| {
            let commits = read.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                // Keep the same commit chosen, wherever it's moved to.
                let chosen = this
                    .selected
                    .and_then(|i| this.commits.get(i))
                    .map(|c| c.hash.clone());
                this.commits = commits;
                this.selected = chosen.and_then(|h| this.commits.iter().position(|c| c.hash == h));
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn commits(&self) -> &[CommitInfo] {
        &self.commits
    }

    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let (Some(root), Some(commit)) = (self.root.clone(), self.commits.get(index).cloned())
        else {
            return;
        };
        self.selected = Some(index);
        self.files.clear();
        let read = cx
            .background_executor()
            .spawn(async move { git_ops::commit_files(&root, &commit.hash) });
        self.files_task = Some(cx.spawn(async move |this, cx| {
            let files = read.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.files = files;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn files(&self) -> &[ChangedFile] {
        &self.files
    }

    /// What the chosen commit did to one of its files.
    pub fn show_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(root), Some(commit), Some(file)) = (
            self.root.clone(),
            self.selected.and_then(|i| self.commits.get(i)).cloned(),
            self.files.get(index).cloned(),
        ) else {
            return;
        };
        let before = git_ops::text_at(&root, &commit.hash, &file.relative, true);
        let after = git_ops::text_at(&root, &commit.hash, &file.relative, false);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, move |_, cx| {
                    crate::diff_view::DiffView::new(
                        format!("{} in {} {}", file.relative, commit.short, commit.subject),
                        (format!("Before {}", commit.short), &before),
                        (commit.short.clone(), &after),
                        cx,
                    )
                });
            })
            .ok();
    }
}

impl Focusable for LogPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for LogPanel {
    fn id(&self) -> &'static str {
        LOG_PANEL
    }
    fn title(&self) -> &'static str {
        "Git"
    }
    fn icon(&self) -> Icon {
        Icon::Branch
    }
    fn position(&self) -> DockPosition {
        DockPosition::Bottom
    }
    fn debug_state(&self, _: &App) -> serde_json::Value {
        serde_json::json!({
            "commits": self.commits.iter().map(|c| c.subject.clone()).collect::<Vec<_>>(),
            "files": self.files.iter().map(|f| format!("{:?} {}", f.state, f.relative)).collect::<Vec<_>>(),
        })
    }
}

impl Render for LogPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let commits: Vec<AnyElement> = self
            .commits
            .iter()
            .enumerate()
            .map(|(index, commit)| {
                let selected = self.selected == Some(index);
                div()
                    .id(("log-commit", index))
                    .debug_selector(move || format!("log-commit-{index}"))
                    .h(px(22.))
                    .px(px(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .when(selected, |d| d.bg(theme.selection))
                    .when(!selected, |d| d.hover(|s| s.bg(theme.hover)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(commit.subject.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(140.))
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(commit.author.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(110.))
                            .text_color(theme.text_faint)
                            .child(commit.when.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(64.))
                            .text_color(theme.text_faint)
                            .child(commit.short.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)))
                    .into_any_element()
            })
            .collect();
        let files: Vec<AnyElement> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let color = match file.state {
                    GitState::Added => theme.git_added,
                    GitState::Deleted => theme.git_deleted,
                    _ => theme.git_modified,
                };
                div()
                    .id(("log-file", index))
                    .debug_selector(move || format!("log-file-{index}"))
                    .h(px(22.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .text_color(color)
                    .hover(|s| s.bg(theme.hover))
                    .child(file.relative.clone())
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.show_file(index, window, cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_row()
            .bg(theme.surface)
            .text_size(px(12.5))
            .text_color(theme.text)
            .child(
                div()
                    .id("log-commits")
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .py(px(4.))
                    .when(commits.is_empty(), |d| {
                        d.child(
                            div()
                                .p(px(12.))
                                .text_color(theme.text_muted)
                                .child("No commits"),
                        )
                    })
                    .children(commits),
            )
            .child(
                div()
                    .id("log-files")
                    .w(px(320.))
                    .flex_none()
                    .border_l_1()
                    .border_color(theme.border)
                    .overflow_y_scroll()
                    .py(px(4.))
                    .when(self.selected.is_none(), |d| {
                        d.child(
                            div()
                                .p(px(12.))
                                .text_color(theme.text_muted)
                                .child("Choose a commit to see its files"),
                        )
                    })
                    .children(files),
            )
    }
}
