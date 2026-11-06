//! A launch's console: the build's and the program's output, the debugger's messages, and for a
//! debug session, a line to evaluate expressions in.

use gleipnir_core::{ConsoleKind, Debugger, LaunchId};
use gleipnir_ui::Theme;
use gleipnir_ui::text_input::TextInput;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, KeyBinding, ScrollStrategy,
    Subscription, UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};
use std::ops::Range;

actions!(debug_console, [Submit]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", Submit, Some("DebugConsoleInput"))]);
}

const LINE_HEIGHT: f32 = 18.;

pub struct ConsoleView {
    debugger: Entity<Debugger>,
    launch: Option<LaunchId>,
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
            self.shown = 0;
            cx.notify();
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

    fn line_count(&self, cx: &App) -> usize {
        self.launch
            .and_then(|id| self.debugger.read(cx).launch(id))
            .map_or(0, |l| l.console.len())
    }
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let count = self.line_count(cx);
        if count > self.shown {
            self.scroll
                .scroll_to_item(count.saturating_sub(1), ScrollStrategy::Bottom);
        }
        self.shown = count;
        let lines = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            let debugger = this.debugger.read(cx);
            let Some(launch) = this.launch.and_then(|id| debugger.launch(id)) else {
                return Vec::new();
            };
            range
                .filter_map(|i| launch.console.get(i))
                .map(|line| {
                    let color = match line.kind {
                        ConsoleKind::Stdout => theme.text,
                        ConsoleKind::Stderr | ConsoleKind::Error => theme.error,
                        ConsoleKind::Info => theme.text_muted,
                        ConsoleKind::Input => theme.accent,
                        ConsoleKind::Result => theme.syntax.string,
                    };
                    div()
                        .h(px(LINE_HEIGHT))
                        .px(px(10.))
                        .whitespace_nowrap()
                        .text_color(color)
                        .child(if line.text.is_empty() {
                            " ".to_string()
                        } else {
                            line.text.clone()
                        })
                })
                .collect::<Vec<_>>()
        });
        let font = gleipnir_editor_font();
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

/// The monospace family the console uses, matching the editor where it can.
fn gleipnir_editor_font() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}
