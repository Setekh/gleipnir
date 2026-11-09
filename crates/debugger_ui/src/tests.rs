//! Running and debugging through the whole window: shortcuts in a real workspace, the real
//! debugger, and a fake adapter served over TCP that pretends to run `src/main.rs`.

use super::*;
use gleipnir_core::{AdapterLauncher, BufferStore, PreparedAdapter};
use gleipnir_dap::DebugAdapter;
use gleipnir_dap::fake::{Out, fake_server};
use gleipnir_packages::RunTargetKind;
use gleipnir_test_support::{isolate_state, project};
use gleipnir_workspace::DockPosition;
use gleipnir_workspace::Panel as _;
use gpui::{TestAppContext, VisualTestContext, size};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAIN: &str =
    "fn main() {\n    let count = 3;\n    let total = count + 1;\n    println!(\"{total}\");\n}\n";

struct Keys;
#[cfg(target_os = "macos")]
impl Keys {
    const RUN: &str = "ctrl-r";
    const DEBUG: &str = "ctrl-d";
    const TOGGLE: &str = "cmd-f8";
    const VIEW: &str = "cmd-shift-f8";
    const RESUME: &str = "cmd-alt-r";
}
#[cfg(not(target_os = "macos"))]
impl Keys {
    const RUN: &str = "shift-f10";
    const DEBUG: &str = "shift-f9";
    const TOGGLE: &str = "ctrl-f8";
    const VIEW: &str = "ctrl-shift-f8";
    const RESUME: &str = "f9";
}

/// A program that stops at the first breakpoint, steps a line at a time and then finishes.
fn handler(main: String, line: Arc<Mutex<u64>>) -> impl FnMut(&mut Out) + Send + 'static {
    let mut breakpoints: Vec<u64> = Vec::new();
    let mut count = "3".to_string();
    move |out: &mut Out| {
        let mut at = line.lock().unwrap();
        match out.command() {
            "initialize" => {
                out.ok(json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsSetVariable": true,
                }));
                out.event("initialized", json!({}));
            }
            "setVariable" => {
                count = out.args()["value"].as_str().unwrap_or("").to_string();
                out.ok(json!({ "value": count.clone() }));
            }
            "setBreakpoints" => {
                breakpoints = out.args()["breakpoints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b["line"].as_u64().unwrap())
                    .collect();
                let placed: Vec<Value> = breakpoints
                    .iter()
                    .map(|l| json!({ "verified": true, "line": l }))
                    .collect();
                out.ok(json!({ "breakpoints": placed }));
            }
            "configurationDone" => {
                out.ok(json!({}));
                match breakpoints.first() {
                    Some(&first) => {
                        *at = first;
                        out.event("stopped", json!({ "reason": "breakpoint", "threadId": 1 }));
                    }
                    None => out.event("terminated", json!({})),
                }
            }
            "threads" => out.ok(json!({ "threads": [{ "id": 1, "name": "main" }] })),
            "stackTrace" => out.ok(json!({ "stackFrames": [
                { "id": 1, "name": "main", "line": *at, "column": 5, "source": { "path": main } },
            ] })),
            "scopes" => {
                out.ok(json!({ "scopes": [{ "name": "Locals", "variablesReference": 7 }] }))
            }
            "variables" => out.ok(json!({ "variables": [
                { "name": "count", "value": count.clone(), "type": "i32", "variablesReference": 0 },
                { "name": "total", "value": "4", "type": "i32", "variablesReference": 0 },
            ] })),
            "evaluate" => {
                let expression = out.args()["expression"].as_str().unwrap_or("").to_string();
                out.ok(json!({ "result": format!("<{expression}>"), "type": "i32", "variablesReference": 0 }))
            }
            "next" => {
                out.ok(json!({}));
                *at += 1;
                out.event("stopped", json!({ "reason": "step", "threadId": 1 }));
            }
            "continue" => {
                out.ok(json!({}));
                out.event("output", json!({ "category": "stdout", "output": "4\n" }));
                out.event("exited", json!({ "exitCode": 0 }));
                out.event("terminated", json!({}));
            }
            _ => out.ok(json!({})),
        }
    }
}

struct Setup<'a> {
    workspace: Entity<Workspace>,
    debugger: Entity<Debugger>,
    cx: &'a mut VisualTestContext,
    root: PathBuf,
}

