//! Running and debugging in the window, over core's `Debugger`: the Run and Debug panels, the run
//! widget in the status bar, breakpoints and Run icons in the gutter, the breakpoint, Evaluate and
//! View Breakpoints popups, and IntelliJ's shortcuts for all of it.

pub mod console;
mod decorations;
mod links;
mod modals;
mod panels;
pub mod test_view;
mod variables;

pub use decorations::DebugDecorations;
pub use modals::{
    BreakpointEditor, BreakpointList, EvaluateModal, RunSettingsModal, SetValueModal,
};
pub use panels::{DEBUG_PANEL, DebugPanel, RUN_PANEL, RunPanel};

use gleipnir_code_nav::{Picker, PickerItem};
use gleipnir_core::{Debugger, DebuggerEvent, LaunchStatus};
use gleipnir_dap::Step;
use gleipnir_editor::{EditorEvent, EditorView};
use gleipnir_packages::RunTarget;
use gleipnir_ui::{Icon, Theme};
use gleipnir_workspace::Workspace;
use gpui::{
    App, AppContext as _, Context, Entity, KeyBinding, Subscription, Window, actions, div,
    prelude::*, px, svg,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;

actions!(
    debugger,
    [
        Run,
        Debug,
        RunConfiguration,
        DebugConfiguration,
        RunFile,
        DebugFile,
        Stop,
        Rerun,
        Resume,
        Pause,
        StepOver,
        StepInto,
        StepOut,
        RunToCursor,
        EvaluateExpression,
        ToggleBreakpoint,
        ViewBreakpoints,
        RemoveAllBreakpoints,
        ShowRunPanel,
        ShowDebugPanel,
        EditConfiguration,
        AttachToProcess,
        RerunFailedTests,
    ]
);

pub fn bind_keys(cx: &mut App) {
    modals::bind_keys(cx);
    panels::bind_keys(cx);
    let editor = Some("Editor");
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("ctrl-r", Run, None),
        KeyBinding::new("ctrl-d", Debug, None),
        KeyBinding::new("ctrl-alt-r", RunConfiguration, None),
        KeyBinding::new("ctrl-alt-d", DebugConfiguration, None),
        KeyBinding::new("ctrl-shift-r", RunFile, None),
        KeyBinding::new("ctrl-shift-d", DebugFile, None),
        KeyBinding::new("cmd-f2", Stop, None),
        KeyBinding::new("cmd-alt-r", Resume, None),
        KeyBinding::new("f8", StepOver, None),
        KeyBinding::new("f7", StepInto, None),
        KeyBinding::new("shift-f8", StepOut, None),
        KeyBinding::new("alt-f9", RunToCursor, editor),
        KeyBinding::new("alt-f8", EvaluateExpression, None),
        KeyBinding::new("cmd-f8", ToggleBreakpoint, editor),
        KeyBinding::new("cmd-shift-f8", ViewBreakpoints, None),
        KeyBinding::new("cmd-4", ShowRunPanel, None),
        KeyBinding::new("cmd-5", ShowDebugPanel, None),
        KeyBinding::new("alt-shift-f5", AttachToProcess, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("shift-f10", Run, None),
        KeyBinding::new("shift-f9", Debug, None),
        KeyBinding::new("alt-shift-f10", RunConfiguration, None),
        KeyBinding::new("alt-shift-f9", DebugConfiguration, None),
        KeyBinding::new("ctrl-shift-f10", RunFile, None),
        KeyBinding::new("ctrl-shift-f9", DebugFile, None),
        KeyBinding::new("ctrl-f2", Stop, None),
        KeyBinding::new("f9", Resume, None),
        KeyBinding::new("f8", StepOver, None),
        KeyBinding::new("f7", StepInto, None),
        KeyBinding::new("shift-f8", StepOut, None),
        KeyBinding::new("alt-f9", RunToCursor, editor),
        KeyBinding::new("alt-f8", EvaluateExpression, None),
        KeyBinding::new("ctrl-f8", ToggleBreakpoint, editor),
        KeyBinding::new("ctrl-shift-f8", ViewBreakpoints, None),
        KeyBinding::new("alt-4", ShowRunPanel, None),
        KeyBinding::new("alt-5", ShowDebugPanel, None),
        KeyBinding::new("ctrl-alt-f5", AttachToProcess, None),
    ]);
}

/// The debugger every part of the window shares. It lives in the Debug panel.
pub fn debugger(workspace: &Workspace, cx: &App) -> Option<Entity<Debugger>> {
    workspace
        .panel::<DebugPanel>(cx)
        .map(|panel| panel.read(cx).debugger().clone())
}

