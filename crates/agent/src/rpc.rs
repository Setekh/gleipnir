//! MCP as its stdio transport speaks it: JSON-RPC 2.0, one message per line. Everything here is
//! plain data in and out, so a connection's thread answers what needs no IDE, and only tool calls
//! go to the window.

use serde_json::{Value, json};

/// The protocol versions we answer to, newest first. Tools work the same in each.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What we offer a client asking for a version we don't know, which it may accept or not.
pub const DEFAULT_VERSION: &str = "2025-06-18";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// What one connection has agreed.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub protocol_version: Option<String>,
    /// The client's name and version, as it gave them.
    pub client: Option<String>,
    /// Whether the client has said it's ready, after `initialize`.
    pub initialized: bool,
}

/// What a line asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// An answer to send straight back.
    Reply(Value),
    /// A tool to run in the IDE, answered with `tool_result` once it's done.
    Call {
        id: Value,
        name: String,
        arguments: Value,
    },
    /// A notification, or an answer to us: nothing to send.
    Nothing,
}

/// What a tool hands back: data, and for tools that read text, the text itself, which reads
/// better on its own than escaped inside JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOutput {
    Json(Value),
    JsonAndText(Value, String),
}

pub fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() },
    })
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The answer to a tool call. A tool that fails says why as its result, marked as an error, so
/// the agent reads it and can try again differently.
pub fn tool_result(id: Value, outcome: Result<ToolOutput, String>) -> Value {
    let text = |text: String| json!({ "type": "text", "text": text });
    let (content, is_error) = match outcome {
        Ok(ToolOutput::Json(value)) => (vec![text(value.to_string())], false),
        Ok(ToolOutput::JsonAndText(value, body)) => {
            (vec![text(value.to_string()), text(body)], false)
        }
        Err(message) => (vec![text(message)], true),
    };
    result(id, json!({ "content": content, "isError": is_error }))
}

/// Handles one line from a client.
pub fn handle_line(session: &mut Session, line: &str) -> Step {
    let line = line.trim();
    if line.is_empty() {
        return Step::Nothing;
    }
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(e) => return Step::Reply(error(Value::Null, PARSE_ERROR, format!("Parse error: {e}"))),
    };
    if message.is_array() {
        return Step::Reply(error(
            Value::Null,
            INVALID_REQUEST,
            "Batches aren't supported: send one message per line",
        ));
    }
    let Some(object) = message.as_object() else {
        return Step::Reply(error(
            Value::Null,
            INVALID_REQUEST,
            "A message must be a JSON object",
        ));
    };
    let id = object.get("id").cloned();
    let valid_id = |id: &Value| id.is_string() || id.is_i64() || id.is_u64();
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        let id = id.filter(valid_id).unwrap_or(Value::Null);
        return Step::Reply(error(id, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // We never ask clients anything, so an answer can only be stray; a message with neither
        // is malformed.
        if object.contains_key("result") || object.contains_key("error") {
            return Step::Nothing;
        }
        let id = id.filter(valid_id).unwrap_or(Value::Null);
        return Step::Reply(error(id, INVALID_REQUEST, "A request needs a method"));
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        if method == "notifications/initialized" {
            session.initialized = true;
        }
        // Others, like cancellation, need nothing from us: a cancelled call's answer is ignored.
        return Step::Nothing;
    };
    if !valid_id(&id) {
        return Step::Reply(error(
            Value::Null,
            INVALID_REQUEST,
            "A request's id must be a string or an integer",
        ));
    }
    // Tools answer before `initialize` too. Clients aren't meant to ask then, and refusing would
    // gain nothing.
    match method {
        "initialize" => initialize(session, id, &params),
        "ping" => Step::Reply(result(id, json!({}))),
        "tools/list" => Step::Reply(result(id, json!({ "tools": crate::tools::list() }))),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Step::Reply(error(id, INVALID_PARAMS, "tools/call needs a tool name"));
            };
            let arguments = match params.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(arguments) if arguments.is_object() => arguments.clone(),
                Some(_) => {
                    return Step::Reply(error(
                        id,
                        INVALID_PARAMS,
                        "A tool's arguments must be an object",
                    ));
                }
            };
            if !crate::tools::is_tool(name) {
                return Step::Reply(error(id, INVALID_PARAMS, format!("Unknown tool: {name}")));
            }
            Step::Call {
                id,
                name: name.to_string(),
                arguments,
            }
        }
        _ => Step::Reply(error(
            id,
            METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        )),
    }
}

