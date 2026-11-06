//! The Run panel, for programs run on their own, and the Debug panel: its toolbar, the frames of
//! the stopped thread, the variables and watches, and the console.

use crate::console::{self, ConsoleView};
use crate::variables::{self, Row, WatchShown};
use crate::{EvaluateExpression, ViewBreakpoints};
use gleipnir_code_nav::{Picker, PickerItem};
use gleipnir_core::{Breakpoints, Debugger, Launch, LaunchId, LaunchStatus};
use gleipnir_dap::Step;
use gleipnir_ui::text_input::TextInput;
use gleipnir_ui::{Icon, Theme};
use gleipnir_workspace::{DockPosition, Panel, Workspace};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight,
    KeyBinding, Stateful, Subscription, WeakEntity, Window, actions, div, prelude::*, px, svg,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

pub const RUN_PANEL: &str = "run";
pub const DEBUG_PANEL: &str = "debug";

actions!(debug_panel, [AddWatch, CancelWatch]);

pub fn bind_keys(cx: &mut App) {
    console::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("enter", AddWatch, Some("WatchInput")),
        KeyBinding::new("escape", CancelWatch, Some("WatchInput")),
    ]);
}

const ROW_HEIGHT: f32 = 22.;

fn status_text(launch: &Launch) -> String {
    match &launch.status {
        LaunchStatus::Building => "Building…".into(),
        LaunchStatus::Starting => "Starting…".into(),
        LaunchStatus::Running => "Running".into(),
        LaunchStatus::Paused => "Paused".into(),
        LaunchStatus::Finished(Some(code)) => format!("Finished with exit code {code}"),
        LaunchStatus::Finished(None) => "Finished".into(),
        LaunchStatus::Failed(why) => why.clone(),
    }
}

fn tool_button(
    id: &'static str,
    icon: Icon,
    color: gpui::Hsla,
    enabled: bool,
    theme: &Theme,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .p(px(3.))
        .rounded(px(4.))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme.hover)))
        .child(
            svg()
                .path(icon.path())
                .size(px(14.))
                .text_color(if enabled { color } else { theme.text_faint }),
        )
}

/// Tabs for the launches a panel shows, when there's more than one.
fn launch_tabs<T: 'static>(
    launches: Vec<(LaunchId, String, bool, bool)>,
    theme: &Theme,
    cx: &mut Context<T>,
    on_click: fn(&mut T, LaunchId, &mut Context<T>),
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap(px(2.))
        .children(launches.into_iter().map(|(id, name, active, live)| {
            div()
                .id(("launch-tab", id as usize))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(active, |d| d.bg(theme.selection_inactive))
                .hover(|s| s.bg(theme.hover))
                .text_color(if live { theme.text } else { theme.text_muted })
                .child(name)
                .on_click(cx.listener(move |this, _, _, cx| on_click(this, id, cx)))
        }))
}

pub struct RunPanel {
    debugger: Entity<Debugger>,
    console: Entity<ConsoleView>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl RunPanel {
    pub fn new(
        debugger: Entity<Debugger>,
        _: WeakEntity<Workspace>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let console = cx.new(|cx| ConsoleView::new(debugger.clone(), false, cx));
        let subscription = cx.observe(&debugger, |this, _, cx| this.sync(cx));
        Self {
            debugger,
            console,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    pub fn console(&self) -> &Entity<ConsoleView> {
        &self.console
    }

    fn shown<'a>(&self, debugger: &'a Debugger) -> Option<&'a Launch> {
        debugger
            .active_launch()
            .filter(|l| !l.debug)
            .or_else(|| debugger.launches().iter().rev().find(|l| !l.debug))
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let id = self.shown(self.debugger.read(cx)).map(|l| l.id);
        self.console.update(cx, |c, cx| c.show(id, cx));
        cx.notify();
    }
}

impl Focusable for RunPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for RunPanel {
    fn id(&self) -> &'static str {
        RUN_PANEL
    }
    fn title(&self) -> &'static str {
        "Run"
    }
    fn icon(&self) -> Icon {
        Icon::Run
    }
    fn position(&self) -> DockPosition {
        DockPosition::Bottom
    }
    fn debug_state(&self, cx: &App) -> serde_json::Value {
        let debugger = self.debugger.read(cx);
        match self.shown(debugger) {
            Some(launch) => serde_json::json!({
                "name": launch.name,
                "status": status_text(launch),
                "console": launch.console_text(),
            }),
            None => serde_json::Value::Null,
        }
    }
}

