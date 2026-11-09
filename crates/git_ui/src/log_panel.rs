//! IntelliJ's Git tool window, its Log: the latest commits with a graph of how branches fork and
//! join, and for the one chosen, the files it changed; double-clicking a file shows what the
//! commit did to it.

use crate::graph::{GraphRow, Level, graph};
use gleipnir_core::ProjectEvent;
use gleipnir_index::GitState;
use gleipnir_index::git_ops::{self, ChangedFile, CommitInfo};
use gleipnir_ui::{Icon, Theme};
use gleipnir_workspace::{DockPosition, Panel, Workspace};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, FocusHandle, Focusable, Hsla, PathBuilder,
    Pixels, ScrollStrategy, Subscription, Task, UniformListScrollHandle, WeakEntity, Window,
    canvas, div, fill, point, prelude::*, px, size, uniform_list,
};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const LOG_PANEL: &str = "git-log";
/// How many commits the log reads. Only the rows on screen are drawn, so this can be long.
const LOG_LENGTH: usize = 2000;
const ROW_HEIGHT: f32 = 22.;
/// How wide each lane of the graph is.
const LANE_WIDTH: f32 = 14.;

pub struct LogPanel {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    workspace: WeakEntity<Workspace>,
    commits: Rc<Vec<CommitInfo>>,
    graph: Rc<Vec<GraphRow>>,
    /// Every branch's commits, as IntelliJ's log starts, or only the current branch's.
    all_branches: bool,
    selected: Option<usize>,
    /// A commit to choose once the log has loaded, as when blame asks for one.
    reveal: Option<String>,
    files: Vec<ChangedFile>,
    scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
    loading: bool,
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
            commits: Default::default(),
            graph: Default::default(),
            all_branches: true,
            selected: None,
            reveal: None,
            files: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            task: None,
            loading: false,
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
        self.loading = true;
        let read = cx.background_executor().spawn({
            let all = self.all_branches;
            async move {
                let commits = git_ops::history_of(&root, LOG_LENGTH, all).unwrap_or_default();
                let graph = graph(
                    commits
                        .iter()
                        .map(|c| (c.hash.as_str(), c.parents.as_slice())),
                );
                (commits, graph)
            }
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let (commits, graph) = read.await;
            this.update(cx, |this, cx| {
                // Keep the same commit chosen, wherever it's moved to.
                let chosen = this
                    .selected
                    .and_then(|i| this.commits.get(i))
                    .map(|c| c.hash.clone());
                this.commits = Rc::new(commits);
                this.graph = Rc::new(graph);
                this.selected = chosen.and_then(|h| this.commits.iter().position(|c| c.hash == h));
                this.loading = false;
                if let Some(hash) = this.reveal.take() {
                    this.reveal(&hash, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_all_branches(&mut self, all: bool, cx: &mut Context<Self>) {
        self.all_branches = all;
        self.refresh(cx);
    }

    /// Chooses a commit by its hash, full or short, and scrolls to it. If the log is still
    /// loading, it's chosen once it has; returns false if it isn't in the log.
    pub fn reveal(&mut self, hash: &str, cx: &mut Context<Self>) -> bool {
        if self.loading {
            self.reveal = Some(hash.to_string());
            return true;
        }
        let Some(index) = self.commits.iter().position(|c| c.hash.starts_with(hash)) else {
            return false;
        };
        self.select(index, cx);
        self.scroll
            .scroll_to_item(index.saturating_sub(3), ScrollStrategy::Top);
        true
    }

    /// What can be done with a commit, as IntelliJ's log offers it.
    fn open_commit_menu(
        &mut self,
        index: usize,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(index, cx);
        let Some(commit) = self.commits.get(index).cloned() else {
            return;
        };
        let workspace = self.workspace.clone();
        let run = |label: &'static str, command: fn(&Path, &str) -> anyhow::Result<String>| {
            let (workspace, hash) = (workspace.clone(), commit.hash.clone());
            move |_: &mut Window, cx: &mut App| {
                let hash = hash.clone();
                workspace
                    .update(cx, |w, cx| {
                        crate::run_git_task(w, label, move |root| command(root, &hash), cx)
                    })
                    .ok();
            }
        };
        let named = |title: &'static str,
                     command: fn(&Path, &str, &str) -> anyhow::Result<String>| {
            let (workspace, hash) = (workspace.clone(), commit.hash.clone());
            move |window: &mut Window, cx: &mut App| {
                let hash = hash.clone();
                workspace
                    .update(cx, |w, cx| {
                        w.ask_for_name(
                            title,
                            "Name",
                            move |name, w, cx| {
                                crate::run_git_task(
                                    w,
                                    "Working…",
                                    move |root| command(root, &name, &hash),
                                    cx,
                                )
                            },
                            window,
                            cx,
                        )
                    })
                    .ok();
            }
        };
        let hash = commit.hash.clone();
        let rebase = {
            let (workspace, hash) = (workspace.clone(), commit.hash.clone());
            move |window: &mut Window, cx: &mut App| {
                workspace
                    .update(cx, |w, cx| {
                        crate::rebase_view::open_rebase(w, &hash, window, cx)
                    })
                    .ok();
            }
        };
        let menu = cx.new(|cx| {
            gleipnir_ui::ContextMenu::new(cx)
                .item("Copy Hash", None, move |_, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(hash.clone()))
                })
                .separator()
                .item(
                    "Cherry-Pick",
                    None,
                    run("Cherry-picking…", git_ops::cherry_pick),
                )
                .item("Revert Commit", None, run("Reverting…", git_ops::revert))
                .separator()
                .item("Interactively Rebase from Here…", None, rebase)
                .separator()
                .item(
                    "New Branch Here…",
                    None,
                    named("New branch here", |root, name, hash| {
                        git_ops::git(root, &["switch", "-c", name, hash])?;
                        Ok(format!("Created {name}"))
                    }),
                )
                .item(
                    "New Tag Here…",
                    None,
                    named("New tag here", |root, name, hash| {
                        git_ops::tag(root, name, hash)
                    }),
                )
        });
        self.workspace
            .update(cx, |w, cx| w.show_menu(menu, position, window, cx))
            .ok();
    }

    pub fn commits(&self) -> &[CommitInfo] {
        &self.commits
    }

    pub fn graph(&self) -> &[GraphRow] {
        &self.graph
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
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

    fn render_commit(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let commit = &self.commits[index];
        let selected = self.selected == Some(index);
        let row = self.graph.get(index).cloned();
        let palette = lane_colors(theme);
        div()
            .id(("log-commit", index))
            .debug_selector(move || format!("log-commit-{index}"))
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(4.))
            .pr(px(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .when(selected, |d| d.bg(theme.selection))
            .when(!selected, |d| d.hover(|s| s.bg(theme.hover)))
            .children(row.map(|row| {
                let width = LANE_WIDTH * row.width as f32;
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| paint_graph(&row, &palette, bounds, window),
                )
                .flex_none()
                .w(px(width))
                .h_full()
            }))
            // Branches and tags here, as IntelliJ labels them.
            .children(commit.refs.iter().map(|r| {
                let tag = r.starts_with("tag: ");
                let label = r.trim_start_matches("tag: ").replace("HEAD -> ", "");
                div()
                    .flex_none()
                    .px(px(5.))
                    .rounded(px(3.))
                    .text_size(px(11.))
                    .border_1()
                    .border_color(if tag { theme.warning } else { theme.accent })
                    .text_color(if tag { theme.warning } else { theme.accent })
                    .child(label)
            }))
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
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.open_commit_menu(index, event.position, window, cx)
                }),
            )
            .into_any_element()
    }
}

/// The graph's colours, from the theme so they suit it, used in turn by each new lane. A colour
/// too like one already in, as a theme's warning often is like its accent, is left out, so
/// lanes side by side don't look like one branch.
fn lane_colors(theme: &Theme) -> Vec<Hsla> {
    distinct_hues([
        theme.accent,
        theme.git_added,
        theme.syntax.keyword,
        theme.warning,
        theme.syntax.string,
        theme.syntax.function,
        theme.git_deleted,
        theme.syntax.type_,
    ])
}

/// The colours whose hues differ from every one kept before them, in order.
fn distinct_hues(colors: impl IntoIterator<Item = Hsla>) -> Vec<Hsla> {
    const CLOSEST: f32 = 25. / 360.;
    let mut kept: Vec<Hsla> = Vec::new();
    for color in colors {
        let near = |other: &Hsla| {
            let apart = (color.h - other.h).abs();
            apart.min(1. - apart) < CLOSEST
        };
        if !kept.iter().any(near) {
            kept.push(color);
        }
    }
    kept
}

/// One row of the graph: its lines, then the commit's dot over them.
fn paint_graph(row: &GraphRow, palette: &[Hsla], bounds: Bounds<Pixels>, window: &mut Window) {
    let x = |lane: usize| bounds.left() + px(LANE_WIDTH * (lane as f32 + 0.5));
    let y = |level: Level| match level {
        Level::Top => bounds.top(),
        Level::Middle => bounds.top() + bounds.size.height / 2.,
        Level::Bottom => bounds.bottom(),
    };
    let color = |index: usize| palette[index % palette.len()];
    for line in &row.lines {
        let mut path = PathBuilder::stroke(px(1.5));
        path.move_to(point(x(line.from.0), y(line.from.1)));
        path.line_to(point(x(line.to.0), y(line.to.1)));
        if let Ok(path) = path.build() {
            window.paint_path(path, color(line.color));
        }
    }
    let radius = px(3.5);
    let center = point(x(row.lane), y(Level::Middle));
    window.paint_quad(
        fill(
            Bounds::new(
                point(center.x - radius, center.y - radius),
                size(radius * 2., radius * 2.),
            ),
            color(row.color),
        )
        .corner_radii(radius),
    );
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
            // Each commit's lane, as `subject: lane`.
            "graph": self.commits.iter().zip(self.graph.iter())
                .map(|(c, r)| format!("{}: {}", c.subject, r.lane)).collect::<Vec<_>>(),
            "selected": self.selected.and_then(|i| self.commits.get(i)).map(|c| c.subject.clone()),
            "files": self.files.iter().map(|f| format!("{:?} {}", f.state, f.relative)).collect::<Vec<_>>(),
        })
    }
}