pub fn add_to_workspace(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let root = workspace.root().map(PathBuf::from);
    let toolchains = workspace.toolchains().clone();
    let debugger = cx.new(|cx| Debugger::new(root, toolchains, cx));
    add_with_debugger(workspace, debugger, window, cx);
}

/// Adds everything around a given debugger, which tests make with a fake adapter.
pub fn add_with_debugger(
    workspace: &mut Workspace,
    debugger: Entity<Debugger>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let weak = cx.weak_entity();
    let run_panel = cx.new(|cx| RunPanel::new(debugger.clone(), weak.clone(), window, cx));
    let debug_panel = cx.new(|cx| DebugPanel::new(debugger.clone(), weak.clone(), window, cx));
    workspace.add_panel(run_panel, cx);
    workspace.add_panel(debug_panel, cx);

    // Breakpoints in files changed outside the editor, like on a branch switch, follow them.
    if let Some(project) = workspace.project().cloned() {
        let debugger = debugger.clone();
        cx.subscribe(&project, move |_, _, event, cx| {
            if let gleipnir_core::ProjectEvent::FilesChanged(paths) = event {
                debugger.update(cx, |d, cx| d.files_changed(paths, cx));
            }
        })
        .detach();
    }

    let hooked = debugger.clone();
    workspace.add_editor_hook(move |_, editor, window, cx| attach(&hooked, editor, window, cx));
    workspace.add_title_item(gleipnir_workspace::TitleSide::Right, 10, run_widget);

    workspace.register_action(|w, _: &Run, _, cx| start_selected(w, false, cx));
    workspace.register_action(|w, _: &Debug, _, cx| start_selected(w, true, cx));
    workspace.register_action(|w, _: &RunConfiguration, window, cx| {
        choose_and_start(w, Some(false), window, cx)
    });
    workspace.register_action(|w, _: &DebugConfiguration, window, cx| {
        choose_and_start(w, Some(true), window, cx)
    });
    workspace.register_action(|w, _: &RunFile, _, cx| start_active_file(w, false, cx));
    workspace.register_action(|w, _: &RerunFailedTests, _, cx| {
        with_debugger(w, cx, |d, cx| {
            // The launch shown, or else the latest that ran tests.
            let id = d
                .active_launch()
                .filter(|l| l.tests.is_some())
                .or_else(|| d.launches().iter().rev().find(|l| l.tests.is_some()))
                .map(|l| l.id);
            if let Some(id) = id {
                d.rerun_failed(id, cx);
            }
        })
    });
    workspace.register_action(|w, _: &DebugFile, _, cx| start_active_file(w, true, cx));
    workspace.register_action(|w, _: &Stop, _, cx| {
        with_debugger(w, cx, |d, cx| {
            let live = d
                .active_launch()
                .filter(|l| l.status.is_live())
                .or_else(|| d.launches().iter().rev().find(|l| l.status.is_live()))
                .map(|l| l.id);
            if let Some(id) = live {
                d.stop(id, cx);
            }
        })
    });
    workspace.register_action(|w, _: &Rerun, _, cx| {
        with_debugger(w, cx, |d, cx| {
            if let Some(id) = d.active_launch().map(|l| l.id) {
                d.rerun(id, cx);
            }
        })
    });
    workspace.register_action(|w, _: &Resume, _, cx| step(w, Step::Continue, cx));
    workspace.register_action(|w, _: &StepOver, _, cx| step(w, Step::Over, cx));
    workspace.register_action(|w, _: &StepInto, _, cx| step(w, Step::Into, cx));
    workspace.register_action(|w, _: &StepOut, _, cx| step(w, Step::Out, cx));
    workspace.register_action(|w, _: &Pause, _, cx| with_debugger(w, cx, |d, cx| d.pause(cx)));
    workspace.register_action(|w, _: &RunToCursor, _, cx| {
        let Some((path, line)) = cursor_line(w, cx) else {
            return;
        };
        with_debugger(w, cx, |d, cx| d.run_to_line(&path, line, cx));
    });
    workspace.register_action(|w, _: &ToggleBreakpoint, _, cx| {
        let Some((path, line)) = cursor_line(w, cx) else {
            return;
        };
        with_debugger(w, cx, |d, cx| d.toggle_breakpoint(&path, line, cx));
    });
    workspace.register_action(|w, _: &RemoveAllBreakpoints, _, cx| {
        with_debugger(w, cx, |d, cx| d.remove_all_breakpoints(cx))
    });
    workspace.register_action(|w, _: &ViewBreakpoints, window, cx| {
        modals::view_breakpoints(w, window, cx)
    });
    workspace
        .register_action(|w, _: &EvaluateExpression, window, cx| modals::evaluate(w, window, cx));
    workspace
        .register_action(|w, _: &AttachToProcess, window, cx| attach_to_process(w, window, cx));
    workspace.register_action(|w, _: &EditConfiguration, window, cx| {
        modals::edit_configuration(w, window, cx)
    });
    workspace
        .register_action(|w, _: &ShowRunPanel, window, cx| w.toggle_panel(RUN_PANEL, window, cx));
    workspace.register_action(|w, _: &ShowDebugPanel, window, cx| {
        w.toggle_panel(DEBUG_PANEL, window, cx)
    });

    cx.subscribe_in(
        &debugger,
        window,
        |workspace, _, event, window, cx| match event {
            DebuggerEvent::ShowLocation { path, line } => {
                workspace
                    .open_path_at(path.clone(), Some((*line as usize, 0)), window, cx)
                    .detach();
            }
            DebuggerEvent::Started { debug, .. } => {
                workspace.show_panel(if *debug { DEBUG_PANEL } else { RUN_PANEL }, cx);
            }
            DebuggerEvent::Stopped(_) => workspace.show_panel(DEBUG_PANEL, cx),
            DebuggerEvent::BreakpointsChanged | DebuggerEvent::SettingsChanged => {
                workspace.schedule_save(cx)
            }
            DebuggerEvent::LaunchesChanged | DebuggerEvent::MarksChanged => cx.notify(),
        },
    )
    .detach();
}

