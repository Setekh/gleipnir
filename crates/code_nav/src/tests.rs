//! Code insight through the whole stack: keys in a real workspace, a real client, and an
//! in-process fake server with fixed answers about a two-file Rust project.

use super::*;
use gleipnir_core::{BufferStore, Launcher};
use gleipnir_lsp::{LanguageServer, path_to_uri, read_message, write_message};
use gleipnir_test_support::{isolate_state, project, run_until};
use gpui::Focusable as _;
use gpui::{Modifiers, TestAppContext, VisualTestContext, size};
use serde_json::{Value, json};
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};

const MAIN: &str =
    "mod shapes;\nfn main() {\n    let a = shapes::area(2.0);\n    let b = shapes::area(3.0);\n}\n";
const SHAPES: &str = "pub fn area(r: f64) -> f64 {\n    r * r\n}\n";

struct Keys;
#[cfg(target_os = "macos")]
impl Keys {
    const DECLARATION: &str = "cmd-b";
    const IMPLEMENTATION: &str = "cmd-alt-b";
    const BACK: &str = "cmd-[";
    const FORWARD: &str = "cmd-]";
    const STRUCTURE: &str = "cmd-f12";
    const REFORMAT: &str = "cmd-alt-l";
    const PROBLEMS: &str = "cmd-6";
}
#[cfg(not(target_os = "macos"))]
impl Keys {
    const DECLARATION: &str = "ctrl-b";
    const IMPLEMENTATION: &str = "ctrl-alt-b";
    const BACK: &str = "ctrl-alt-left";
    const FORWARD: &str = "ctrl-alt-right";
    const STRUCTURE: &str = "ctrl-f12";
    const REFORMAT: &str = "ctrl-alt-l";
    const PROBLEMS: &str = "alt-6";
}

/// What the fake was asked, for tests to check.
#[derive(Default)]
struct Log {
    methods: Vec<String>,
    /// Our answers to its requests, by id.
    responses: Vec<Value>,
}

fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> Value {
    json!({ "start": { "line": l1, "character": c1 }, "end": { "line": l2, "character": c2 } })
}

fn edit(l1: u32, c1: u32, l2: u32, c2: u32, text: &str) -> Value {
    json!({ "range": range(l1, c1, l2, c2), "newText": text })
}

/// Answers a request about the project at `root`.
fn answer(root: &Path, method: &str, params: &Value) -> Value {
    let main = path_to_uri(&root.join("src/main.rs")).as_str().to_string();
    let shapes = path_to_uri(&root.join("src/shapes.rs"))
        .as_str()
        .to_string();
    let location =
        |uri: &str, l: u32, c1: u32, c2: u32| json!({ "uri": uri, "range": range(l, c1, l, c2) });
    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
    match method {
        "initialize" => json!({ "capabilities": {
            "textDocumentSync": 2,
            "definitionProvider": true,
            "implementationProvider": true,
            "referencesProvider": true,
            "renameProvider": true,
            "codeActionProvider": true,
            "documentFormattingProvider": true,
            "documentSymbolProvider": true,
            "hoverProvider": true,
            "completionProvider": { "triggerCharacters": [":"], "resolveProvider": true },
            "executeCommandProvider": { "commands": ["fix"] },
        } }),
        // Every name resolves to `area`'s declaration.
        "textDocument/definition" => json!([location(&shapes, 0, 7, 11)]),
        "textDocument/implementation" => {
            json!([location(&main, 2, 20, 24), location(&main, 3, 20, 24)])
        }
        "textDocument/references" => {
            json!([location(&main, 2, 20, 24), location(&main, 3, 20, 24)])
        }
        "textDocument/rename" => {
            let name = params["newName"].as_str().unwrap();
            json!({ "changes": {
                shapes.clone(): [edit(0, 7, 0, 11, name)],
                main.clone(): [edit(2, 20, 2, 24, name), edit(3, 20, 3, 24, name)],
            } })
        }
        "textDocument/codeAction" => json!([
            { "title": "Run the fix command", "command": { "title": "fix", "command": "fix" } },
            { "title": "Add a comment", "kind": "quickfix", "isPreferred": true,
              "edit": { "changes": { uri: [edit(0, 0, 0, 0, "// fixed\n")] } } },
        ]),
        "textDocument/formatting" => json!([edit(1, 0, 1, 4, "  ")]),
        "textDocument/documentSymbol" => json!([
            { "name": "area", "kind": 12, "range": range(0, 0, 2, 1), "selectionRange": range(0, 7, 0, 11), "detail": "fn(f64) -> f64",
              "children": [{ "name": "r", "kind": 13, "range": range(0, 12, 0, 13), "selectionRange": range(0, 12, 0, 13) }] }
        ]),
        "textDocument/hover" => {
            json!({ "contents": { "kind": "markdown", "value": "```rust\nfn area(r: f64) -> f64\n```" } })
        }
        "textDocument/completion" => json!({ "isIncomplete": false, "items": [
            { "label": "area", "kind": 3, "insertTextFormat": 2, "insertText": "area($0)", "data": 1 },
            { "label": "perimeter", "kind": 3 },
        ] }),
        "completionItem/resolve" => {
            let mut item = params.clone();
            item["additionalTextEdits"] = json!([edit(0, 0, 0, 0, "use shapes::area;\n")]);
            item
        }
        _ => Value::Null,
    }
}