fn target(root: &Path, run: &str) -> RunTarget {
    RunTarget {
        package_id: "rust".into(),
        name: "p".into(),
        kind: RunTargetKind::Binary,
        build: None,
        run: vec!["/bin/sh".into(), "-c".into(), run.into()],
        program: Some(root.join("target/debug/p")),
        cwd: root.to_path_buf(),
        source: Some(root.join("src/main.rs")),
        args: Vec::new(),
    }
}

fn setup(cx: &mut TestAppContext) -> (tempfile::TempDir, Setup<'_>) {
    isolate_state();
    cx.executor().allow_parking();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_editor::bind_keys(cx);
        gleipnir_workspace::bind_keys(cx);
        gleipnir_code_nav::bind_keys(cx);
        bind_keys(cx);
    });
    let (dir, root) = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("src/main.rs", MAIN),
    ]);
    let main = root.join("src/main.rs").display().to_string();
    let line = Arc::new(Mutex::new(0));
    let (address, _) = fake_server(move |_| handler(main.clone(), line.clone()));
    let launcher: AdapterLauncher = Rc::new(move |_, _, _| {
        Ok(PreparedAdapter {
            adapter_id: "fake".into(),
            connect: Box::new(move || DebugAdapter::connect_to("fake", address)),
        })
    });
    let store = cx.new(|_| BufferStore::new());
    let project_root = root.clone();
    let made = Rc::new(std::cell::RefCell::new(None));
    let keep = made.clone();
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(store, Some(project_root.clone()), window, cx);
        let toolchains = workspace.toolchains().clone();
        let debugger = cx.new(|cx| {
            let mut debugger =
                Debugger::with_launcher(Some(project_root.clone()), toolchains, launcher, cx);
            debugger.set_targets(vec![target(&project_root, "echo ran; exit 3")], cx);
            debugger
        });
        *keep.borrow_mut() = Some(debugger.clone());
        add_with_debugger(&mut workspace, debugger, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1200.), px(800.)));
    let debugger = made.borrow().clone().unwrap();
    (
        dir,
        Setup {
            workspace,
            debugger,
            cx,
            root,
        },
    )
}

impl Setup<'_> {
    fn open(&mut self, line: usize) -> Entity<EditorView> {
        let path = self.root.join("src/main.rs");
        let workspace = self.workspace.clone();
        let task = self.cx.update(|window, cx| {
            workspace.update(cx, |w, cx| {
                w.open_path_at(path, Some((line, 0)), window, cx)
            })
        });
        gleipnir_test_support::resolve(self.cx, task).unwrap()
    }

    fn editor_state(&mut self) -> gleipnir_editor::EditorState {
        let workspace = self.workspace.clone();
        self.cx.run_until_parked();
        self.cx
            .update(|_, cx| workspace.read(cx).editor_state(cx))
            .expect("an editor")
    }

    fn panel_state(&mut self) -> Value {
        let workspace = self.workspace.clone();
        self.cx
            .update(|window, cx| workspace.read(cx).snapshot(window, cx).panels["debug"].clone())
    }

    fn bottom_panel(&mut self) -> Option<String> {
        let workspace = self.workspace.clone();
        self.cx.update(|window, cx| {
            let snapshot = workspace.read(cx).snapshot(window, cx);
            snapshot
                .docks
                .iter()
                .find(|d| d.position == DockPosition::Bottom && d.visible)
                .and_then(|d| d.active.clone())
        })
    }

    fn wait(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "timed out");
            self.cx.executor().advance_clock(Duration::from_millis(100));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn status(&mut self) -> Option<LaunchStatus> {
        let debugger = self.debugger.clone();
        self.cx
            .update(|_, cx| debugger.read(cx).active_launch().map(|l| l.status.clone()))
    }
}

#[gpui::test]
fn breakpoints_toggle_from_the_gutter_and_the_shortcut(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open(1);
    s.cx.update(|_, cx| editor.update(cx, |_, cx| cx.emit(EditorEvent::GutterClicked { line: 2 })));
    assert_eq!(s.editor_state().breakpoints, [3]);
    s.cx.simulate_keystrokes(Keys::TOGGLE);
    assert_eq!(s.editor_state().breakpoints, [2, 3]);
    s.cx.update(|_, cx| editor.update(cx, |_, cx| cx.emit(EditorEvent::GutterClicked { line: 2 })));
    assert_eq!(s.editor_state().breakpoints, [2]);

    // A new line opened above it moves it down with its line.
    s.cx.simulate_keystrokes("home enter");
    assert_eq!(s.editor_state().breakpoints, [3]);
}