/// Breakpoints, Run icons and values in an editor, and its gutter clicks.
fn attach(
    debugger: &Entity<Debugger>,
    editor: &Entity<EditorView>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Vec<Subscription> {
    let buffer = editor.read(cx).buffer().clone();
    debugger.update(cx, |d, cx| d.track_buffer(&buffer, cx));
    let provider = Rc::new(DebugDecorations::new(debugger.clone(), buffer.downgrade()));
    editor.update(cx, |editor, cx| editor.set_decorations(provider, cx));

    let buffer_text = buffer.clone();
    let path_of = move |cx: &App| buffer.read(cx).path().map(Path::to_path_buf);
    let path_of = Rc::new(path_of);
    let debugger_for_events = debugger.clone();
    let events = cx.subscribe_in(editor, window, move |workspace, _, event, window, cx| {
        let Some(path) = path_of(cx) else { return };
        match event {
            EditorEvent::GutterClicked { line } => {
                debugger_for_events.update(cx, |d, cx| d.toggle_breakpoint(&path, *line as u32, cx))
            }
            EditorEvent::RunClicked { line } => {
                // A test on that line, or else what the file runs.
                let text = buffer_text.read(cx).buffer().text();
                let d = debugger_for_events.read(cx);
                let target = d
                    .test_at(&path, &text, *line)
                    .or_else(|| d.target_for_file(&path));
                if let Some(target) = target {
                    run_or_debug_menu(workspace, target, window, cx);
                }
            }
            EditorEvent::GutterMenu { line, .. } => {
                modals::edit_breakpoint(workspace, path, *line as u32, window, cx)
            }
            _ => {}
        }
    });
    // The gutter shows the debugger's state, so it redraws when that changes.
    let weak_editor = editor.downgrade();
    let repaint = cx.subscribe(debugger, move |_, _, event, cx| {
        if matches!(
            event,
            DebuggerEvent::MarksChanged | DebuggerEvent::BreakpointsChanged
        ) && let Some(editor) = weak_editor.upgrade()
        {
            editor.update(cx, |_, cx| cx.notify());
        }
    });
    vec![events, repaint]
}

fn with_debugger(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
    f: impl FnOnce(&mut Debugger, &mut Context<Debugger>),
) {
    if let Some(debugger) = debugger(workspace, cx) {
        debugger.update(cx, f);
    }
}

fn step(workspace: &mut Workspace, step: Step, cx: &mut Context<Workspace>) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    if !debugger.read(cx).is_paused() {
        return;
    }
    debugger.update(cx, |d, cx| d.resume(step, cx));
}

/// The active editor's file and the cursor's line.
pub(crate) fn cursor_line(workspace: &Workspace, cx: &App) -> Option<(PathBuf, u32)> {
    let editor = workspace.active_editor(cx)?;
    let view = editor.read(cx);
    let model = view.buffer().read(cx);
    let path = model.path()?.to_path_buf();
    let line = model
        .buffer()
        .offset_to_point(view.selections().primary().head)
        .line;
    Some((path, line as u32))
}

fn start(workspace: &mut Workspace, target: RunTarget, debug: bool, cx: &mut Context<Workspace>) {
    with_debugger(workspace, cx, |d, cx| {
        d.start(target, debug, cx);
    });
}

