//! Tool calls through the whole stack: a real workspace with real buffers and editors, the real
//! debugger running small shell scripts, an in-process fake language server, and the real socket.

use super::*;
use gleipnir_core::{AdapterLauncher, BufferStore, Debugger, LanguageServers, Launcher};
use gleipnir_editor::EditorView;
use gleipnir_git_ui::DiffView;
use gleipnir_lsp::{LanguageServer, path_to_uri, read_message, write_message};
use gleipnir_packages::{RunTarget, RunTargetKind};
use gleipnir_test_support::{isolate_state, project};
use gpui::{Modifiers, TestAppContext, VisualTestContext, WeakEntity, px, size};
use serde_json::json;
use std::cell::RefCell;
use std::io::BufReader;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAIN: &str =
    "mod shapes;\nfn main() {\n    let a = shapes::area(2.0);\n    let b = shapes::area(3.0);\n}\n";
const SHAPES: &str = "pub fn area(r: f64) -> f64 {\n    r * r\n}\n";
const NOTES: &str = "one\ntwo\nthree\ntwo\n";

/// Sockets go in a short folder of their own for the whole run, as the system's temporary folder
/// can be too long for a socket's path, and the person's real one is left alone.
fn isolate_sockets() -> PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("gt")
            .tempdir_in("/tmp")
            .unwrap()
    });
    // SAFETY: set before any window starts listening, and never changed after.
    unsafe { std::env::set_var("GLEIPNIR_SOCKET_DIR", dir.path()) };
    dir.path().to_path_buf()
}

fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> serde_json::Value {
    json!({ "start": { "line": l1, "character": c1 }, "end": { "line": l2, "character": c2 } })
}

/// A language server that says every name is `area` in shapes.rs, used twice in main.rs, and
/// that each file it opens has a warning on its first word.
fn fake_language_server(root: PathBuf) -> Launcher {
    Rc::new(move |_, _, _| {
        let (client_read, server_write) = std::io::pipe()?;
        let (server_read, client_write) = std::io::pipe()?;
        let root = root.clone();
        std::thread::spawn(move || {
            let main = path_to_uri(&root.join("src/main.rs")).as_str().to_string();
            let shapes = path_to_uri(&root.join("src/shapes.rs"))
                .as_str()
                .to_string();
            let location = |uri: &str, l: u32, c1: u32, c2: u32| json!({ "uri": uri, "range": range(l, c1, l, c2) });
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            while let Ok(Some(message)) = read_message(&mut reader) {
                let Some(method) = message["method"].as_str() else {
                    continue;
                };
                if method == "textDocument/didOpen" {
                    let uri = message["params"]["textDocument"]["uri"].clone();
                    let diagnostics = json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
                        "uri": uri, "diagnostics": [
                            { "range": range(0, 0, 0, 3), "message": "unused", "severity": 2 },
                            { "range": range(1, 4, 1, 5), "message": "broken", "severity": 1, "source": "fake" },
                        ]
                    } });
                    write_message(&mut writer, &diagnostics).ok();
                }
                let result = match method {
                    "initialize" => json!({ "capabilities": {
                        "textDocumentSync": 2,
                        "definitionProvider": true,
                        "referencesProvider": true,
                    } }),
                    "textDocument/definition" => json!([location(&shapes, 0, 7, 11)]),
                    "textDocument/references" => {
                        json!([location(&main, 2, 20, 24), location(&main, 3, 20, 24)])
                    }
                    _ => serde_json::Value::Null,
                };
                if message.get("id").is_some() {
                    let reply = json!({ "jsonrpc": "2.0", "id": message["id"], "result": result });
                    write_message(&mut writer, &reply).ok();
                }
            }
        });
        Ok(LanguageServer::from_streams(
            "fake",
            client_read,
            client_write,
            None::<std::io::Empty>,
        ))
    })
}

/// Something to run: a shell script, as the project's configuration `name`.
fn script_target(
    root: &Path,
    name: &str,
    kind: RunTargetKind,
    source: &str,
    script: &str,
) -> RunTarget {
    RunTarget {
        package_id: "rust".into(),
        name: name.into(),
        kind,
        build: None,
        run: vec!["/bin/sh".into(), "-c".into(), script.into()],
        program: None,
        cwd: root.to_path_buf(),
        source: Some(root.join(source)),
        args: Vec::new(),
    }
}