impl Render for RunPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let debugger = self.debugger.read(cx);
        let shown = self.shown(debugger);
        let (id, status, live) = match shown {
            Some(l) => (Some(l.id), status_text(l), l.status.is_live()),
            None => (None, String::new(), false),
        };
        let tabs: Vec<_> = debugger
            .launches()
            .iter()
            .filter(|l| !l.debug)
            .map(|l| (l.id, l.name.clone(), Some(l.id) == id, l.status.is_live()))
            .collect();
        let many = tabs.len() > 1;
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_size(px(12.5))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .h(px(30.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        tool_button(
                            "run-rerun",
                            Icon::Rerun,
                            theme.git_added,
                            id.is_some(),
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(id) = id {
                                this.debugger.update(cx, |d, cx| d.rerun(id, cx));
                            }
                        })),
                    )
                    .child(
                        tool_button("run-stop", Icon::Stop, theme.error, live, &theme).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if let Some(id) = id {
                                    this.debugger.update(cx, |d, cx| d.stop(id, cx));
                                }
                            }),
                        ),
                    )
                    .when(many, |d| {
                        d.child(launch_tabs(tabs, &theme, cx, |this: &mut Self, id, cx| {
                            this.debugger.update(cx, |d, cx| d.set_active(id, cx))
                        }))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(status),
                    ),
            )
            .child(match id {
                Some(_) => div().flex_1().min_h_0().child(self.console.clone()),
                None => div()
                    .flex_1()
                    .p(px(12.))
                    .text_color(theme.text_faint)
                    .child(empty_hint("Nothing has run yet.", false)),
            })
    }
}

fn empty_hint(first: &str, debug: bool) -> String {
    let keys = match (cfg!(target_os = "macos"), debug) {
        (true, false) => "⌃R",
        (true, true) => "⌃D",
        (false, false) => "Shift+F10",
        (false, true) => "Shift+F9",
    };
    let what = if debug { "debug" } else { "run" };
    format!("{first} {keys} to {what} the configuration chosen in the status bar.")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Tab {
    Debugger,
    Console,
}

/// What the Debug panel saves with the project: its breakpoints, watches and run configuration.
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    breakpoints: Breakpoints,
    #[serde(default)]
    watches: Vec<String>,
    #[serde(default)]
    configuration: Option<String>,
    /// Each adapter's exception filters, as last seen.
    #[serde(default)]
    exception_filters: std::collections::BTreeMap<String, Vec<gleipnir_dap::ExceptionFilter>>,
    /// Filters turned on or off by hand.
    #[serde(default)]
    exception_choices: std::collections::BTreeMap<String, bool>,
    /// Arguments, environment and folder set for run configurations.
    #[serde(default)]
    run_settings: std::collections::BTreeMap<String, gleipnir_core::RunSettings>,
}

pub struct DebugPanel {
    debugger: Entity<Debugger>,
    workspace: WeakEntity<Workspace>,
    console: Entity<ConsoleView>,
    focus_handle: FocusHandle,
    tab: Tab,
    /// Variables whose open state differs from the default, by name path.
    toggled: HashSet<String>,
    watch_input: Option<Entity<TextInput>>,
    _subscription: Subscription,
}

impl DebugPanel {
    pub fn new(
        debugger: Entity<Debugger>,
        workspace: WeakEntity<Workspace>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let console = cx.new(|cx| ConsoleView::new(debugger.clone(), true, cx));
        let subscription = cx.observe(&debugger, |this, _, cx| this.sync(cx));
        Self {
            debugger,
            workspace,
            console,
            focus_handle: cx.focus_handle(),
            tab: Tab::Debugger,
            toggled: HashSet::new(),
            watch_input: None,
            _subscription: subscription,
        }
    }

    pub fn debugger(&self) -> &Entity<Debugger> {
        &self.debugger
    }

    pub fn console(&self) -> &Entity<ConsoleView> {
        &self.console
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let id = self.debugger.read(cx).active_debug_launch().map(|l| l.id);
        self.console.update(cx, |c, cx| c.show(id, cx));
        cx.notify();
    }

