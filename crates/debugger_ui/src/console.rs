//! A launch's console: the build's and the program's output, the debugger's messages, and for a
//! debug session, a line to evaluate expressions in.

use gleipnir_core::{ConsoleKind, ConsoleLine, Debugger, Launch, LaunchId};
use gleipnir_ui::Theme;
use gleipnir_ui::links::{find_links, resolve};
use gleipnir_ui::text_input::TextInput;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    ScrollStrategy, Subscription, UniformListScrollHandle, Window, actions, div, prelude::*, px,
    uniform_list,
};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

actions!(debug_console, [Submit]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", Submit, Some("DebugConsoleInput"))]);
}

const LINE_HEIGHT: f32 = 18.;

/// A place in a file mentioned in the output was clicked: the file, and a line and column from 0.
pub struct OpenLocation(pub PathBuf, pub usize, pub usize);

impl EventEmitter<OpenLocation> for ConsoleView {}

pub struct ConsoleView {
    debugger: Entity<Debugger>,
    launch: Option<LaunchId>,
    /// A node of the launch's test results, to show only its output.
    test: Option<usize>,
    /// That node's output, by the results' version it was gathered at.
    test_lines: Option<(LaunchId, usize, u64, Rc<Vec<ConsoleLine>>)>,
    scroll: UniformListScrollHandle,
    /// Lines last shown, to follow new output.
    shown: usize,
    /// Where input for the program is typed, and in a debug session, expressions while stopped.
    input: Option<Entity<TextInput>>,
    evaluates: bool,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl ConsoleView {
    pub fn new(debugger: Entity<Debugger>, evaluates: bool, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&debugger, |_, _, cx| cx.notify());
        Self {
            debugger,
            launch: None,
            test: None,
            test_lines: None,
            scroll: UniformListScrollHandle::new(),
            shown: 0,
            input: Some(cx.new(|cx| {
                TextInput::new(
                    if evaluates {
                        "Input to the program, or an expression to evaluate while stopped"
                    } else {
                        "Input to the program"
                    },
                    cx,
                )
            })),
            evaluates,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    pub fn show(&mut self, launch: Option<LaunchId>, cx: &mut Context<Self>) {
        if self.launch != launch {
            self.launch = launch;
            self.test = None;
            self.shown = 0;
            cx.notify();
        }
    }

    /// Shows only a test's output, or with `None`, everything.
    pub fn show_test(&mut self, test: Option<usize>, cx: &mut Context<Self>) {
        if self.test != test {
            self.test = test;
            self.shown = 0;
            cx.notify();
        }
    }

    /// The lines shown: the launch's console, or the chosen test's output.
    fn test_output(&mut self, cx: &App) -> Option<Rc<Vec<ConsoleLine>>> {
        let (id, test) = (self.launch?, self.test?);
        let launch = self.debugger.read(cx).launch(id)?;
        let tests = launch.tests.as_ref()?;
        if let Some((cached_id, cached_test, version, lines)) = &self.test_lines
            && (*cached_id, *cached_test, *version) == (id, test, tests.version())
        {
            return Some(lines.clone());
        }
        let lines = Rc::new(tests.output_of(test));
        self.test_lines = Some((id, test, tests.version(), lines.clone()));
        Some(lines)
    }

    /// The text shown, for tests and the driver.
    pub fn shown_text(&self, cx: &App) -> String {
        let Some(launch) = self.launch.and_then(|id| self.debugger.read(cx).launch(id)) else {
            return String::new();
        };
        let lines = match (self.test, &launch.tests) {
            (Some(test), Some(tests)) => tests.output_of(test),
            _ => launch.console.clone(),
        };
        lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Opens a place mentioned in the output, looking for a relative path where the launch ran,
    /// and beside its program and tests.
    pub fn open_link(
        &mut self,
        path: &str,
        line: u32,
        column: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let Some(launch) = self.launch.and_then(|id| self.debugger.read(cx).launch(id)) else {
            return;
        };
        if let Some(found) = resolve(path, &link_folders(launch)) {
            cx.emit(OpenLocation(
                found,
                line.saturating_sub(1) as usize,
                column.unwrap_or(1).saturating_sub(1) as usize,
            ));
        }
    }

    pub fn input(&self) -> Option<&Entity<TextInput>> {
        self.input.as_ref()
    }

    /// Types a line and presses Enter, for tests and the driver.
    pub fn enter(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(input) = &self.input {
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
        self.submit(cx);
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.input else { return };
        // Input goes as typed, spaces and all, and an empty line is an answer too.
        let expression = input.read(cx).text();
        let Some(id) = self.launch else { return };
        let evaluates = self.evaluates;
        let stopped = {
            let d = self.debugger.read(cx);
            d.is_paused() && d.active_debug_launch().map(|l| l.id) == Some(id)
        };
        if evaluates && stopped && expression.trim().is_empty() {
            return;
        }
        input.update(cx, |input, cx| input.set_text("", cx));
        self.debugger.update(cx, |d, cx| {
            if evaluates && stopped {
                d.evaluate_in_console(expression.trim(), cx);
            } else if d.accepts_input(id) {
                d.send_input(id, &expression, cx);
            } else if evaluates {
                d.evaluate_in_console(expression.trim(), cx);
            }
        });
    }

    fn line_count(&mut self, cx: &App) -> usize {
        if let Some(lines) = self.test_output(cx) {
            return lines.len();
        }
        self.launch
            .and_then(|id| self.debugger.read(cx).launch(id))
            .map_or(0, |l| l.console.len())
    }
}

/// Where a launch's relative paths may be from: where it ran, its program's and source's
/// folders, and the folders its tests were found in.
fn link_folders(launch: &Launch) -> Vec<PathBuf> {
    let target = &launch.target;
    let mut folders = vec![target.cwd.clone()];
    for path in [&target.program, &target.source].into_iter().flatten() {
        let folder = if path.is_dir() {
            path.clone()
        } else {
            path.parent().map(PathBuf::from).unwrap_or_default()
        };
        folders.push(folder);
    }
    if let Some(tests) = &launch.tests {
        for node in tests.nodes() {
            if let Some((file, _)) = &node.location
                && let Some(folder) = file.parent()
                && !folders.iter().any(|f| f == folder)
            {
                folders.push(folder.to_path_buf());
            }
        }
    }
    folders
}

impl Focusable for ConsoleView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.input {
            Some(input) => input.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Render for ConsoleView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let count = self.line_count(cx);
        if count > self.shown {
            self.scroll
                .scroll_to_item(count.saturating_sub(1), ScrollStrategy::Bottom);
        }
        self.shown = count;
        let lines = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            let shown: Vec<ConsoleLine> = match this.test_output(cx) {
                Some(lines) => range.filter_map(|i| lines.get(i).cloned()).collect(),
                None => {
                    let debugger = this.debugger.read(cx);
                    let Some(launch) = this.launch.and_then(|id| debugger.launch(id)) else {
                        return Vec::new();
                    };
                    range
                        .filter_map(|i| launch.console.get(i).cloned())
                        .collect()
                }
            };
            shown
                .into_iter()
                .enumerate()
                .map(|(row, line)| {
                    let color = match line.kind {
                        ConsoleKind::Stdout => theme.text,
                        ConsoleKind::Stderr | ConsoleKind::Error => theme.error,
                        ConsoleKind::Info => theme.text_muted,
                        ConsoleKind::Input => theme.accent,
                        ConsoleKind::Result => theme.syntax.string,
                    };
                    let mut row_div = div()
                        .id(("console-line", row))
                        .h(px(LINE_HEIGHT))
                        .px(px(10.))
                        .flex()
                        .flex_row()
                        .whitespace_nowrap()
                        .text_color(color);
                    if line.text.is_empty() {
                        return row_div.child(" ");
                    }
                    let mut at = 0;
                    for (index, link) in find_links(&line.text).into_iter().enumerate() {
                        if link.range.start > at {
                            row_div = row_div.child(line.text[at..link.range.start].to_string());
                        }
                        let (path, number, column) = (link.path.clone(), link.line, link.column);
                        row_div = row_div.child(
                            div()
                                .id(("console-link", index))
                                .text_color(theme.accent)
                                .underline()
                                .cursor_pointer()
                                .child(line.text[link.range.clone()].to_string())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_link(&path, number, column, cx)
                                })),
                        );
                        at = link.range.end;
                    }
                    if at < line.text.len() {
                        row_div = row_div.child(line.text[at..].to_string());
                    }
                    row_div
                })
                .collect::<Vec<_>>()
        });
        let font = gleipnir_editor::editor_font_family(window, cx);
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .font_family(font)
            .text_size(px(12.5))
            .child(
                uniform_list("console", count, lines)
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .py(px(4.)),
            )
            .when_some(self.input.clone(), |d, input| {
                d.child(
                    div()
                        .key_context("DebugConsoleInput")
                        .on_action(cx.listener(|this, _: &Submit, _, cx| this.submit(cx)))
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(4.))
                        .border_t_1()
                        .border_color(theme.border)
                        .child(div().text_color(theme.accent).child(">"))
                        .child(div().flex_1().child(input)),
                )
            })
    }
}