fn fake_launcher(root: PathBuf, log: Arc<Mutex<Log>>) -> Launcher {
    Rc::new(move |_, _, _| {
        let (client_read, server_write) = std::io::pipe()?;
        let (server_read, client_write) = std::io::pipe()?;
        let (root, log) = (root.clone(), log.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            while let Ok(Some(message)) = read_message(&mut reader) {
                let Some(method) = message["method"].as_str() else {
                    log.lock().unwrap().responses.push(message.clone());
                    continue;
                };
                log.lock().unwrap().methods.push(method.to_string());
                let params = &message["params"];
                if method == "textDocument/didOpen" {
                    let uri = params["textDocument"]["uri"].clone();
                    let diagnostics = json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
                        "uri": uri, "diagnostics": [{ "range": range(0, 0, 0, 3), "message": "unused", "severity": 2 }]
                    } });
                    write_message(&mut writer, &diagnostics).ok();
                }
                if method == "workspace/executeCommand" {
                    // The command's work comes back as a request for us to apply an edit.
                    let main = path_to_uri(&root.join("src/main.rs")).as_str().to_string();
                    let request = json!({ "jsonrpc": "2.0", "id": 99, "method": "workspace/applyEdit", "params": {
                        "edit": { "changes": { main: [edit(0, 0, 0, 0, "// commanded\n")] } }
                    } });
                    write_message(&mut writer, &request).ok();
                }
                if message.get("id").is_some() {
                    let reply = json!({ "jsonrpc": "2.0", "id": message["id"], "result": answer(&root, method, params) });
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

struct Setup<'a> {
    workspace: Entity<Workspace>,
    cx: &'a mut VisualTestContext,
    root: PathBuf,
    log: Arc<Mutex<Log>>,
}

fn setup(cx: &mut TestAppContext) -> (tempfile::TempDir, Setup<'_>) {
    isolate_state();
    cx.executor().allow_parking();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_editor::bind_keys(cx);
        gleipnir_workspace::bind_keys(cx);
        bind_keys(cx);
    });
    let (dir, root) = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("src/main.rs", MAIN),
        ("src/shapes.rs", SHAPES),
    ]);
    let log = Arc::new(Mutex::new(Log::default()));
    let store = cx.new(|_| BufferStore::new());
    let (project_root, launcher) = (root.clone(), fake_launcher(root.clone(), log.clone()));
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(store, Some(project_root.clone()), window, cx);
        let toolchains = workspace.toolchains().clone();
        let servers = cx
            .new(|cx| LanguageServers::with_launcher(Some(project_root), toolchains, launcher, cx));
        workspace.set_language_servers(servers);
        add_to_workspace(&mut workspace, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1200.), px(800.)));
    (
        dir,
        Setup {
            workspace,
            cx,
            root,
            log,
        },
    )
}

