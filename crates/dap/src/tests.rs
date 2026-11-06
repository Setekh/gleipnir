//! Tests against an in-process fake adapter, scripted per test to behave like a real one or to
//! misbehave on purpose.

use super::*;
use crate::fake::{Out, fake};
use crate::session::{self as s};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A program with one function, `main`, stopped at line 3 with two locals: `count`, and `point`
/// with fields.
fn simple_program(out: &mut Out) {
    match out.command() {
        "initialize" => {
            out.ok(json!({ "supportsConfigurationDoneRequest": true }));
            out.event("initialized", json!({}));
        }
        "setBreakpoints" => {
            let lines: Vec<u64> = out.args()["breakpoints"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["line"].as_u64().unwrap())
                .collect();
            let placed: Vec<Value> = lines
                .iter()
                .enumerate()
                .map(|(i, line)| json!({ "id": i + 1, "verified": true, "line": line }))
                .collect();
            out.ok(json!({ "breakpoints": placed }));
        }
        "configurationDone" => {
            out.ok(json!({}));
            out.event("process", json!({ "name": "program" }));
            out.event(
                "output",
                json!({ "category": "stdout", "output": "hello\n" }),
            );
            out.event(
                "stopped",
                json!({ "reason": "breakpoint", "threadId": 1, "hitBreakpointIds": [1] }),
            );
        }
        "threads" => out.ok(json!({ "threads": [{ "id": 1, "name": "main" }] })),
        "stackTrace" => out.ok(json!({ "stackFrames": [
            { "id": 10, "name": "main", "line": 3, "column": 5,
              "source": { "name": "main.rs", "path": "/p/src/main.rs" } },
            { "id": 11, "name": "start", "line": 1, "column": 1, "presentationHint": "subtle" },
        ] })),
        "scopes" => out.ok(json!({ "scopes": [
            { "name": "Locals", "variablesReference": 100, "expensive": false },
        ] })),
        "variables" => match out.args()["variablesReference"].as_i64() {
            Some(100) => out.ok(json!({ "variables": [
                { "name": "count", "value": "3", "type": "i32", "variablesReference": 0 },
                { "name": "point", "value": "Point", "variablesReference": 101 },
            ] })),
            Some(101) => out.ok(json!({ "variables": [
                { "name": "x", "value": "1", "variablesReference": 0 },
                { "name": "y", "value": "2", "variablesReference": 0 },
            ] })),
            _ => out.fail(json!({})),
        },
        "evaluate" => out.ok(json!({ "result": "6", "type": "i32", "variablesReference": 0 })),
        "next" => {
            out.ok(json!({}));
            out.event("stopped", json!({ "reason": "step", "threadId": 1 }));
        }
        "continue" => {
            out.ok(json!({ "allThreadsContinued": true }));
            out.event("exited", json!({ "exitCode": 0 }));
            out.event("terminated", json!({}));
        }
        _ => out.ok(json!({})),
    }
}

fn launch() -> LaunchRequest {
    LaunchRequest {
        adapter_id: "fake".into(),
        request: "launch".into(),
        arguments: json!({ "program": "/p/target/debug/p" }),
    }
}

fn breakpoints_at(lines: &[u32]) -> Vec<FileBreakpoints> {
    vec![FileBreakpoints {
        path: "/p/src/main.rs".into(),
        breakpoints: lines
            .iter()
            .map(|&line| SourceBreakpoint {
                line,
                ..Default::default()
            })
            .collect(),
    }]
}

