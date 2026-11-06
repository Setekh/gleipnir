use crate::transport::{read_message, write_message};
use anyhow::{Context as _, Result, anyhow};
use futures::channel::{mpsc, oneshot};
use lsp_types::notification::Notification as LspNotification;
use lsp_types::request::Request as LspRequest;
use lsp_types::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;

/// Something the server sent that wasn't a response to us.
#[derive(Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    /// A request the client has to answer itself, like `workspace/applyEdit`. Answer it with
    /// `LanguageServer::respond`.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// The server stopped. Its last lines of stderr explain why, if it said.
    Exited {
        stderr: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The server answered with an error.
    Server { code: i64, message: String },
    /// The server isn't running any more.
    Gone,
    /// What came back didn't match the request's result type.
    BadResponse(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server { code, message } => write!(f, "{message} ({code})"),
            Self::Gone => write!(f, "the language server stopped"),
            Self::BadResponse(error) => {
                write!(f, "the language server's answer didn't make sense: {error}")
            }
        }
    }
}

impl std::error::Error for RequestError {}

/// LSP's code for a request cancelled by the client, which isn't worth reporting.
pub const REQUEST_CANCELLED: i64 = -32800;

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RequestError>>>>>;

/// How many lines of the server's stderr to keep, for explaining a crash.
const STDERR_LINES: usize = 40;

pub struct ServerOptions {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// A running language server.
pub struct LanguageServer {
    name: String,
    outgoing: mpsc::UnboundedSender<Value>,
    pending: Pending,
    next_id: AtomicI64,
    alive: Arc<AtomicBool>,
    capabilities: Mutex<ServerCapabilities>,
    child: Mutex<Option<Child>>,
}

impl LanguageServer {
    /// Starts a server process. Returns it, and the stream of what it sends besides responses.
    pub fn spawn(
        name: &str,
        options: ServerOptions,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Incoming>)> {
        let mut child = Command::new(&options.program)
            .args(&options.args)
            .env_clear()
            .envs(options.env.iter().cloned())
            .current_dir(&options.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("couldn't start {}", options.program.display()))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let (server, incoming) = Self::from_streams(name, stdout, stdin, Some(stderr));
        *server.child.lock().expect("the child's lock") = Some(child);
        Ok((server, incoming))
    }

    /// Talks to a server over any pair of streams. Tests use this with an in-process fake.
    pub fn from_streams(
        name: &str,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        stderr: Option<impl Read + Send + 'static>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<Incoming>) {
        let (outgoing, mut outgoing_rx) = mpsc::unbounded::<Value>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded::<Incoming>();
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let server = Arc::new(Self {
            name: name.to_string(),
            outgoing: outgoing.clone(),
            pending: pending.clone(),
            next_id: AtomicI64::new(1),
            alive: alive.clone(),
            capabilities: Mutex::new(ServerCapabilities::default()),
            child: Mutex::new(None),
        });

        let thread_name = format!("lsp-{name}");
        let mut writer = writer;
        thread::Builder::new()
            .name(format!("{thread_name}-write"))
            .spawn(move || {
                while let Some(message) =
                    futures::executor::block_on(futures::StreamExt::next(&mut outgoing_rx))
                {
                    if write_message(&mut writer, &message).is_err() {
                        break;
                    }
                }
            })
            .expect("a thread for writing to the server");

        let stderr_tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        if let Some(stderr) = stderr {
            let tail = stderr_tail.clone();
            thread::Builder::new()
                .name(format!("{thread_name}-stderr"))
                .spawn(move || {
                    use std::io::BufRead as _;
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        let mut tail = tail.lock().expect("the stderr lock");
                        if tail.len() == STDERR_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                })
                .expect("a thread for the server's stderr");
        }

        thread::Builder::new()
            .name(format!("{thread_name}-read"))
            .spawn(move || {
                let mut reader = BufReader::new(reader);
                while let Ok(Some(message)) = read_message(&mut reader) {
                    dispatch(message, &pending, &outgoing, &incoming_tx);
                }
                alive.store(false, Ordering::SeqCst);
                // Everything still waiting gets an answer now, rather than never.
                for (_, waiter) in pending.lock().expect("the pending lock").drain() {
                    waiter.send(Err(RequestError::Gone)).ok();
                }
                let stderr = stderr_tail
                    .lock()
                    .expect("the stderr lock")
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                incoming_tx.unbounded_send(Incoming::Exited { stderr }).ok();
            })
            .expect("a thread for reading from the server");

        (server, incoming_rx)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        self.capabilities
            .lock()
            .expect("the capabilities lock")
            .clone()
    }

