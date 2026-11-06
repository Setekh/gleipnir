//! Tests with an in-process fake server, which keeps its own copy of each document by applying the
//! changes we send. That's how sync is checked: the fake's copy must always match the buffer.

use super::*;
use gleipnir_lsp::{read_message, write_message};
use gleipnir_text::ops::EditContext;
use gleipnir_text::{SelectionSet, TextEdit, UndoGrouping};
use gpui::TestAppContext;
use serde_json::{Value, json};
use std::collections::HashMap as Map;
use std::io::BufReader;
use std::sync::Mutex;

/// What the fake has been told, shared with the test.
#[derive(Default)]
struct FakeState {
    documents: Map<String, String>,
    versions: Map<String, i64>,
    log: Vec<String>,
    initialized: u32,
    /// Each `didChangeWatchedFiles`, as (file name, change type).
    watched: Vec<Vec<(String, u64)>>,
}

/// Applies an LSP change to text, converting the UTF-16 range independently of our own code.
fn apply_change(text: &mut String, change: &Value) {
    let Some(range) = change.get("range") else {
        *text = change["text"].as_str().unwrap().to_string();
        return;
    };
    let offset = |position: &Value| -> usize {
        let line = position["line"].as_u64().unwrap() as usize;
        let character = position["character"].as_u64().unwrap() as usize;
        let mut line_start = 0;
        for _ in 0..line {
            line_start = text[line_start..]
                .find('\n')
                .map_or(text.len(), |i| line_start + i + 1);
        }
        let mut units = 0;
        let mut byte = line_start;
        for c in text[line_start..].chars() {
            if units >= character || c == '\n' {
                break;
            }
            units += c.len_utf16();
            byte += c.len_utf8();
        }
        byte
    };
    let (start, end) = (offset(&range["start"]), offset(&range["end"]));
    text.replace_range(start..end, change["text"].as_str().unwrap());
}

type Shared = Arc<Mutex<FakeState>>;