/// Prints what Rust's test runner prints for two tests, one failing.
const TEST_SCRIPT: &str = "echo '     Running unittests src/calc.rs (target/debug/deps/p-0123abcd)' >&2; \
    printf 'running 2 tests\\ntest tests::adds ... ok\\ntest tests::fails ... FAILED\\n\\nfailures:\\n\\n\
    ---- tests::fails stdout ----\\nboom\\n\\nfailures:\\n    tests::fails\\n\\n\
    test result: FAILED. 1 passed; 1 failed; finished in 0.01s\\n'; exit 101";

struct Setup<'a> {
    workspace: Entity<Workspace>,
    debugger: Entity<Debugger>,
    cx: &'a mut VisualTestContext,
    root: PathBuf,
}

fn setup(cx: &mut TestAppContext) -> (tempfile::TempDir, Setup<'_>) {
    isolate_state();
    isolate_sockets();
    cx.executor().allow_parking();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_editor::bind_keys(cx);
        gleipnir_workspace::bind_keys(cx);
        gleipnir_git_ui::bind_keys(cx);
    });
    let (dir, root) = project(&[
        ("src/main.rs", MAIN),
        ("src/shapes.rs", SHAPES),
        (
            "src/calc.rs",
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        ),
        ("notes.txt", NOTES),
    ]);
    let store = cx.new(|_| BufferStore::new());
    let project_root = root.clone();
    let made = Rc::new(RefCell::new(None));
    let keep = made.clone();
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(store, Some(project_root.clone()), window, cx);
        let toolchains = workspace.toolchains().clone();
        let servers = cx.new(|cx| {
            LanguageServers::with_launcher(
                Some(project_root.clone()),
                toolchains.clone(),
                fake_language_server(project_root.clone()),
                cx,
            )
        });
        workspace.set_language_servers(servers);
        let adapters: AdapterLauncher = Rc::new(|_, _, _| anyhow::bail!("no debugger in tests"));
        let debugger = cx.new(|cx| {
            let mut debugger =
                Debugger::with_launcher(Some(project_root.clone()), toolchains, adapters, cx);
            debugger.set_targets(
                vec![
                    script_target(
                        &project_root,
                        "hello",
                        RunTargetKind::Binary,
                        "src/main.rs",
                        "echo hello from the program; exit 3",
                    ),
                    script_target(
                        &project_root,
                        "tests in calc",
                        RunTargetKind::Test,
                        "src/calc.rs",
                        TEST_SCRIPT,
                    ),
                ],
                cx,
            );
            debugger
        });
        *keep.borrow_mut() = Some(debugger.clone());
        // Registers open files with the language servers, as in the app.
        gleipnir_code_nav::add_to_workspace(&mut workspace, window, cx);
        gleipnir_debugger_ui::add_with_debugger(&mut workspace, debugger, window, cx);
        add_to_workspace(&mut workspace, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1400.), px(900.)));
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
    /// Runs the app, moving its clock on, until a condition holds.
    fn wait(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting");
            self.cx.executor().advance_clock(Duration::from_millis(100));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn agent(&mut self) -> WeakEntity<Agent> {
        let id = self.workspace.entity_id();
        self.cx
            .update(|_, cx| cx.global::<Agents>().0.get(&id).unwrap().downgrade())
    }

    /// Starts a tool call, as the socket's clients do, and returns where its answer will be.
    fn start_call(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Rc<RefCell<Option<Result<rpc::ToolOutput, String>>>> {
        let agent = self.agent();
        let (root, name) = (self.root.clone(), name.to_string());
        let slot = Rc::new(RefCell::new(None));
        let answer = slot.clone();
        let workspace = self.workspace.clone();
        self.cx.update(|window, cx| {
            workspace.update(cx, |_, cx| {
                cx.spawn_in(window, async move |workspace, cx| {
                    let outcome = ide::run(&workspace, &agent, &root, &name, &arguments, cx).await;
                    *answer.borrow_mut() = Some(outcome);
                })
                .detach()
            })
        });
        slot
    }

    fn finish(
        &mut self,
        slot: Rc<RefCell<Option<Result<rpc::ToolOutput, String>>>>,
    ) -> Result<Value, String> {
        self.wait(|_| slot.borrow().is_some());
        let outcome = slot.borrow_mut().take().unwrap();
        outcome.map(|output| match output {
            rpc::ToolOutput::Json(value) => value,
            rpc::ToolOutput::JsonAndText(mut value, text) => {
                value["text"] = json!(text);
                value
            }
        })
    }

    fn call(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let slot = self.start_call(name, arguments);
        self.finish(slot)
    }

    fn open(&mut self, relative: &str) -> Entity<EditorView> {
        let path = self.root.join(relative);
        let workspace = self.workspace.clone();
        let task = self
            .cx
            .update(|window, cx| workspace.update(cx, |w, cx| w.open_path(path, window, cx)));
        let slot = Rc::new(RefCell::new(None));
        let result = slot.clone();
        self.cx.update(|_, cx| {
            cx.spawn(async move |_| *result.borrow_mut() = Some(task.await))
                .detach()
        });
        self.wait(|_| slot.borrow().is_some());
        slot.borrow_mut().take().unwrap().unwrap()
    }

    fn text(&mut self, editor: &Entity<EditorView>) -> String {
        self.cx
            .update(|_, cx| editor.read(cx).buffer().read(cx).buffer().text())
    }

    fn dirty(&mut self, editor: &Entity<EditorView>) -> bool {
        self.cx
            .update(|_, cx| editor.read(cx).buffer().read(cx).buffer().is_dirty())
    }

    fn type_at_start(&mut self, editor: &Entity<EditorView>, text: &str) {
        let text = text.to_string();
        self.cx.update(|_, cx| {
            let buffer = editor.read(cx).buffer().clone();
            buffer.update(cx, |b, cx| {
                b.apply_external_edits(vec![gleipnir_text::TextEdit::new(0..0, text)], cx)
            })
        });
    }

    fn undo(&mut self) {
        self.cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-z"
        } else {
            "ctrl-z"
        });
        self.cx.run_until_parked();
    }

    fn tabs(&mut self) -> Vec<String> {
        let workspace = self.workspace.clone();
        self.cx.update(|window, cx| {
            let snapshot = workspace.read(cx).snapshot(window, cx);
            snapshot.panes[snapshot.active_pane].tabs.clone()
        })
    }

    fn active_file(&mut self) -> Option<PathBuf> {
        let workspace = self.workspace.clone();
        self.cx.update(|_, cx| workspace.read(cx).active_file(cx))
    }

    fn review(&mut self) -> Option<Entity<DiffView>> {
        let workspace = self.workspace.clone();
        self.cx
            .update(|_, cx| workspace.read(cx).active_modal::<DiffView>())
    }
}

