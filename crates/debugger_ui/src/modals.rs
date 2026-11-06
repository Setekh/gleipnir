//! Popups: editing one breakpoint (right-click in the gutter), every breakpoint in the project
//! (View Breakpoints), and Evaluate Expression.

use crate::debugger;
use gleipnir_core::{Breakpoint, Debugger};
use gleipnir_ui::list::ListNav;
use gleipnir_ui::text_input::TextInput;
use gleipnir_ui::{Icon, Theme};
use gleipnir_workspace::{ModalView, Workspace};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, Subscription, WeakEntity, Window, actions, div, prelude::*, px, svg,
};
use std::path::{Path, PathBuf};

actions!(
    debugger_modals,
    [
        Confirm,
        Cancel,
        NextField,
        Remove,
        ToggleEnabled,
        SelectNext,
        SelectPrevious
    ]
);

pub fn bind_keys(cx: &mut App) {
    let editor = Some("BreakpointEditor");
    let list = Some("BreakpointList");
    let evaluate = Some("EvaluateModal");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, editor),
        KeyBinding::new("escape", Cancel, editor),
        KeyBinding::new("tab", NextField, editor),
        KeyBinding::new("enter", Confirm, list),
        KeyBinding::new("escape", Cancel, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("space", ToggleEnabled, list),
        KeyBinding::new("backspace", Remove, list),
        KeyBinding::new("delete", Remove, list),
        KeyBinding::new("enter", Confirm, evaluate),
        KeyBinding::new("escape", Cancel, evaluate),
    ]);
}

fn frame(theme: &Theme, context: &'static str, width: f32) -> gpui::Div {
    div()
        .key_context(context)
        .occlude()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(6.))
        .p(px(10.))
        .bg(theme.surface_raised)
        .border_1()
        .border_color(theme.border)
        .rounded(px(8.))
        .shadow_lg()
        .text_size(px(theme.ui_font_size))
}

fn title(theme: &Theme, text: String) -> impl IntoElement {
    div()
        .text_color(theme.text_muted)
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

fn field(theme: &Theme, label: &'static str, input: Entity<TextInput>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_color(theme.text_faint)
                .text_size(px(11.5))
                .child(label),
        )
        .child(
            div()
                .px(px(6.))
                .py(px(3.))
                .rounded(px(4.))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .child(input),
        )
}

fn display_path(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|r| path.strip_prefix(r).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Right-click on a line in the gutter: edit its breakpoint, making one if there's none.
pub fn edit_breakpoint(
    workspace: &mut Workspace,
    path: PathBuf,
    line: u32,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    let root = workspace.root().map(Path::to_path_buf);
    workspace.toggle_modal(window, cx, move |window, cx| {
        BreakpointEditor::new(debugger, path, line, root, window, cx)
    });
}

pub struct BreakpointEditor {
    debugger: Entity<Debugger>,
    path: PathBuf,
    line: u32,
    title: String,
    enabled: bool,
    condition: Entity<TextInput>,
    hits: Entity<TextInput>,
    log: Entity<TextInput>,
}

impl EventEmitter<DismissEvent> for BreakpointEditor {}
impl ModalView for BreakpointEditor {}

impl Focusable for BreakpointEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.condition.read(cx).focus_handle(cx)
    }
}

impl BreakpointEditor {
    pub fn new(
        debugger: Entity<Debugger>,
        path: PathBuf,
        line: u32,
        root: Option<PathBuf>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let existing = debugger
            .read(cx)
            .breakpoints()
            .get(&path, line)
            .cloned()
            .unwrap_or_else(|| Breakpoint::at(line));
        let input = |placeholder: &'static str, text: Option<&String>, cx: &mut Context<Self>| {
            let text = text.cloned().unwrap_or_default();
            cx.new(|cx| {
                let mut input = TextInput::new(placeholder, cx);
                input.set_text(&text, cx);
                input
            })
        };
        let condition = input(
            "Stop only when this is true",
            existing.condition.as_ref(),
            cx,
        );
        let hits = input(
            "Stop only on these hits, like >= 3",
            existing.hit_condition.as_ref(),
            cx,
        );
        let log = input(
            "Log this instead of stopping; {expression} shows a value",
            existing.log_message.as_ref(),
            cx,
        );
        Self {
            debugger,
            title: format!("{}:{}", display_path(root.as_deref(), &path), line + 1),
            path,
            line,
            enabled: existing.enabled,
            condition,
            hits,
            log,
        }
    }

    /// Fills the fields, for tests and the driver.
    pub fn set_fields(&mut self, condition: &str, hits: &str, log: &str, cx: &mut Context<Self>) {
        self.condition.update(cx, |i, cx| i.set_text(condition, cx));
        self.hits.update(cx, |i, cx| i.set_text(hits, cx));
        self.log.update(cx, |i, cx| i.set_text(log, cx));
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let read = |input: &Entity<TextInput>, cx: &App| {
            Some(input.read(cx).text().trim().to_string()).filter(|t| !t.is_empty())
        };
        let breakpoint = Breakpoint {
            line: self.line,
            enabled: self.enabled,
            condition: read(&self.condition, cx),
            hit_condition: read(&self.hits, cx),
            log_message: read(&self.log, cx),
        };
        let path = self.path.clone();
        self.debugger
            .update(cx, |d, cx| d.set_breakpoint(&path, breakpoint, cx));
        cx.emit(DismissEvent);
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let (path, line) = (self.path.clone(), self.line);
        self.debugger
            .update(cx, |d, cx| d.remove_breakpoint(&path, line, cx));
        cx.emit(DismissEvent);
    }

    fn next_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fields = [&self.condition, &self.hits, &self.log];
        let focused = fields
            .iter()
            .position(|f| f.read(cx).focus_handle(cx).is_focused(window))
            .unwrap_or(0);
        let next = fields[(focused + 1) % fields.len()]
            .read(cx)
            .focus_handle(cx);
        next.focus(window, cx);
    }
}