impl Setup<'_> {
    /// Opens a file with the cursor at a line and column, once its server is running.
    fn open(&mut self, relative: &str, line: usize, column: usize) -> Entity<EditorView> {
        let path = self.root.join(relative);
        let workspace = self.workspace.clone();
        let task = self.cx.update(|window, cx| {
            workspace.update(cx, |w, cx| {
                w.open_path_at(path, Some((line, column)), window, cx)
            })
        });
        let editor = gleipnir_test_support::resolve(self.cx, task).unwrap();
        let servers = self
            .cx
            .update(|_, cx| workspace.read(cx).language_servers().clone());
        run_until(self.cx, |cx| {
            cx.update(|_, cx| {
                matches!(
                    servers.read(cx).status("rust"),
                    Some(ServerStatus::Running { .. })
                )
            })
        });
        editor
    }

    fn active(&mut self) -> Option<(String, usize, usize)> {
        let workspace = self.workspace.clone();
        let root = self.root.clone();
        self.cx.update(|_, cx| {
            let editor = workspace.read(cx).active_editor(cx)?;
            let view = editor.read(cx);
            let model = view.buffer().read(cx);
            let point = model
                .buffer()
                .offset_to_point(view.selections().primary().head);
            let path = model
                .path()?
                .strip_prefix(&root)
                .ok()?
                .display()
                .to_string();
            Some((path, point.line, point.col))
        })
    }

    /// Waits for the cursor to land in a file, as jumps finish asynchronously.
    fn wait_for(&mut self, expected: (&str, usize, usize)) {
        let expected = (expected.0.to_string(), expected.1, expected.2);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            let active = self.active();
            if active.as_ref() == Some(&expected) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected {expected:?}, at {active:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn picker(&mut self) -> Option<Entity<Picker>> {
        let workspace = self.workspace.clone();
        self.cx
            .update(|_, cx| workspace.read(cx).active_modal::<Picker>())
    }

    fn wait_for_picker(&mut self) -> (String, Vec<String>) {
        run_until(self.cx, |cx| {
            let workspace = self.workspace.clone();
            cx.update(|_, cx| workspace.read(cx).active_modal::<Picker>().is_some())
        });
        let picker = self.picker().unwrap();
        self.cx.update(|_, cx| {
            (
                picker.read(cx).title().to_string(),
                picker.read(cx).labels(),
            )
        })
    }

    fn text(&mut self, editor: &Entity<EditorView>) -> String {
        self.cx
            .update(|_, cx| editor.read(cx).buffer().read(cx).buffer().text())
    }

    fn panel_state(&mut self, id: &str) -> Value {
        let workspace = self.workspace.clone();
        self.cx
            .update(|window, cx| workspace.read(cx).snapshot(window, cx).panels[id].clone())
    }

    fn message(&mut self) -> Option<String> {
        let workspace = self.workspace.clone();
        self.cx
            .update(|window, cx| workspace.read(cx).snapshot(window, cx).message)
    }
}

#[gpui::test]
fn go_to_declaration_opens_the_other_file_and_back_returns(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/main.rs", 2, 21);
    s.cx.simulate_keystrokes(Keys::DECLARATION);
    s.wait_for(("src/shapes.rs", 0, 7));
    s.cx.simulate_keystrokes(Keys::BACK);
    s.wait_for(("src/main.rs", 2, 21));
    s.cx.simulate_keystrokes(Keys::FORWARD);
    s.wait_for(("src/shapes.rs", 0, 7));
}

#[gpui::test]
fn a_secondary_click_goes_to_the_declaration(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("src/main.rs", 2, 21);
    s.cx.update(|_, cx| {
        editor.update(cx, |_, cx| {
            cx.emit(EditorEvent::GoToDefinition { offset: 0 })
        })
    });
    s.wait_for(("src/shapes.rs", 0, 7));
}

#[gpui::test]
fn go_to_declaration_on_the_declaration_shows_its_usages(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/shapes.rs", 0, 8);
    s.cx.simulate_keystrokes(Keys::DECLARATION);
    let (title, labels) = s.wait_for_picker();
    assert_eq!(title, "Usages of area (2)");
    assert_eq!(
        labels,
        ["let a = shapes::area(2.0);", "let b = shapes::area(3.0);"]
    );
    s.cx.simulate_keystrokes("down enter");
    s.wait_for(("src/main.rs", 3, 20));
}

#[gpui::test]
fn several_implementations_offer_a_choice(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/shapes.rs", 0, 8);
    s.cx.simulate_keystrokes(Keys::IMPLEMENTATION);
    let (title, labels) = s.wait_for_picker();
    assert_eq!(title, "Choose Implementation");
    assert_eq!(labels.len(), 2);
    // It pops up just below the caret, not at the top of the window.
    s.cx.run_until_parked();
    let workspace = s.workspace.clone();
    let editor =
        s.cx.update(|_, cx| workspace.read(cx).active_editor(cx))
            .unwrap();
    let caret =
        s.cx.update(|_, cx| editor.read(cx).caret_bounds(cx))
            .expect("the caret is in view");
    let row = s.cx.debug_bounds("picker-row-0").expect("a row");
    assert!(
        row.top() > caret.bottom() && row.top() < caret.bottom() + px(80.),
        "the first row is at {:?}, the caret at {caret:?}",
        row.top()
    );
    assert!((row.left() - caret.left()).abs() < px(40.));
    s.cx.simulate_keystrokes("enter");
    s.wait_for(("src/main.rs", 2, 20));
}

#[gpui::test]
fn find_usages_fills_the_find_panel_grouped_by_file(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/shapes.rs", 0, 8);
    s.cx.simulate_keystrokes("alt-f7");
    run_until(s.cx, |_| true);
    let mut state = s.panel_state(USAGES_PANEL);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while state["searching"] == json!(true) && std::time::Instant::now() < deadline {
        run_until(s.cx, |_| true);
        state = s.panel_state(USAGES_PANEL);
    }
    assert_eq!(state["title"], "Usages of area");
    assert_eq!(
        state["usages"],
        json!([
            "src/main.rs:3: let a = shapes::area(2.0);",
            "src/main.rs:4: let b = shapes::area(3.0);"
        ])
    );
    let workspace = s.workspace.clone();
    let docks =
        s.cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).docks);
    assert!(
        docks
            .iter()
            .any(|d| d.visible && d.panels.iter().any(|p| p == USAGES_PANEL))
    );

    // Moving through the results shows each in the editor, and the list keeps the keyboard.
    let panel =
        s.cx.update(|_, cx| workspace.read(cx).panel::<UsagesPanel>(cx))
            .unwrap();
    s.cx.update(|window, cx| {
        let focus = panel.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    });
    // The list starts on the first usage.
    s.cx.simulate_keystrokes("down");
    run_until(s.cx, |_| true);
    assert_eq!(s.active(), Some(("src/main.rs".into(), 3, 20)));
    s.cx.simulate_keystrokes("up");
    run_until(s.cx, |_| true);
    assert_eq!(s.active(), Some(("src/main.rs".into(), 2, 20)));
    let focused =
        s.cx.update(|window, cx| panel.read(cx).focus_handle(cx).is_focused(window));
    assert!(focused, "the list keeps the keyboard");
}

