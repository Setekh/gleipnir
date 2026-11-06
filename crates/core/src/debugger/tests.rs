//! The debugger against a scripted fake adapter served over TCP, so child sessions work as they
//! do with js-debug, and against real processes for builds and Run.

use super::*;
use crate::toolchains::Toolchains;
use gleipnir_dap::fake::{Out, fake_server};
use gleipnir_packages::RunTargetKind;
use gpui::{AppContext as _, TestAppContext};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Instant;

const SRC: &str = "/p/src/main.rs";

fn wait(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.update(|_| {});
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out");
        cx.executor().advance_clock(Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The fake program: it stops at the first breakpoint it's given, steps a line at a time, and
/// continues to the next breakpoint below, or to the end.
#[derive(Default)]
struct Program {
    /// Lines with breakpoints, counting from one, as last set.
    breakpoints: Vec<u64>,
    /// Every `setBreakpoints` the fake got: the lines, and their conditions.
    set_calls: Vec<Vec<(u64, Option<String>)>>,
    line: u64,
    /// Crash on this command, as a broken adapter would.
    crash_on: Option<&'static str>,
    /// Start the real session as a child, as js-debug does.
    child_session: bool,
}

type Shared = Arc<Mutex<Program>>;

fn frames(line: u64) -> Value {
    json!({ "stackFrames": [
        { "id": 10, "name": "main", "line": line, "column": 1, "source": { "path": SRC } },
        { "id": 11, "name": "start", "line": 1, "column": 1, "source": { "path": SRC } },
    ] })
}

fn handler(program: Shared, connection: usize) -> impl FnMut(&mut Out) + Send + 'static {
    move |out: &mut Out| {
        let mut p = program.lock().unwrap();
        if p.crash_on.is_some_and(|c| c == out.command()) {
            out.die();
            return;
        }
        let parent = p.child_session && connection == 0;
        match out.command() {
            "initialize" => {
                out.ok(json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsConditionalBreakpoints": true,
                    "supportsLogPoints": true,
                }));
                out.event("initialized", json!({}));
            }
            "launch" if parent => {
                out.ok(json!({}));
                out.send(json!({
                    "seq": 900, "type": "request", "command": "startDebugging",
                    "arguments": { "request": "launch",
                                   "configuration": { "name": "child", "__pendingTargetId": "t1" } },
                }));
            }
            "setBreakpoints" => {
                let list = out.args()["breakpoints"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let lines: Vec<(u64, Option<String>)> = list
                    .iter()
                    .map(|b| {
                        (
                            b["line"].as_u64().unwrap(),
                            b["condition"].as_str().map(String::from),
                        )
                    })
                    .collect();
                p.breakpoints = lines.iter().map(|(l, _)| *l).collect();
                p.set_calls.push(lines.clone());
                let placed: Vec<Value> = lines
                    .iter()
                    .enumerate()
                    .map(|(i, (line, _))| json!({ "id": i + 1, "verified": *line < 100, "line": line }))
                    .collect();
                out.ok(json!({ "breakpoints": placed }));
            }
            "configurationDone" if parent => out.ok(json!({})),
            "configurationDone" => {
                out.ok(json!({}));
                out.event(
                    "output",
                    json!({ "category": "stdout", "output": "hello\n" }),
                );
                out.event(
                    "output",
                    json!({ "category": "telemetry", "output": "secret" }),
                );
                match p.breakpoints.first() {
                    Some(&line) => {
                        p.line = line;
                        out.event("stopped", json!({ "reason": "breakpoint", "threadId": 1 }));
                    }
                    None => {
                        out.event("exited", json!({ "exitCode": 0 }));
                        out.event("terminated", json!({}));
                    }
                }
            }
            "threads" => out.ok(json!({ "threads": [{ "id": 1, "name": "main" }] })),
            "stackTrace" => out.ok(frames(p.line)),
            "scopes" => out.ok(json!({ "scopes": [
                { "name": "Locals", "variablesReference": 100 },
                { "name": "Registers", "variablesReference": 200, "expensive": true },
            ] })),
            "variables" => match out.args()["variablesReference"].as_i64() {
                Some(100) => out.ok(json!({ "variables": [
                    { "name": "count", "value": p.line.to_string(), "variablesReference": 0 },
                    { "name": "point", "value": "Point", "variablesReference": 101 },
                ] })),
                Some(101) => out.ok(json!({ "variables": [
                    { "name": "x", "value": "1", "variablesReference": 0 },
                ] })),
                _ => out.fail(json!({})),
            },
            "evaluate" => {
                let expression = out.args()["expression"].as_str().unwrap_or("").to_string();
                if expression == "bad" {
                    out.fail(json!({ "error": { "id": 1, "format": "no such name" } }));
                } else {
                    let frame = out.args()["frameId"].clone();
                    out.ok(json!({ "result": format!("{expression}@{frame}"), "variablesReference": 0 }));
                }
            }
            "next" => {
                out.ok(json!({}));
                p.line += 1;
                out.event("stopped", json!({ "reason": "step", "threadId": 1 }));
            }
            "continue" => {
                out.ok(json!({ "allThreadsContinued": true }));
                let current = p.line;
                match p.breakpoints.iter().find(|&&l| l > current) {
                    Some(&line) => {
                        p.line = line;
                        out.event("stopped", json!({ "reason": "breakpoint", "threadId": 1 }));
                    }
                    None => {
                        out.event("output", json!({ "category": "stdout", "output": "bye\n" }));
                        out.event("exited", json!({ "exitCode": 0 }));
                        out.event("terminated", json!({}));
                    }
                }
            }
            "disconnect" => {
                out.ok(json!({}));
                out.event("terminated", json!({}));
            }
            _ => out.ok(json!({})),
        }
    }
}