#[test]
fn a_whole_session_launch_stop_inspect_step_and_finish() {
    let mut fake = fake(simple_program);
    let started = block_on(s::start(&fake.adapter, &launch(), &breakpoints_at(&[3, 7]))).unwrap();
    assert!(started.capabilities.supports_configuration_done_request);
    let lines: Vec<_> = started.breakpoints[0].1.iter().map(|b| b.line).collect();
    assert_eq!(lines, [Some(3), Some(7)]);

    let mut state = SessionState::new();
    let output = loop {
        match fake.next_incoming() {
            Incoming::Event { event, body } => {
                if let Change::Output(output) = state.apply(&event, &body) {
                    break output;
                }
            }
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(output.output, "hello\n");
    let stopped = fake.event("stopped");
    assert_eq!(state.apply("stopped", &stopped), Change::Stopped);
    assert_eq!(state.stopped().unwrap().reason, "breakpoint");
    assert_eq!(state.stopped().unwrap().thread_id, Some(1));

    let threads = block_on(s::threads(&fake.adapter)).unwrap();
    assert_eq!(threads[0].name, "main");
    let frames = block_on(s::stack_trace(&fake.adapter, 1, 20)).unwrap();
    assert_eq!(frames[0].line, 3);
    assert_eq!(
        frames[0].source.as_ref().unwrap().path.as_deref(),
        Some(std::path::Path::new("/p/src/main.rs"))
    );
    assert_eq!(frames[1].presentation_hint.as_deref(), Some("subtle"));
    let scopes = block_on(s::scopes(&fake.adapter, frames[0].id)).unwrap();
    let locals = block_on(s::variables(&fake.adapter, scopes[0].variables_reference)).unwrap();
    assert_eq!(locals[0].name, "count");
    assert_eq!(locals[0].type_name.as_deref(), Some("i32"));
    let fields = block_on(s::variables(&fake.adapter, locals[1].variables_reference)).unwrap();
    assert_eq!(fields.len(), 2);
    let result = block_on(s::evaluate(&fake.adapter, "count * 2", Some(10), "watch")).unwrap();
    assert_eq!(result.result, "6");

    block_on(s::resume(&fake.adapter, 1, Step::Over)).unwrap();
    state.resumed();
    assert_eq!(state.run, RunState::Running);
    let stopped = fake.event("stopped");
    state.apply("stopped", &stopped);
    assert_eq!(state.stopped().unwrap().reason, "step");

    block_on(s::resume(&fake.adapter, 1, Step::Continue)).unwrap();
    state.resumed();
    let exited = fake.event("exited");
    assert_eq!(state.apply("exited", &exited), Change::Exited(0));
    assert_eq!(state.apply("terminated", &json!({})), Change::Ended);
    assert_eq!(state.run, RunState::Ended);

    let commands = fake.commands();
    let handshake: Vec<&str> = commands.iter().take(4).map(String::as_str).collect();
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

#[test]
fn launch_answered_only_after_configuration_done_does_not_deadlock() {
    // lldb-dap and js-debug hold the launch response until configuration is done.
    let held = Arc::new(Mutex::new(None::<i64>));
    let fake = fake(move |out| match out.command() {
        "initialize" => out.ok(json!({ "supportsConfigurationDoneRequest": true })),
        "launch" => {
            *held.lock().unwrap() = Some(out.seq);
            out.event("initialized", json!({}));
            out.later();
        }
        "configurationDone" => {
            out.ok(json!({}));
            let seq = held.lock().unwrap().take().unwrap();
            out.answer(seq, "launch");
        }
        _ => out.ok(json!({})),
    });
    block_on(s::start(&fake.adapter, &launch(), &[])).unwrap();
}

#[test]
fn initialized_sent_after_the_launch_response_still_configures() {
    // delve answers launch first, then says it's initialized.
    let fake = fake(|out| match out.command() {
        "initialize" => out.ok(json!({ "supportsConfigurationDoneRequest": true })),
        "launch" => {
            out.ok(json!({}));
            out.event("initialized", json!({}));
        }
        _ => out.ok(json!({ "breakpoints": [{ "verified": true, "line": 5 }] })),
    });
    let started = block_on(s::start(&fake.adapter, &launch(), &breakpoints_at(&[5]))).unwrap();
    assert!(started.breakpoints[0].1[0].verified);
    assert!(fake.commands().contains(&"configurationDone".to_string()));
}

#[test]
fn a_failed_launch_reports_the_adapters_formatted_reason() {
    let fake = fake(|out| match out.command() {
        "initialize" => out.ok(json!({})),
        "launch" => out.fail(json!({ "error": {
            "id": 3000, "format": "could not launch {program}: no such file",
            "variables": { "program": "/p/missing" },
        } })),
        _ => out.ok(json!({})),
    });
    let error = block_on(s::start(&fake.adapter, &launch(), &[])).unwrap_err();
    assert_eq!(
        error,
        RequestError::Adapter("could not launch /p/missing: no such file".into())
    );
}

#[test]
fn exception_filters_on_by_default_are_sent_and_configuration_done_only_if_supported() {
    let fake = fake(|out| match out.command() {
        "initialize" => {
            out.ok(json!({ "exceptionBreakpointFilters": [
                { "filter": "uncaught", "label": "Uncaught Exceptions", "default": true },
                { "filter": "all", "label": "All Exceptions", "default": false },
            ] }));
            out.event("initialized", json!({}));
        }
        _ => out.ok(json!({})),
    });
    block_on(s::start(&fake.adapter, &launch(), &[])).unwrap();
    let sent: Vec<Value> = fake.received.try_iter().collect();
    let filters = sent
        .iter()
        .find(|m| m["command"] == "setExceptionBreakpoints")
        .unwrap();
    assert_eq!(filters["arguments"]["filters"], json!(["uncaught"]));
    assert!(!sent.iter().any(|m| m["command"] == "configurationDone"));
}

#[test]
fn an_adapter_crash_fails_requests_in_flight_and_says_it_exited() {
    let mut fake = fake(|out| match out.command() {
        "threads" => out.die(),
        _ => out.ok(json!({})),
    });
    let error = block_on(s::threads(&fake.adapter)).unwrap_err();
    assert_eq!(error, RequestError::Gone);
    assert!(matches!(fake.next_incoming(), Incoming::Exited { .. }));
    assert!(!fake.adapter.is_alive());
    assert_eq!(
        block_on(s::threads(&fake.adapter)).unwrap_err(),
        RequestError::Gone
    );
    // The handshake can't hang waiting for `initialized` from an adapter that's gone.
    assert_eq!(
        block_on(fake.adapter.initialized()).unwrap_err(),
        RequestError::Gone
    );
}

#[test]
fn reverse_requests_reach_the_client_and_can_be_answered() {
    let mut fake = fake(|out| {
        if out.command() == "launch" {
            out.ok(json!({}));
            out.send(json!({
                "seq": 40, "type": "request", "command": "startDebugging",
                "arguments": { "request": "launch",
                               "configuration": { "__pendingTargetId": "abc" } },
            }));
        } else {
            out.ok(json!({}));
        }
    });
    block_on(fake.adapter.request("launch", json!({}))).unwrap();
    let Incoming::Request {
        seq,
        command,
        arguments,
    } = fake.next_incoming()
    else {
        panic!("expected a request")
    };
    assert_eq!((seq, command.as_str()), (40, "startDebugging"));
    let start: StartDebugging = serde_json::from_value(arguments).unwrap();
    assert_eq!(start.configuration["__pendingTargetId"], "abc");
    fake.adapter.respond(seq, "startDebugging", Ok(json!({})));
    std::thread::sleep(Duration::from_millis(50));
    let answer = fake
        .received
        .try_iter()
        .find(|m| m["type"] == "response")
        .unwrap();
    assert_eq!(answer["request_seq"], 40);
    assert_eq!(answer["success"], true);
}

#[test]
fn a_tcp_adapter_takes_a_second_connection_for_a_child_session() {
    // A listener standing in for js-debug, which serves each connection as its own session.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().take(2).enumerate() {
            let stream = stream.unwrap();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                while let Ok(Some(message)) = read_message(&mut reader) {
                    let reply = json!({
                        "seq": 0, "type": "response", "request_seq": message["seq"],
                        "success": true, "command": message["command"],
                        "body": { "connection": index },
                    });
                    write_message(&mut writer, &reply).unwrap();
                }
            });
        }
    });
    let (parent, _) = DebugAdapter::connect_to("tcp", address).unwrap();
    let (child, _) = parent.connect_again("tcp child").unwrap();
    let parent_answer = block_on(parent.request("threads", json!({}))).unwrap();
    let child_answer = block_on(child.request("threads", json!({}))).unwrap();
    assert_eq!(parent_answer["connection"], 0);
    assert_eq!(child_answer["connection"], 1);
}

#[test]
fn a_stdio_adapter_cannot_start_child_sessions() {
    let fake = fake(|out| out.ok(json!({})));
    assert!(fake.adapter.connect_again("child").is_err());
}

#[test]
fn state_ignores_continued_after_the_end_and_bad_bodies() {
    let mut state = SessionState::new();
    assert_eq!(state.apply("process", &json!({})), Change::Continued);
    assert_eq!(state.apply("stopped", &json!("nonsense")), Change::Nothing);
    assert_eq!(state.apply("terminated", &Value::Null), Change::Ended);
    assert_eq!(state.apply("continued", &json!({})), Change::Nothing);
    state.resumed();
    assert_eq!(state.run, RunState::Ended);
}
