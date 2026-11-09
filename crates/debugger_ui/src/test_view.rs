//! A test run's results as a tree beside its console: each suite, group and test with its state
//! and time, the chosen test's output in the console, and running any of them again.

use crate::console::{ConsoleView, OpenLocation};
use crate::panels::tool_button;
use gleipnir_core::{Debugger, LaunchId, NodeKind, TEST_ROOT, TestRun, TestState};
use gleipnir_ui::{ContextMenu, Icon, MenuDismissed, Theme};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    MouseButton, MouseDownEvent, Pixels, Point, ScrollStrategy, Subscription,
    UniformListScrollHandle, Window, actions, anchored, deferred, div, prelude::*, px, svg,
    uniform_list,
};
use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

actions!(
    test_results,
    [
        SelectPrevious,
        SelectNext,
        Collapse,
        Expand,
        JumpToSource,
        RerunFailed
    ]
);

pub fn bind_keys(cx: &mut App) {
    let context = Some("TestResults");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("enter", JumpToSource, context),
        KeyBinding::new("f4", JumpToSource, context),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-down"
            } else {
                "ctrl-down"
            },
            JumpToSource,
            context,
        ),
    ]);
}

const ROW_HEIGHT: f32 = 22.;
const INDENT: f32 = 14.;

pub struct TestResultsView {
    debugger: Entity<Debugger>,
    console: Entity<ConsoleView>,
    launch: Option<LaunchId>,
    selected: Option<usize>,
    collapsed: HashSet<usize>,
    show_passed: bool,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    _subscription: Subscription,
}

impl EventEmitter<OpenLocation> for TestResultsView {}

/// A state's icon and color.
fn state_icon(state: TestState, theme: &Theme) -> (Icon, gpui::Hsla) {
    match state {
        TestState::Running => (Icon::TestRunning, theme.accent),
        TestState::Passed => (Icon::TestPassed, theme.git_added),
        TestState::Failed => (Icon::TestFailed, theme.error),
        TestState::Skipped => (Icon::TestSkipped, theme.text_muted),
        TestState::Stopped => (Icon::Stop, theme.text_muted),
    }
}