struct Setup {
    debugger: Entity<Debugger>,
    program: Shared,
    received: mpsc::Receiver<(usize, Value)>,
    events: Rc<std::cell::RefCell<Vec<DebuggerEvent>>>,
}

impl Setup {
    fn read<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&Debugger) -> R) -> R {
        self.debugger.read_with(cx, |d, _| f(d))
    }

    fn status(&self, cx: &mut TestAppContext) -> Option<LaunchStatus> {
        self.read(cx, |d| d.active_launch().map(|l| l.status.clone()))
    }

    fn wait_for(&self, cx: &mut TestAppContext, status: LaunchStatus) {
        let debugger = self.debugger.clone();
        wait(cx, |cx| {
            debugger.read_with(cx, |d, _| d.active_launch().map(|l| l.status.clone()))
                == Some(status.clone())
        });
    }

    fn console(&self, cx: &mut TestAppContext) -> String {
        self.read(cx, |d| d.active_launch().unwrap().console_text())
    }

    fn commands(&self) -> Vec<(usize, String)> {
        self.received
            .try_iter()
            .filter(|(_, m)| m["type"] == "request")
            .map(|(c, m)| (c, m["command"].as_str().unwrap_or("").to_string()))
            .collect()
    }
}

fn setup(program: Program, cx: &mut TestAppContext) -> Setup {
    // The fake and the programs run on threads of their own.
    cx.executor().allow_parking();
    let program = Arc::new(Mutex::new(program));
    let shared = program.clone();
    let (address, received) = fake_server(move |connection| handler(shared.clone(), connection));
    let launcher: AdapterLauncher = Rc::new(move |_, _, _| {
        Ok(PreparedAdapter {
            adapter_id: "fake".into(),
            connect: Box::new(move || DebugAdapter::connect_to("fake", address)),
        })
    });
    let env = gleipnir_packages::Env::from_pairs([("PATH", "/usr/bin:/bin")]);
    let toolchains = cx.new(|cx| {
        Toolchains::with_environment(None, env, std::env::temp_dir().join("no-tools"), cx)
    });
    let debugger = cx.new(|cx| Debugger::with_launcher(None, toolchains, launcher, cx));
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|cx| {
        cx.subscribe(&debugger, move |_, event: &DebuggerEvent, _| {
            seen.borrow_mut().push(event.clone())
        })
        .detach();
    });
    Setup {
        debugger,
        program,
        received,
        events,
    }
}