#[gpui::test]
fn an_edit_lands_in_the_open_buffer_as_one_undo_step(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("notes.txt");
    // The person's own unsaved edit, which the agent's must keep.
    s.type_at_start(&editor, "zero\n");
    let answer = s
        .call(
            "edit_file",
            json!({ "path": "notes.txt", "old_text": "three\n", "new_text": "THREE\n" }),
        )
        .unwrap();
    assert_eq!(answer["replacements"], 1);
    assert_eq!(answer["line"], 4);
    assert_eq!(answer["saved"], false);
    assert_eq!(s.text(&editor), "zero\none\ntwo\nTHREE\ntwo\n");
    assert!(s.dirty(&editor));
    assert_eq!(
        std::fs::read_to_string(s.root.join("notes.txt")).unwrap(),
        NOTES
    );

    // read_file sees what the editor has, unsaved edits included.
    let read = s
        .call(
            "read_file",
            json!({ "path": "notes.txt", "start_line": 4, "end_line": 5 }),
        )
        .unwrap();
    assert_eq!(read["text"], "THREE\ntwo\n");
    assert_eq!(read["from"], "editor");
    assert_eq!(read["unsaved_edits"], true);
    assert_eq!(read["total_lines"], 5);

    // One undo takes the agent's edit back, and only that.
    s.undo();
    assert_eq!(s.text(&editor), "zero\none\ntwo\nthree\ntwo\n");

    // Every occurrence, when asked for, is still one step.
    let answer = s
        .call(
            "edit_file",
            json!({ "path": "notes.txt", "old_text": "two", "new_text": "2", "replace_all": true }),
        )
        .unwrap();
    assert_eq!(answer["replacements"], 2);
    assert_eq!(s.text(&editor), "zero\none\n2\nthree\n2\n");
    s.undo();
    assert_eq!(s.text(&editor), "zero\none\ntwo\nthree\ntwo\n");
}