#[gpui::test]
fn debugging_stops_at_the_breakpoint_and_shows_where_and_what(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open(2);
    s.cx.simulate_keystrokes(Keys::TOGGLE);
    s.cx.simulate_keystrokes(Keys::DEBUG);
    s.wait(|s| s.status() == Some(LaunchStatus::Paused));
    s.wait(|s| !s.editor_state().inline_values.is_empty());
    assert_eq!(s.bottom_panel().as_deref(), Some(DEBUG_PANEL));

    let state = s.editor_state();
    assert_eq!(state.execution_line, Some(3));
    // Values beside the lines that use them, from the start of `main` down to the stop.
    assert_eq!(
        state.inline_values,
        ["2: count: 3", "3: total: 4, count: 3"]
    );
    let panel = s.panel_state();
    assert_eq!(panel["launch"]["frames"], json!(["main:3"]));
    let rows: Vec<String> = serde_json::from_value(panel["variable_rows"].clone()).unwrap();
    assert_eq!(rows, ["Locals", "  count = 3", "  total = 4"]);

    s.cx.simulate_keystrokes("f8");
    s.wait(|s| s.editor_state().execution_line == Some(4));
    s.cx.simulate_keystrokes(Keys::RESUME);
    s.wait(|s| s.status() == Some(LaunchStatus::Finished(Some(0))));
    assert_eq!(s.editor_state().execution_line, None);
    let console = s.panel_state()["launch"]["console"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        console.contains("Process finished with exit code 0"),
        "{console}"
    );
}

#[gpui::test]
fn watches_and_evaluate_use_the_stopped_frame(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open(2);
    s.cx.simulate_keystrokes(Keys::TOGGLE);
    s.cx.update(|_, cx| {
        s.debugger
            .update(cx, |d, cx| d.add_watch("count * 2".into(), cx))
    });
    s.cx.simulate_keystrokes(Keys::DEBUG);
    s.wait(|s| {
        s.panel_state()["launch"]["watches"]
            .as_array()
            .is_some_and(|w| w.first() == Some(&json!("<count * 2>")))
    });

    // Alt+F8 starts from the name under the cursor.
    s.cx.simulate_keystrokes("end left left left left left left left left left left");
    s.cx.simulate_keystrokes("alt-f8");
    let workspace = s.workspace.clone();
    let modal =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<EvaluateModal>())
            .expect("the Evaluate popup");
    s.cx.simulate_keystrokes("enter");
    s.wait(|s| s.cx.update(|_, cx| modal.read(cx).result_text()).is_some());
    assert_eq!(
        s.cx.update(|_, cx| modal.read(cx).result_text()),
        Some("<count> (i32)".into())
    );
}

#[gpui::test]
fn a_breakpoint_can_be_given_a_condition_and_listed(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open(1);
    s.cx.update(|_, cx| {
        editor.update(cx, |_, cx| {
            cx.emit(EditorEvent::GutterMenu {
                line: 1,
                position: gpui::point(px(10.), px(10.)),
            })
        })
    });
    let workspace = s.workspace.clone();
    let popup =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<BreakpointEditor>())
            .expect("the breakpoint popup");
    s.cx.update(|_, cx| {
        popup.update(cx, |p, cx| {
            p.set_fields("count > 2", "", "", cx);
            p.confirm(cx);
        })
    });
    let breakpoint = s.cx.update(|_, cx| {
        s.debugger
            .read(cx)
            .breakpoints()
            .get(&s.root.join("src/main.rs"), 1)
            .cloned()
    });
    assert_eq!(breakpoint.unwrap().condition.as_deref(), Some("count > 2"));

    // On the breakpoint's own line, the shortcut opens its settings.
    s.cx.simulate_keystrokes(Keys::VIEW);
    assert!(
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<BreakpointEditor>())
            .is_some()
    );
    s.cx.simulate_keystrokes("escape down");
    // Elsewhere, the whole list.
    s.cx.simulate_keystrokes(Keys::VIEW);
    let list =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<BreakpointList>())
            .expect("the breakpoint list");
    assert_eq!(
        s.cx.update(|_, cx| list.read(cx).labels(cx)),
        ["src/main.rs:2 if count > 2"]
    );
    s.cx.simulate_keystrokes("down space");
    assert_eq!(
        s.cx.update(|_, cx| list.read(cx).labels(cx)),
        ["src/main.rs:2 (disabled) if count > 2"]
    );
    s.cx.simulate_keystrokes("backspace");
    assert!(s.cx.update(|_, cx| list.read(cx).labels(cx)).is_empty());
}