fn target(build: Option<&str>, run: &str) -> RunTarget {
    let sh = |script: &str| vec!["/bin/sh".to_string(), "-c".into(), script.into()];
    RunTarget {
        package_id: "rust".into(),
        name: "p".into(),
        kind: RunTargetKind::Binary,
        build: build.map(sh),
        run: sh(run),
        program: Some("/p/target/debug/p".into()),
        cwd: std::env::temp_dir(),
        source: Some(SRC.into()),
    }
}

fn src() -> PathBuf {
    PathBuf::from(SRC)
}

#[gpui::test]
fn debugging_stops_at_a_breakpoint_then_steps_and_runs_to_the_end(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    setup.debugger.update(cx, |d, cx| {
        // Zero-based in the editor: line 4 to the adapter.
        d.toggle_breakpoint(&src(), 3, cx);
        d.start(target(None, ""), true, cx);
    });
    setup.wait_for(cx, LaunchStatus::Paused);
    let debugger = setup.debugger.clone();
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.frame_values().is_some())
    });

    assert_eq!(
        setup.read(cx, |d| d.execution_point()),
        Some((src(), 3, true))
    );
    let (path, values) = setup.read(cx, |d| d.frame_values()).unwrap();
    assert_eq!(path, src());
    assert_eq!(values[0], ("count".into(), "4".into()));
    assert!(
        setup
            .events
            .borrow()
            .contains(&DebuggerEvent::ShowLocation {
                path: src(),
                line: 3
            })
    );
    assert_eq!(
        setup.read(cx, |d| d.breakpoint_status(&src(), 3).cloned()),
        Some(BreakpointStatus {
            verified: true,
            message: None
        })
    );
    // Output reached the console, but the adapter's telemetry didn't.
    assert!(setup.console(cx).contains("hello"));
    assert!(!setup.console(cx).contains("secret"));

    setup.debugger.update(cx, |d, cx| d.resume(Step::Over, cx));
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.execution_point().map(|p| p.1)) == Some(4)
    });
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.frame_values().is_some())
    });
    assert_eq!(
        setup.read(cx, |d| d.frame_values()).unwrap().1[0].1,
        "5",
        "variables are fetched again at each stop"
    );

    setup
        .debugger
        .update(cx, |d, cx| d.resume(Step::Continue, cx));
    setup.wait_for(cx, LaunchStatus::Finished(Some(0)));
    let console = setup.console(cx);
    assert!(console.contains("bye"), "{console}");
    assert!(
        console.contains("Process finished with exit code 0"),
        "{console}"
    );
    assert_eq!(setup.read(cx, |d| d.execution_point()), None);
    // Statuses belong to sessions, so they go when the last one does.
    assert_eq!(
        setup.read(cx, |d| d.breakpoint_status(&src(), 3).cloned()),
        None
    );

    let handshake: Vec<String> = setup
        .commands()
        .into_iter()
        .map(|(_, c)| c)
        .take(4)
        .collect();
    assert_eq!(
        handshake,
        [
            "initialize",
            "launch",
            "setBreakpoints",
            "configurationDone"
        ]
    );
}