#[gpui::test]
fn rename_edits_open_files_and_saves_the_rest(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("src/shapes.rs", 0, 8);
    s.cx.simulate_keystrokes("shift-f6");
    let workspace = s.workspace.clone();
    let modal =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<RenameModal>())
            .expect("the rename popup");
    s.cx.update(|_, cx| modal.update(cx, |m, cx| m.set_name("surface", cx)));
    s.cx.simulate_keystrokes("enter");
    let main = s.root.join("src/main.rs");
    run_until(s.cx, |_| {
        std::fs::read_to_string(&main).unwrap().contains("surface")
    });
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "mod shapes;\nfn main() {\n    let a = shapes::surface(2.0);\n    let b = shapes::surface(3.0);\n}\n"
    );
    assert!(s.text(&editor).starts_with("pub fn surface(r: f64)"));
    let dirty =
        s.cx.update(|_, cx| editor.read(cx).buffer().read(cx).buffer().is_dirty());
    assert!(dirty, "the open file is left for the user to save");
    assert_eq!(
        s.message().as_deref(),
        Some("Renamed area to surface in 2 files")
    );

    // One undo, in the open file, takes back the whole rename, saving the file that wasn't open.
    let undo = if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    };
    s.cx.simulate_keystrokes(undo);
    run_until(s.cx, |_| {
        std::fs::read_to_string(&main).unwrap().contains("area")
    });
    assert_eq!(std::fs::read_to_string(&main).unwrap(), MAIN);
    assert_eq!(s.text(&editor), SHAPES);
}

