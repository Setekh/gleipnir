//! `gleipnir mcp`: what an agent starts as its MCP server. It's only a pipe to a window's socket,
//! copying the agent's messages there and the answers back, so it starts at once and holds no
//! state; the window does the work. It ends when either side closes.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const USAGE: &str = "Usage: gleipnir mcp [--socket <path>]

Connects an AI agent to a Gleipnir window as an MCP server, over stdin and stdout.
The window's socket is GLEIPNIR_SOCKET, which agents started in Gleipnir's terminal
are given, or the --socket path.";

/// Runs `gleipnir mcp` with the arguments after `mcp`, returning the exit code.
pub fn main(args: Vec<OsString>) -> i32 {
    let socket = match socket_from(args, std::env::var_os("GLEIPNIR_SOCKET")) {
        Ok(Some(socket)) => socket,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(message) => {
            eprintln!("gleipnir mcp: {message}");
            return 2;
        }
    };
    let stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!(
                "gleipnir mcp: no Gleipnir window is listening at {} ({error}). Is Gleipnir \
                 running, with this project's window open? Each window has its own socket, and \
                 it changes when Gleipnir restarts, so restart the agent from Gleipnir's terminal.",
                socket.display()
            );
            return 1;
        }
    };
    match pipe(stream, io::stdin(), io::stdout()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("gleipnir mcp: the connection to Gleipnir failed: {error}");
            1
        }
    }
}

/// The socket to connect to, from `--socket` or else the environment; `None` asks for help.
fn socket_from(args: Vec<OsString>, env: Option<OsString>) -> Result<Option<PathBuf>, String> {
    let mut socket = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--socket") => match args.next() {
                Some(path) => socket = Some(PathBuf::from(path)),
                None => return Err("--socket needs a path".into()),
            },
            Some("--help" | "-h") => return Ok(None),
            _ => {
                return Err(format!(
                    "unexpected argument {}\n\n{USAGE}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    match socket.or(env.filter(|s| !s.is_empty()).map(PathBuf::from)) {
        Some(socket) => Ok(Some(socket)),
        None => Err(
            "GLEIPNIR_SOCKET isn't set. Start the agent from Gleipnir's terminal, which sets \
             it, or pass --socket <path>."
                .into(),
        ),
    }
}

/// Copies `input` to the socket and the socket to `output` until the socket closes. When the
/// input ends, the socket hears so, and answers still owed arrive before it closes.
pub fn pipe(
    stream: UnixStream,
    mut input: impl Read + Send + 'static,
    mut output: impl Write,
) -> io::Result<()> {
    let mut to_socket = stream.try_clone()?;
    std::thread::spawn(move || {
        io::copy(&mut input, &mut to_socket).ok();
        to_socket.shutdown(std::net::Shutdown::Write).ok();
    });
    let mut from_socket = stream;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = match from_socket.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        output.write_all(&buffer[..read])?;
        // Each answer goes as soon as it arrives, as the agent waits for it.
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_socket_comes_from_the_flag_or_the_environment() {
        let env = Some(OsString::from("/tmp/env.sock"));
        assert_eq!(
            socket_from(args(&[]), env.clone()),
            Ok(Some(PathBuf::from("/tmp/env.sock")))
        );
        assert_eq!(
            socket_from(args(&["--socket", "/tmp/flag.sock"]), env),
            Ok(Some(PathBuf::from("/tmp/flag.sock")))
        );
        assert!(
            socket_from(args(&[]), None)
                .unwrap_err()
                .contains("GLEIPNIR_SOCKET isn't set")
        );
        assert!(
            socket_from(args(&["--socket"]), None)
                .unwrap_err()
                .contains("needs a path")
        );
        assert!(
            socket_from(args(&["--verbose"]), None)
                .unwrap_err()
                .contains("unexpected argument --verbose")
        );
        assert_eq!(socket_from(args(&["--help"]), None), Ok(None));
    }

    #[test]
    fn the_pipe_carries_both_ways_and_ends_with_the_socket() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        // An echoing server, which closes once the client has finished sending.
        std::thread::spawn(move || {
            let mut reader = theirs.try_clone().unwrap();
            let mut writer = theirs;
            io::copy(&mut reader, &mut writer).unwrap();
        });
        let input = io::Cursor::new(b"{\"a\":1}\n{\"b\":2}\n".to_vec());
        let mut output = Vec::new();
        pipe(ours, input, &mut output).unwrap();
        assert_eq!(output, b"{\"a\":1}\n{\"b\":2}\n");
    }
}