    fn rows(&self, cx: &App) -> (Vec<Row>, Vec<i64>) {
        let debugger = self.debugger.read(cx);
        let session = debugger
            .active_debug_launch()
            .and_then(Launch::focused)
            .filter(|s| s.is_stopped());
        variables::rows(session, debugger.watches(), &self.toggled)
    }

    /// Opens or closes a row, fetching its children if they're needed.
    pub fn toggle(&mut self, key: &str, reference: Option<i64>, cx: &mut Context<Self>) {
        if !self.toggled.remove(key) {
            self.toggled.insert(key.to_string());
        }
        if let Some(reference) = reference {
            self.debugger
                .update(cx, |d, cx| d.load_variables(reference, cx));
        }
        cx.notify();
    }

    /// Opens the popup to change a variable's value, if the adapter can.
    pub fn set_value(
        &mut self,
        container: i64,
        name: String,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.debugger.read(cx).can_set_values() {
            return;
        }
        let debugger = self.debugger.clone();
        let value = value.to_string();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, move |_, cx| {
                    crate::SetValueModal::new(debugger, container, name, &value, cx)
                })
            })
            .ok();
    }

    /// Variables as shown, one per line, indented by depth: for tests and the driver.
    pub fn variable_lines(&self, cx: &App) -> Vec<String> {
        self.rows(cx)
            .0
            .iter()
            .map(|row| match row {
                Row::Watch {
                    expression, value, ..
                } => match value {
                    WatchShown::Pending => format!("{expression} = …"),
                    WatchShown::Value { text, .. } => format!("{expression} = {text}"),
                    WatchShown::Error(e) => format!("{expression}: {e}"),
                },
                Row::Scope { name, .. } => name.clone(),
                Row::Variable {
                    name, value, depth, ..
                } => format!("{}{name} = {value}", "  ".repeat(*depth)),
                Row::Loading { depth } => format!("{}…", "  ".repeat(*depth)),
            })
            .collect()
    }

    pub fn show_console(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::Console;
        cx.notify();
    }

    pub fn start_adding_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new("Expression to watch", cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.watch_input = Some(input);
        cx.notify();
    }

    fn add_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.watch_input.take() {
            let expression = input.read(cx).text();
            self.debugger
                .update(cx, |d, cx| d.add_watch(expression, cx));
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn choose_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let debugger = self.debugger.clone();
        let Some(session) = debugger
            .read(cx)
            .active_debug_launch()
            .and_then(Launch::focused)
        else {
            return;
        };
        let threads = session.threads.clone();
        let selected = threads
            .iter()
            .position(|t| Some(t.id) == session.thread)
            .unwrap_or(0);
        let items = threads
            .iter()
            .map(|t| PickerItem {
                label: t.name.clone(),
                detail: format!("#{}", t.id),
                indent: 0,
            })
            .collect();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, move |_, cx| {
                    Picker::new(
                        "Thread",
                        items,
                        selected,
                        move |index, _, cx| {
                            let id = threads[index].id;
                            debugger.update(cx, |d, cx| d.select_thread(id, cx));
                        },
                        cx,
                    )
                })
            })
            .ok();
    }

    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let debugger = self.debugger.read(cx);
        let launch = debugger.active_debug_launch();
        let id = launch.map(|l| l.id);
        let live = launch.is_some_and(|l| l.status.is_live());
        let paused = debugger.is_paused();
        let running = launch.is_some_and(|l| l.status == LaunchStatus::Running);
        let status = launch.map(status_text).unwrap_or_default();
        let tabs: Vec<_> = debugger
            .launches()
            .iter()
            .filter(|l| l.debug)
            .map(|l| (l.id, l.name.clone(), Some(l.id) == id, l.status.is_live()))
            .collect();
        let many = tabs.len() > 1;
        let step = |step: Step| {
            move |this: &mut Self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                this.debugger.update(cx, |d, cx| d.resume(step, cx));
            }
        };
        let separator = || div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(theme.border);
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .h(px(30.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                tool_button(
                    "debug-rerun",
                    Icon::Rerun,
                    theme.git_added,
                    id.is_some(),
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(id) = id {
                        this.debugger.update(cx, |d, cx| d.rerun(id, cx));
                    }
                })),
            )
            .child(
                tool_button("debug-resume", Icon::Resume, theme.git_added, paused, theme)
                    .on_click(cx.listener(step(Step::Continue))),
            )
            .child(
                tool_button("debug-pause", Icon::Pause, theme.text, running, theme).on_click(
                    cx.listener(|this, _, _, cx| this.debugger.update(cx, |d, cx| d.pause(cx))),
                ),
            )
            .child(
                tool_button("debug-stop", Icon::Stop, theme.error, live, theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(id) = id {
                            this.debugger.update(cx, |d, cx| d.stop(id, cx));
                        }
                    }),
                ),
            )
            .child(separator())
            .child(
                tool_button("debug-step-over", Icon::StepOver, theme.text, paused, theme)
                    .on_click(cx.listener(step(Step::Over))),
            )
            .child(
                tool_button("debug-step-into", Icon::StepInto, theme.text, paused, theme)
                    .on_click(cx.listener(step(Step::Into))),
            )
            .child(
                tool_button("debug-step-out", Icon::StepOut, theme.text, paused, theme)
                    .on_click(cx.listener(step(Step::Out))),
            )
            .child(
                tool_button(
                    "debug-run-to-cursor",
                    Icon::RunToCursor,
                    theme.text,
                    paused,
                    theme,
                )
                .on_click(cx.listener(|_, _, window, cx| {
                    window.dispatch_action(Box::new(crate::RunToCursor), cx)
                })),
            )
            .child(separator())
            .child(
                tool_button("debug-evaluate", Icon::Console, theme.text, paused, theme).on_click(
                    cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(EvaluateExpression), cx)
                    }),
                ),
            )
            .child(
                tool_button(
                    "debug-breakpoints",
                    Icon::Breakpoints,
                    theme.breakpoint,
                    true,
                    theme,
                )
                .on_click(cx.listener(|_, _, window, cx| {
                    window.dispatch_action(Box::new(ViewBreakpoints), cx)
                })),
            )
            .child(separator())
            .when(many, |d| {
                d.child(launch_tabs(tabs, theme, cx, |this: &mut Self, id, cx| {
                    this.debugger.update(cx, |d, cx| d.set_active(id, cx))
                }))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(status),
            )
    }

    fn render_frames(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let debugger = self.debugger.read(cx);
        let root = debugger.root().map(Path::to_path_buf);
        let Some(session) = debugger
            .active_debug_launch()
            .and_then(Launch::focused)
            .filter(|s| s.is_stopped())
        else {
            let running = debugger
                .active_debug_launch()
                .is_some_and(|l| l.status.is_live());
            return div()
                .p(px(10.))
                .text_color(theme.text_faint)
                .child(if running {
                    "The program is running. Frames show when it stops."
                } else {
                    "Not stopped"
                })
                .into_any_element();
        };
        let thread_name = session
            .threads
            .iter()
            .find(|t| Some(t.id) == session.thread)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        let several_threads = session.threads.len() > 1;
        let selected = session.frame;
        let frames: Vec<_> = session
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                let location = frame
                    .source
                    .as_ref()
                    .and_then(|s| s.path.as_deref().map(|p| (p, s)))
                    .map(|(path, _)| {
                        let shown = root
                            .as_deref()
                            .and_then(|r| path.strip_prefix(r).ok())
                            .unwrap_or(path);
                        format!("{}:{}", shown.display(), frame.line)
                    })
                    .or_else(|| frame.source.as_ref().and_then(|s| s.name.clone()))
                    .unwrap_or_default();
                // Library code, outside the project, is dimmed as IntelliJ does.
                let outside = frame
                    .source
                    .as_ref()
                    .and_then(|s| s.path.as_deref())
                    .is_none_or(|p| root.as_deref().is_some_and(|r| !p.starts_with(r)));
                let subtle = frame.presentation_hint.as_deref() == Some("subtle") || outside;
                (index, frame.name.clone(), location, subtle)
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("debug-thread")
                    .debug_selector(|| "debug-thread".into())
                    .flex_none()
                    .px(px(10.))
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_muted)
                    .child(if several_threads {
                        format!("Thread: {thread_name} ▾")
                    } else {
                        format!("Thread: {thread_name}")
                    })
                    .when(several_threads, |d| {
                        d.cursor_pointer().hover(|s| s.bg(theme.hover)).on_click(
                            cx.listener(|this, _, window, cx| this.choose_thread(window, cx)),
                        )
                    }),
            )
            .child(
                div()
                    .id("debug-frames")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(frames.into_iter().map(|(index, name, location, subtle)| {
                        div()
                            .id(("frame", index))
                            .h(px(ROW_HEIGHT))
                            .w_full()
                            .px(px(10.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .when(index == selected, |d| d.bg(theme.selection))
                            .hover(|s| s.bg(theme.hover))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(if subtle { theme.text_muted } else { theme.text })
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_faint)
                                    .child(location),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.debugger.update(cx, |d, cx| d.select_frame(index, cx))
                            }))
                    })),
            )
            .into_any_element()
    }

    fn render_variables(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (rows, missing) = self.rows(cx);
        if !missing.is_empty() {
            // Open rows from the last stop need their children for this one.
            let debugger = self.debugger.clone();
            cx.defer(move |cx| {
                debugger.update(cx, |d, cx| {
                    for reference in missing {
                        d.load_variables(reference, cx);
                    }
                })
            });
        }
        let _ = window;
        let adding = self.watch_input.clone();
        div()
            .id("debug-variables")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .id("add-watch")
                    .debug_selector(|| "add-watch".into())
                    .h(px(ROW_HEIGHT))
                    .px(px(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .text_color(theme.text_faint)
                    .child(
                        svg()
                            .path(Icon::Plus.path())
                            .size(px(12.))
                            .text_color(theme.text_faint),
                    )
                    .child(match adding {
                        Some(input) => div()
                            .key_context("WatchInput")
                            .flex_1()
                            .on_action(cx.listener(|this, _: &AddWatch, window, cx| {
                                this.add_watch(window, cx)
                            }))
                            .on_action(cx.listener(|this, _: &CancelWatch, window, cx| {
                                this.watch_input = None;
                                this.focus_handle.focus(window, cx);
                                cx.notify();
                            }))
                            .child(input)
                            .into_any_element(),
                        None => div()
                            .cursor_pointer()
                            .child("Add a watch")
                            .into_any_element(),
                    })
                    .when(self.watch_input.is_none(), |d| {
                        d.cursor_pointer().on_click(
                            cx.listener(|this, _, window, cx| this.start_adding_watch(window, cx)),
                        )
                    }),
            )
            .children(
                rows.into_iter()
                    .enumerate()
                    .map(|(index, row)| self.render_row(index, row, theme, cx)),
            )
            .into_any_element()
    }

    fn render_row(
        &self,
        index: usize,
        row: Row,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (depth, expandable, expanded) = match &row {
            Row::Watch { expanded, .. } => (0, row.reference().is_some(), *expanded),
            Row::Scope { expanded, .. } => (0, true, *expanded),
            Row::Variable {
                depth, expanded, ..
            } => (*depth, row.reference().is_some(), *expanded),
            Row::Loading { depth } => (*depth, false, false),
        };
        let chevron = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let content: AnyElement = match &row {
            Row::Watch {
                expression, value, ..
            } => {
                let (text, color) = match value {
                    WatchShown::Pending => ("…".to_string(), theme.text_faint),
                    WatchShown::Value { text, .. } => (text.clone(), theme.text),
                    WatchShown::Error(error) => (error.clone(), theme.error),
                };
                div()
                    .flex()
                    .flex_row()
                    .gap(px(4.))
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.syntax.property)
                            .child(expression.clone()),
                    )
                    .child(div().flex_none().text_color(theme.text_faint).child("="))
                    .child(div().min_w_0().truncate().text_color(color).child(text))
                    .into_any_element()
            }
            Row::Scope { name, .. } => div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(name.clone())
                .into_any_element(),
            Row::Variable {
                name,
                value,
                type_name,
                ..
            } => div()
                .flex()
                .flex_row()
                .gap(px(4.))
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.syntax.property)
                        .child(name.clone()),
                )
                .child(div().flex_none().text_color(theme.text_faint).child("="))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text)
                        .child(value.clone()),
                )
                .when_some(type_name.clone().filter(|t| !t.is_empty()), |d, t| {
                    d.child(div().flex_none().text_color(theme.text_faint).child(t))
                })
                .into_any_element(),
            Row::Loading { .. } => div()
                .text_color(theme.text_faint)
                .child("Loading…")
                .into_any_element(),
        };
        let key = row.key().map(String::from);
        let reference = row.reference();
        let settable = match &row {
            Row::Variable {
                name,
                value,
                container,
                ..
            } => Some((*container, name.clone(), value.clone())),
            _ => None,
        };
        let watch_index = match &row {
            Row::Watch { index, .. } => Some(*index),
            _ => None,
        };
        div()
            .id(("variable", index))
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(10. + 14. * depth as f32))
            .pr(px(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .hover(|s| s.bg(theme.hover))
            .child(div().size(px(12.)).flex_none().when(expandable, |d| {
                d.child(
                    svg()
                        .path(chevron.path())
                        .size(px(12.))
                        .text_color(theme.text_faint),
                )
            }))
            .child(div().flex_1().min_w_0().child(content))
            .when_some(watch_index, |d, index| {
                d.child(
                    div()
                        .id(("remove-watch", index))
                        .flex_none()
                        .cursor_pointer()
                        .child(
                            svg()
                                .path(Icon::Close.path())
                                .size(px(11.))
                                .text_color(theme.text_faint),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.debugger.update(cx, |d, cx| d.remove_watch(index, cx))
                        })),
                )
            })
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    // A double-click changes a value, as F2 does in IntelliJ.
                    if event.click_count() > 1 {
                        if let Some((container, name, value)) = settable.clone() {
                            this.set_value(container, name, &value, window, cx);
                        }
                        return;
                    }
                    if expandable && let Some(key) = &key {
                        this.toggle(key, reference, cx);
                    }
                }),
            )
            .when(expandable, |d| d.cursor_pointer())
            .into_any_element()
    }
}