    /// Sends a request. Dropping the returned future before it finishes cancels the request on
    /// the server, so work nobody's waiting for stops.
    pub fn request<R: LspRequest>(self: &Arc<Self>, params: R::Params) -> RequestFuture<R::Result>
    where
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        if !self.is_alive() {
            sender.send(Err(RequestError::Gone)).ok();
        } else {
            self.pending
                .lock()
                .expect("the pending lock")
                .insert(id, sender);
            let message =
                json!({ "jsonrpc": "2.0", "id": id, "method": R::METHOD, "params": params });
            if self.outgoing.unbounded_send(message).is_err() {
                self.pending.lock().expect("the pending lock").remove(&id);
            }
        }
        RequestFuture {
            id,
            receiver,
            server: Arc::downgrade(self),
            done: false,
            _result: std::marker::PhantomData,
        }
    }

    pub fn notify<N: LspNotification>(&self, params: N::Params)
    where
        N::Params: Serialize,
    {
        let message = json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params });
        self.outgoing.unbounded_send(message).ok();
    }

    /// Answers a request the server sent us.
    pub fn respond(&self, id: Value, result: Result<Value, (i64, String)>) {
        let message = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        };
        self.outgoing.unbounded_send(message).ok();
    }

    fn cancel(&self, id: i64) {
        if self
            .pending
            .lock()
            .expect("the pending lock")
            .remove(&id)
            .is_some()
        {
            self.notify::<lsp_types::notification::Cancel>(CancelParams {
                id: NumberOrString::Number(id as i32),
            });
        }
    }

    /// The handshake. Must come before anything else.
    pub async fn initialize(
        self: &Arc<Self>,
        root: &Path,
        options: Option<Value>,
    ) -> Result<InitializeResult> {
        let root_uri = crate::uri::path_to_uri(root);
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: root
                    .file_name()
                    .map_or_else(|| "root".into(), |n| n.to_string_lossy().into_owned()),
            }]),
            initialization_options: options,
            capabilities: client_capabilities(),
            client_info: Some(ClientInfo {
                name: "Gleipnir".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            ..Default::default()
        };
        let result = self
            .request::<lsp_types::request::Initialize>(params)
            .await
            .map_err(|e| anyhow!("{} didn't start: {e}", self.name))?;
        *self.capabilities.lock().expect("the capabilities lock") = result.capabilities.clone();
        self.notify::<lsp_types::notification::Initialized>(InitializedParams {});
        Ok(result)
    }

    /// Asks the server to stop, then makes sure it has.
    pub async fn shutdown(self: &Arc<Self>) {
        if self.is_alive() {
            self.request::<lsp_types::request::Shutdown>(()).await.ok();
            self.notify::<lsp_types::notification::Exit>(());
        }
        self.kill_after_grace();
    }

    fn kill_after_grace(&self) {
        let Some(mut child) = self.child.lock().expect("the child's lock").take() else {
            return;
        };
        thread::spawn(move || {
            for _ in 0..20 {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(std::time::Duration::from_millis(50));
            }
            child.kill().ok();
            child.wait().ok();
        });
    }
}

impl Drop for LanguageServer {
    fn drop(&mut self) {
        self.kill_after_grace();
    }
}

