use crate::transport::{read_message, write_message};
use anyhow::{Context as _, Result, bail};
use futures::channel::{mpsc, oneshot};
use futures::future::{FutureExt as _, Shared};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

/// Something the adapter sent that wasn't a response to us.
#[derive(Debug)]
pub enum Incoming {
    Event {
        event: String,
        body: Value,
    },
    /// A request the client answers itself, like js-debug's `startDebugging`. Answer it with
    /// `DebugAdapter::respond`.
    Request {
        seq: i64,
        command: String,
        arguments: Value,
    },
    /// The connection closed. The adapter's last lines of output explain why, if it said.
    Exited {
        stderr: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The adapter refused, with its reason.
    Adapter(String),
    /// The adapter isn't running any more.
    Gone,
    /// What came back didn't match what the request returns.
    BadResponse(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Adapter(message) => write!(f, "{message}"),
            Self::Gone => write!(f, "the debugger stopped"),
            Self::BadResponse(error) => {
                write!(f, "the debugger's answer didn't make sense: {error}")
            }
        }
    }
}

impl std::error::Error for RequestError {}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RequestError>>>>>;

const OUTPUT_LINES: usize = 40;
/// How long a TCP adapter gets to start listening.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How to start an adapter.
#[derive(Clone, Debug)]
pub struct AdapterCommand {
    pub program: PathBuf,
    /// `{port}` is replaced by a free port for TCP adapters.
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// Connect over TCP rather than talking on stdin and stdout.
    pub tcp: bool,
}

/// The adapter's process, shared by every connection to it, and killed when the last one goes.
struct Process {
    child: Mutex<Option<Child>>,
    /// The last lines it printed, for explaining a crash.
    output: Arc<Mutex<VecDeque<String>>>,
}