#[gpui::test]
fn choosing_a_caller_frame_shows_it_and_watches_evaluate_there(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    setup.debugger.update(cx, |d, cx| {
        d.toggle_breakpoint(&src(), 5, cx);
        d.add_watch("count + 1".into(), cx);
        d.start(target(None, ""), true, cx);
    });
    let debugger = setup.debugger.clone();
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| {
            d.active_launch()
                .and_then(Launch::focused)
                .is_some_and(|s| matches!(s.watches.first(), Some(WatchValue::Value(_))))
        })
    });
    let watch = |cx: &mut TestAppContext| {
        debugger.read_with(cx, |d, _| {
            d.active_launch()
                .unwrap()
                .focused()
                .unwrap()
                .watches
                .first()
                .cloned()
        })
    };
    let Some(WatchValue::Value(value)) = watch(cx) else {
        panic!()
    };
    assert_eq!(value.result, "count + 1@10");

    setup.debugger.update(cx, |d, cx| d.select_frame(1, cx));
    wait(
        cx,
        |cx| matches!(watch(cx), Some(WatchValue::Value(v)) if v.result == "count + 1@11"),
    );
    assert_eq!(
        setup.read(cx, |d| d.execution_point()),
        Some((src(), 0, false)),
        "a caller isn't the top frame"
    );
    assert!(
        setup
            .events
            .borrow()
            .contains(&DebuggerEvent::ShowLocation {
                path: src(),
                line: 0
            })
    );

    // Evaluating in the console shows the expression and its value, or the error.
    setup.debugger.update(cx, |d, cx| {
        d.evaluate_in_console("point.x", cx);
    });
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| {
            d.active_launch()
                .unwrap()
                .console_text()
                .contains("point.x@11")
        })
    });
    setup
        .debugger
        .update(cx, |d, cx| d.evaluate_in_console("bad", cx));
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| {
            d.active_launch()
                .unwrap()
                .console_text()
                .contains("no such name")
        })
    });
    let console = setup.console(cx);
    assert!(console.contains("> point.x"), "{console}");

    // Children load when asked for.
    setup.debugger.update(cx, |d, cx| d.load_variables(101, cx));
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| {
            d.active_launch()
                .unwrap()
                .focused()
                .unwrap()
                .variables
                .contains_key(&101)
        })
    });
}

#[gpui::test]
fn breakpoints_follow_edits_and_changes_reach_a_live_session(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    let buffer = cx.new(|cx| BufferModel::new("a\nb\nc\nd\ne\nf\n", Some(src()), cx));
    setup.debugger.update(cx, |d, cx| {
        d.track_buffer(&buffer, cx);
        d.toggle_breakpoint(&src(), 2, cx);
        d.start(target(None, ""), true, cx);
    });
    setup.wait_for(cx, LaunchStatus::Paused);

    // A line typed above moves the breakpoint down, and the adapter hears of it.
    buffer.update(cx, |buffer, cx| {
        let mut selections = gleipnir_text::SelectionSet::cursor(0);
        buffer.edit(&mut selections, EntityId::from(1u64), cx, |e| {
            let edits = vec![gleipnir_text::TextEdit::new(0..0, "new\n")];
            (
                (),
                e.apply_edits(edits, None, gleipnir_text::UndoGrouping::Distinct),
            )
        });
    });
    let debugger = setup.debugger.clone();
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.breakpoints().get(&src(), 3).is_some())
    });
    let program = setup.program.clone();
    wait(cx, |_| {
        program.lock().unwrap().set_calls.last().map(|c| c[0].0) == Some(4)
    });

    // A condition goes to the adapter with the breakpoint.
    setup.debugger.update(cx, |d, cx| {
        d.set_breakpoint(
            &src(),
            Breakpoint {
                condition: Some("count > 2".into()),
                ..Breakpoint::at(5)
            },
            cx,
        )
    });
    wait(cx, |_| {
        program.lock().unwrap().set_calls.last().map(Vec::len) == Some(2)
    });
    let last = program.lock().unwrap().set_calls.last().cloned().unwrap();
    assert_eq!(last[1], (6, Some("count > 2".into())));

    // Disabling one stops sending it.
    setup.debugger.update(cx, |d, cx| {
        d.set_breakpoint(
            &src(),
            Breakpoint {
                enabled: false,
                ..Breakpoint::at(5)
            },
            cx,
        )
    });
    wait(cx, |_| {
        program.lock().unwrap().set_calls.last().map(Vec::len) == Some(1)
    });
    assert!(
        setup
            .events
            .borrow()
            .contains(&DebuggerEvent::BreakpointsChanged)
    );
}