#[gpui::test]
fn an_edit_that_is_missing_or_ambiguous_changes_nothing(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("notes.txt");
    let ambiguous = s
        .call(
            "edit_file",
            json!({ "path": "notes.txt", "old_text": "two", "new_text": "2" }),
        )
        .unwrap_err();
    assert!(ambiguous.contains("2 times, on lines 2, 4"), "{ambiguous}");
    let missing = s
        .call(
            "edit_file",
            json!({ "path": "notes.txt", "old_text": "four", "new_text": "4" }),
        )
        .unwrap_err();
    assert!(missing.contains("wasn't found"), "{missing}");
    let spacing = s
        .call(
            "edit_file",
            json!({ "path": "notes.txt", "old_text": "one  two", "new_text": "x" }),
        )
        .unwrap_err();
    assert!(spacing.contains("different spacing"), "{spacing}");
    assert_eq!(s.text(&editor), NOTES);
    assert!(!s.dirty(&editor));
}

#[gpui::test]
fn a_closed_file_is_edited_in_a_tab_beside_and_saved_on_request(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/main.rs");
    s.call(
        "edit_file",
        json!({ "path": "src/calc.rs", "old_text": "a + b", "new_text": "b + a" }),
    )
    .unwrap();
    // A tab shows it, but the person stays where they were.
    assert_eq!(s.tabs(), ["main.rs", "calc.rs"]);
    assert_eq!(s.active_file(), Some(s.root.join("src/main.rs")));
    let calc = s.root.join("src/calc.rs");
    assert!(std::fs::read_to_string(&calc).unwrap().contains("a + b"));
    let open = s.call("get_open_files", json!({})).unwrap();
    assert_eq!(
        open["files"],
        json!([
            { "path": "src/main.rs", "active": true, "dirty": false, "language": "Rust" },
            { "path": "src/calc.rs", "active": false, "dirty": true, "language": "Rust" },
        ])
    );
    assert_eq!(open["active"], "src/main.rs");

    let saved = s
        .call("save_file", json!({ "path": "src/calc.rs" }))
        .unwrap();
    assert_eq!(saved["saved"], true);
    s.wait(|_| std::fs::read_to_string(&calc).unwrap().contains("b + a"));
    let not_open = s.call("save_file", json!({ "path": "notes.txt" })).unwrap();
    assert_eq!(not_open["saved"], false);
}

#[gpui::test]
fn paths_outside_the_project_are_refused(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "keep out").unwrap();
    for (tool, path) in [
        ("read_file", "../secret.txt".to_string()),
        ("read_file", secret.display().to_string()),
        ("open_file", "/etc/hosts".to_string()),
        ("save_file", "../../x".to_string()),
    ] {
        let error = s.call(tool, json!({ "path": path })).unwrap_err();
        assert!(
            error.contains("outside the project"),
            "{tool} {path}: {error}"
        );
    }
    let error = s
        .call(
            "edit_file",
            json!({ "path": secret.display().to_string(), "old_text": "keep", "new_text": "come" }),
        )
        .unwrap_err();
    assert!(error.contains("outside the project"), "{error}");
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "keep out");
    assert!(s.tabs().is_empty());
}

#[gpui::test]
fn files_not_open_are_read_from_disk(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let read = s
        .call("read_file", json!({ "path": "notes.txt", "start_line": 2 }))
        .unwrap();
    assert_eq!(read["text"], "two\nthree\ntwo\n");
    assert_eq!(read["from"], "disk");
    assert_eq!(read["start_line"], 2);
    assert_eq!(read["end_line"], 4);
    let error = s
        .call("read_file", json!({ "path": "notes.txt", "start_line": 9 }))
        .unwrap_err();
    assert!(error.contains("has 4 line(s)"), "{error}");
    // Reading opens nothing.
    assert!(s.tabs().is_empty());
}