impl Process {
    fn output(&self) -> String {
        let output = self.output.lock().expect("the output lock");
        output.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    fn exited(&self) -> bool {
        let mut child = self.child.lock().expect("the child's lock");
        child
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_some())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let Some(mut child) = self.child.lock().expect("the child's lock").take() else {
            return;
        };
        // A disconnected adapter usually exits by itself; give it a moment before insisting.
        thread::spawn(move || {
            for _ in 0..20 {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            child.kill().ok();
            child.wait().ok();
        });
    }
}

fn keep_tail(reader: impl Read + Send + 'static, tail: Arc<Mutex<VecDeque<String>>>, name: String) {
    thread::Builder::new()
        .name(name)
        .spawn(move || {
            use std::io::BufRead as _;
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let mut tail = tail.lock().expect("the output lock");
                if tail.len() == OUTPUT_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        })
        .expect("a thread for the adapter's output");
}

/// A connection to a debug adapter.
pub struct DebugAdapter {
    name: String,
    outgoing: mpsc::UnboundedSender<Value>,
    pending: Pending,
    next_seq: Arc<AtomicI64>,
    alive: Arc<AtomicBool>,
    /// Where a TCP adapter listens, so child sessions can connect again.
    address: Option<SocketAddr>,
    process: Option<Arc<Process>>,
    initialized: Shared<oneshot::Receiver<()>>,
}

pub type Connected = (Arc<DebugAdapter>, mpsc::UnboundedReceiver<Incoming>);

impl DebugAdapter {
    /// Starts an adapter and connects to it. Blocks while a TCP adapter starts listening, so call
    /// it off the UI thread.
    pub fn start(name: &str, command: &AdapterCommand) -> Result<Connected> {
        let port = if command.tcp {
            Some(free_port()?)
        } else {
            None
        };
        let args: Vec<String> = command
            .args
            .iter()
            .map(|a| match port {
                Some(port) => a.replace("{port}", &port.to_string()),
                None => a.clone(),
            })
            .collect();
        let mut child = Command::new(&command.program)
            .args(&args)
            .env_clear()
            .envs(command.env.iter().cloned())
            .current_dir(&command.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("couldn't start {}", command.program.display()))?;
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let stderr = child.stderr.take().expect("stderr is piped");
        keep_tail(stderr, output.clone(), format!("dap-{name}-stderr"));

        let Some(port) = port else {
            let stdin = child.stdin.take().expect("stdin is piped");
            let stdout = child.stdout.take().expect("stdout is piped");
            let process = Arc::new(Process {
                child: Mutex::new(Some(child)),
                output,
            });
            return Ok(Self::connect(name, stdout, stdin, None, Some(process)));
        };

        let stdout = child.stdout.take().expect("stdout is piped");
        keep_tail(stdout, output.clone(), format!("dap-{name}-stdout"));
        let process = Arc::new(Process {
            child: Mutex::new(Some(child)),
            output,
        });
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let started = Instant::now();
        let stream = loop {
            match TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
                Ok(stream) => break stream,
                Err(error) => {
                    if process.exited() {
                        bail!("{name} stopped before it was ready: {}", process.output());
                    }
                    if started.elapsed() > CONNECT_TIMEOUT {
                        bail!("{name} never started listening: {error}");
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        };
        Self::over_tcp(name, stream, address, Some(process))
    }

    fn over_tcp(
        name: &str,
        stream: TcpStream,
        address: SocketAddr,
        process: Option<Arc<Process>>,
    ) -> Result<Connected> {
        stream.set_nodelay(true).ok();
        let reader = stream.try_clone()?;
        Ok(Self::connect(name, reader, stream, Some(address), process))
    }

    /// Connects to an adapter that's already listening, which nothing here started.
    pub fn connect_to(name: &str, address: SocketAddr) -> Result<Connected> {
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .with_context(|| format!("couldn't connect to {name} at {address}"))?;
        Self::over_tcp(name, stream, address, None)
    }

    /// Another connection to the same adapter, for a child session. Only TCP adapters have one.
    pub fn connect_again(&self, name: &str) -> Result<Connected> {
        let Some(address) = self.address else {
            bail!("{} can't start another session", self.name)
        };
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .with_context(|| format!("couldn't connect to {} again", self.name))?;
        Self::over_tcp(name, stream, address, self.process.clone())
    }

    /// Talks to an adapter over any pair of streams. Tests use this with an in-process fake.
    pub fn from_streams(
        name: &str,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Connected {
        Self::connect(name, reader, writer, None, None)
    }

    fn connect(
        name: &str,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        address: Option<SocketAddr>,
        process: Option<Arc<Process>>,
    ) -> Connected {
        let (outgoing, mut outgoing_rx) = mpsc::unbounded::<Value>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded::<Incoming>();
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (initialized_tx, initialized_rx) = oneshot::channel();
        let adapter = Arc::new(Self {
            name: name.to_string(),
            outgoing,
            pending: pending.clone(),
            next_seq: Arc::new(AtomicI64::new(1)),
            alive: alive.clone(),
            address,
            process: process.clone(),
            initialized: initialized_rx.shared(),
        });

        let mut writer = writer;
        thread::Builder::new()
            .name(format!("dap-{name}-write"))
            .spawn(move || {
                while let Some(message) =
                    futures::executor::block_on(futures::StreamExt::next(&mut outgoing_rx))
                {
                    if write_message(&mut writer, &message).is_err() {
                        break;
                    }
                }
            })
            .expect("a thread for writing to the adapter");

        thread::Builder::new()
            .name(format!("dap-{name}-read"))
            .spawn(move || {
                let mut reader = BufReader::new(reader);
                let mut initialized = Some(initialized_tx);
                while let Ok(Some(message)) = read_message(&mut reader) {
                    dispatch(message, &pending, &incoming_tx, &mut initialized);
                }
                alive.store(false, Ordering::SeqCst);
                for (_, waiter) in pending.lock().expect("the pending lock").drain() {
                    waiter.send(Err(RequestError::Gone)).ok();
                }
                // Give the process a moment to finish writing why it stopped.
                thread::sleep(Duration::from_millis(50));
                let stderr = process.map(|p| p.output()).unwrap_or_default();
                incoming_tx.unbounded_send(Incoming::Exited { stderr }).ok();
            })
            .expect("a thread for reading from the adapter");

        (adapter, incoming_rx)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// The last lines the adapter's process printed.
    pub fn output(&self) -> String {
        self.process
            .as_ref()
            .map(|p| p.output())
            .unwrap_or_default()
    }

    /// Resolves when the adapter sends `initialized`, meaning it's ready for breakpoints.
    pub fn initialized(&self) -> impl Future<Output = Result<(), RequestError>> + use<> {
        self.initialized
            .clone()
            .map(|r| r.map_err(|_| RequestError::Gone))
    }

    /// Sends a request; the future resolves to the response's body.
    pub fn request(&self, command: &str, arguments: Value) -> RequestFuture {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        if !self.is_alive() {
            sender.send(Err(RequestError::Gone)).ok();
        } else {
            self.pending
                .lock()
                .expect("the pending lock")
                .insert(seq, sender);
            let message = json!({
                "seq": seq,
                "type": "request",
                "command": command,
                "arguments": arguments,
            });
            if self.outgoing.unbounded_send(message).is_err() {
                self.pending.lock().expect("the pending lock").remove(&seq);
            }
        }
        RequestFuture { receiver }
    }

    /// A request whose body is parsed as `T`.
    pub async fn request_as<T: DeserializeOwned>(
        &self,
        command: &str,
        arguments: Value,
    ) -> Result<T, RequestError> {
        let body = self.request(command, arguments).await?;
        serde_json::from_value(body).map_err(|e| RequestError::BadResponse(e.to_string()))
    }

    /// Answers a request the adapter sent us.
    pub fn respond(&self, request_seq: i64, command: &str, result: Result<Value, String>) {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let message = match result {
            Ok(body) => json!({
                "seq": seq, "type": "response", "request_seq": request_seq,
                "success": true, "command": command, "body": body,
            }),
            Err(message) => json!({
                "seq": seq, "type": "response", "request_seq": request_seq,
                "success": false, "command": command, "message": message,
            }),
        };
        self.outgoing.unbounded_send(message).ok();
    }
}

fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("no free port for the debugger")?;
    Ok(listener.local_addr()?.port())
}

/// An adapter's reason for refusing: a formatted error in the body if there is one, as `message`
/// is often just a short code like "cancelled".
fn error_text(message: &Value) -> String {
    let short = message
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let Some(error) = message.get("body").and_then(|b| b.get("error")) else {
        return short;
    };
    let Some(format) = error.get("format").and_then(Value::as_str) else {
        return short;
    };
    let mut text = format.to_string();
    if let Some(variables) = error.get("variables").and_then(Value::as_object) {
        for (name, value) in variables {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), String::from);
            text = text.replace(&format!("{{{name}}}"), &value);
        }
    }
    text
}

fn dispatch(
    message: Value,
    pending: &Pending,
    incoming: &mpsc::UnboundedSender<Incoming>,
    initialized: &mut Option<oneshot::Sender<()>>,
) {
    match message.get("type").and_then(Value::as_str) {
        Some("response") => {
            let Some(seq) = message.get("request_seq").and_then(Value::as_i64) else {
                return;
            };
            let Some(waiter) = pending.lock().expect("the pending lock").remove(&seq) else {
                return;
            };
            let success = message
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let result = if success {
                Ok(message.get("body").cloned().unwrap_or(Value::Null))
            } else {
                Err(RequestError::Adapter(error_text(&message)))
            };
            waiter.send(result).ok();
        }
        Some("event") => {
            let event = message
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if event == "initialized"
                && let Some(signal) = initialized.take()
            {
                signal.send(()).ok();
            }
            let body = message.get("body").cloned().unwrap_or(Value::Null);
            incoming
                .unbounded_send(Incoming::Event { event, body })
                .ok();
        }
        Some("request") => {
            let seq = message.get("seq").and_then(Value::as_i64).unwrap_or(0);
            let command = message
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = message.get("arguments").cloned().unwrap_or(Value::Null);
            incoming
                .unbounded_send(Incoming::Request {
                    seq,
                    command,
                    arguments,
                })
                .ok();
        }
        _ => {}
    }
}

/// A request in flight. DAP has no cancellation that every adapter supports, so dropping it just
/// ignores the answer.
pub struct RequestFuture {
    receiver: oneshot::Receiver<Result<Value, RequestError>>,
}

impl Future for RequestFuture {
    type Output = Result<Value, RequestError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(RequestError::Gone)),
        }
    }
}
