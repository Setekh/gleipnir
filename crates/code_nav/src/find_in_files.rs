//! Find in Files (Cmd+Shift+F): text or a pattern across the project, as you type, with a preview
//! of each match. Enter opens one; Cmd+Enter moves the results to the Find panel.

use crate::panels::{USAGES_PANEL, Usage, UsagesPanel};
use gleipnir_index::find::{FileMatch, FindOptions, MATCH_LIMIT, Matcher, find_in_files};
use gleipnir_index::search::Cancel;
use gleipnir_text::Utf16Point;
use gleipnir_ui::Theme;
use gleipnir_ui::list::ListNav;
use gleipnir_ui::text_input::{TextInput, TextInputEvent};
use gleipnir_workspace::{ModalView, Workspace};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, HighlightStyle, KeyBinding, SharedString, StyledText, Subscription, Task,
    WeakEntity, Window, actions, div, prelude::*, px, uniform_list,
};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

actions!(
    find_in_files,
    [
        SelectNext,
        SelectPrevious,
        Open,
        OpenInPanel,
        Dismiss,
        ToggleCase,
        ToggleWords,
        ToggleRegex
    ]
);

const ROW_HEIGHT: f32 = 24.;
const VISIBLE_ROWS: usize = 12;
/// Lines shown each side of the match in the preview.
const PREVIEW_CONTEXT: usize = 3;

pub fn bind_keys(cx: &mut App) {
    let context = Some("FindInFiles");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("enter", Open, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-enter", OpenInPanel, context),
        KeyBinding::new("alt-c", ToggleCase, context),
        KeyBinding::new("alt-w", ToggleWords, context),
        KeyBinding::new("alt-x", ToggleRegex, context),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", OpenInPanel, context),
        KeyBinding::new("alt-c", ToggleCase, context),
        KeyBinding::new("alt-w", ToggleWords, context),
        KeyBinding::new("alt-x", ToggleRegex, context),
    ]);
}

pub struct FindInFiles {
    workspace: WeakEntity<Workspace>,
    root: PathBuf,
    query: Entity<TextInput>,
    mask: Entity<TextInput>,
    options: FindOptions,
    matches: Vec<FileMatch>,
    files: usize,
    error: Option<String>,
    searching: bool,
    nav: ListNav,
    cancel: Cancel,
    _search: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for FindInFiles {}
impl ModalView for FindInFiles {}

impl Focusable for FindInFiles {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

/// Opens Find in Files, starting from the selection, if there's one on a single line.
pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(root) = workspace.root().map(PathBuf::from) else {
        workspace.show_message("Find in Files searches a project; open a folder first", cx);
        return;
    };
    let initial = workspace
        .active_editor(cx)
        .map(|editor| {
            let view = editor.read(cx);
            let range = view.selections().primary().range();
            view.buffer().read(cx).buffer().slice(range)
        })
        .filter(|text| !text.is_empty() && !text.contains('\n'))
        .unwrap_or_default();
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |_, cx| {
        FindInFiles::new(weak, root, &initial, cx)
    });
}