#[gpui::test]
fn the_selection_and_cursor_are_what_the_person_has(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let none = s.call("get_selection", json!({})).unwrap();
    assert_eq!(none["path"], Value::Null);

    let opened = s
        .call(
            "open_file",
            json!({ "path": "src/main.rs", "line": 3, "column": 13, "end_line": 3, "end_column": 27 }),
        )
        .unwrap();
    assert_eq!(opened["selected"], "shapes::area(2");
    let selection = s.call("get_selection", json!({})).unwrap();
    assert_eq!(selection["path"], "src/main.rs");
    assert_eq!(selection["selection"]["text"], "shapes::area(2");
    assert_eq!(
        selection["selection"]["start"],
        json!({ "line": 3, "column": 13 })
    );
    assert_eq!(
        selection["selection"]["end"],
        json!({ "line": 3, "column": 27 })
    );

    // With nothing selected, the cursor's line.
    s.call(
        "open_file",
        json!({ "path": "src/main.rs", "line": 2, "column": 4 }),
    )
    .unwrap();
    let cursor = s.call("get_selection", json!({})).unwrap();
    assert_eq!(cursor["selection"], Value::Null);
    assert_eq!(cursor["cursor"], json!({ "line": 2, "column": 4 }));
    assert_eq!(
        cursor["current_line"],
        json!({ "line": 2, "text": "fn main() {" })
    );

    let error = s
        .call("open_file", json!({ "path": "src/main.rs", "line": 40 }))
        .unwrap_err();
    assert!(error.contains("there's no line 40"), "{error}");
}

#[gpui::test]
fn problems_definitions_and_usages_come_from_the_language_server(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let definition = s
        .call(
            "find_definition",
            json!({ "path": "src/main.rs", "line": 3, "column": 22 }),
        )
        .unwrap();
    assert_eq!(
        definition["definitions"],
        json!([{ "path": "src/shapes.rs", "line": 1, "column": 8, "text": "pub fn area(r: f64) -> f64 {" }])
    );
    // By name, the project's own index finds where it's declared, and the server its usages.
    let workspace = s.workspace.clone();
    s.wait(|s| {
        s.cx.update(|_, cx| {
            workspace
                .read(cx)
                .project()
                .is_some_and(|p| !p.read(cx).is_busy())
        })
    });
    let declared = s
        .call("find_definition", json!({ "symbol": "area" }))
        .unwrap();
    assert_eq!(declared["definitions"][0]["path"], "src/shapes.rs");
    assert_eq!(declared["definitions"][0]["line"], 1);
    let usages = s.call("find_usages", json!({ "symbol": "area" })).unwrap();
    assert_eq!(usages["count"], 2);
    assert_eq!(
        usages["usages"][1],
        json!({ "path": "src/main.rs", "line": 4, "column": 21, "text": "let b = shapes::area(3.0);" })
    );
    let unknown = s
        .call("find_usages", json!({ "symbol": "nowhere" }))
        .unwrap_err();
    assert!(unknown.contains("Nothing named nowhere"), "{unknown}");

    // Opening the files told the server about them, which found problems in each.
    s.open("src/main.rs");
    s.wait(|s| {
        s.call("get_problems", json!({ "path": "src/main.rs" }))
            .unwrap()["errors"]
            == 1
    });
    let problems = s
        .call("get_problems", json!({ "path": "src/main.rs" }))
        .unwrap();
    assert_eq!(
        problems["problems"][0],
        json!({ "path": "src/main.rs", "line": 2, "column": 5, "end_line": 2, "end_column": 6,
                "severity": "error", "message": "broken", "source": "fake" })
    );
    assert_eq!(problems["warnings"], 1);
    let errors = s
        .call("get_problems", json!({ "errors_only": true }))
        .unwrap();
    let listed = errors["problems"].as_array().unwrap();
    assert!(
        listed.iter().all(|p| p["severity"] == "error"),
        "{listed:?}"
    );
    assert!(listed.len() >= 2, "both files have an error: {listed:?}");
}