impl Render for LogPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let count = self.commits.len();
        let rows = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            range
                .map(|index| this.render_commit(index, &theme, cx))
                .collect::<Vec<_>>()
        });
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
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(28.))
                            .px(px(10.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .id("log-all-branches")
                                    .debug_selector(|| "log-all-branches".into())
                                    .cursor_pointer()
                                    .child(if self.all_branches {
                                        "☑ All branches"
                                    } else {
                                        "☐ All branches"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let all = !this.all_branches;
                                        this.set_all_branches(all, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id("log-refresh")
                                    .cursor_pointer()
                                    .text_color(theme.text_muted)
                                    .child("Refresh")
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .when(count == 0, |d| {
                                d.child(
                                    div()
                                        .p(px(12.))
                                        .text_color(theme.text_muted)
                                        .child("No commits"),
                                )
                            })
                            .when(count > 0, |d| {
                                d.child(
                                    uniform_list("log-commits", count, rows)
                                        .track_scroll(&self.scroll)
                                        .size_full(),
                                )
                            }),
                    ),
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

#[cfg(test)]
mod lane_color_tests {
    use super::*;

    #[test]
    fn colours_alike_are_kept_once() {
        let yellow = gpui::hsla(45. / 360., 0.8, 0.6, 1.);
        let amber = gpui::hsla(38. / 360., 0.9, 0.55, 1.);
        let green = gpui::hsla(120. / 360., 0.5, 0.5, 1.);
        let red = gpui::hsla(355. / 360., 0.7, 0.6, 1.);
        let pink = gpui::hsla(5. / 360., 0.7, 0.6, 1.);
        assert_eq!(
            distinct_hues([yellow, green, amber, red, pink]),
            [yellow, green, red]
        );
    }
}