fn start_selected(workspace: &mut Workspace, debug: bool, cx: &mut Context<Workspace>) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    match debugger.read(cx).selected_target().cloned() {
        Some(target) => start(workspace, target, debug, cx),
        None => workspace.show_message(
            "Nothing to run: no Cargo binaries, Go or Odin main packages, or package.json scripts were found",
            cx,
        ),
    }
}

/// The test the cursor is in, as IntelliJ runs with Ctrl+Shift+R: the innermost function around
/// it that's a test, or else the test module it's in.
fn test_at_cursor(
    workspace: &Workspace,
    debugger: &Debugger,
    path: &std::path::Path,
    cx: &App,
) -> Option<RunTarget> {
    let editor = workspace.active_editor(cx)?;
    let editor = editor.read(cx);
    let model = editor.buffer().read(cx);
    let text = model.buffer();
    let whole = text.text();
    let cursor = editor.selections().primary().head;
    if let Some(syntax) = model.syntax() {
        let mut offset = cursor;
        while let Some(function) = syntax.enclosing_function(offset) {
            let line = text.offset_to_point(function.start).line;
            if let Some(test) = debugger.test_at(path, &whole, line) {
                return Some(test);
            }
            if function.start == 0 {
                break;
            }
            offset = function.start - 1;
        }
    }
    // Inside a test module but not a test: the nearest module above that's run as tests.
    let language = model.language()?;
    let cursor_line = text.offset_to_point(cursor).line;
    decorations::test_candidates(language, path, &whole)
        .into_iter()
        .filter(|&line| line <= cursor_line)
        .rev()
        .filter(|&line| block_end(&whole, text.line_start(line)) > cursor)
        .filter_map(|line| debugger.test_at(path, &whole, line))
        .find(|target| target.single_test().is_none())
}

/// Where the braced block opened on or after `from` closes, counting braces.
fn block_end(text: &str, from: usize) -> usize {
    let mut depth = 0;
    for (offset, c) in text[from..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return from + offset;
                }
            }
            _ => {}
        }
    }
    text.len()
}

fn start_active_file(workspace: &mut Workspace, debug: bool, cx: &mut Context<Workspace>) {
    let (Some(debugger), Some(path)) = (debugger(workspace, cx), workspace.active_file(cx)) else {
        return;
    };
    if let Some(test) = test_at_cursor(workspace, debugger.read(cx), &path, cx) {
        start(workspace, test, debug, cx);
        return;
    }
    match debugger.read(cx).target_for_file(&path) {
        Some(target) => start(workspace, target, debug, cx),
        None => workspace.show_message(
            format!(
                "{} isn't something that runs",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            cx,
        ),
    }
}

/// The run configurations to choose from, then runs or debugs the choice. With `mode` unset,
/// choosing only selects it for the Run and Debug buttons.
fn choose_and_start(
    workspace: &mut Workspace,
    mode: Option<bool>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    let d = debugger.read(cx);
    let targets: Vec<RunTarget> = d.targets().to_vec();
    if targets.is_empty() {
        workspace.show_message("Nothing to run was found in this project", cx);
        return;
    }
    let selected = d
        .selected_target()
        .and_then(|s| targets.iter().position(|t| t.name == s.name))
        .unwrap_or(0);
    let root = workspace.root().map(Path::to_path_buf);
    let items = targets
        .iter()
        .map(|t| PickerItem {
            label: t.name.clone(),
            detail: target_detail(t, root.as_deref()),
            indent: 0,
        })
        .collect();
    let title = match mode {
        Some(true) => "Debug",
        Some(false) => "Run",
        None => "Run Configuration",
    };
    workspace.toggle_modal(window, cx, move |_, cx| {
        Picker::new(
            title,
            items,
            selected,
            move |index, _, cx| {
                let target = targets[index].clone();
                debugger.update(cx, |d, cx| match mode {
                    Some(debug) => {
                        d.start(target, debug, cx);
                    }
                    None => d.select_target(&target.name, cx),
                });
            },
            cx,
        )
    });
}

/// Run > Attach to Process: a process, then which debugger to attach with.
fn attach_to_process(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    let processes = gleipnir_core::list_processes();
    let items = processes
        .iter()
        .map(|p| PickerItem {
            label: p.name.clone(),
            detail: format!("{}   {}", p.pid, p.command),
            indent: 0,
        })
        .collect();
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |_, cx| {
        Picker::new(
            "Attach to Process",
            items,
            0,
            move |index, window, cx| {
                let process = processes[index].clone();
                // Debuggers that can attach, with the likeliest first.
                let mut choices: Vec<(&str, &str)> = vec![
                    ("rust", "LLDB: Rust, Odin, C, C++"),
                    ("go", "Delve: Go"),
                    ("typescript", "js-debug: Node.js"),
                ];
                if process.name.contains("node") {
                    choices.rotate_left(2);
                }
                let items = choices
                    .iter()
                    .map(|(_, label)| PickerItem {
                        label: label.to_string(),
                        ..Default::default()
                    })
                    .collect();
                let Some(workspace) = weak.upgrade() else {
                    return;
                };
                // Opened after this picker has gone.
                window.defer(cx, move |window, cx| {
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.toggle_modal(window, cx, move |_, cx| {
                            Picker::new(
                                format!("Attach to {} with", process.name),
                                items,
                                0,
                                move |index, _, cx| {
                                    let package = choices[index].0;
                                    debugger.update(cx, |d, cx| {
                                        d.attach(package, process.pid, &process.name, cx);
                                    });
                                },
                                cx,
                            )
                        })
                    });
                });
            },
            cx,
        )
    });
}