#[gpui::test]
fn tests_and_configurations_run_and_report(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let none = s.call("get_test_results", json!({})).unwrap();
    assert_eq!(none["result"], "none");

    let run = s
        .call(
            "run_tests",
            json!({ "path": "src/calc.rs", "wait_seconds": 30 }),
        )
        .unwrap();
    assert_eq!(run["configuration"], "tests in calc");
    assert_eq!(run["finished"], true);
    assert_eq!(run["result"], "failed");
    assert_eq!(run["counts"]["passed"], 1);
    assert_eq!(run["counts"]["failed"], 1);
    assert_eq!(run["failures"][0]["name"], "tests::fails");
    assert!(
        run["failures"][0]["output"]
            .as_str()
            .unwrap()
            .contains("boom")
    );
    let latest = s.call("get_test_results", json!({})).unwrap();
    assert_eq!(latest["failures"], run["failures"]);

    let listed = s.call("run_configuration", json!({})).unwrap();
    assert_eq!(listed["configurations"][0]["name"], "hello");
    let ran = s
        .call(
            "run_configuration",
            json!({ "name": "hello", "wait_seconds": 30 }),
        )
        .unwrap();
    assert_eq!(ran["exit_code"], 3);
    assert!(
        ran["output"]
            .as_str()
            .unwrap()
            .contains("hello from the program")
    );
    let missing = s
        .call("run_configuration", json!({ "name": "nope" }))
        .unwrap_err();
    assert!(missing.contains("there are: hello"), "{missing}");
}

#[gpui::test]
fn tests_are_found_by_name_line_or_file(cx: &mut TestAppContext) {
    let (_dir, s) = setup(cx);
    let lib = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {}\n\n    #[test]\n    fn fails() {}\n}\n";
    std::fs::write(s.root.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    std::fs::write(s.root.join("src/lib.rs"), lib).unwrap();
    let path = tools::resolve_path(&s.root, "src/lib.rs").unwrap();
    let debugger = s.debugger.clone();
    let mut find = |choice: tools::TestChoice| {
        s.cx.update(|_, cx| ide::find_tests(debugger.read(cx), &path, lib, &choice))
    };
    let named = find(tools::TestChoice::Named("fails".into())).unwrap();
    assert_eq!(named.args, ["--exact", "tests::fails"]);
    let full = find(tools::TestChoice::Named("tests::adds".into())).unwrap();
    assert_eq!(full.args, ["--exact", "tests::adds"]);
    let line = find(tools::TestChoice::Line(4)).unwrap();
    assert_eq!(line.single_test(), Some("adds"));
    let file = find(tools::TestChoice::File).unwrap();
    assert_eq!(file.kind, RunTargetKind::Test);
    assert_eq!(file.single_test(), None);
    let unknown = find(tools::TestChoice::Named("nope".into())).unwrap_err();
    assert!(unknown.contains("Its tests are: adds, fails"), "{unknown}");
    let not_a_test = find(tools::TestChoice::Line(1)).unwrap_err();
    assert!(
        not_a_test.contains("tests are declared on lines"),
        "{not_a_test}"
    );
}

#[gpui::test]
fn git_status_lists_the_branch_and_changes(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let git = |args: &[&str]| gleipnir_index::git_ops::git(&s.root, args).unwrap();
    git(&["init", "-q", "-b", "trunk"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "user.email", "t@t"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "first"]);
    std::fs::write(s.root.join("notes.txt"), "changed\n").unwrap();
    std::fs::write(s.root.join("new.txt"), "new\n").unwrap();
    let status = s.call("get_git_status", json!({})).unwrap();
    assert_eq!(status["branch"], "trunk");
    assert_eq!(status["changed_files"], 2);
    let changes = status["changes"].as_array().unwrap();
    assert!(
        changes.contains(&json!({ "path": "notes.txt", "status": "modified" })),
        "{changes:?}"
    );
    assert!(
        changes.contains(&json!({ "path": "new.txt", "status": "untracked" })),
        "{changes:?}"
    );
}

#[gpui::test]
fn a_proposed_edit_is_accepted_in_part(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("notes.txt");
    let call = s.start_call(
        "propose_edit",
        json!({ "path": "notes.txt", "old_text": "two", "new_text": "2", "replace_all": true }),
    );
    s.wait(|s| s.review().is_some());
    let view = s.review().unwrap();
    assert!(s.cx.update(|_, cx| view.read(cx).is_reviewing()));
    // Nothing changes until the person decides.
    assert_eq!(s.text(&editor), NOTES);
    let rows = s.cx.update(|_, cx| view.read(cx).change_rows());
    assert_eq!(rows.len(), 2);
    // Accept the first by its button, reject the second.
    s.cx.run_until_parked();
    let accept =
        s.cx.debug_bounds(Box::leak(
            format!("review-accept-{}", rows[0]).into_boxed_str(),
        ))
        .expect("the first change's accept button");
    s.cx.simulate_click(accept.center(), Modifiers::none());
    assert_eq!(s.text(&editor), "one\n2\nthree\ntwo\n");
    let rows = s.cx.update(|_, cx| view.read(cx).change_rows());
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.reject_change(rows[0], cx)));

    let answer = s.finish(call).unwrap();
    assert_eq!(answer["status"], "partly accepted");
    assert_eq!(answer["accepted"], 1);
    assert_eq!(answer["rejected"], 1);
    assert_eq!(
        answer["changes"],
        json!([
            { "decision": "accepted", "line": 2, "removed": "two\n", "added": "2\n" },
            { "decision": "rejected", "line": 4, "removed": "two\n", "added": "2\n" },
        ])
    );
    assert!(
        s.review().is_none(),
        "the review closes once all is decided"
    );
    assert!(s.dirty(&editor));
    s.undo();
    assert_eq!(s.text(&editor), NOTES);
}