#[gpui::test]
fn run_shows_the_programs_output_in_the_run_panel(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open(0);
    s.cx.simulate_keystrokes(Keys::RUN);
    s.wait(|s| s.status() == Some(LaunchStatus::Finished(Some(3))));
    assert_eq!(s.bottom_panel().as_deref(), Some(RUN_PANEL));
    let workspace = s.workspace.clone();
    let run =
        s.cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).panels["run"].clone());
    let console = run["console"].as_str().unwrap();
    assert!(console.contains("ran"), "{console}");
    assert!(console.contains("exit code 3"), "{console}");
}

#[gpui::test]
fn breakpoints_and_watches_are_saved_with_the_project(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open(2);
    s.cx.simulate_keystrokes(Keys::TOGGLE);
    s.cx.update(|_, cx| {
        s.debugger
            .update(cx, |d, cx| d.add_watch("total".into(), cx))
    });
    let workspace = s.workspace.clone();
    let saved = s.cx.update(|_, cx| workspace.read(cx).to_saved(cx));
    let debug = &saved.panels["debug"];
    let file = s.root.join("src/main.rs").display().to_string();
    assert_eq!(debug["breakpoints"][&file][0]["line"], 2);
    assert_eq!(debug["watches"], json!(["total"]));
    // Nothing was chosen, so the first configuration is used and nothing is saved for it.
    assert_eq!(debug["configuration"], Value::Null);

    // A fresh debugger takes them back.
    s.cx.update(|_, cx| {
        s.debugger.update(cx, |d, cx| {
            d.remove_all_breakpoints(cx);
            d.set_watches(Vec::new(), cx);
        })
    });
    let panel =
        s.cx.update(|_, cx| workspace.read(cx).panel::<DebugPanel>(cx))
            .unwrap();
    s.cx.update(|window, cx| panel.update(cx, |p, cx| p.restore_state(debug.clone(), window, cx)));
    let (count, watches) = s.cx.update(|_, cx| {
        let d = s.debugger.read(cx);
        (d.breakpoints().len(), d.watches().to_vec())
    });
    assert_eq!((count, watches), (1, vec!["total".to_string()]));
}

#[gpui::test]
fn what_is_typed_in_the_run_console_reaches_the_program(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open(0);
    let target = target(&s.root, "read name; echo hello $name");
    s.cx.update(|_, cx| {
        s.debugger.update(cx, |d, cx| {
            d.start(target, false, cx);
        })
    });
    s.wait(|s| s.status() == Some(LaunchStatus::Running));
    let workspace = s.workspace.clone();
    let panel =
        s.cx.update(|_, cx| workspace.read(cx).panel::<RunPanel>(cx))
            .unwrap();
    // The console follows the running launch once the panel hears of it.
    s.wait(|s| {
        let debugger = s.debugger.clone();
        s.cx.update(|_, cx| {
            let id = debugger.read(cx).active_launch().map(|l| l.id).unwrap();
            debugger.read(cx).accepts_input(id)
        })
    });
    s.cx.update(|_, cx| {
        let console = panel.read(cx).console().clone();
        console.update(cx, |c, cx| c.enter("gleipnir", cx));
    });
    s.wait(|s| s.status() == Some(LaunchStatus::Finished(Some(0))));
    let console =
        s.cx.update(|_, cx| s.debugger.read(cx).active_launch().unwrap().console_text());
    assert!(console.contains("hello gleipnir"), "{console}");
}

#[gpui::test]
fn a_value_can_be_changed_while_stopped(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open(2);
    s.cx.simulate_keystrokes(Keys::TOGGLE);
    s.cx.simulate_keystrokes(Keys::DEBUG);
    s.wait(|s| {
        s.panel_state()["variable_rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r == "  count = 3"))
    });
    let workspace = s.workspace.clone();
    let panel =
        s.cx.update(|_, cx| workspace.read(cx).panel::<DebugPanel>(cx))
            .unwrap();
    s.cx.update(|window, cx| {
        panel.update(cx, |p, cx| p.set_value(7, "count".into(), "3", window, cx))
    });
    let modal =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<SetValueModal>())
            .expect("the Set Value popup");
    s.cx.update(|_, cx| {
        modal.update(cx, |m, cx| {
            m.set_text("10", cx);
            m.confirm(cx);
        })
    });
    s.wait(|s| {
        s.panel_state()["variable_rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r == "  count = 10"))
    });
}

#[gpui::test]
fn a_configuration_can_be_given_arguments_environment_and_a_folder(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open(0);
    s.cx.dispatch_action(EditConfiguration);
    let workspace = s.workspace.clone();
    let modal =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<RunSettingsModal>())
            .expect("the configuration popup");
    s.cx.update(|_, cx| {
        modal.update(cx, |m, cx| {
            m.set_fields(
                "--port 80 'two words'",
                "MODE=dev LEVEL=\"very high\"",
                "/tmp",
                cx,
            );
            m.confirm(cx);
        })
    });
    let settings = s.cx.update(|_, cx| s.debugger.read(cx).run_settings("p"));
    assert_eq!(settings.args, ["--port", "80", "two words"]);
    assert_eq!(settings.env["LEVEL"], "very high");
    assert_eq!(settings.cwd.as_deref(), Some(Path::new("/tmp")));

    // Opened again, the fields show what was saved.
    s.cx.dispatch_action(EditConfiguration);
    let saved = s.cx.update(|_, cx| workspace.read(cx).to_saved(cx));
    assert_eq!(
        saved.panels["debug"]["run_settings"]["p"]["args"][2],
        "two words"
    );
}