/// Routes one message from the server. Requests that need nothing from the app are answered here.
fn dispatch(
    message: Value,
    pending: &Pending,
    outgoing: &mpsc::UnboundedSender<Value>,
    incoming: &mpsc::UnboundedSender<Incoming>,
) {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(String::from);
    let id = message.get("id").cloned();
    match (method, id) {
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(waiter) = pending.lock().expect("the pending lock").remove(&id) else {
                return;
            };
            let result = match message.get("error") {
                Some(error) => Err(RequestError::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            waiter.send(result).ok();
        }
        (Some(method), Some(id)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            match automatic_response(&method, &params) {
                Some(result) => {
                    outgoing
                        .unbounded_send(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
                        .ok();
                }
                None => {
                    incoming
                        .unbounded_send(Incoming::Request { id, method, params })
                        .ok();
                }
            }
        }
        (Some(method), None) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            incoming
                .unbounded_send(Incoming::Notification { method, params })
                .ok();
        }
        (None, None) => {}
    }
}

/// Answers for requests that need nothing from the app.
fn automatic_response(method: &str, params: &Value) -> Option<Value> {
    match method {
        // No settings of our own yet: one null per item asked about means "use your defaults".
        "workspace/configuration" => {
            let count = params
                .get("items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Some(Value::Array(vec![Value::Null; count]))
        }
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "workspace/codeLens/refresh"
        | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/diagnostic/refresh" => Some(Value::Null),
        "window/showMessageRequest" => Some(Value::Null),
        "workspace/workspaceFolders" => Some(Value::Null),
        _ => None,
    }
}

/// A request in flight. Resolves to the server's answer; dropping it early cancels the request.
pub struct RequestFuture<T> {
    id: i64,
    receiver: oneshot::Receiver<Result<Value, RequestError>>,
    server: std::sync::Weak<LanguageServer>,
    done: bool,
    _result: std::marker::PhantomData<T>,
}

impl<T> Unpin for RequestFuture<T> {}

impl<T: DeserializeOwned> Future for RequestFuture<T> {
    type Output = Result<T, RequestError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let polled = Pin::new(&mut self.receiver).poll(cx);
        match polled {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.done = true;
                let value = match result {
                    Ok(Ok(value)) => value,
                    Ok(Err(error)) => return Poll::Ready(Err(error)),
                    Err(_) => return Poll::Ready(Err(RequestError::Gone)),
                };
                Poll::Ready(
                    serde_json::from_value(value)
                        .map_err(|e| RequestError::BadResponse(e.to_string())),
                )
            }
        }
    }
}

impl<T> Drop for RequestFuture<T> {
    fn drop(&mut self) {
        if !self.done
            && let Some(server) = self.server.upgrade()
        {
            server.cancel(self.id);
        }
    }
}

/// What we tell servers we can do. Every feature here is one the editor really handles, as a
/// server may change its answers based on what's claimed.
pub fn client_capabilities() -> ClientCapabilities {
    let markdown = vec![MarkupKind::Markdown, MarkupKind::PlainText];
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![PositionEncodingKind::UTF16]),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            apply_edit: Some(true),
            workspace_edit: Some(WorkspaceEditClientCapabilities {
                document_changes: Some(true),
                ..Default::default()
            }),
            configuration: Some(true),
            workspace_folders: Some(true),
            symbol: Some(WorkspaceSymbolClientCapabilities::default()),
            // We report changes made outside the editor, but for every file rather than the
            // patterns a server registers, so servers that can watch for themselves still do.
            did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
                dynamic_registration: Some(false),
                relative_pattern_support: None,
            }),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                ..Default::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    documentation_format: Some(markdown.clone()),
                    resolve_support: Some(CompletionItemCapabilityResolveSupport {
                        properties: vec![
                            "additionalTextEdits".into(),
                            "documentation".into(),
                            "detail".into(),
                        ],
                    }),
                    insert_replace_support: Some(true),
                    label_details_support: Some(true),
                    ..Default::default()
                }),
                context_support: Some(true),
                ..Default::default()
            }),
            hover: Some(HoverClientCapabilities {
                content_format: Some(markdown.clone()),
                ..Default::default()
            }),
            signature_help: Some(SignatureHelpClientCapabilities {
                signature_information: Some(SignatureInformationSettings {
                    documentation_format: Some(markdown),
                    parameter_information: Some(ParameterInformationSettings {
                        label_offset_support: Some(true),
                    }),
                    active_parameter_support: Some(true),
                }),
                ..Default::default()
            }),
            definition: Some(GotoCapability {
                link_support: Some(true),
                ..Default::default()
            }),
            implementation: Some(GotoCapability {
                link_support: Some(true),
                ..Default::default()
            }),
            type_definition: Some(GotoCapability {
                link_support: Some(true),
                ..Default::default()
            }),
            references: Some(DynamicRegistrationClientCapabilities::default()),
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            rename: Some(RenameClientCapabilities {
                prepare_support: Some(true),
                ..Default::default()
            }),
            code_action: Some(CodeActionClientCapabilities {
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: [
                            "",
                            "quickfix",
                            "refactor",
                            "refactor.extract",
                            "refactor.inline",
                            "refactor.rewrite",
                            "source",
                            "source.organizeImports",
                        ]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    },
                }),
                resolve_support: Some(CodeActionCapabilityResolveSupport {
                    properties: vec!["edit".into()],
                }),
                ..Default::default()
            }),
            formatting: Some(DynamicRegistrationClientCapabilities::default()),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        // rust-analyzer says when it's finished loading the project, which progress alone
        // doesn't: there are gaps between its steps where nothing is in progress.
        experimental: Some(serde_json::json!({ "serverStatusNotification": true })),
        ..Default::default()
    }
}