#[gpui::test]
fn a_review_follows_the_file_changing_underneath_it(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("notes.txt");
    let call = s.start_call(
        "propose_edit",
        json!({ "path": "notes.txt", "old_text": "three", "new_text": "3" }),
    );
    s.wait(|s| s.review().is_some());
    let view = s.review().unwrap();
    // Another edit lands while the person looks, as from a second agent.
    s.type_at_start(&editor, "zero\n");
    let row = s.cx.update(|_, cx| view.read(cx).change_rows()[0]);
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.accept_change(row, cx)));
    // Nothing's taken from a diff of text that's gone: the proposal is carried over to the file
    // as it is now, without undoing the other edit, and shown again.
    assert_eq!(s.text(&editor), "zero\none\ntwo\nthree\ntwo\n");
    assert!(s.cx.update(|_, cx| view.read(cx).decisions().is_empty()));
    let rows = s.cx.update(|_, cx| view.read(cx).change_rows());
    assert_eq!(rows.len(), 1, "only the proposal's change");
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.accept_change(rows[0], cx)));
    assert_eq!(s.text(&editor), "zero\none\ntwo\n3\ntwo\n");
    let answer = s.finish(call).unwrap();
    assert_eq!(answer["status"], "accepted");
    assert_eq!(answer["changes"][0]["line"], 4);
    assert_eq!(answer["changes"][0]["added"], "3\n");

    // A change the other edit clashes with is left out.
    let call = s.start_call(
        "propose_edit",
        json!({ "path": "notes.txt", "old_text": "one\n", "new_text": "1\n" }),
    );
    s.wait(|s| s.review().is_some());
    let view = s.review().unwrap();
    s.cx.update(|_, cx| {
        let buffer = editor.read(cx).buffer().clone();
        buffer.update(cx, |b, cx| {
            b.apply_external_edits(vec![gleipnir_text::TextEdit::new(5..8, "uno")], cx)
        })
    });
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.accept_all(cx)));
    let answer = s.finish(call).unwrap();
    assert_eq!(answer["status"], "rejected");
    assert_eq!(answer["undecided"], 1);
    assert_eq!(s.text(&editor), "zero\nuno\ntwo\n3\ntwo\n");
}