const LIB: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {}\n\n    #[test]\n    fn fails() {\n        panic!(\"boom\");\n    }\n}\n";

#[gpui::test]
fn test_results_fill_the_run_panel_and_open_what_is_clicked(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let lib = s.root.join("src/lib.rs");
    std::fs::write(&lib, LIB).unwrap();
    let script = "echo '     Running unittests src/lib.rs (target/debug/deps/p-0123abcd)' >&2; \
        printf 'running 2 tests\\ntest tests::adds ... ok\\ntest tests::fails ... FAILED\\n\\nfailures:\\n\\n\
        ---- tests::fails stdout ----\\n\\nthread tests::fails panicked at src/lib.rs:8:9:\\nboom\\n\\n\
        failures:\\n    tests::fails\\n\\ntest result: FAILED. 1 passed; 1 failed; finished in 0.01s\\n'; exit 101";
    let mut test = target(&s.root, script);
    test.kind = RunTargetKind::Test;
    test.name = "tests in p".into();
    s.cx.update(|_, cx| s.debugger.update(cx, |d, cx| d.start(test, false, cx)));
    s.wait(|s| s.status() == Some(LaunchStatus::Finished(Some(101))));
    let debugger = s.debugger.clone();
    let failed = lib.clone();
    s.wait(|s| {
        s.cx.update(|_, cx| debugger.read(cx).test_result(&failed, "fails"))
            == Some(gleipnir_core::TestState::Failed)
    });

    let workspace = s.workspace.clone();
    let run =
        s.cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).panels["run"].clone());
    assert_eq!(
        run["tests"],
        json!([
            "Failed Test Results",
            "  Failed p",
            "    Failed tests",
            "      Passed adds",
            "      Failed fails"
        ])
    );
    assert_eq!(run["test_summary"], "1 failed, 1 passed of 2");

    // Choosing the failed test shows only its output.
    let panel =
        s.cx.update(|_, cx| workspace.read(cx).panel::<RunPanel>(cx))
            .unwrap();
    let view = s.cx.update(|_, cx| panel.read(cx).tests().clone());
    let fails = s.cx.update(|_, cx| {
        let d = debugger.read(cx);
        let tests = d.active_launch().unwrap().tests.as_ref().unwrap();
        tests.failed_tests()[0]
    });
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.select(fails, cx)));
    let shown =
        s.cx.update(|_, cx| panel.read(cx).console().read(cx).shown_text(cx));
    assert!(shown.contains("boom"), "{shown}");
    assert!(!shown.contains("running 2 tests"), "{shown}");

    // Jumping to its source opens the test's function, which the gutter marks as failed.
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.jump_to_source(fails, cx)));
    let state = s.editor_state();
    assert_eq!(state.path.as_deref(), Some(lib.to_str().unwrap()));
    assert_eq!(state.cursor, (6, 0));
    assert_eq!(state.run_results, ["4: passed", "7: failed"]);

    // The panic's location in the output is a link to the line.
    s.cx.update(|_, cx| {
        let console = panel.read(cx).console().clone();
        console.update(cx, |c, cx| c.open_link("src/lib.rs", 8, Some(9), cx));
    });
    assert_eq!(s.editor_state().cursor, (7, 8));

    // Rerun Failed runs that test alone.
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.rerun_failed(cx)));
    let rerun =
        s.cx.update(|_, cx| debugger.read(cx).active_launch().unwrap().target.clone());
    assert_eq!(rerun.name, "test fails");
    assert_eq!(rerun.args, ["--exact", "tests::fails"]);
}
