//! Each window's socket, which only the person running Gleipnir can open: it's in a folder only
//! they can enter, and itself only they can read or write. A thread accepts connections, and
//! each connection gets a thread reading it and one writing to it, so a slow client holds up no
//! one else. What needs no IDE, like `initialize`, is answered on the connection's thread; tool
//! calls go to the window and are answered when they're done, in whatever order they finish.

use crate::rpc::{self, Step, ToolOutput};
use futures::channel::mpsc::UnboundedSender;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// The longest path a Unix socket can have: macOS has 104 bytes for it, Linux 108, both with a
/// terminating zero.
const MAX_SOCKET_PATH: usize = 103;

/// A tool call from a client, to run in the window.
pub struct Call {
    pub id: Value,
    pub name: String,
    pub arguments: Value,
    reply: mpsc::Sender<String>,
}

impl Call {
    /// Sends the tool's answer back to the client that asked.
    pub fn answer(self, outcome: Result<ToolOutput, String>) {
        let line = rpc::tool_result(self.id, outcome).to_string();
        self.reply.send(line).ok();
    }
}

fn uid() -> u32 {
    // SAFETY: getuid can't fail and has no preconditions.
    unsafe { libc::getuid() }
}

/// Where sockets go: a short folder in `/tmp`, as the platforms' temporary folders and the state
/// folder can be too long for a socket's path. `GLEIPNIR_SOCKET_DIR` chooses another, for tests.
pub fn socket_dir() -> PathBuf {
    match std::env::var_os("GLEIPNIR_SOCKET_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(format!("/tmp/gleipnir-{}", uid())),
    }
}