impl Focusable for DebugPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for DebugPanel {
    fn id(&self) -> &'static str {
        DEBUG_PANEL
    }
    fn title(&self) -> &'static str {
        "Debug"
    }
    fn icon(&self) -> Icon {
        Icon::Debug
    }
    fn position(&self) -> DockPosition {
        DockPosition::Bottom
    }

    fn save_state(&self, cx: &App) -> serde_json::Value {
        let debugger = self.debugger.read(cx);
        let (exception_filters, exception_choices) = debugger.exception_settings();
        serde_json::to_value(Saved {
            breakpoints: debugger.breakpoints().clone(),
            watches: debugger.watches().to_vec(),
            configuration: debugger.selected_target_name().map(String::from),
            exception_filters,
            exception_choices,
            run_settings: debugger.all_run_settings().clone(),
        })
        .unwrap_or_default()
    }

    fn restore_state(&mut self, state: serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        let Ok(saved) = serde_json::from_value::<Saved>(state) else {
            return;
        };
        self.debugger.update(cx, |d, cx| {
            d.set_all_breakpoints(saved.breakpoints, cx);
            d.set_watches(saved.watches, cx);
            d.set_selected_target(saved.configuration, cx);
            d.set_exception_settings(saved.exception_filters, saved.exception_choices);
            d.restore_run_settings(saved.run_settings);
        });
    }

    fn debug_state(&self, cx: &App) -> serde_json::Value {
        let mut state = self.debugger.read(cx).debug_state(cx);
        state["tab"] = serde_json::json!(format!("{:?}", self.tab));
        state["variable_rows"] = serde_json::json!(self.variable_lines(cx));
        state
    }
}