/// A launcher whose servers are fakes. `crash_on` makes a server die on that method.
fn fake_launcher(state: Shared, crash_on: Option<&'static str>) -> Launcher {
    Rc::new(move |_, _, _| {
        let (client_read, server_write) = std::io::pipe()?;
        let (server_read, client_write) = std::io::pipe()?;
        let state = state.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            while let Ok(Some(message)) = read_message(&mut reader) {
                let method = message["method"].as_str().unwrap_or("").to_string();
                let params = &message["params"];
                state.lock().unwrap().log.push(method.clone());
                if Some(method.as_str()) == crash_on {
                    return;
                }
                match method.as_str() {
                    "initialize" => {
                        state.lock().unwrap().initialized += 1;
                        let reply = json!({ "jsonrpc": "2.0", "id": message["id"], "result": { "capabilities": {
                            "textDocumentSync": 2, "definitionProvider": true, "hoverProvider": true
                        } } });
                        write_message(&mut writer, &reply).ok();
                    }
                    // Like rust-analyzer, it says when it's loaded the project.
                    "initialized" => {
                        let status = json!({ "jsonrpc": "2.0", "method": "experimental/serverStatus", "params": {
                            "health": "ok", "quiescent": true
                        } });
                        write_message(&mut writer, &status).ok();
                    }
                    "textDocument/didOpen" => {
                        let uri = params["textDocument"]["uri"].as_str().unwrap().to_string();
                        let mut state = state.lock().unwrap();
                        state.documents.insert(
                            uri.clone(),
                            params["textDocument"]["text"].as_str().unwrap().to_string(),
                        );
                        state.versions.insert(
                            uri.clone(),
                            params["textDocument"]["version"].as_i64().unwrap(),
                        );
                        // Report a problem on line 0 of every file opened.
                        let diagnostics = json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
                            "uri": uri, "diagnostics": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 2 } }, "message": "bad", "severity": 1 }]
                        } });
                        write_message(&mut writer, &diagnostics).ok();
                    }
                    "textDocument/didChange" => {
                        let uri = params["textDocument"]["uri"].as_str().unwrap().to_string();
                        let mut state = state.lock().unwrap();
                        let mut text = state.documents.get(&uri).cloned().unwrap_or_default();
                        for change in params["contentChanges"].as_array().unwrap() {
                            apply_change(&mut text, change);
                        }
                        state.documents.insert(uri.clone(), text);
                        let version = params["textDocument"]["version"].as_i64().unwrap();
                        let previous = state.versions.insert(uri, version).unwrap_or(0);
                        assert!(version > previous, "versions must increase");
                    }
                    "workspace/didChangeWatchedFiles" => {
                        let changes = params["changes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|c| {
                                let uri = c["uri"].as_str().unwrap();
                                let name = uri.rsplit('/').next().unwrap().to_string();
                                (name, c["type"].as_u64().unwrap())
                            })
                            .collect();
                        state.lock().unwrap().watched.push(changes);
                    }
                    "textDocument/didClose" => {
                        let uri = params["textDocument"]["uri"].as_str().unwrap().to_string();
                        state.lock().unwrap().documents.remove(&uri);
                    }
                    "textDocument/definition" => {
                        let uri = params["textDocument"]["uri"].clone();
                        let reply = json!({ "jsonrpc": "2.0", "id": message["id"], "result": [
                            { "uri": uri, "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 3 } } }
                        ] });
                        write_message(&mut writer, &reply).ok();
                    }
                    _ => {
                        if message.get("id").is_some() {
                            write_message(
                                &mut writer,
                                &json!({ "jsonrpc": "2.0", "id": message["id"], "result": null }),
                            )
                            .ok();
                        }
                    }
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

struct Setup {
    servers: Entity<LanguageServers>,
    state: Shared,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(crash_on: Option<&'static str>, cx: &mut TestAppContext) -> Setup {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let state: Shared = Arc::default();
    let toolchains = cx.new(|cx| {
        Toolchains::with_environment(None, gleipnir_packages::Env::new(), root.join("tools"), cx)
    });
    wait(cx, |cx| {
        toolchains.read_with(cx, |t, _| !t.packages().is_empty())
    });
    let launcher = fake_launcher(state.clone(), crash_on);
    let servers =
        cx.new(|cx| LanguageServers::with_launcher(Some(root.clone()), toolchains, launcher, cx));
    Setup {
        servers,
        state,
        _dir: dir,
        root,
    }
}

/// Runs the app until a condition holds. The test clock only moves when told to, so it's moved
/// along too, which is what lets restart delays elapse.
fn wait(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // An update lets the app process releases of dropped entities.
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

/// Waits for a task that may run on the background executor, which the test scheduler only runs
/// when driven.
fn finish<T: 'static>(cx: &mut TestAppContext, task: Task<T>) -> T {
    let slot = Rc::new(std::cell::RefCell::new(None));
    let result = slot.clone();
    cx.spawn(async move |_| *result.borrow_mut() = Some(task.await))
        .detach();
    wait(cx, |_| slot.borrow().is_some());
    slot.borrow_mut().take().unwrap()
}

fn buffer(text: &str, path: &Path, cx: &mut TestAppContext) -> Entity<BufferModel> {
    let path = path.to_path_buf();
    cx.new(|cx| BufferModel::new(text, Some(path), cx))
}

fn server_text(s: &Setup, path: &Path) -> Option<String> {
    s.state
        .lock()
        .unwrap()
        .documents
        .get(path_to_uri(path).as_str())
        .cloned()
}

fn edit(buffer: &Entity<BufferModel>, edits: Vec<TextEdit>, cx: &mut TestAppContext) {
    buffer.update(cx, |b, cx| {
        let mut selections = SelectionSet::cursor(0);
        b.edit(
            &mut selections,
            EntityId::from(1u64),
            cx,
            |e: &mut EditContext| ((), e.apply_edits(edits, None, UndoGrouping::Distinct)),
        );
    });
}

#[gpui::test]
fn opening_a_file_starts_its_server_once_and_opens_the_document(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let (a, b) = (s.root.join("a.rs"), s.root.join("b.rs"));
    let first = buffer("fn a() {}", &a, cx);
    let second = buffer("fn b() {}", &b, cx);
    s.servers.update(cx, |servers, cx| {
        servers.register_buffer(&first, cx);
        servers.register_buffer(&second, cx);
    });
    wait(cx, |_| server_text(&s, &b).is_some());
    assert_eq!(server_text(&s, &a).as_deref(), Some("fn a() {}"));
    assert_eq!(
        s.state.lock().unwrap().initialized,
        1,
        "one server for the language"
    );
    let status = s
        .servers
        .read_with(cx, |servers, _| servers.status("rust").cloned());
    assert_eq!(status, Some(ServerStatus::Running { busy: None }));
}

#[gpui::test]
fn files_without_a_language_server_are_left_alone(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let notes = buffer("text", &s.root.join("notes.md"), cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&notes, cx));
    cx.run_until_parked();
    assert_eq!(s.state.lock().unwrap().initialized, 0);
}

#[gpui::test]
fn edits_keep_the_server_copy_identical_including_unicode(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let path = s.root.join("a.rs");
    let model = buffer("fn main() {\n    let é = \"😀\";\n}\n", &path, cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |_| server_text(&s, &path).is_some());

    // A multi-cursor style batch, then a deletion across lines, then an insert after an emoji.
    edit(
        &model,
        vec![TextEdit::new(0..0, "// top\n"), TextEdit::new(12..12, "x")],
        cx,
    );
    let text = model.read_with(cx, |b, _| b.buffer().text());
    let emoji_end = text.find('😀').unwrap() + '😀'.len_utf8();
    edit(
        &model,
        vec![TextEdit::new(emoji_end..emoji_end, "日本")],
        cx,
    );
    edit(&model, vec![TextEdit::new(3..20, "")], cx);
    model.update(cx, |b, cx| {
        b.undo(EntityId::from(1u64), cx);
    });
    wait(cx, |cx| {
        server_text(&s, &path) == Some(model.read_with(cx, |b, _| b.buffer().text()))
    });
}

#[gpui::test]
fn diagnostics_arrive_on_the_buffer_and_follow_edits(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let path = s.root.join("a.rs");
    let model = buffer("ab cd", &path, cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |cx| {
        !model.read_with(cx, |b, _| b.diagnostics().is_empty())
    });
    let diagnostic = model.read_with(cx, |b, _| b.diagnostics()[0].clone());
    assert_eq!(
        (diagnostic.range.clone(), diagnostic.message.as_str()),
        (0..2, "bad")
    );
    // Typing before it moves it along.
    edit(&model, vec![TextEdit::new(0..0, "xx")], cx);
    assert_eq!(
        model.read_with(cx, |b, _| b.diagnostics()[0].range.clone()),
        2..4
    );
    assert!(s.servers.read_with(cx, |servers, _| {
        servers.all_diagnostics().contains_key(&path)
    }));
}

#[gpui::test]
fn requests_convert_positions_both_ways(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let path = s.root.join("a.rs");
    let model = buffer("😀x\nfoo bar", &path, cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |_| server_text(&s, &path).is_some());
    let task = s.servers.update(cx, |servers, cx| {
        servers.goto(&model, 4, GotoKind::Definition, cx)
    });
    let locations = finish(cx, task).unwrap();
    assert_eq!(
        locations,
        [Location {
            path: path.clone(),
            start: Utf16Point::new(1, 0),
            end: Utf16Point::new(1, 3)
        }]
    );
}

#[gpui::test]
fn closing_a_buffer_closes_the_document(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let path = s.root.join("a.rs");
    let model = buffer("x", &path, cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |_| server_text(&s, &path).is_some());
    drop(model);
    wait(cx, |_| server_text(&s, &path).is_none());
}

#[gpui::test]
fn a_crashed_server_restarts_and_reopens_its_documents(cx: &mut TestAppContext) {
    let s = setup(Some("textDocument/hover"), cx);
    let path = s.root.join("a.rs");
    let model = buffer("fn a() {}", &path, cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |_| server_text(&s, &path).is_some());

    // The fake dies on hover.
    let crash = s
        .servers
        .update(cx, |servers, cx| servers.hover(&model, 0, cx));
    assert!(finish(cx, crash).is_err());
    wait(cx, |cx| {
        matches!(
            s.servers.read_with(cx, |sv, _| sv.status("rust").cloned()),
            Some(ServerStatus::Failed(_))
        )
    });
    // It comes back by itself, and gets the document again.
    wait(cx, |_| s.state.lock().unwrap().initialized == 2);
    wait(cx, |_| {
        s.state
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|m| *m == "textDocument/didOpen")
            .count()
            == 2
    });
}