#[gpui::test]
fn unverified_breakpoints_say_so(cx: &mut TestAppContext) {
    // The fake can't place breakpoints past line 100.
    let setup = setup(Program::default(), cx);
    setup.debugger.update(cx, |d, cx| {
        d.toggle_breakpoint(&src(), 1, cx);
        d.toggle_breakpoint(&src(), 150, cx);
        d.start(target(None, ""), true, cx);
    });
    setup.wait_for(cx, LaunchStatus::Paused);
    let mut status = |line| setup.read(cx, |d| d.breakpoint_status(&src(), line).cloned());
    assert!(status(1).unwrap().verified);
    assert!(!status(150).unwrap().verified);
}

#[gpui::test]
fn run_to_line_stops_there_once(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    setup.debugger.update(cx, |d, cx| {
        d.toggle_breakpoint(&src(), 1, cx);
        d.start(target(None, ""), true, cx);
    });
    setup.wait_for(cx, LaunchStatus::Paused);
    let debugger = setup.debugger.clone();
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.execution_point().is_some())
    });
    setup
        .debugger
        .update(cx, |d, cx| d.run_to_line(&src(), 7, cx));
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.execution_point().map(|p| p.1)) == Some(7)
    });
    // The one-off breakpoint is gone from the adapter after the stop, and was never the project's.
    let program = setup.program.clone();
    wait(cx, |_| program.lock().unwrap().breakpoints == [2]);
    assert!(setup.read(cx, |d| d.breakpoints().get(&src(), 7).is_none()));
}

#[gpui::test]
fn js_debug_style_child_sessions_do_the_debugging(cx: &mut TestAppContext) {
    let setup = setup(
        Program {
            child_session: true,
            ..Program::default()
        },
        cx,
    );
    setup.debugger.update(cx, |d, cx| {
        d.toggle_breakpoint(&src(), 3, cx);
        d.start(target(None, ""), true, cx);
    });
    setup.wait_for(cx, LaunchStatus::Paused);
    let debugger = setup.debugger.clone();
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.execution_point().is_some())
    });
    let (sessions, focus) = setup.read(cx, |d| {
        let launch = d.active_launch().unwrap();
        (launch.sessions.len(), launch.focus)
    });
    assert_eq!(sessions, 2);
    assert_eq!(focus, Some(1), "the child stopped, so it has the focus");
    let commands = setup.commands();
    assert!(commands.contains(&(1, "launch".into())), "{commands:?}");
    assert!(commands.contains(&(1, "setBreakpoints".into())));

    // Stepping goes to the child.
    setup.debugger.update(cx, |d, cx| d.resume(Step::Over, cx));
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| d.execution_point().map(|p| p.1)) == Some(4)
    });
    assert!(setup.commands().contains(&(1, "next".into())));

    // Stopping ends the parent, and with it the launch.
    let id = setup.read(cx, |d| d.active_launch().unwrap().id);
    setup.debugger.update(cx, |d, cx| d.stop(id, cx));
    setup.wait_for(cx, LaunchStatus::Finished(None));
}

#[gpui::test]
fn an_adapter_crash_fails_the_launch(cx: &mut TestAppContext) {
    let setup = setup(
        Program {
            crash_on: Some("launch"),
            ..Program::default()
        },
        cx,
    );
    setup.debugger.update(cx, |d, cx| {
        d.start(target(None, ""), true, cx);
    });
    let debugger = setup.debugger.clone();
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| {
            matches!(
                d.active_launch().map(|l| &l.status),
                Some(LaunchStatus::Failed(_))
            )
        })
    });
    assert!(!setup.read(cx, |d| d.is_running()));
}

#[gpui::test]
fn a_program_without_breakpoints_runs_to_the_end_under_the_debugger(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    setup.debugger.update(cx, |d, cx| {
        d.start(target(None, ""), true, cx);
    });
    setup.wait_for(cx, LaunchStatus::Finished(Some(0)));
    assert!(setup.console(cx).contains("hello"));
}

