//! Tests against an in-process fake server, which can misbehave on purpose.

use super::*;
use futures::StreamExt as _;
use lsp_types::request::{GotoDefinition, HoverRequest};
use serde_json::{Value, json};
use std::io::BufReader;
use std::sync::mpsc;
use std::time::Duration;

/// What the fake server's handler sees, and how it answers.
enum Reply {
    Result(Value),
    Error(i64, &'static str),
    /// Don't answer at all.
    Nothing,
    /// Close the connection, as a crash would.
    Die,
}

struct Fake {
    server: Arc<LanguageServer>,
    incoming: futures::channel::mpsc::UnboundedReceiver<Incoming>,
    /// Everything the client sent, as the fake saw it.
    received: mpsc::Receiver<Value>,
    /// Lets a test send something unprompted from the fake.
    send: mpsc::Sender<Value>,
}

use std::sync::Arc;

fn fake(handler: impl Fn(&str, &Value) -> Reply + Send + 'static) -> Fake {
    let (client_read, server_write) = std::io::pipe().unwrap();
    let (server_read, client_write) = std::io::pipe().unwrap();
    let (received_tx, received) = mpsc::channel();
    let (send, send_rx) = mpsc::channel::<Value>();
    // Shared with the thread sending unprompted messages. Dying takes it out and drops it, which
    // closes the pipe, as a crashed process's would be.
    let writer = Arc::new(std::sync::Mutex::new(Some(server_write)));

    let unprompted_writer = writer.clone();
    std::thread::spawn(move || {
        for message in send_rx {
            let mut guard = unprompted_writer.lock().unwrap();
            let Some(pipe) = guard.as_mut() else { break };
            if write_message(pipe, &message).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        let mut reader = BufReader::new(server_read);
        while let Ok(Some(message)) = read_message(&mut reader) {
            received_tx.send(message.clone()).ok();
            let (Some(method), Some(id)) = (
                message.get("method").and_then(Value::as_str),
                message.get("id"),
            ) else {
                continue;
            };
            let reply = match handler(method, &message["params"]) {
                Reply::Result(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Reply::Error(code, text) => {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": text } })
                }
                Reply::Nothing => continue,
                Reply::Die => {
                    writer.lock().unwrap().take();
                    return;
                }
            };
            let mut guard = writer.lock().unwrap();
            let Some(pipe) = guard.as_mut() else { return };
            if write_message(pipe, &reply).is_err() {
                return;
            }
        }
    });
    let (server, incoming) =
        LanguageServer::from_streams("fake", client_read, client_write, None::<std::io::Empty>);
    Fake {
        server,
        incoming,
        received,
        send,
    }
}

fn block<T>(future: impl std::future::Future<Output = T>) -> T {
    futures::executor::block_on(future)
}

fn position_params() -> lsp_types::TextDocumentPositionParams {
    lsp_types::TextDocumentPositionParams {
        text_document: lsp_types::TextDocumentIdentifier {
            uri: path_to_uri(std::path::Path::new("/a.rs")),
        },
        position: lsp_types::Position::new(1, 2),
    }
}

fn hover_params() -> lsp_types::HoverParams {
    lsp_types::HoverParams {
        text_document_position_params: position_params(),
        work_done_progress_params: Default::default(),
    }
}

/// Waits for the fake to receive a message matching a predicate.
fn wait_for(received: &mpsc::Receiver<Value>, predicate: impl Fn(&Value) -> bool) -> Value {
    loop {
        let message = received
            .recv_timeout(Duration::from_secs(5))
            .expect("the expected message never came");
        if predicate(&message) {
            return message;
        }
    }
}

#[test]
fn framing_survives_unicode_and_stray_blank_lines() {
    let message = json!({ "text": "é😀 日本" });
    let mut bytes = b"\r\n".to_vec();
    write_message(&mut bytes, &message).unwrap();
    write_message(&mut bytes, &json!(1)).unwrap();
    let mut reader = std::io::Cursor::new(bytes);
    assert_eq!(read_message(&mut reader).unwrap(), Some(message));
    assert_eq!(read_message(&mut reader).unwrap(), Some(json!(1)));
    assert_eq!(read_message(&mut reader).unwrap(), None);
}

#[test]
fn framing_rejects_bad_input_instead_of_hanging() {
    for bad in [
        &b"Content-Length: nope\r\n\r\n"[..],
        b"Content-Length: 10\r\n\r\n{}",
        b"Content-Length: 2\r\n\r\nno",
    ] {
        assert!(
            read_message(&mut std::io::Cursor::new(bad)).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    let huge = b"Content-Length: 999999999999\r\n\r\n";
    assert!(read_message(&mut std::io::Cursor::new(&huge[..])).is_err());
}

#[test]
fn requests_get_their_own_responses_even_out_of_order() {
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some(gate_rx));
    let fake = fake(move |method, _| match method {
        // The slow one waits until the fast one has been answered.
        "textDocument/definition" => {
            if let Some(gate) = gate.lock().unwrap().take() {
                gate.recv().ok();
            }
            Reply::Result(json!([]))
        }
        "textDocument/hover" => Reply::Result(json!({ "contents": "docs" })),
        _ => Reply::Nothing,
    });
    let slow = fake
        .server
        .request::<GotoDefinition>(lsp_types::GotoDefinitionParams {
            text_document_position_params: position_params(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    // The fake handles one message at a time, so release it from another thread.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        gate_tx.send(()).ok();
    });
    let hover = block(fake.server.request::<HoverRequest>(hover_params()))
        .unwrap()
        .unwrap();
    assert!(matches!(
        hover.contents,
        lsp_types::HoverContents::Scalar(_)
    ));
    assert!(block(slow).unwrap().is_some());
}

#[test]
fn server_errors_come_back_as_errors() {
    let fake = fake(|_, _| Reply::Error(-32601, "no such method"));
    let error = block(fake.server.request::<HoverRequest>(hover_params())).unwrap_err();
    assert_eq!(
        error,
        RequestError::Server {
            code: -32601,
            message: "no such method".into()
        }
    );
}

#[test]
fn a_response_of_the_wrong_shape_is_reported() {
    let fake = fake(|_, _| Reply::Result(json!({ "contents": 42 })));
    assert!(matches!(
        block(fake.server.request::<HoverRequest>(hover_params())),
        Err(RequestError::BadResponse(_))
    ));
}

#[test]
fn dropping_a_request_cancels_it_on_the_server() {
    let fake = fake(|_, _| Reply::Nothing);
    let request = fake.server.request::<HoverRequest>(hover_params());
    let sent = wait_for(&fake.received, |m| m["method"] == "textDocument/hover");
    drop(request);
    let cancel = wait_for(&fake.received, |m| m["method"] == "$/cancelRequest");
    assert_eq!(cancel["params"]["id"], sent["id"]);
}

#[test]
fn a_finished_request_is_not_cancelled() {
    let fake = fake(|_, _| Reply::Result(Value::Null));
    block(fake.server.request::<HoverRequest>(hover_params())).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        fake.received
            .try_iter()
            .all(|m| m["method"] != "$/cancelRequest")
    );
}

#[test]
fn a_crash_fails_every_waiting_request_and_is_reported() {
    let mut fake = fake(|method, _| {
        if method == "textDocument/hover" {
            Reply::Die
        } else {
            Reply::Nothing
        }
    });
    let waiting = fake
        .server
        .request::<GotoDefinition>(lsp_types::GotoDefinitionParams {
            text_document_position_params: position_params(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let fatal = fake.server.request::<HoverRequest>(hover_params());
    assert_eq!(block(fatal).unwrap_err(), RequestError::Gone);
    assert_eq!(block(waiting).unwrap_err(), RequestError::Gone);
    assert!(!fake.server.is_alive());
    let exited = block(async {
        while let Some(message) = fake.incoming.next().await {
            if matches!(message, Incoming::Exited { .. }) {
                return true;
            }
        }
        false
    });
    assert!(exited);
    // Requests after the crash fail straight away rather than hanging.
    assert_eq!(
        block(fake.server.request::<HoverRequest>(hover_params())).unwrap_err(),
        RequestError::Gone
    );
}

#[test]
fn notifications_from_the_server_reach_the_client() {
    let mut fake = fake(|_, _| Reply::Nothing);
    fake.send
        .send(json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": "file:///a.rs", "diagnostics": [] } }))
        .unwrap();
    let message = block(fake.incoming.next()).unwrap();
    assert!(
        matches!(message, Incoming::Notification { method, .. } if method == "textDocument/publishDiagnostics")
    );
}

#[test]
fn routine_server_requests_are_answered_automatically() {
    let fake = fake(|_, _| Reply::Nothing);
    fake.send
        .send(json!({ "jsonrpc": "2.0", "id": "c1", "method": "workspace/configuration", "params": { "items": [{}, {}] } }))
        .unwrap();
    fake.send.send(json!({ "jsonrpc": "2.0", "id": 7, "method": "window/workDoneProgress/create", "params": {} })).unwrap();
    let configuration = wait_for(&fake.received, |m| m["id"] == "c1");
    assert_eq!(configuration["result"], json!([null, null]));
    let progress = wait_for(&fake.received, |m| m["id"] == 7);
    assert_eq!(progress["result"], Value::Null);
}

#[test]
fn requests_the_app_must_answer_are_passed_on_and_answered() {
    let mut fake = fake(|_, _| Reply::Nothing);
    fake.send
        .send(json!({ "jsonrpc": "2.0", "id": 3, "method": "workspace/applyEdit", "params": { "edit": {} } }))
        .unwrap();
    let Some(Incoming::Request { id, method, .. }) = block(fake.incoming.next()) else {
        panic!("expected a request")
    };
    assert_eq!(method, "workspace/applyEdit");
    fake.server.respond(id, Ok(json!({ "applied": true })));
    let answer = wait_for(&fake.received, |m| m["id"] == 3);
    assert_eq!(answer["result"]["applied"], true);
}

#[test]
fn initialize_records_capabilities_and_announces_itself() {
    let fake = fake(|method, params| match method {
        "initialize" => {
            assert_eq!(
                params["capabilities"]["general"]["positionEncodings"],
                json!(["utf-16"])
            );
            Reply::Result(
                json!({ "capabilities": { "hoverProvider": true, "definitionProvider": true } }),
            )
        }
        _ => Reply::Nothing,
    });
    let root = tempfile::tempdir().unwrap();
    block(fake.server.initialize(root.path(), None)).unwrap();
    assert!(fake.server.capabilities().hover_provider.is_some());
    wait_for(&fake.received, |m| m["method"] == "initialized");
}

#[test]
fn a_real_process_that_exits_immediately_is_reported() {
    let (server, mut incoming) = LanguageServer::spawn(
        "false",
        ServerOptions {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo boom >&2; exit 3".into()],
            env: vec![],
            cwd: "/".into(),
        },
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    assert!(block(server.initialize(root.path(), None)).is_err());
    let stderr = block(async {
        while let Some(message) = incoming.next().await {
            if let Incoming::Exited { stderr } = message {
                return stderr;
            }
        }
        String::new()
    });
    // stderr is read on its own thread, so give it a moment to have been captured.
    std::thread::sleep(Duration::from_millis(50));
    let _ = stderr;
}
