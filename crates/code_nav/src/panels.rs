//! The bottom panels: Find, with the usages from the last search, and Problems, with every file's
//! diagnostics.

use gleipnir_core::{LanguageServerEvent, LanguageServers, Location, Severity};
use gleipnir_text::{Buffer, Utf16Point};
use gleipnir_ui::list::ListNav;
use gleipnir_ui::{Icon, Theme};
use gleipnir_workspace::{DockPosition, Panel, Workspace};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, HighlightStyle,
    KeyBinding, SharedString, StyledText, Subscription, Task, WeakEntity, Window, actions, div,
    prelude::*, px, uniform_list,
};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

actions!(code_nav_panel, [SelectNext, SelectPrevious, Open]);

pub const USAGES_PANEL: &str = "usages";
pub const PROBLEMS_PANEL: &str = "problems";
const ROW_HEIGHT: f32 = 22.;

pub fn bind_keys(cx: &mut App) {
    let context = Some("ResultsPanel");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("enter", Open, context),
    ]);
}

/// One line that a search found, ready to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub path: PathBuf,
    pub start: Utf16Point,
    /// Zero-based.
    pub line: usize,
    /// The line, without its indentation.
    pub text: String,
    /// What matched, as bytes of `text`.
    pub highlight: Range<usize>,
}

/// Reads the lines locations point at, from open buffers where there are any, as they may have
/// unsaved changes, and from disk otherwise.
pub fn previews(workspace: &Workspace, locations: Vec<Location>, cx: &App) -> Task<Vec<Usage>> {
    let store = workspace.store().read(cx);
    // Copied out as text, as the buffer can't leave the main thread.
    let mut open: HashMap<PathBuf, String> = HashMap::new();
    for location in &locations {
        if !open.contains_key(&location.path)
            && let Some(buffer) = store.get(&location.path)
        {
            open.insert(location.path.clone(), buffer.read(cx).buffer().text());
        }
    }
    cx.background_spawn(async move {
        let mut buffers: HashMap<PathBuf, Option<Buffer>> = HashMap::new();
        locations
            .into_iter()
            .filter_map(|location| {
                let buffer = buffers
                    .entry(location.path.clone())
                    .or_insert_with(|| {
                        let text = open
                            .remove(&location.path)
                            .or_else(|| std::fs::read_to_string(&location.path).ok())?;
                        Some(Buffer::new(&text))
                    })
                    .as_ref()?;
                Some(usage_at(buffer, location))
            })
            .collect()
    })
}

fn usage_at(buffer: &Buffer, location: Location) -> Usage {
    let start = buffer.utf16_to_offset(location.start);
    let end = buffer.utf16_to_offset(location.end).max(start);
    let point = buffer.offset_to_point(start);
    let line_start = buffer.line_start(point.line);
    let line = buffer.line(point.line);
    let indent = line.len() - line.trim_start().len();
    let text = line.trim_start().trim_end().to_string();
    let from = (start - line_start).saturating_sub(indent).min(text.len());
    let to = (end.min(line_start + line.len()) - line_start)
        .saturating_sub(indent)
        .clamp(from, text.len());
    Usage {
        path: location.path,
        start: location.start,
        line: point.line,
        text,
        highlight: from..to,
    }
}

/// A path to show: relative to the project, if it's inside it.
pub fn display_path(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|r| path.strip_prefix(r).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

enum Row {
    File { path: PathBuf, count: usize },
    Item(usize),
}

fn group_rows<T>(items: &[T], path_of: impl Fn(&T) -> &Path) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index < items.len() {
        let path = path_of(&items[index]).to_path_buf();
        let count = items[index..]
            .iter()
            .take_while(|i| path_of(i) == path)
            .count();
        rows.push(Row::File { path, count });
        rows.extend((index..index + count).map(Row::Item));
        index += count;
    }
    rows
}

fn header(theme: &Theme, title: SharedString, summary: String) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .gap(px(10.))
        .px(px(10.))
        .py(px(5.))
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .text_color(theme.text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_color(theme.text_faint).child(summary))
        .into_any_element()
}

fn file_row(theme: &Theme, root: Option<&Path>, path: &Path, count: usize) -> gpui::Div {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = path
        .parent()
        .map(|p| display_path(root, p))
        .unwrap_or_default();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_color(theme.text)
                .font_weight(FontWeight::MEDIUM)
                .child(name),
        )
        .child(div().text_color(theme.text_faint).child(folder))
        .child(div().text_color(theme.text_faint).child(format!("{count}")))
}

pub struct UsagesPanel {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    root: Option<PathBuf>,
    title: SharedString,
    usages: Vec<Usage>,
    rows: Vec<Row>,
    nav: ListNav,
    searching: bool,
}