impl FindInFiles {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        root: PathBuf,
        initial: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            let mut input = TextInput::new("Text to find", cx);
            input.set_text(initial, cx);
            input
        });
        let mask = cx.new(|cx| TextInput::new("File mask, like *.rs, *.go", cx));
        let subscriptions = vec![
            cx.subscribe(&query, |this, _, _: &TextInputEvent, cx| this.search(cx)),
            cx.subscribe(&mask, |this, _, _: &TextInputEvent, cx| this.search(cx)),
        ];
        let mut this = Self {
            workspace,
            root,
            query,
            mask,
            options: FindOptions::default(),
            matches: Vec::new(),
            files: 0,
            error: None,
            searching: false,
            nav: ListNav::new(),
            cancel: Cancel::new(),
            _search: None,
            _subscriptions: subscriptions,
        };
        this.search(cx);
        this
    }

    pub fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.update(cx, |q, cx| q.set_text(text, cx));
        self.search(cx);
    }

    pub fn set_options(&mut self, options: FindOptions, cx: &mut Context<Self>) {
        self.mask
            .update(cx, |m, cx| m.set_text(&options.file_mask, cx));
        self.options = options;
        self.search(cx);
    }

    pub fn is_searching(&self) -> bool {
        self.searching
    }

    /// Each match as `path:line: text`, one-based, for tests and the driver.
    pub fn labels(&self) -> Vec<String> {
        self.matches
            .iter()
            .map(|m| format!("{}:{}: {}", m.path, m.line + 1, m.text))
            .collect()
    }

    pub fn summary(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        let count = self.matches.len();
        let more = if count >= MATCH_LIMIT { "+" } else { "" };
        match count {
            0 if self.searching => "Searching…".into(),
            0 => "No matches".into(),
            1 => "1 match".into(),
            _ => format!(
                "{count}{more} matches in {} file{}",
                self.files,
                if self.files == 1 { "" } else { "s" }
            ),
        }
    }

    /// Searches again, once typing pauses, cancelling a search still running.
    fn search(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.cancel = Cancel::new();
        let query = self.query.read(cx).text();
        self.options.file_mask = self.mask.read(cx).text();
        if query.is_empty() {
            self.matches.clear();
            self.error = None;
            self.searching = false;
            self.nav.set_count(0);
            cx.notify();
            return;
        }
        let matcher = match Matcher::new(&query, &self.options) {
            Ok(matcher) => matcher,
            Err(error) => {
                self.error = Some(error.to_string());
                self.matches.clear();
                self.nav.set_count(0);
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.searching = true;
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let (project, store) = {
            let workspace = workspace.read(cx);
            (workspace.project().cloned(), workspace.store().clone())
        };
        let files = project
            .map(|p| p.update(cx, |p, _| p.files()))
            .unwrap_or_default();
        // Open files with unsaved edits are searched as they stand.
        let mut open: HashMap<Arc<str>, String> = HashMap::new();
        for buffer in store.update(cx, |store, _| store.open_buffers()) {
            let model = buffer.read(cx);
            if let Some(relative) = model
                .path()
                .and_then(|p| p.strip_prefix(&self.root).ok())
                .filter(|_| model.buffer().is_dirty())
            {
                open.insert(relative.to_string_lossy().into(), model.buffer().text());
            }
        }
        let root = self.root.clone();
        let cancel = self.cancel.clone();
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let found = cx
                .background_spawn(
                    async move { find_in_files(&root, &files, &matcher, &open, &cancel) },
                )
                .await;
            this.update(cx, |this, cx| {
                let mut files: Vec<&Arc<str>> = found.iter().map(|m| &m.path).collect();
                files.dedup();
                this.files = files.len();
                this.matches = found;
                this.searching = false;
                this.nav.set_count(this.matches.len());
                if !this.matches.is_empty() && this.nav.selected().is_none() {
                    this.nav.select(0);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle(&mut self, change: impl FnOnce(&mut FindOptions), cx: &mut Context<Self>) {
        change(&mut self.options);
        self.search(cx);
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(found) = self
            .nav
            .selected()
            .and_then(|i| self.matches.get(i))
            .cloned()
        else {
            return;
        };
        cx.emit(DismissEvent);
        let path = self.root.join(&*found.path);
        self.workspace
            .update(cx, |w, cx| {
                let start = Utf16Point::new(found.line as u32, found.utf16_column as u32);
                crate::open_location(w, path, start, true, window, cx)
            })
            .ok();
    }

    /// Moves the results to the Find panel, to work through at leisure.
    fn open_in_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let usages: Vec<Usage> = self
            .matches
            .iter()
            .map(|m| Usage {
                path: self.root.join(&*m.path),
                start: Utf16Point::new(m.line as u32, m.utf16_column as u32),
                line: m.line,
                text: m.text.clone(),
                highlight: m.highlight.clone(),
            })
            .collect();
        let title = format!("Occurrences of '{}'", self.query.read(cx).text());
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                if let Some(panel) = workspace.panel::<UsagesPanel>(cx) {
                    panel.update(cx, |panel, cx| {
                        panel.start_search(title, cx);
                        panel.set_usages(usages, cx);
                    });
                }
                workspace.show_panel(USAGES_PANEL, cx);
                workspace.focus_center(window, cx);
            })
            .ok();
    }

    /// A few lines around the selected match, read from disk.
    fn preview(&self) -> Option<(Vec<(usize, String)>, usize)> {
        let found = self.nav.selected().and_then(|i| self.matches.get(i))?;
        let text = std::fs::read_to_string(self.root.join(&*found.path)).ok()?;
        let first = found.line.saturating_sub(PREVIEW_CONTEXT);
        let lines = text
            .lines()
            .enumerate()
            .skip(first)
            .take(PREVIEW_CONTEXT * 2 + 1)
            .map(|(i, l)| (i, l.replace('\t', "    ")))
            .collect();
        Some((lines, found.line))
    }

    fn render_row(&self, row: usize, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let found = &self.matches[row];
        let selected = self.nav.selected() == Some(row);
        let highlight = HighlightStyle {
            background_color: Some(theme.editor_selection),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let range = found.highlight.clone();
        let ranges: Vec<(Range<usize>, HighlightStyle)> = (range.end <= found.text.len()
            && found.text.is_char_boundary(range.start)
            && found.text.is_char_boundary(range.end))
        .then_some((range, highlight))
        .into_iter()
        .collect();
        let name = found
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&found.path)
            .to_string();
        div()
            .id(("find-row", row))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .when(selected, |r| r.bg(theme.selection))
            .hover(|r| r.bg(theme.hover))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.nav.select(row);
                    if event.click_count() > 1 {
                        this.open_selected(window, cx);
                    }
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .child(
                        StyledText::new(SharedString::from(found.text.clone()))
                            .with_highlights(ranges),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_faint)
                    .child(format!("{name} {}", found.line + 1)),
            )
            .into_any_element()
    }
}

impl Render for FindInFiles {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let count = self.matches.len();
        let rows = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            range
                .map(|row| this.render_row(row, &theme, cx))
                .collect::<Vec<_>>()
        });
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .cursor_pointer()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if on { theme.text } else { theme.text_faint })
                .when(on, |d| d.bg(theme.selection))
                .hover(|d| d.bg(theme.hover))
                .child(label)
        };
        let preview = self.preview().map(|(lines, at)| {
            div()
                .flex()
                .flex_col()
                .px(px(10.))
                .py(px(6.))
                .border_t_1()
                .border_color(theme.border)
                .bg(theme.editor_background)
                .font_family(if cfg!(target_os = "macos") {
                    "Menlo"
                } else {
                    "DejaVu Sans Mono"
                })
                .text_size(px(12.))
                .children(lines.into_iter().map(move |(line, text)| {
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(10.))
                        .when(line == at, |d| d.bg(theme.editor_active_line))
                        .child(
                            div()
                                .w(px(36.))
                                .flex_none()
                                .text_color(theme.editor_gutter_text)
                                .child((line + 1).to_string()),
                        )
                        .child(div().whitespace_nowrap().text_color(theme.text).child(text))
                }))
        });
        div()
            .key_context("FindInFiles")
            .occlude()
            .w(px(760.))
            .flex()
            .flex_col()
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(theme.ui_font_size))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.nav.select_next();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.nav.select_previous();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Open, window, cx| this.open_selected(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenInPanel, window, cx| this.open_in_panel(window, cx)),
            )
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &ToggleCase, _, cx| {
                this.toggle(|o| o.case_sensitive = !o.case_sensitive, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleWords, _, cx| {
                this.toggle(|o| o.whole_words = !o.whole_words, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleRegex, _, cx| this.toggle(|o| o.regex = !o.regex, cx)),
            )
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_muted)
                            .child("Find in Files"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme.text_faint)
                            .child(self.summary()),
                    )
                    .child(
                        toggle("find-case", "Cc", self.options.case_sensitive).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.toggle(|o| o.case_sensitive = !o.case_sensitive, cx)
                            }),
                        ),
                    )
                    .child(
                        toggle("find-words", "W", self.options.whole_words).on_click(cx.listener(
                            |this, _, _, cx| this.toggle(|o| o.whole_words = !o.whole_words, cx),
                        )),
                    )
                    .child(toggle("find-regex", ".*", self.options.regex).on_click(
                        cx.listener(|this, _, _, cx| this.toggle(|o| o.regex = !o.regex, cx)),
                    )),
            )
            .child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .flex()
                    .flex_row()
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().child(self.query.clone()))
                    .child(
                        div()
                            .w(px(180.))
                            .pl(px(8.))
                            .border_l_1()
                            .border_color(theme.border)
                            .child(self.mask.clone()),
                    ),
            )
            .child(if count == 0 {
                div()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.text_faint)
                    .child(if self.query.read(cx).text().is_empty() {
                        "Type to search the project"
                    } else {
                        ""
                    })
                    .into_any_element()
            } else {
                uniform_list("find-in-files", count, rows)
                    .track_scroll(self.nav.scroll_handle())
                    .h(px(ROW_HEIGHT * count.min(VISIBLE_ROWS) as f32))
                    .into_any_element()
            })
            .children(preview)
    }
}