/// Makes the sockets' folder, or checks the one there: a real folder, not a link, that belongs to
/// us, which only we can enter.
pub fn prepare_dir(dir: &Path) -> io::Result<()> {
    if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(dir)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error);
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} isn't a folder, so it can't hold Gleipnir's sockets",
            dir.display()
        )));
    }
    if meta.uid() != uid() {
        return Err(io::Error::other(format!(
            "{} belongs to someone else, so Gleipnir won't put its sockets there",
            dir.display()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks the process exists and could be signalled.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Removes sockets left by Gleipnirs that ended without removing theirs, as after a crash. A
/// socket's name starts with the process id that made it.
pub fn remove_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let me = std::process::id() as i32;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".sock"))
            .and_then(|n| n.split('-').next())
            .and_then(|pid| pid.parse::<i32>().ok())
        else {
            continue;
        };
        if pid != me && pid > 0 && !alive(pid) {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

/// Numbers this process's sockets, one per window.
static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

/// This process's sockets, removed when it exits, even by `exit` without the app quitting
/// properly. Only a crash leaves them, for the next start to clear.
static LIVE_SOCKETS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

extern "C" fn remove_live_sockets() {
    // Never waits: a thread holding the lock as the process ends would hang it.
    if let Ok(paths) = LIVE_SOCKETS.try_lock() {
        for path in paths.iter() {
            std::fs::remove_file(path).ok();
        }
    }
}

fn remember(path: &Path) {
    static AT_EXIT: std::sync::Once = std::sync::Once::new();
    AT_EXIT.call_once(|| {
        // SAFETY: the handler only touches statics and the file system.
        unsafe { libc::atexit(remove_live_sockets) };
    });
    LIVE_SOCKETS.lock().unwrap().push(path.to_path_buf());
}

fn forget(path: &Path) {
    LIVE_SOCKETS.lock().unwrap().retain(|p| p != path);
}
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

/// A window's socket, listening. Dropping it stops listening, ends its connections and removes
/// the socket.
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, UnixStream>>>,
}

impl Server {
    /// Listens on a new socket in `dir`, sending tool calls to `calls`.
    pub fn start(dir: &Path, calls: UnboundedSender<Call>) -> io::Result<Self> {
        prepare_dir(dir)?;
        remove_stale(dir);
        let name = format!(
            "{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
        let path = dir.join(name);
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(io::Error::other(format!(
                "{} is too long for a socket's path",
                path.display()
            )));
        }
        // One left by an earlier process that had our id.
        std::fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        remember(&path);
        let stop = Arc::new(AtomicBool::new(false));
        let connections: Arc<Mutex<HashMap<u64, UnixStream>>> = Arc::default();
        let (accept_stop, accept_connections) = (stop.clone(), connections.clone());
        std::thread::Builder::new()
            .name("mcp-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if accept_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        // Out of file descriptors, say: let it pass rather than spin.
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    };
                    let id = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
                    if let Ok(clone) = stream.try_clone() {
                        accept_connections.lock().unwrap().insert(id, clone);
                    }
                    let (calls, connections) = (calls.clone(), accept_connections.clone());
                    std::thread::Builder::new()
                        .name("mcp-connection".into())
                        .spawn(move || {
                            serve(stream, calls);
                            connections.lock().unwrap().remove(&id);
                        })
                        .ok();
                }
            })?;
        Ok(Self {
            path,
            stop,
            connections,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many clients are connected.
    #[cfg(test)]
    pub fn connection_count(&self) -> usize {
        self.connections.lock().unwrap().len()
    }

    fn shutdown(&mut self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // Wakes the accepting thread, which then sees it should stop.
        UnixStream::connect(&self.path).ok();
        for (_, stream) in self.connections.lock().unwrap().drain() {
            stream.shutdown(std::net::Shutdown::Both).ok();
        }
        std::fs::remove_file(&self.path).ok();
        forget(&self.path);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reads one client's messages until it closes, answering each.
fn serve(stream: UnixStream, calls: UnboundedSender<Call>) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let (reply, replies) = mpsc::channel::<String>();
    // Ends once the reader and every call still running have let go of `reply`.
    std::thread::Builder::new()
        .name("mcp-writer".into())
        .spawn(move || {
            for line in replies {
                if writer
                    .write_all(line.as_bytes())
                    .and_then(|()| writer.write_all(b"\n"))
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
            writer.shutdown(std::net::Shutdown::Write).ok();
        })
        .ok();
    let mut session = rpc::Session::default();
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        match rpc::handle_line(&mut session, &text) {
            Step::Reply(answer) => {
                reply.send(answer.to_string()).ok();
            }
            Step::Call {
                id,
                name,
                arguments,
            } => {
                let call = Call {
                    id: id.clone(),
                    name,
                    arguments,
                    reply: reply.clone(),
                };
                if calls.unbounded_send(call).is_err() {
                    let closed = rpc::error(id, rpc::INTERNAL_ERROR, "The Gleipnir window closed");
                    reply.send(closed.to_string()).ok();
                }
            }
            Step::Nothing => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;
    use std::io::Read as _;

    fn short_dir() -> tempfile::TempDir {
        // The system's temporary folder can be too long for a socket on macOS.
        tempfile::Builder::new()
            .prefix("gs")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn the_folder_and_socket_are_only_ours() {
        let dir = short_dir();
        let sockets = dir.path().join("s");
        let (calls, _receiver) = futures::channel::mpsc::unbounded();
        let server = Server::start(&sockets, calls).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&sockets), 0o700);
        assert_eq!(mode(server.path()), 0o600);
        let name = server.path().file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(&format!("{}-", std::process::id())),
            "{name}"
        );
        let path = server.path().to_path_buf();
        drop(server);
        assert!(!path.exists(), "the socket is removed when the server goes");
    }

    #[test]
    fn a_loose_folder_is_tightened_and_a_link_refused() {
        let dir = short_dir();
        let loose = dir.path().join("loose");
        std::fs::create_dir(&loose).unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o777)).unwrap();
        prepare_dir(&loose).unwrap();
        assert_eq!(
            std::fs::metadata(&loose).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&loose, &link).unwrap();
        assert!(prepare_dir(&link).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(prepare_dir(&file).is_err());
    }

    #[test]
    fn sockets_of_ended_processes_are_removed() {
        let dir = short_dir();
        // A process that has certainly ended: one we started and waited for.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let stale = dir.path().join(format!("{dead}-1.sock"));
        let mine = dir.path().join(format!("{}-99.sock", std::process::id()));
        let other = dir.path().join("notes.txt");
        for path in [&stale, &mine, &other] {
            std::fs::write(path, "").unwrap();
        }
        remove_stale(dir.path());
        assert!(!stale.exists());
        assert!(mine.exists());
        assert!(other.exists());
    }

    #[test]
    fn several_clients_are_answered_and_tool_calls_reach_the_window() {
        let dir = short_dir();
        let (calls, mut receiver) = futures::channel::mpsc::unbounded();
        let server = Server::start(dir.path(), calls).unwrap();
        // Stands in for the window: answers each call with its name.
        std::thread::spawn(move || {
            futures::executor::block_on(async {
                while let Some(call) = receiver.next().await {
                    let name = call.name.clone();
                    call.answer(Ok(ToolOutput::Json(serde_json::json!({ "ran": name }))));
                }
            })
        });
        let clients: Vec<_> = (0..3)
            .map(|n| {
                let path = server.path().to_path_buf();
                std::thread::spawn(move || {
                    let mut stream = UnixStream::connect(&path).unwrap();
                    let lines = format!(
                        "{}\n{}\n",
                        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
                        format_args!(
                            r#"{{"jsonrpc":"2.0","id":{n},"method":"tools/call","params":{{"name":"get_selection"}}}}"#
                        )
                    );
                    stream.write_all(lines.as_bytes()).unwrap();
                    stream.shutdown(std::net::Shutdown::Write).unwrap();
                    let answers: Vec<Value> = BufReader::new(stream)
                        .lines()
                        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
                        .collect();
                    (n, answers)
                })
            })
            .collect();
        for client in clients {
            let (n, answers) = client.join().unwrap();
            assert_eq!(answers.len(), 2, "{answers:?}");
            assert_eq!(answers[0]["result"], serde_json::json!({}));
            assert_eq!(answers[1]["id"], n);
            assert_eq!(
                answers[1]["result"]["content"][0]["text"],
                r#"{"ran":"get_selection"}"#
            );
        }
        // A client still connected is cut off when the window goes.
        let mut idle = UnixStream::connect(server.path()).unwrap();
        while server.connection_count() == 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(server);
        let mut rest = String::new();
        assert_eq!(idle.read_to_string(&mut rest).unwrap(), 0);
    }
}