#[gpui::test]
fn intentions_apply_edits_and_run_commands(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("src/main.rs", 2, 21);
    s.cx.simulate_keystrokes("alt-enter");
    let (_, labels) = s.wait_for_picker();
    assert_eq!(
        labels,
        ["Add a comment", "Run the fix command"],
        "preferred fixes first"
    );
    s.cx.simulate_keystrokes("enter");
    run_until(s.cx, |cx| {
        cx.update(|_, cx| {
            editor
                .read(cx)
                .buffer()
                .read(cx)
                .buffer()
                .text()
                .starts_with("// fixed\n")
        })
    });

    s.cx.simulate_keystrokes("alt-enter");
    s.wait_for_picker();
    s.cx.simulate_keystrokes("down enter");
    run_until(s.cx, |cx| {
        cx.update(|_, cx| {
            editor
                .read(cx)
                .buffer()
                .read(cx)
                .buffer()
                .text()
                .starts_with("// commanded\n")
        })
    });
    let log = s.log.clone();
    run_until(s.cx, |_| !log.lock().unwrap().responses.is_empty());
    assert_eq!(
        log.lock().unwrap().responses[0]["result"],
        json!({ "applied": true })
    );
}

#[gpui::test]
fn reformat_applies_the_servers_edits_as_one_undo_step(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("src/shapes.rs", 0, 0);
    s.cx.simulate_keystrokes(Keys::REFORMAT);
    run_until(s.cx, |cx| {
        cx.update(|_, cx| {
            editor
                .read(cx)
                .buffer()
                .read(cx)
                .buffer()
                .text()
                .contains("\n  r * r")
        })
    });
    s.cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    assert_eq!(s.text(&editor), SHAPES);
}

#[gpui::test]
fn file_structure_lists_nested_symbols_and_jumps(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/shapes.rs", 1, 4);
    s.cx.simulate_keystrokes(Keys::STRUCTURE);
    let (title, labels) = s.wait_for_picker();
    assert_eq!(
        (title.as_str(), labels),
        ("shapes.rs", vec!["area".to_string(), "r".to_string()])
    );
    s.cx.simulate_keystrokes("enter");
    s.wait_for(("src/shapes.rs", 0, 7));
}

#[gpui::test]
fn problems_show_in_the_panel_and_the_editor(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("src/shapes.rs", 0, 0);
    run_until(s.cx, |cx| {
        cx.update(|_, cx| !editor.read(cx).buffer().read(cx).diagnostics().is_empty())
    });
    assert_eq!(
        s.panel_state(PROBLEMS_PANEL)["problems"],
        json!(["src/shapes.rs:1: unused"])
    );
    s.cx.simulate_keystrokes(Keys::PROBLEMS);
    let workspace = s.workspace.clone();
    let docks =
        s.cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).docks);
    assert!(
        docks
            .iter()
            .any(|d| d.visible && d.panels.iter().any(|p| p == PROBLEMS_PANEL))
    );
}

#[gpui::test]
fn completion_comes_from_the_server_with_its_import_and_snippet(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let editor = s.open("src/main.rs", 4, 0);
    s.cx.simulate_input("ar");
    run_until(s.cx, |cx| {
        cx.update(|_, cx| editor.read(cx).completion_labels() == ["area"])
    });
    s.cx.simulate_keystrokes("enter");
    run_until(s.cx, |cx| {
        cx.update(|_, cx| {
            editor
                .read(cx)
                .buffer()
                .read(cx)
                .buffer()
                .text()
                .starts_with("use shapes::area;\n")
        })
    });
    let text = s.text(&editor);
    assert!(text.ends_with("area()}\n"), "{text}");
    let head =
        s.cx.update(|_, cx| editor.read(cx).selections().primary().head);
    assert_eq!(
        head,
        text.find("area()").unwrap() + 5,
        "inside the parentheses"
    );
}