#[gpui::test]
fn a_server_that_keeps_crashing_is_given_up_on(cx: &mut TestAppContext) {
    let s = setup(Some("initialize"), cx);
    let model = buffer("x", &s.root.join("a.rs"), cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |cx| {
        matches!(
            s.servers.read_with(cx, |sv, _| sv.status("rust").cloned()),
            Some(ServerStatus::Failed(reason)) if reason.contains("won't be restarted")
        )
    });
}

#[gpui::test]
fn without_the_tool_the_server_is_reported_missing(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let toolchains = cx.new(|cx| {
        Toolchains::with_environment(
            None,
            gleipnir_packages::Env::from_pairs([("PATH", "/nonexistent")]),
            dir.path().join("tools"),
            cx,
        )
    });
    wait(cx, |cx| {
        toolchains.read_with(cx, |t, _| !t.packages().is_empty())
    });
    let servers = cx.new(|cx| LanguageServers::new(Some(dir.path().to_path_buf()), toolchains, cx));
    // ols isn't in any SDK, and PATH is empty, so it can't be found.
    let model = buffer("package p", &dir.path().join("a.odin"), cx);
    servers.update(cx, |servers, cx| servers.register_buffer(&model, cx));
    let status = servers.read_with(cx, |sv, _| sv.status("odin").cloned());
    // ~/Odin has its own ols on this machine, so it may well be found there.
    if let Some(ServerStatus::Missing(reason)) = &status {
        assert!(reason.contains("ols"), "{reason}");
        let hover = servers.update(cx, |sv, cx| sv.hover(&model, 0, cx));
        let error = finish(cx, hover).unwrap_err();
        assert!(error.to_string().contains("ols"));
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 24, ..Default::default() })]

    /// The roadmap's check for sync: after any edits, the server sees exactly our text.
    #[test]
    fn any_edits_leave_the_server_in_sync(
        edits in proptest::collection::vec((0usize..60, 0usize..8, "[ab \n😀é日]{0,5}"), 1..15)
    ) {
        let mut app = TestAppContext::single();
        let cx = &mut app;
        let s = setup(None, cx);
        let path = s.root.join("p.rs");
        let model = buffer("fn main() {\n    let 😀 = é;\n}\n", &path, cx);
        s.servers.update(cx, |servers, cx| servers.register_buffer(&model, cx));
        wait(cx, |_| server_text(&s, &path).is_some());
        for (at, len, text) in edits {
            let range = model.read_with(cx, |b, _| {
                let start = b.buffer().clip(at);
                start..b.buffer().clip(start + len)
            });
            edit(&model, vec![TextEdit::new(range, text)], cx);
        }
        wait(cx, |cx| server_text(&s, &path) == Some(model.read_with(cx, |b, _| b.buffer().text())));
    }
}