impl UsagesPanel {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        root: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            root,
            title: "Find".into(),
            usages: Vec::new(),
            rows: Vec::new(),
            nav: ListNav::new(),
            searching: false,
        }
    }

    pub fn start_search(&mut self, title: String, cx: &mut Context<Self>) {
        self.title = title.into();
        self.searching = true;
        self.usages.clear();
        self.rows.clear();
        self.nav.set_count(0);
        cx.notify();
    }

    pub fn set_usages(&mut self, usages: Vec<Usage>, cx: &mut Context<Self>) {
        let mut usages = usages;
        usages.sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
        usages.dedup();
        self.rows = group_rows(&usages, |u| &u.path);
        self.usages = usages;
        self.searching = false;
        self.nav = ListNav::new();
        self.nav.set_count(self.rows.len());
        // The first usage, not its file's heading.
        self.nav.select(1.min(self.rows.len().saturating_sub(1)));
        cx.notify();
    }

    pub fn usages(&self) -> &[Usage] {
        &self.usages
    }

    /// Shows the selected usage in the editor while the list keeps the keyboard, as moving
    /// through results in IntelliJ's Find panel does.
    fn preview_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Item(index)) = self.nav.selected().and_then(|r| self.rows.get(r)) else {
            return;
        };
        let usage = self.usages[*index].clone();
        let focus = self.focus_handle.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                let open =
                    crate::open_location_then(workspace, usage.path, usage.start, window, cx);
                cx.spawn_in(window, async move |_, cx| {
                    open.await;
                    cx.update(|window, cx| window.focus(&focus, cx)).ok();
                })
                .detach();
            })
            .ok();
    }

    fn open_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Item(index)) = self.rows.get(row) else {
            return;
        };
        let usage = self.usages[*index].clone();
        self.workspace
            .update(cx, |workspace, cx| {
                crate::open_location(workspace, usage.path, usage.start, false, window, cx)
            })
            .ok();
    }

    fn render_row(&self, row: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.nav.selected() == Some(row);
        let content = match &self.rows[row] {
            Row::File { path, count } => file_row(theme, self.root.as_deref(), path, *count),
            Row::Item(index) => {
                let usage = &self.usages[*index];
                let highlight = HighlightStyle {
                    background_color: Some(theme.editor_selection),
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                };
                div()
                    .pl(px(18.))
                    .flex()
                    .flex_row()
                    .gap(px(10.))
                    .child(
                        div()
                            .w(px(40.))
                            .flex_none()
                            .text_color(theme.text_faint)
                            .child(format!("{}", usage.line + 1)),
                    )
                    .child(
                        div().whitespace_nowrap().text_color(theme.text).child(
                            StyledText::new(SharedString::from(usage.text.clone()))
                                .with_highlights(
                                    (!usage.highlight.is_empty())
                                        .then(|| (usage.highlight.clone(), highlight)),
                                ),
                        ),
                    )
            }
        };
        content
            .id(("usage-row", row))
            .debug_selector(|| format!("usage-row-{row}"))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(10.))
            .items_center()
            .when(selected, |r| r.bg(theme.selection))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.nav.select(row);
                    if event.click_count() >= 2 || !matches!(this.rows[row], Row::File { .. }) {
                        this.open_row(row, window, cx);
                    }
                    cx.notify();
                }),
            )
            .into_any_element()
    }
}

impl Focusable for UsagesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for UsagesPanel {
    fn id(&self) -> &'static str {
        USAGES_PANEL
    }
    fn title(&self) -> &'static str {
        "Find"
    }
    fn icon(&self) -> Icon {
        Icon::Search
    }
    fn position(&self) -> DockPosition {
        DockPosition::Bottom
    }
    fn debug_state(&self, _: &App) -> serde_json::Value {
        serde_json::json!({
            "title": self.title.to_string(),
            "searching": self.searching,
            "usages": self.usages.iter().map(|u| format!("{}:{}: {}", display_path(self.root.as_deref(), &u.path), u.line + 1, u.text)).collect::<Vec<_>>(),
        })
    }
}

impl Render for UsagesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let files = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::File { .. }))
            .count();
        let summary = if self.searching {
            "Searching…".to_string()
        } else {
            match self.usages.len() {
                0 => "No usages".into(),
                1 => "1 usage".into(),
                n => format!(
                    "{n} usages in {files} file{}",
                    if files == 1 { "" } else { "s" }
                ),
            }
        };
        let rows = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            range
                .map(|row| this.render_row(row, &theme, cx))
                .collect::<Vec<_>>()
        });
        div()
            .key_context("ResultsPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_size(px(12.5))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                this.nav.select_next();
                this.preview_selected(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.nav.select_previous();
                this.preview_selected(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Open, window, cx| {
                if let Some(row) = this.nav.selected() {
                    this.open_row(row, window, cx);
                }
            }))
            .child(header(&theme, self.title.clone(), summary))
            .child(
                uniform_list("usages", self.rows.len(), rows)
                    .track_scroll(self.nav.scroll_handle())
                    .flex_1(),
            )
    }
}

struct Problem {
    path: PathBuf,
    start: Utf16Point,
    severity: Severity,
    message: String,
}

pub struct ProblemsPanel {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    root: Option<PathBuf>,
    problems: Vec<Problem>,
    rows: Vec<Row>,
    nav: ListNav,
    _subscription: Subscription,
}