#[gpui::test]
fn find_in_files_searches_the_project_and_fills_the_find_panel(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    s.open("src/main.rs", 0, 0);
    let workspace = s.workspace.clone();
    let project =
        s.cx.update(|_, cx| workspace.read(cx).project().cloned().unwrap());
    run_until(s.cx, |cx| {
        cx.update(|_, cx| project.update(cx, |p, _| p.files().len() >= 3))
    });
    let find = if cfg!(target_os = "macos") {
        "cmd-shift-f"
    } else {
        "ctrl-shift-f"
    };
    s.cx.simulate_keystrokes(find);
    let modal =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<FindInFiles>())
            .expect("the Find in Files popup");
    s.cx.update(|_, cx| modal.update(cx, |m, cx| m.set_query("area", cx)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        s.cx.executor()
            .advance_clock(std::time::Duration::from_millis(200));
        run_until(s.cx, |_| true);
        if s.cx.update(|_, cx| !modal.read(cx).is_searching()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the search never finished"
        );
    }
    let labels = s.cx.update(|_, cx| modal.read(cx).labels());
    assert_eq!(
        labels,
        [
            "src/main.rs:3: let a = shapes::area(2.0);",
            "src/main.rs:4: let b = shapes::area(3.0);",
            "src/shapes.rs:1: pub fn area(r: f64) -> f64 {",
        ]
    );
    // Down to the declaration, and Enter opens it there.
    s.cx.simulate_keystrokes("down down enter");
    run_until(s.cx, |_| true);
    assert_eq!(s.active(), Some(("src/shapes.rs".into(), 0, 7)));

    // Cmd+Enter moves the results to the Find panel instead.
    s.cx.simulate_keystrokes(find);
    let modal =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<FindInFiles>())
            .unwrap();
    s.cx.update(|_, cx| modal.update(cx, |m, cx| m.set_query("shapes::", cx)));
    s.cx.executor()
        .advance_clock(std::time::Duration::from_millis(200));
    run_until(s.cx, |cx| cx.update(|_, cx| !modal.read(cx).is_searching()));
    let to_panel = if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    };
    s.cx.simulate_keystrokes(to_panel);
    let state = s.panel_state(USAGES_PANEL);
    assert_eq!(state["title"], "Occurrences of 'shapes::'");
    assert_eq!(state["usages"].as_array().unwrap().len(), 2);
}

/// Waits for Find in Files to finish searching.
fn finish_search(modal: &Entity<FindInFiles>, cx: &mut VisualTestContext) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(200));
        run_until(cx, |_| true);
        if cx.update(|_, cx| !modal.read(cx).is_searching()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the search never finished"
        );
    }
}

#[gpui::test]
fn replace_in_files_does_one_match_then_all_open_or_not(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(cx);
    let main = s.open("src/main.rs", 0, 0);
    let workspace = s.workspace.clone();
    let project =
        s.cx.update(|_, cx| workspace.read(cx).project().cloned().unwrap());
    run_until(s.cx, |cx| {
        cx.update(|_, cx| project.update(cx, |p, _| p.files().len() >= 3))
    });
    s.cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-shift-r"
    } else {
        "ctrl-shift-r"
    });
    let modal =
        s.cx.update(|_, cx| workspace.read(cx).active_modal::<FindInFiles>())
            .expect("the Replace in Files popup");
    s.cx.update(|_, cx| {
        modal.update(cx, |m, cx| {
            m.set_query("area", cx);
            m.set_replacement("surface", cx);
        })
    });
    finish_search(&modal, s.cx);
    let main_text = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| main.read(cx).buffer().read(cx).buffer().text())
    };

    // Replace: the selected match only, in the open file, left unsaved.
    let click = |selector: &'static str, cx: &mut VisualTestContext| {
        cx.run_until_parked();
        let bounds = cx.debug_bounds(selector).expect(selector);
        cx.simulate_click(bounds.center(), Modifiers::none());
        run_until(cx, |_| true);
    };
    click("replace-selected", s.cx);
    run_until(s.cx, |cx| main_text(cx).contains("shapes::surface(2.0)"));
    assert!(main_text(s.cx).contains("shapes::area(3.0)"));
    finish_search(&modal, s.cx);
    assert_eq!(s.cx.update(|_, cx| modal.read(cx).labels()).len(), 2);

    // Replace All asks first, then changes the rest, saving the file that wasn't open.
    click("replace-all", s.cx);
    assert!(
        main_text(s.cx).contains("shapes::area(3.0)"),
        "only asked so far"
    );
    click("replace-all", s.cx);
    let shapes = s.root.join("src/shapes.rs");
    run_until(s.cx, |cx| {
        main_text(cx).contains("shapes::surface(3.0)")
            && std::fs::read_to_string(&shapes)
                .unwrap()
                .contains("pub fn surface(")
    });
    let saved = std::fs::read_to_string(s.root.join("src/main.rs")).unwrap();
    assert!(saved.contains("area"), "the open file is left unsaved");
}
