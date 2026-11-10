//! Opening a file in a Gleipnir that's already running, as when Unity or a terminal asks for one:
//! the window whose project holds the file shows it and comes to the front, rather than another
//! Gleipnir starting. Each window is asked in turn through its socket; one whose project doesn't
//! hold the file refuses, as its tools only reach their own project's files.

use serde_json::{Value, json};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// How long a window may take to answer before the next is tried, or a new Gleipnir starts.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Shows a file, at a line and column counted from 1, in a running window whose project holds it.
/// Returns whether one did.
pub fn open_in_running_window(path: &Path, line: Option<u32>, column: Option<u32>) -> bool {
    let mut arguments = json!({ "path": path.to_string_lossy(), "activate": true });
    if let Some(line) = line {
        arguments["line"] = json!(line.max(1));
        if let Some(column) = column {
            arguments["column"] = json!(column.max(1));
        }
    }
    crate::socket::live_sockets(&crate::socket::socket_dir())
        .iter()
        .any(|socket| call(socket, "open_file", &arguments).is_some_and(|ok| ok))
}

/// Calls a tool on a window's socket, returning whether it succeeded, or nothing if the window
/// didn't answer.
fn call(socket: &Path, tool: &str, arguments: &Value) -> Option<bool> {
    let stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(TIMEOUT)).ok()?;
    let mut writer = stream.try_clone().ok()?;
    let messages = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "gleipnir-open", "version": env!("CARGO_PKG_VERSION") },
            },
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }),
    ];
    for message in messages {
        writeln!(writer, "{message}").ok()?;
    }
    writer.flush().ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let Ok(answer) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if answer["id"] == 2 {
            return Some(answer["result"]["isError"] == false);
        }
    }
}