impl Render for BreakpointEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let enabled = self.enabled;
        frame(&theme, "BreakpointEditor", 460.)
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &NextField, window, cx| this.next_field(window, cx)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(title(&theme, format!("Breakpoint at {}", self.title)))
            .child(
                div()
                    .id("breakpoint-enabled")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(12.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(theme.border)
                            .when(enabled, |d| d.bg(theme.accent)),
                    )
                    .child(div().text_color(theme.text).child("Enabled"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.enabled = !this.enabled;
                        cx.notify();
                    })),
            )
            .child(field(&theme, "Condition", self.condition.clone()))
            .child(field(&theme, "Hit count", self.hits.clone()))
            .child(field(&theme, "Log message", self.log.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .pt(px(4.))
                    .child(
                        div()
                            .id("breakpoint-remove")
                            .cursor_pointer()
                            .text_color(theme.error)
                            .child("Remove")
                            .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                    )
                    .child(
                        div()
                            .text_color(theme.text_faint)
                            .child("Enter to apply, Tab for the next field"),
                    ),
            )
    }
}

/// Cmd+Shift+F8: on a line with a breakpoint, its settings, as in IntelliJ; anywhere else, every
/// breakpoint in the project.
pub fn view_breakpoints(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    if let Some((path, line)) = crate::cursor_line(workspace, cx)
        && debugger.read(cx).breakpoints().get(&path, line).is_some()
        && workspace.active_modal::<BreakpointEditor>().is_none()
    {
        edit_breakpoint(workspace, path, line, window, cx);
        return;
    }
    let root = workspace.root().map(Path::to_path_buf);
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |_, cx| {
        BreakpointList::new(debugger, weak, root, cx)
    });
}

pub struct BreakpointList {
    debugger: Entity<Debugger>,
    workspace: WeakEntity<Workspace>,
    root: Option<PathBuf>,
    focus_handle: FocusHandle,
    nav: ListNav,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for BreakpointList {}
impl ModalView for BreakpointList {}

impl Focusable for BreakpointList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BreakpointList {
    pub fn new(
        debugger: Entity<Debugger>,
        workspace: WeakEntity<Workspace>,
        root: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&debugger, |this, _, cx| {
            let count = this.entries(cx).len();
            this.nav.set_count(count);
            cx.notify();
        });
        let mut nav = ListNav::new();
        nav.set_count(debugger.read(cx).breakpoints().len());
        Self {
            debugger,
            workspace,
            root,
            focus_handle: cx.focus_handle(),
            nav,
            _subscription: subscription,
        }
    }

    fn entries(&self, cx: &App) -> Vec<(PathBuf, Breakpoint)> {
        self.debugger
            .read(cx)
            .breakpoints()
            .files()
            .flat_map(|(path, list)| list.iter().map(move |b| (path.to_path_buf(), b.clone())))
            .collect()
    }

    /// Each as `file:line`, with what's special about it: for tests and the driver.
    pub fn labels(&self, cx: &App) -> Vec<String> {
        self.entries(cx)
            .iter()
            .map(|(path, b)| {
                let mut label = format!(
                    "{}:{}",
                    display_path(self.root.as_deref(), path),
                    b.line + 1
                );
                if !b.enabled {
                    label.push_str(" (disabled)");
                }
                if let Some(condition) = &b.condition {
                    label.push_str(&format!(" if {condition}"));
                }
                if let Some(log) = &b.log_message {
                    label.push_str(&format!(" logs {log}"));
                }
                label
            })
            .collect()
    }