impl ProblemsPanel {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        root: Option<PathBuf>,
        servers: &Entity<LanguageServers>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(servers, |this, servers, event, cx| {
            if matches!(event, LanguageServerEvent::DiagnosticsChanged(_)) {
                this.refresh(&servers, cx);
            }
        });
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            workspace,
            root,
            problems: Vec::new(),
            rows: Vec::new(),
            nav: ListNav::new(),
            _subscription: subscription,
        };
        this.refresh(servers, cx);
        this
    }

    fn refresh(&mut self, servers: &Entity<LanguageServers>, cx: &mut Context<Self>) {
        let mut problems: Vec<Problem> = servers
            .read(cx)
            .all_diagnostics()
            .iter()
            .flat_map(|(path, list)| {
                list.iter().map(move |(range, severity, message)| Problem {
                    path: path.clone(),
                    start: Utf16Point::new(range.start.line, range.start.character),
                    severity: *severity,
                    message: message.lines().next().unwrap_or_default().to_string(),
                })
            })
            .filter(|p| p.severity <= Severity::Warning)
            .collect();
        problems.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.severity.cmp(&b.severity))
                .then(a.start.cmp(&b.start))
        });
        self.rows = group_rows(&problems, |p| &p.path);
        self.problems = problems;
        self.nav.set_count(self.rows.len());
        cx.notify();
    }

    pub fn counts(&self) -> (usize, usize) {
        let errors = self
            .problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
            .count();
        (errors, self.problems.len() - errors)
    }

    fn open_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Item(index)) = self.rows.get(row) else {
            return;
        };
        let (path, start) = (
            self.problems[*index].path.clone(),
            self.problems[*index].start,
        );
        self.workspace
            .update(cx, |workspace, cx| {
                crate::open_location(workspace, path, start, false, window, cx)
            })
            .ok();
    }

    fn render_row(&self, row: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.nav.selected() == Some(row);
        let content = match &self.rows[row] {
            Row::File { path, count } => file_row(theme, self.root.as_deref(), path, *count),
            Row::Item(index) => {
                let problem = &self.problems[*index];
                let color = if problem.severity == Severity::Error {
                    theme.error
                } else {
                    theme.warning
                };
                div()
                    .pl(px(18.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().size(px(7.)).flex_none().rounded_full().bg(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text)
                            .child(problem.message.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text_faint)
                            .child(format!(
                                ":{}:{}",
                                problem.start.line + 1,
                                problem.start.character + 1
                            )),
                    )
            }
        };
        content
            .id(("problem-row", row))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(10.))
            .items_center()
            .when(selected, |r| r.bg(theme.selection))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.nav.select(row);
                this.open_row(row, window, cx);
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Focusable for ProblemsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ProblemsPanel {
    fn id(&self) -> &'static str {
        PROBLEMS_PANEL
    }
    fn title(&self) -> &'static str {
        "Problems"
    }
    fn icon(&self) -> Icon {
        Icon::Problems
    }
    fn position(&self) -> DockPosition {
        DockPosition::Bottom
    }
    fn debug_state(&self, _: &App) -> serde_json::Value {
        serde_json::json!({
            "problems": self.problems.iter().map(|p| format!("{}:{}: {}", display_path(self.root.as_deref(), &p.path), p.start.line + 1, p.message)).collect::<Vec<_>>(),
        })
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let (errors, warnings) = self.counts();
        let summary = match (errors, warnings) {
            (0, 0) => "No problems".to_string(),
            (e, w) => format!(
                "{e} error{}, {w} warning{}",
                if e == 1 { "" } else { "s" },
                if w == 1 { "" } else { "s" }
            ),
        };
        let rows = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            range
                .map(|row| this.render_row(row, &theme, cx))
                .collect::<Vec<_>>()
        });
        div()
            .key_context("ResultsPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_size(px(12.5))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.nav.select_next();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.nav.select_previous();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Open, window, cx| {
                if let Some(row) = this.nav.selected() {
                    this.open_row(row, window, cx);
                }
            }))
            .child(header(&theme, "Problems".into(), summary))
            .child(
                uniform_list("problems", self.rows.len(), rows)
                    .track_scroll(self.nav.scroll_handle())
                    .flex_1(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_usage_shows_its_line_without_indentation_and_highlights_the_match() {
        let buffer = Buffer::new("fn main() {\n    let é = area(1);\n}");
        let usage = usage_at(
            &buffer,
            Location {
                path: "/p/a.rs".into(),
                start: Utf16Point::new(1, 12),
                end: Utf16Point::new(1, 16),
            },
        );
        assert_eq!(usage.line, 1);
        assert_eq!(usage.text, "let é = area(1);");
        assert_eq!(&usage.text[usage.highlight.clone()], "area");
    }

    #[test]
    fn rows_group_by_file() {
        let paths = [
            PathBuf::from("/a"),
            PathBuf::from("/a"),
            PathBuf::from("/b"),
        ];
        let rows = group_rows(&paths, |p| p);
        let shape: Vec<_> = rows
            .iter()
            .map(|r| match r {
                Row::File { count, .. } => format!("file {count}"),
                Row::Item(i) => format!("item {i}"),
            })
            .collect();
        assert_eq!(shape, ["file 2", "item 0", "item 1", "file 1", "item 2"]);
    }
}