#[gpui::test]
fn a_request_made_along_with_an_edit_is_answered_about_the_new_text(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let path = s.root.join("a.rs");
    let model = buffer("fn a() {}", &path, cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |_| server_text(&s, &path).is_some());
    // The edit's event, which normally carries it to the server, comes after this update.
    let task = cx.update(|cx| {
        model.update(cx, |b, cx| {
            let mut selections = SelectionSet::cursor(0);
            b.edit(
                &mut selections,
                EntityId::from(1u64),
                cx,
                |e: &mut EditContext| {
                    (
                        (),
                        e.apply_edits(
                            vec![TextEdit::new(0..0, "pub ")],
                            None,
                            UndoGrouping::Distinct,
                        ),
                    )
                },
            );
        });
        s.servers.read(cx).hover(&model, 4, cx)
    });
    finish(cx, task).ok();
    let log = s.state.lock().unwrap().log.clone();
    let hover = log.iter().position(|m| m == "textDocument/hover").unwrap();
    let change = log
        .iter()
        .position(|m| m == "textDocument/didChange")
        .unwrap();
    assert!(change < hover, "the server had the edit first: {log:?}");
    wait(cx, |_| true);
    assert_eq!(
        server_text(&s, &path).as_deref(),
        Some("pub fn a() {}"),
        "and only once"
    );
}

#[gpui::test]
fn rust_analyzer_counts_as_busy_until_it_has_loaded_the_project(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&s.servers, move |servers, event, cx| {
            if let LanguageServerEvent::StatusChanged(_) = event {
                record
                    .borrow_mut()
                    .push(servers.read(cx).status("rust").cloned());
            }
        })
        .detach()
    });
    let model = buffer("fn a() {}", &s.root.join("a.rs"), cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&model, cx));
    wait(cx, |_| {
        seen.borrow().last() == Some(&Some(ServerStatus::Running { busy: None }))
    });
    let loading = Some(ServerStatus::Running {
        busy: Some("loading the project".into()),
    });
    assert!(seen.borrow().contains(&loading), "{:?}", seen.borrow());
}

#[gpui::test]
fn files_changed_elsewhere_are_reported_unless_open_or_irrelevant(cx: &mut TestAppContext) {
    let s = setup(None, cx);
    let open = s.root.join("a.rs");
    let buffer = buffer("fn a() {}", &open, cx);
    s.servers
        .update(cx, |servers, cx| servers.register_buffer(&buffer, cx));
    wait(cx, |_| server_text(&s, &open).is_some());

    std::fs::write(s.root.join("b.rs"), "fn b() {}").unwrap();
    std::fs::write(s.root.join("Cargo.toml"), "[package]").unwrap();
    let changed: Vec<PathBuf> = [
        "a.rs",
        "b.rs",
        "Cargo.toml",
        "gone.rs",
        "notes.txt",
        "target/x.rs",
    ]
    .iter()
    .map(|p| s.root.join(p))
    .collect();
    s.servers
        .read_with(cx, |servers, _| servers.files_changed(&changed));
    let state = s.state.clone();
    wait(cx, |_| !state.lock().unwrap().watched.is_empty());
    let watched = s.state.lock().unwrap().watched[0].clone();
    // Changed is 2 and deleted is 3, in LSP's numbering.
    assert_eq!(
        watched,
        [
            ("b.rs".to_string(), 2),
            ("Cargo.toml".to_string(), 2),
            ("gone.rs".to_string(), 3),
        ]
    );
}
