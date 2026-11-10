//! `gleipnir mcp` as agents run it: the built binary, piping its stdin and stdout to a socket.
//! A small server here stands in for a window, answering with the same protocol handling.

use gleipnir_agent::rpc::{Session, Step, handle_line};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn gleipnir() -> Command {
    Command::new(env!("CARGO_BIN_EXE_gleipnir"))
}

/// A short folder for a socket, as the system's temporary folder can be too long for one.
fn socket_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("gm")
        .tempdir_in("/tmp")
        .unwrap()
}

/// Answers one client the way a window does, without tools: any tool call gets an error.
fn serve_once(listener: UnixListener) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut session = Session::default();
        for line in BufReader::new(stream).lines() {
            if let Step::Reply(answer) = handle_line(&mut session, &line.unwrap()) {
                writeln!(writer, "{answer}").unwrap();
            }
        }
    })
}

#[test]
fn gleipnir_mcp_pipes_an_agent_to_the_window() {
    let dir = socket_dir();
    let socket: PathBuf = dir.path().join("window.sock");
    let server = serve_once(UnixListener::bind(&socket).unwrap());
    let mut child = gleipnir()
        .arg("mcp")
        .env("GLEIPNIR_SOCKET", &socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"test"}}}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    // The answer comes as soon as it's ready, with the agent still connected.
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    let answer: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(answer["result"]["serverInfo"]["name"], "gleipnir");
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"ping"}}"#).unwrap();
    // The agent going away ends it.
    drop(stdin);
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let lines: Vec<serde_json::Value> = rest
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines,
        [serde_json::json!({ "jsonrpc": "2.0", "id": 2, "result": {} })]
    );
    assert!(child.wait().unwrap().success());
    server.join().unwrap();
}

#[test]
fn gleipnir_mcp_says_when_there_is_no_window() {
    let dir = socket_dir();
    let output = gleipnir()
        .args(["mcp", "--socket"])
        .arg(dir.path().join("gone.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("no Gleipnir window is listening"), "{error}");

    let output = gleipnir()
        .arg("mcp")
        .env_remove("GLEIPNIR_SOCKET")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("GLEIPNIR_SOCKET isn't set"), "{error}");
}