pub fn format_duration(duration: Duration) -> String {
    let ms = duration.as_secs_f64() * 1000.;
    if ms < 1. {
        "<1 ms".into()
    } else if ms < 1000. {
        format!("{} ms", ms.round())
    } else if ms < 60_000. {
        format!("{:.1} s", ms / 1000.)
    } else {
        let seconds = duration.as_secs();
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

/// What the summary line says: how many passed, failed and were skipped, of how many.
pub fn summary(tests: &TestRun) -> String {
    let c = tests.nodes()[TEST_ROOT].counts;
    let state = tests.state(TEST_ROOT);
    if c.total == 0 {
        return match state {
            TestState::Running => "Waiting for tests…".into(),
            TestState::Failed => "No tests ran: the run failed".into(),
            _ => "No tests ran".into(),
        };
    }
    let mut parts = Vec::new();
    if c.failed > 0 {
        parts.push(format!("{} failed", c.failed));
    }
    if c.passed > 0 || c.failed == 0 {
        parts.push(format!("{} passed", c.passed));
    }
    if c.skipped > 0 {
        parts.push(format!("{} skipped", c.skipped));
    }
    if c.stopped > 0 {
        parts.push(format!("{} stopped", c.stopped));
    }
    let done = c.passed + c.failed + c.skipped + c.stopped;
    let mut text = parts.join(", ");
    if state == TestState::Running {
        text.push_str(&format!(" ({done} of {} so far)", c.total));
    } else {
        text.push_str(&format!(" of {}", c.total));
    }
    text
}

impl TestResultsView {
    pub fn new(
        debugger: Entity<Debugger>,
        console: Entity<ConsoleView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&debugger, |_, _, cx| cx.notify());
        Self {
            debugger,
            console,
            launch: None,
            selected: None,
            collapsed: HashSet::new(),
            show_passed: gleipnir_core::settings(cx).tests.show_passed,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            menu: None,
            _subscription: subscription,
        }
    }

    pub fn show(&mut self, launch: Option<LaunchId>, cx: &mut Context<Self>) {
        if self.launch != launch {
            self.launch = launch;
            self.selected = None;
            self.collapsed.clear();
            self.console.update(cx, |c, cx| c.show_test(None, cx));
            cx.notify();
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The rows shown, as nodes and their depth: everything but collapsed nodes' children,
    /// suites with no tests, and with passed tests hidden, those.
    pub fn rows(&self, cx: &App) -> Vec<(usize, usize)> {
        let Some(tests) = self.tests(cx) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let mut stack = vec![(TEST_ROOT, 0)];
        while let Some((index, depth)) = stack.pop() {
            let node = &tests.nodes()[index];
            let state = tests.state(index);
            if node.kind == NodeKind::Suite
                && node.counts.total == 0
                && state != TestState::Failed
                && tests.is_finished()
            {
                continue;
            }
            if !self.show_passed
                && index != TEST_ROOT
                && matches!(state, TestState::Passed | TestState::Skipped)
            {
                continue;
            }
            rows.push((index, depth));
            if !self.collapsed.contains(&index) {
                stack.extend(node.children.iter().rev().map(|&c| (c, depth + 1)));
            }
        }
        rows
    }

    fn tests<'a>(&self, cx: &'a App) -> Option<&'a TestRun> {
        self.debugger.read(cx).launch(self.launch?)?.tests.as_ref()
    }

    pub fn select(&mut self, node: usize, cx: &mut Context<Self>) {
        self.selected = Some(node);
        let shown = (node != TEST_ROOT).then_some(node);
        self.console.update(cx, |c, cx| c.show_test(shown, cx));
        if let Some(row) = self.rows(cx).iter().position(|(n, _)| *n == node) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if rows.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| rows.iter().position(|(n, _)| *n == s));
        let next = match (at, forward) {
            (None, _) => 0,
            (Some(at), true) => (at + 1).min(rows.len() - 1),
            (Some(at), false) => at.saturating_sub(1),
        };
        self.select(rows[next].0, cx);
    }

    fn set_expanded(&mut self, expand: bool, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(tests) = self.tests(cx) else { return };
        let node = &tests.nodes()[selected];
        let has_children = !node.children.is_empty();
        let parent = node.parent;
        let open = has_children && !self.collapsed.contains(&selected);
        match (expand, open) {
            (true, false) if has_children => {
                self.collapsed.remove(&selected);
            }
            // Already open: step into it.
            (true, true) => self.step(true, cx),
            (false, true) => {
                self.collapsed.insert(selected);
            }
            (false, false) => {
                if let Some(parent) = parent {
                    self.select(parent, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    pub fn toggle(&mut self, node: usize, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&node) {
            self.collapsed.insert(node);
        }
        cx.notify();
    }

    /// Shows or hides passed tests, and remembers it for the next run, as IntelliJ does.
    pub fn set_show_passed(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_passed = show;
        if gleipnir_core::settings(cx).tests.show_passed != show {
            gleipnir_core::update_settings(cx, |s| s.tests.show_passed = show).ok();
        }
        cx.notify();
    }

    /// Opens where a node's test is declared, or its first test's.
    pub fn jump_to_source(&mut self, node: usize, cx: &mut Context<Self>) {
        let Some(id) = self.launch else { return };
        if let Some((path, line)) = self.debugger.read(cx).test_location(id, node) {
            cx.emit(OpenLocation(path, line as usize, 0));
        }
    }

    pub fn run(&mut self, node: usize, debug: bool, cx: &mut Context<Self>) {
        let Some(id) = self.launch else { return };
        self.debugger
            .update(cx, |d, cx| d.run_tests(id, &[node], debug, cx));
    }

    pub fn rerun_failed(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.launch else { return };
        self.debugger.update(cx, |d, cx| d.rerun_failed(id, cx));
    }

    fn open_menu(
        &mut self,
        node: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(node, cx);
        let Some(tests) = self.tests(cx) else { return };
        let label = match tests.nodes()[node].kind {
            NodeKind::Root => "All".to_string(),
            _ => tests.nodes()[node].label.clone(),
        };
        let view = cx.entity().downgrade();
        let item = |action: fn(&mut Self, usize, &mut Context<Self>)| {
            let view = view.clone();
            move |_: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| action(this, node, cx)).ok();
            }
        };
        let menu = cx.new(|cx| {
            ContextMenu::new(cx)
                .item(
                    format!("Run '{label}'"),
                    None,
                    item(|this, node, cx| this.run(node, false, cx)),
                )
                .item(
                    format!("Debug '{label}'"),
                    None,
                    item(|this, node, cx| this.run(node, true, cx)),
                )
                .separator()
                .item(
                    "Jump to Source",
                    Some("F4".into()),
                    item(|this, node, cx| this.jump_to_source(node, cx)),
                )
        });
        let focus = menu.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &MenuDismissed, window, cx| {
                this.menu = None;
                window.focus(&this.focus_handle, cx);
                cx.notify();
            });
        self.menu = Some((menu, position, subscription));
        cx.notify();
    }

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (text, failed, state) = match self.tests(cx) {
            Some(tests) => (
                summary(tests),
                self.launch
                    .is_some_and(|id| self.debugger.read(cx).has_failed_tests(id)),
                tests.state(TEST_ROOT),
            ),
            None => (String::new(), false, TestState::Running),
        };
        let (icon, color) = state_icon(state, theme);
        let show_passed = self.show_passed;
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .h(px(28.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                tool_button(
                    "tests-rerun-failed",
                    Icon::RerunFailed,
                    theme.error,
                    failed,
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.rerun_failed(cx))),
            )
            .child(
                tool_button(
                    "tests-show-passed",
                    Icon::TestPassed,
                    theme.git_added,
                    true,
                    theme,
                )
                .when(show_passed, |d| d.bg(theme.selection_inactive))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.set_show_passed(!show_passed, cx)),
                ),
            )
            .child(svg().path(icon.path()).size(px(13.)).text_color(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(text),
            )
    }
}