#[gpui::test]
fn running_streams_output_and_reports_the_exit_code(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    setup.debugger.update(cx, |d, cx| {
        d.start(
            target(Some("echo compiling"), "echo out; echo err >&2; exit 4"),
            false,
            cx,
        );
    });
    setup.wait_for(cx, LaunchStatus::Finished(Some(4)));
    let console = setup.console(cx);
    for expected in [
        "compiling",
        "out",
        "err",
        "Process finished with exit code 4",
    ] {
        assert!(console.contains(expected), "{expected} in {console}");
    }
    let kinds: Vec<ConsoleKind> = setup.read(cx, |d| {
        d.active_launch()
            .unwrap()
            .console
            .iter()
            .map(|l| l.kind)
            .collect()
    });
    assert!(kinds.contains(&ConsoleKind::Stderr));
    assert!(setup.events.borrow().contains(&DebuggerEvent::Started {
        id: 1,
        debug: false
    }));
}

#[gpui::test]
fn a_failed_build_never_runs_the_program(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    setup.debugger.update(cx, |d, cx| {
        d.start(
            target(Some("echo broken >&2; exit 2"), "echo ran"),
            true,
            cx,
        );
    });
    setup.wait_for(
        cx,
        LaunchStatus::Failed("Build failed with exit code 2".into()),
    );
    let console = setup.console(cx);
    assert!(console.contains("broken"));
    assert!(!console.contains("ran"));
    assert!(setup.commands().is_empty(), "the debugger never started");
}

#[gpui::test]
fn stopping_a_running_program_kills_it_and_rerunning_replaces_the_tab(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    let id = setup.debugger.update(cx, |d, cx| {
        d.start(target(None, "echo started; sleep 30"), false, cx)
    });
    let debugger = setup.debugger.clone();
    wait(cx, |cx| {
        debugger.read_with(cx, |d, _| {
            d.active_launch()
                .unwrap()
                .console_text()
                .contains("started")
        })
    });
    assert_eq!(setup.status(cx), Some(LaunchStatus::Running));
    setup.debugger.update(cx, |d, cx| d.stop(id, cx));
    setup.wait_for(cx, LaunchStatus::Finished(None));

    let again = setup.debugger.update(cx, |d, cx| d.rerun(id, cx)).unwrap();
    assert_ne!(again, id);
    assert_eq!(setup.read(cx, |d| d.launches().len()), 1);
    setup.debugger.update(cx, |d, cx| d.stop(again, cx));
    setup.wait_for(cx, LaunchStatus::Finished(None));
}

#[gpui::test]
fn files_map_to_the_targets_that_run_them(cx: &mut TestAppContext) {
    let setup = setup(Program::default(), cx);
    let dir = tempfile::tempdir().unwrap();
    let go_dir = dir.path().join("cmd/server");
    std::fs::create_dir_all(&go_dir).unwrap();
    let mut rust = target(None, "");
    rust.source = Some(dir.path().join("src/main.rs"));
    let mut go = target(None, "");
    go.name = "./cmd/server".into();
    go.package_id = "go".into();
    go.source = Some(go_dir.clone());
    setup
        .debugger
        .update(cx, |d, cx| d.set_targets(vec![rust, go], cx));
    let name = |path: PathBuf, cx: &mut TestAppContext| {
        setup.read(cx, |d| d.target_for_file(&path).map(|t| t.name))
    };
    assert_eq!(name(dir.path().join("src/main.rs"), cx), Some("p".into()));
    assert_eq!(
        name(go_dir.join("main.go"), cx),
        Some("./cmd/server".into())
    );
    assert_eq!(name(dir.path().join("src/lib.rs"), cx), None);
    // A package folder only claims files in its own language.
    assert_eq!(name(go_dir.join("notes.rs"), cx), None);
    // A script runs itself.
    assert_eq!(name(dir.path().join("tool.ts"), cx), Some("tool.ts".into()));
}