#[gpui::test]
fn a_proposal_still_being_reviewed_is_answered_later(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let pending = s
        .call(
            "propose_edit",
            json!({ "path": "src/calc.rs", "old_text": "a + b", "new_text": "a.wrapping_add(b)", "wait_seconds": 0 }),
        )
        .unwrap();
    assert_eq!(pending["status"], "pending");
    let id = pending["id"].as_u64().unwrap();
    s.wait(|s| s.review().is_some());
    let still = s
        .call("get_proposal", json!({ "id": id, "wait_seconds": 0 }))
        .unwrap();
    assert_eq!(still["status"], "pending");

    let asked = s.start_call("get_proposal", json!({ "id": id, "wait_seconds": 30 }));
    let view = s.review().unwrap();
    s.cx.update(|_, cx| view.update(cx, |v, cx| v.accept_all(cx)));
    let answer = s.finish(asked).unwrap();
    assert_eq!(answer["status"], "accepted");
    assert_eq!(answer["saved"], false);
    let read = s
        .call(
            "read_file",
            json!({ "path": "src/calc.rs", "start_line": 2, "end_line": 2 }),
        )
        .unwrap();
    assert_eq!(read["text"], "    a.wrapping_add(b)\n");

    // Closing a review leaves out what wasn't decided.
    let closed = s.start_call(
        "propose_edit",
        json!({ "path": "notes.txt", "old_text": "one", "new_text": "1" }),
    );
    s.wait(|s| s.review().is_some());
    s.cx.simulate_keystrokes("escape");
    let answer = s.finish(closed).unwrap();
    assert_eq!(answer["status"], "rejected");
    assert_eq!(answer["undecided"], 1);
    assert_eq!(
        std::fs::read_to_string(s.root.join("notes.txt")).unwrap(),
        NOTES
    );
    let unknown = s.call("get_proposal", json!({ "id": 99 })).unwrap_err();
    assert!(unknown.contains("no proposal with id 99"), "{unknown}");
}

/// Runs `gleipnir mcp`'s pipe on a thread, with these lines as its input, until it ends.
fn pipe_lines(socket: PathBuf, lines: Vec<String>) -> Arc<Mutex<Option<Vec<Value>>>> {
    let output = Arc::new(Mutex::new(None));
    let done = output.clone();
    std::thread::spawn(move || {
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        let input = std::io::Cursor::new(lines.join("\n") + "\n");
        let mut out = Vec::new();
        pipe(stream, input, &mut out).unwrap();
        let answers = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        *done.lock().unwrap() = Some(answers);
    });
    output
}

#[gpui::test]
fn agents_reach_the_window_through_its_socket(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/main.rs");
    let workspace = s.workspace.clone();
    let socket =
        s.cx.update(|_, cx| socket_path(workspace.read(cx), cx))
            .expect("the window listens");
    assert!(socket.starts_with(socket_dir()));
    let config = mcp_config_json(Path::new("/Applications/Gleipnir"), &socket);
    assert_eq!(
        config["mcpServers"]["gleipnir"],
        json!({ "command": "/Applications/Gleipnir", "args": ["mcp"], "env": { "GLEIPNIR_SOCKET": socket.to_string_lossy() } })
    );

    let lines = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_open_files","arguments":{}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"../etc"}}}"#.to_string(),
    ];
    // Two agents at once.
    let first = pipe_lines(socket.clone(), lines.clone());
    let second = pipe_lines(socket.clone(), lines);
    s.wait(|_| first.lock().unwrap().is_some() && second.lock().unwrap().is_some());
    for output in [first, second] {
        let mut answers = output.lock().unwrap().take().unwrap();
        answers.sort_by_key(|a| a["id"].as_u64());
        assert_eq!(answers.len(), 4, "{answers:?}");
        assert_eq!(answers[0]["result"]["protocolVersion"], "2025-06-18");
        assert!(answers[1]["result"]["tools"].as_array().unwrap().len() >= 15);
        let files: Value =
            serde_json::from_str(answers[2]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(files["active"], "src/main.rs");
        assert_eq!(answers[3]["result"]["isError"], true);
    }

    // The socket goes with the window.
    let Setup { workspace, cx, .. } = s;
    drop(workspace);
    cx.update(|window, _| window.remove_window());
    let executor = cx.executor();
    let deadline = Instant::now() + Duration::from_secs(10);
    while socket.exists() {
        assert!(Instant::now() < deadline, "the socket outlived its window");
        executor.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}