impl Focusable for TestResultsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TestResultsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let count = self.rows(cx).len();
        let rows = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            let rows = this.rows(cx);
            let Some(tests) = this.tests(cx) else {
                return Vec::new();
            };
            range
                .filter_map(|i| rows.get(i).copied())
                .map(|(index, depth)| {
                    let node = &tests.nodes()[index];
                    let (icon, color) = state_icon(tests.state(index), &theme);
                    let open = !this.collapsed.contains(&index);
                    let has_children = !node.children.is_empty();
                    let selected = this.selected == Some(index);
                    let label = if node.kind == NodeKind::Root {
                        "Test Results".to_string()
                    } else {
                        node.label.clone()
                    };
                    div()
                        .id(("test-row", index))
                        .h(px(ROW_HEIGHT))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .pl(px(6. + depth as f32 * INDENT))
                        .pr(px(8.))
                        .when(selected, |d| d.bg(theme.selection))
                        .when(!selected, |d| d.hover(|s| s.bg(theme.hover)))
                        .child(
                            div()
                                .id(("test-chevron", index))
                                .w(px(12.))
                                .flex_none()
                                .when(has_children, |d| {
                                    d.child(
                                        svg()
                                            .path(if open {
                                                Icon::ChevronDown.path()
                                            } else {
                                                Icon::ChevronRight.path()
                                            })
                                            .size(px(10.))
                                            .text_color(theme.text_muted),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle(index, cx)
                                        },
                                    ))
                                }),
                        )
                        .child(
                            svg()
                                .path(icon.path())
                                .flex_none()
                                .size(px(13.))
                                .text_color(color),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text)
                                .child(label)
                                .when_some(node.detail.clone(), |d, detail| {
                                    d.child(
                                        div().ml(px(6.)).text_color(theme.text_faint).child(detail),
                                    )
                                    .flex()
                                    .flex_row()
                                }),
                        )
                        .when_some(node.duration, |d, duration| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_color(theme.text_faint)
                                    .child(format_duration(duration)),
                            )
                        })
                        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                            this.select(index, cx);
                            if event.click_count() == 2 {
                                this.jump_to_source(index, cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.open_menu(index, event.position, window, cx)
                            }),
                        )
                })
                .collect::<Vec<_>>()
        });
        let menu = self.menu.as_ref().map(|(menu, position, _)| {
            deferred(
                anchored()
                    .position(*position)
                    .snap_to_window()
                    .child(menu.clone()),
            )
            .with_priority(2)
        });
        div()
            .key_context("TestResults")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| this.set_expanded(false, cx)))
            .on_action(cx.listener(|this, _: &Expand, _, cx| this.set_expanded(true, cx)))
            .on_action(cx.listener(|this, _: &JumpToSource, _, cx| {
                if let Some(node) = this.selected {
                    this.jump_to_source(node, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &RerunFailed, _, cx| this.rerun_failed(cx)))
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(12.5))
            .child(self.render_toolbar(&theme, cx))
            .child(
                uniform_list("test-results", count, rows)
                    .track_scroll(&self.scroll)
                    .flex_1(),
            )
            .children(menu)
    }
}