    fn selected(&self, cx: &App) -> Option<(PathBuf, Breakpoint)> {
        self.entries(cx).get(self.nav.selected()?).cloned()
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, breakpoint)) = self.selected(cx) else {
            return;
        };
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |w, cx| {
                w.open_path_at(path, Some((breakpoint.line as usize, 0)), window, cx)
                    .detach()
            })
            .ok();
    }

    fn toggle_enabled(&mut self, cx: &mut Context<Self>) {
        let Some((path, breakpoint)) = self.selected(cx) else {
            return;
        };
        self.debugger.update(cx, |d, cx| {
            d.set_breakpoint(
                &path,
                Breakpoint {
                    enabled: !breakpoint.enabled,
                    ..breakpoint
                },
                cx,
            )
        });
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some((path, breakpoint)) = self.selected(cx) else {
            return;
        };
        self.debugger
            .update(cx, |d, cx| d.remove_breakpoint(&path, breakpoint.line, cx));
    }
}

impl Render for BreakpointList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let entries = self.entries(cx);
        let labels = self.labels(cx);
        let selected = self.nav.selected();
        frame(&theme, "BreakpointList", 520.)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.open(window, cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.nav.select_next();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.nav.select_previous();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleEnabled, _, cx| this.toggle_enabled(cx)))
            .on_action(cx.listener(|this, _: &Remove, _, cx| this.remove(cx)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(title(&theme, "Breakpoints".into()))
            .when(entries.is_empty(), |d| {
                d.child(
                    div()
                        .text_color(theme.text_faint)
                        .child("No breakpoints. Click beside a line number to add one."),
                )
            })
            .child(
                div()
                    .id("breakpoint-list")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .children(entries.iter().zip(labels).enumerate().map(
                        |(index, ((_, breakpoint), label))| {
                            let color = if breakpoint.enabled {
                                theme.breakpoint
                            } else {
                                theme.text_faint
                            };
                            div()
                                .id(("breakpoint", index))
                                .h(px(24.))
                                .px(px(6.))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .when(selected == Some(index), |d| d.bg(theme.selection))
                                .child(div().size(px(9.)).flex_none().rounded_full().bg(color))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme.text)
                                        .child(label),
                                )
                                .on_click(cx.listener(
                                    move |this, event: &gpui::ClickEvent, window, cx| {
                                        this.nav.select(index);
                                        if event.click_count() > 1 {
                                            this.open(window, cx);
                                        }
                                        cx.notify();
                                    },
                                ))
                        },
                    )),
            )
            .child(
                div()
                    .text_color(theme.text_faint)
                    .child("Enter opens · Space enables or disables · Delete removes"),
            )
    }
}

/// Alt+F8: evaluates an expression in the stopped frame, starting from the selection or the
/// name under the cursor.
pub fn evaluate(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    if !debugger.read(cx).is_paused() {
        workspace.show_message("Evaluating needs a program stopped in the debugger", cx);
        return;
    }
    let initial = workspace
        .active_editor(cx)
        .map(|editor| {
            let view = editor.read(cx);
            let text = view.buffer().read(cx).buffer();
            let selection = view.selections().primary().range();
            if selection.is_empty() {
                text.word_at(selection.start)
                    .map(|r| text.slice(r))
                    .unwrap_or_default()
            } else {
                text.slice(selection)
            }
        })
        .unwrap_or_default();
    workspace.toggle_modal(window, cx, move |_, cx| {
        EvaluateModal::new(debugger, &initial, cx)
    });
}

pub struct EvaluateModal {
    debugger: Entity<Debugger>,
    input: Entity<TextInput>,
    result: Option<Result<(String, Option<String>), String>>,
}

impl EventEmitter<DismissEvent> for EvaluateModal {}
impl ModalView for EvaluateModal {}

impl Focusable for EvaluateModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl EvaluateModal {
    pub fn new(debugger: Entity<Debugger>, initial: &str, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Expression", cx);
            input.set_text(initial, cx);
            input
        });
        Self {
            debugger,
            input,
            result: None,
        }
    }

    pub fn set_expression(&mut self, expression: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_text(expression, cx));
    }

    /// The last result as shown, for tests and the driver.
    pub fn result_text(&self) -> Option<String> {
        self.result.as_ref().map(|r| match r {
            Ok((value, Some(type_name))) => format!("{value} ({type_name})"),
            Ok((value, None)) => value.clone(),
            Err(error) => format!("error: {error}"),
        })
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let expression = self.input.read(cx).text();
        if expression.trim().is_empty() {
            return;
        }
        let task = self
            .debugger
            .update(cx, |d, cx| d.evaluate(&expression, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.result = Some(result.map(|r| (r.result, r.type_name)));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for EvaluateModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let result = self.result_text();
        let failed = matches!(self.result, Some(Err(_)));
        frame(&theme, "EvaluateModal", 520.)
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(title(&theme, "Evaluate".into()))
            .child(field(
                &theme,
                "Expression, in the selected frame",
                self.input.clone(),
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .min_h(px(22.))
                    .child(
                        svg()
                            .path(Icon::Console.path())
                            .size(px(12.))
                            .text_color(theme.text_faint),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(if failed { theme.error } else { theme.text })
                            .child(result.unwrap_or_else(|| "Enter evaluates".into())),
                    ),
            )
    }
}