impl Render for DebugPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let has_launch = self.debugger.read(cx).active_debug_launch().is_some();
        let tab = self.tab;
        let tab_button = |id: &'static str, label: &'static str, this_tab: Tab| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(10.))
                .h_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .text_color(if tab == this_tab {
                    theme.text
                } else {
                    theme.text_muted
                })
                .when(tab == this_tab, |d| {
                    d.border_b_2().border_color(theme.accent)
                })
                .child(label)
        };
        let body: AnyElement = if !has_launch {
            div()
                .p(px(12.))
                .text_color(theme.text_faint)
                .child(empty_hint("Nothing is being debugged.", true))
                .into_any_element()
        } else if tab == Tab::Console {
            div()
                .size_full()
                .child(self.console.clone())
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .flex_row()
                .child(
                    div()
                        .w(relative_width(0.4))
                        .h_full()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(self.render_frames(&theme, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .child(self.render_variables(&theme, window, cx)),
                )
                .into_any_element()
        };
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_size(px(12.5))
            .child(self.render_toolbar(&theme, cx))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .h(px(26.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        tab_button("debug-tab-debugger", "Debugger", Tab::Debugger).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.tab = Tab::Debugger;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        tab_button("debug-tab-console", "Console", Tab::Console).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.tab = Tab::Console;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn relative_width(fraction: f32) -> gpui::Length {
    gpui::relative(fraction).into()
}