fn initialize(session: &mut Session, id: Value, params: &Value) -> Step {
    let Some(asked) = params.get("protocolVersion").and_then(Value::as_str) else {
        return Step::Reply(error(
            id,
            INVALID_PARAMS,
            "initialize needs a protocolVersion",
        ));
    };
    // The client's version if we speak it; otherwise ours, for the client to accept or not.
    let version = if PROTOCOL_VERSIONS.contains(&asked) {
        asked
    } else {
        DEFAULT_VERSION
    };
    session.protocol_version = Some(version.to_string());
    session.client = params.get("clientInfo").map(|info| {
        let name = info["name"].as_str().unwrap_or("unknown");
        match info["version"].as_str() {
            Some(version) => format!("{name} {version}"),
            None => name.to_string(),
        }
    });
    Step::Reply(result(
        id,
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "gleipnir",
                "title": "Gleipnir",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": crate::INSTRUCTIONS,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(session: &mut Session, line: &str) -> Value {
        match handle_line(session, line) {
            Step::Reply(value) => value,
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    #[test]
    fn initialize_agrees_a_version_and_says_what_gleipnir_is() {
        let mut session = Session::default();
        let answer = reply(
            &mut session,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"claude-code","version":"2.1"}}}"#,
        );
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(answer["result"]["serverInfo"]["name"], "gleipnir");
        assert!(answer["result"]["capabilities"]["tools"].is_object());
        assert!(
            answer["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("get_selection")
        );
        assert_eq!(session.client.as_deref(), Some("claude-code 2.1"));
        assert!(!session.initialized);
        let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(handle_line(&mut session, initialized), Step::Nothing);
        assert!(session.initialized);
    }

    #[test]
    fn an_unknown_version_gets_ours_and_older_ones_are_kept() {
        let mut session = Session::default();
        let ask = |v: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":"a","method":"initialize","params":{{"protocolVersion":"{v}"}}}}"#
            )
        };
        let answer = reply(&mut session, &ask("2099-01-01"));
        assert_eq!(answer["result"]["protocolVersion"], DEFAULT_VERSION);
        let answer = reply(&mut session, &ask("2024-11-05"));
        assert_eq!(answer["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(answer["id"], "a");
        let missing = r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}"#;
        assert_eq!(
            reply(&mut session, missing)["error"]["code"],
            INVALID_PARAMS
        );
    }

    #[test]
    fn tools_are_listed_with_their_schemas() {
        let mut session = Session::default();
        let answer = reply(
            &mut session,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        );
        let tools = answer["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for name in [
            "get_open_files",
            "get_selection",
            "get_problems",
            "get_test_results",
            "get_git_status",
            "find_definition",
            "find_usages",
            "read_file",
            "open_file",
            "edit_file",
            "save_file",
            "run_tests",
            "run_configuration",
            "propose_edit",
            "get_proposal",
        ] {
            assert!(names.contains(&name), "{name} is missing from {names:?}");
        }
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            assert!(tool["description"].as_str().unwrap().len() > 40, "{tool}");
        }
    }

    #[test]
    fn a_tool_call_goes_to_the_ide_with_its_arguments() {
        let mut session = Session::default();
        let step = handle_line(
            &mut session,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"a.rs"}}}"#,
        );
        assert_eq!(
            step,
            Step::Call {
                id: json!(7),
                name: "read_file".into(),
                arguments: json!({ "path": "a.rs" }),
            }
        );
        // No arguments at all is an empty object.
        let step = handle_line(
            &mut session,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"get_selection"}}"#,
        );
        assert!(matches!(step, Step::Call { arguments, .. } if arguments == json!({})));
    }

    #[test]
    fn mistakes_get_the_errors_json_rpc_names() {
        let mut session = Session::default();
        let code =
            |session: &mut Session, line: &str| reply(session, line)["error"]["code"].clone();
        assert_eq!(code(&mut session, "{not json"), PARSE_ERROR);
        assert_eq!(code(&mut session, "[1, 2]"), INVALID_REQUEST);
        assert_eq!(code(&mut session, "42"), INVALID_REQUEST);
        assert_eq!(
            code(&mut session, r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
            INVALID_REQUEST
        );
        assert_eq!(
            code(
                &mut session,
                r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#
            ),
            INVALID_REQUEST
        );
        assert_eq!(
            code(
                &mut session,
                r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#
            ),
            METHOD_NOT_FOUND
        );
        assert_eq!(
            code(
                &mut session,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"rm_rf"}}"#
            ),
            INVALID_PARAMS
        );
        assert_eq!(
            code(
                &mut session,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_file","arguments":[1]}}"#
            ),
            INVALID_PARAMS
        );
        assert_eq!(
            code(
                &mut session,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#
            ),
            INVALID_PARAMS
        );
        // The id comes back with the error, so the client knows which request failed.
        let answer = reply(
            &mut session,
            r#"{"jsonrpc":"2.0","id":"x","method":"nope"}"#,
        );
        assert_eq!(answer["id"], "x");
    }

    #[test]
    fn notifications_and_stray_answers_get_no_reply() {
        let mut session = Session::default();
        for line in [
            "",
            "   ",
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
            r#"{"jsonrpc":"2.0","method":"anything/else"}"#,
            r#"{"jsonrpc":"2.0","id":5,"result":{}}"#,
        ] {
            assert_eq!(handle_line(&mut session, line), Step::Nothing, "{line}");
        }
        assert_eq!(
            reply(&mut session, r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#)["result"],
            json!({})
        );
    }

    #[test]
    fn tool_results_carry_text_and_say_when_they_failed() {
        let ok = tool_result(json!(1), Ok(ToolOutput::Json(json!({ "a": 1 }))));
        assert_eq!(ok["result"]["isError"], false);
        assert_eq!(ok["result"]["content"][0]["type"], "text");
        assert_eq!(ok["result"]["content"][0]["text"], r#"{"a":1}"#);
        let both = tool_result(
            json!(2),
            Ok(ToolOutput::JsonAndText(
                json!({ "lines": 1 }),
                "fn x() {}\n".into(),
            )),
        );
        assert_eq!(both["result"]["content"][1]["text"], "fn x() {}\n");
        let failed = tool_result(json!(3), Err("old_text wasn't found".into()));
        assert_eq!(failed["result"]["isError"], true);
        assert_eq!(
            failed["result"]["content"][0]["text"],
            "old_text wasn't found"
        );
        assert_eq!(failed["id"], 3);
    }
}