fn target_detail(target: &RunTarget, root: Option<&Path>) -> String {
    let package = gleipnir_packages::registry()
        .package(&target.package_id)
        .map(|p| p.name().to_string())
        .unwrap_or_default();
    let source = target
        .source
        .as_deref()
        .map(|s| s.strip_prefix(root.unwrap_or(Path::new(""))).unwrap_or(s))
        .map(|s| s.display().to_string())
        .unwrap_or_else(|| target.run.join(" "));
    format!("{package}   {source}")
}

fn run_or_debug_menu(
    workspace: &mut Workspace,
    target: RunTarget,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(debugger) = debugger(workspace, cx) else {
        return;
    };
    let items = vec![
        PickerItem {
            label: format!("Run '{}'", target.name),
            ..Default::default()
        },
        PickerItem {
            label: format!("Debug '{}'", target.name),
            ..Default::default()
        },
    ];
    workspace.toggle_modal(window, cx, move |_, cx| {
        Picker::new(
            target.name.clone(),
            items,
            0,
            move |index, _, cx| {
                debugger.update(cx, |d, cx| {
                    d.start(target, index == 1, cx);
                });
            },
            cx,
        )
    });
}

/// The run configuration and the Run, Debug and Stop buttons, in the title bar as IntelliJ has them.
fn run_widget(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> Option<gpui::AnyElement> {
    let debugger = debugger(workspace, cx)?;
    let theme = cx.global::<Theme>().clone();
    let d = debugger.read(cx);
    let selected = d.selected_target().map(|t| t.name.clone());
    let live = d.launches().iter().any(|l| l.status.is_live());
    let state = d
        .active_launch()
        .filter(|l| l.status.is_live())
        .map(|l| match &l.status {
            LaunchStatus::Building => "building",
            LaunchStatus::Starting => "starting",
            LaunchStatus::Paused => "paused",
            _ => {
                if l.debug {
                    "debugging"
                } else {
                    "running"
                }
            }
        });
    let icon_button = |id: &'static str, icon: Icon, color, enabled: bool| {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .p(px(6.))
            .rounded(px(6.))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme.hover)))
            .child(
                svg()
                    .path(icon.path())
                    .size(px(16.))
                    .text_color(if enabled { color } else { theme.text_faint }),
            )
    };
    let has_target = selected.is_some();
    Some(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .child(
                div()
                    .id("run-configuration")
                    .debug_selector(|| "run-configuration".into())
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .max_w(px(220.))
                    .truncate()
                    .text_color(if has_target {
                        theme.text_muted
                    } else {
                        theme.text_faint
                    })
                    .child(match (&selected, state) {
                        (Some(name), Some(state)) => format!("{name} ({state}) ▾"),
                        (Some(name), None) => format!("{name} ▾"),
                        (None, _) => "No configurations".to_string(),
                    })
                    .on_click(
                        cx.listener(|w, _, window, cx| choose_and_start(w, None, window, cx)),
                    ),
            )
            .child(
                icon_button("run-button", Icon::Run, theme.git_added, has_target)
                    .on_click(cx.listener(|w, _, _, cx| start_selected(w, false, cx))),
            )
            .child(
                icon_button("debug-button", Icon::Debug, theme.git_added, has_target)
                    .on_click(cx.listener(|w, _, _, cx| start_selected(w, true, cx))),
            )
            .child(
                icon_button("stop-button", Icon::Stop, theme.error, live).on_click(
                    cx.listener(|_, _, window, cx| window.dispatch_action(Box::new(Stop), cx)),
                ),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests;
