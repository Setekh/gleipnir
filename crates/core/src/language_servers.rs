//! Running language servers for a window: one per package, started when the first file that needs
//! it opens, kept in step with every open buffer, and restarted if it crashes.

use crate::buffer::{BufferEvent, BufferModel, Diagnostic, Severity};
use crate::toolchains::{ToolchainEvent, Toolchains};
use anyhow::{Result, anyhow};
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gleipnir_lsp::lsp_types::{self as lsp, notification as notif, request as req};
use gleipnir_lsp::{Incoming, LanguageServer, ServerOptions, path_to_uri, uri_to_path};
use gleipnir_packages::registry;
use gleipnir_syntax::Language;
use gleipnir_text::{Buffer, Utf16Point};
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Subscription, Task, WeakEntity,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Starts a server for a package. Swappable so tests can use an in-process fake.
pub type Launcher =
    Rc<dyn Fn(&str, &Path, &App) -> Result<(Arc<LanguageServer>, UnboundedReceiver<Incoming>)>>;

/// Where a server's own readiness report is kept among its progress, so it counts as busy.
const SERVER_STATUS_TOKEN: &str = "\0server-status";
/// Where the warm-up request is kept among a server's progress, so it counts as busy meanwhile.
const WARM_UP_TOKEN: &str = "\0warm-up";

/// More crashes than this in a minute and we stop restarting, as something's properly wrong.
const MAX_RESTARTS_PER_MINUTE: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Starting,
    /// Running, with whatever it's currently reporting progress on.
    Running {
        busy: Option<String>,
    },
    /// The tool it needs is being installed, after which it starts.
    Installing,
    /// The tool it needs isn't installed.
    Missing(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum LanguageServerEvent {
    StatusChanged(String),
    DiagnosticsChanged(PathBuf),
    /// A server asked for edits to be applied, as a code action's command does. The workspace
    /// applies them and answers through `respond_to_apply_edit`.
    ApplyEditRequested {
        package_id: String,
        request_id: serde_json::Value,
        edit: lsp::WorkspaceEdit,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GotoKind {
    Definition,
    Implementation,
    TypeDefinition,
}

/// A place in a file, in the server's coordinates, as the file may not be open to convert with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub start: Utf16Point,
    pub end: Utf16Point,
}

#[derive(Clone, Debug)]
pub struct SymbolHit {
    pub name: String,
    pub kind: lsp::SymbolKind,
    pub container: Option<String>,
    pub location: Location,
}

struct Server {
    status: ServerStatus,
    handle: Option<Arc<LanguageServer>>,
    crashes: Vec<Instant>,
    /// Progress tokens in flight, with their titles.
    progress: BTreeMap<String, String>,
    /// Whether it's been sent its warm-up request, for servers that want one.
    warmed: bool,
    _task: Option<Task<()>>,
}

struct Document {
    buffer: WeakEntity<BufferModel>,
    package_id: String,
    path: PathBuf,
    language_id: &'static str,
    /// The version we last told the server. Cells, as catching a document up before a request
    /// happens where only a shared reference is available.
    version: Cell<i32>,
    /// The buffer version the server's copy matches.
    synced: Cell<u64>,
    /// Whether the server has it open. Edits before then are covered by the full text sent on open.
    opened_on: Option<Arc<LanguageServer>>,
    _subscriptions: Vec<Subscription>,
}

pub struct LanguageServers {
    root: Option<PathBuf>,
    toolchains: Entity<Toolchains>,
    launcher: Launcher,
    servers: HashMap<String, Server>,
    documents: HashMap<EntityId, Document>,
    /// The latest diagnostics for every file, open or not, for the Problems panel.
    diagnostics: BTreeMap<PathBuf, Vec<(lsp::Range, Severity, String)>>,
    _toolchains: Subscription,
}

impl EventEmitter<LanguageServerEvent> for LanguageServers {}

fn language_id(language: Language, path: &Path) -> &'static str {
    match language {
        Language::Rust => "rust",
        Language::Go => "go",
        Language::Odin => "odin",
        Language::TypeScript => "typescript",
        Language::Tsx => "typescriptreact",
        Language::JavaScript if path.extension().is_some_and(|e| e == "jsx") => "javascriptreact",
        Language::JavaScript => "javascript",
        Language::CSharp => "csharp",
        Language::Json if allows_comments(path) => "jsonc",
        Language::Json => "json",
        Language::Html => "html",
        Language::Css => "css",
        Language::Markdown => "markdown",
    }
}

/// JSON files that are read with comments allowed, which the JSON server checks for by the
/// document's language: TypeScript's and VS Code's configuration, and `.jsonc` files.
fn allows_comments(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    path.extension().is_some_and(|e| e == "jsonc")
        || name.starts_with("tsconfig")
        || name.starts_with("jsconfig")
        || path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|dir| dir == ".vscode")
}

fn to_lsp(point: Utf16Point) -> lsp::Position {
    lsp::Position::new(point.line, point.character)
}

fn from_lsp(position: lsp::Position) -> Utf16Point {
    Utf16Point::new(position.line, position.character)
}

pub fn severity(severity: Option<lsp::DiagnosticSeverity>) -> Severity {
    match severity {
        Some(lsp::DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(lsp::DiagnosticSeverity::INFORMATION) => Severity::Information,
        Some(lsp::DiagnosticSeverity::HINT) => Severity::Hint,
        _ => Severity::Error,
    }
}

/// Converts a server range to offsets in a buffer.
pub fn range_to_offsets(buffer: &Buffer, range: &lsp::Range) -> std::ops::Range<usize> {
    let start = buffer.utf16_to_offset(from_lsp(range.start));
    let end = buffer.utf16_to_offset(from_lsp(range.end)).max(start);
    start..end
}

/// The default way to start a server: the package's tool, found by the toolchains, with its SDK.
pub fn toolchain_launcher(toolchains: Entity<Toolchains>, root: Option<PathBuf>) -> Launcher {
    Rc::new(move |package_id, cwd, cx| {
        let package = registry()
            .package(package_id)
            .ok_or_else(|| anyhow!("no package {package_id}"))?;
        let spec = package
            .manifest
            .server
            .as_ref()
            .ok_or_else(|| anyhow!("{package_id} has no language server"))?;
        let toolchains = toolchains.read(cx);
        let program = toolchains
            .tool_path(package_id, &spec.tool)
            .ok_or_else(|| anyhow!("{} isn't installed", spec.tool))?;
        let env = toolchains.env_for(package_id).unwrap_or_default();
        let cwd = root.clone().unwrap_or_else(|| cwd.to_path_buf());
        LanguageServer::spawn(
            &spec.tool,
            ServerOptions {
                program,
                args: spec.args.clone(),
                env: env
                    .vars()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                cwd,
            },
        )
    })
}

impl LanguageServers {
    pub fn new(
        root: Option<PathBuf>,
        toolchains: Entity<Toolchains>,
        cx: &mut Context<Self>,
    ) -> Self {
        let launcher = toolchain_launcher(toolchains.clone(), root.clone());
        Self::with_launcher(root, toolchains, launcher, cx)
    }

    pub fn with_launcher(
        root: Option<PathBuf>,
        toolchains: Entity<Toolchains>,
        launcher: Launcher,
        cx: &mut Context<Self>,
    ) -> Self {
        // A server waiting for its tool to be found, or installed, can start once it is.
        let subscription = cx.subscribe(&toolchains, |this, _, _: &ToolchainEvent, cx| {
            this.retry_waiting(cx)
        });
        Self {
            root,
            toolchains,
            launcher,
            servers: HashMap::new(),
            documents: HashMap::new(),
            diagnostics: BTreeMap::new(),
            _toolchains: subscription,
        }
    }

    /// Every server started so far, by package.
    pub fn statuses(&self) -> impl Iterator<Item = (&str, &ServerStatus)> {
        self.servers.iter().map(|(id, s)| (id.as_str(), &s.status))
    }

    pub fn status(&self, package_id: &str) -> Option<&ServerStatus> {
        self.servers.get(package_id).map(|s| &s.status)
    }

    pub fn server_for(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Option<Arc<LanguageServer>> {
        let package = Self::package_for(buffer, cx)?;
        self.servers
            .get(&package)?
            .handle
            .clone()
            .filter(|h| h.is_alive())
    }

    pub fn capabilities_for(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Option<lsp::ServerCapabilities> {
        Some(self.server_for(buffer, cx)?.capabilities())
    }

    /// The package whose server handles a buffer, and that server's tool name.
    pub fn server_name_for(buffer: &Entity<BufferModel>, cx: &App) -> Option<(String, String)> {
        let language = buffer.read(cx).language()?;
        let package = registry().package_for_language(language)?;
        let server = package.manifest.server.as_ref()?;
        Some((package.id().to_string(), server.tool.clone()))
    }

    fn package_for(buffer: &Entity<BufferModel>, cx: &App) -> Option<String> {
        let language = buffer.read(cx).language()?;
        Some(registry().package_for_language(language)?.id().to_string())
    }

    /// Every file's diagnostics, for the Problems panel.
    pub fn all_diagnostics(&self) -> &BTreeMap<PathBuf, Vec<(lsp::Range, Severity, String)>> {
        &self.diagnostics
    }

    /// Starts syncing a buffer with its language's server, starting the server if needed.
    pub fn register_buffer(&mut self, buffer: &Entity<BufferModel>, cx: &mut Context<Self>) {
        let id = buffer.entity_id();
        if self.documents.contains_key(&id) {
            return;
        }
        let model = buffer.read(cx);
        let (Some(path), Some(language)) = (model.path().map(Path::to_path_buf), model.language())
        else {
            return;
        };
        let Some(package) = registry().package_for_language(language) else {
            return;
        };
        if package.manifest.server.is_none() {
            return;
        }
        let package_id = package.id().to_string();
        let subscriptions = vec![
            cx.subscribe(buffer, |this, buffer, event, cx| {
                this.buffer_event(&buffer, event, cx)
            }),
            cx.observe_release(buffer, move |this, _, _| this.close_document(id)),
        ];
        self.documents.insert(
            id,
            Document {
                buffer: buffer.downgrade(),
                package_id: package_id.clone(),
                language_id: language_id(language, &path),
                path,
                version: Cell::new(0),
                synced: Cell::new(0),
                opened_on: None,
                _subscriptions: subscriptions,
            },
        );
        self.ensure_server(&package_id, cx);
        self.open_documents(&package_id, cx);
    }

    fn ensure_server(&mut self, package_id: &str, cx: &mut Context<Self>) {
        if let Some(server) = self.servers.get(package_id)
            && !matches!(
                server.status,
                ServerStatus::Missing(_) | ServerStatus::Installing
            )
        {
            return;
        }
        // The toolchains are still looking for SDKs: wait for them rather than report it missing.
        if self.toolchains.read(cx).packages().is_empty() {
            self.servers.insert(
                package_id.to_string(),
                Server::new(ServerStatus::Missing("looking for tools".into())),
            );
            return;
        }
        self.start(package_id, cx);
    }

    fn retry_waiting(&mut self, cx: &mut Context<Self>) {
        let waiting: Vec<String> = self
            .servers
            .iter()
            .filter(|(_, s)| {
                matches!(
                    s.status,
                    ServerStatus::Missing(_) | ServerStatus::Installing
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        for package_id in waiting {
            self.start(&package_id, cx);
        }
    }

    /// What a server whose tool couldn't be found is waiting for: its tool being installed, if
    /// it can be, or the user.
    fn install_missing(
        &mut self,
        package_id: &str,
        reason: String,
        cx: &mut Context<Self>,
    ) -> ServerStatus {
        let Some(tool) = registry()
            .package(package_id)
            .and_then(|p| p.manifest.server.as_ref())
            .map(|s| s.tool.clone())
        else {
            return ServerStatus::Missing(reason);
        };
        let installing = self
            .toolchains
            .update(cx, |t, cx| t.install_missing(package_id, &tool, cx));
        if installing {
            return ServerStatus::Installing;
        }
        match self.toolchains.read(cx).install_error(&tool) {
            Some(error) => ServerStatus::Missing(format!("couldn't install {tool}: {error}")),
            None => ServerStatus::Missing(reason),
        }
    }

    fn start(&mut self, package_id: &str, cx: &mut Context<Self>) {
        let root = self.root.clone().unwrap_or_else(|| PathBuf::from("/"));
        let launched = (self.launcher)(package_id, &root, cx);
        let (handle, incoming) = match launched {
            Ok(launched) => launched,
            Err(error) => {
                let crashes = self
                    .servers
                    .remove(package_id)
                    .map(|s| s.crashes)
                    .unwrap_or_default();
                let status = self.install_missing(package_id, format!("{error:#}"), cx);
                let mut server = Server::new(status);
                server.crashes = crashes;
                self.servers.insert(package_id.to_string(), server);
                cx.emit(LanguageServerEvent::StatusChanged(package_id.to_string()));
                return;
            }
        };
        let crashes = self
            .servers
            .remove(package_id)
            .map(|s| s.crashes)
            .unwrap_or_default();
        let package = package_id.to_string();
        let init_handle = handle.clone();
        let options = registry()
            .package(package_id)
            .and_then(|p| p.manifest.server.as_ref())
            .and_then(|s| s.initialization_options.clone());
        let task = cx.spawn(async move |this, cx| {
            let initialized = init_handle.initialize(&root, options).await;
            let ok = this
                .update(cx, |this, cx| match initialized {
                    Ok(_) => {
                        let reports_ready = registry()
                            .package(&package)
                            .and_then(|p| p.manifest.server.as_ref())
                            .is_some_and(|s| s.reports_ready);
                        if let Some(server) = this.servers.get_mut(&package) {
                            // Loading until it says otherwise, if it's one that will.
                            let busy = reports_ready.then(|| "loading the project".to_string());
                            if let Some(task) = &busy {
                                server
                                    .progress
                                    .insert(SERVER_STATUS_TOKEN.to_string(), task.clone());
                            }
                            server.status = ServerStatus::Running { busy };
                        }
                        cx.emit(LanguageServerEvent::StatusChanged(package.clone()));
                        this.open_documents(&package, cx);
                        true
                    }
                    Err(error) => {
                        this.server_failed(&package, format!("{error:#}"), cx);
                        false
                    }
                })
                .unwrap_or(false);
            if ok {
                Self::listen(this, package, init_handle, incoming, cx).await;
            }
        });
        let mut server = Server::new(ServerStatus::Starting);
        server.handle = Some(handle);
        server.crashes = crashes;
        server._task = Some(task);
        self.servers.insert(package_id.to_string(), server);
        cx.emit(LanguageServerEvent::StatusChanged(package_id.to_string()));
        cx.notify();
    }

    async fn listen(
        this: WeakEntity<Self>,
        package: String,
        handle: Arc<LanguageServer>,
        mut incoming: UnboundedReceiver<Incoming>,
        cx: &mut gpui::AsyncApp,
    ) {
        while let Some(message) = incoming.next().await {
            let alive = this.update(cx, |this, cx| match message {
                Incoming::Notification { method, params } => {
                    this.notification(&package, &method, params, cx)
                }
                Incoming::Request { id, method, params } => {
                    if method == "workspace/applyEdit"
                        && let Ok(params) =
                            serde_json::from_value::<lsp::ApplyWorkspaceEditParams>(params)
                    {
                        cx.emit(LanguageServerEvent::ApplyEditRequested {
                            package_id: package.clone(),
                            request_id: id,
                            edit: params.edit,
                        });
                    } else {
                        handle.respond(id, Err((-32601, format!("{method} isn't supported"))));
                    }
                }
                Incoming::Exited { stderr } => {
                    let reason = stderr
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("it exited")
                        .to_string();
                    this.server_failed(&package, reason, cx);
                }
            });
            if alive.is_err() {
                break;
            }
        }
    }

    /// Files changed on disk, perhaps by another program, like a branch switch. Each running
    /// server hears about its own languages' files and project manifests, except those it has
    /// open: their buffers reload, and the edits reach it as usual.
    pub fn files_changed(&self, paths: &[PathBuf]) {
        const MANIFESTS: &[&str] = &[
            "Cargo.toml",
            "Cargo.lock",
            "go.mod",
            "go.sum",
            "go.work",
            "package.json",
            "tsconfig.json",
            "jsconfig.json",
            "ols.json",
        ];
        const SKIPPED: &[&str] = &[".git", "target", "node_modules"];
        for (package_id, server) in &self.servers {
            let Some(handle) = server.handle.clone().filter(|h| h.is_alive()) else {
                continue;
            };
            let Some(package) = registry().package(package_id) else {
                continue;
            };
            let extensions: Vec<&str> = package
                .manifest
                .languages
                .iter()
                .flat_map(|l| l.extensions.iter().map(String::as_str))
                .collect();
            let open: Vec<&Path> = self
                .documents
                .values()
                .filter(|d| d.package_id == *package_id && d.opened_on.is_some())
                .map(|d| d.path.as_path())
                .collect();
            let changes: Vec<lsp::FileEvent> = paths
                .iter()
                .filter(|path| {
                    let skipped = path
                        .components()
                        .any(|c| SKIPPED.contains(&c.as_os_str().to_string_lossy().as_ref()));
                    let name = path.file_name().map(|n| n.to_string_lossy());
                    let extension = path.extension().map(|e| e.to_string_lossy());
                    let relevant = name.as_deref().is_some_and(|n| MANIFESTS.contains(&n))
                        || extension
                            .as_deref()
                            .is_some_and(|e| extensions.contains(&e));
                    !skipped && relevant && !open.contains(&path.as_path())
                })
                .map(|path| lsp::FileEvent {
                    uri: path_to_uri(path),
                    typ: if path.exists() {
                        lsp::FileChangeType::CHANGED
                    } else {
                        lsp::FileChangeType::DELETED
                    },
                })
                .collect();
            if !changes.is_empty() {
                handle.notify::<notif::DidChangeWatchedFiles>(lsp::DidChangeWatchedFilesParams {
                    changes,
                });
            }
        }
    }

    /// Answers a server's request to apply edits.
    pub fn respond_to_apply_edit(
        &self,
        package_id: &str,
        request_id: serde_json::Value,
        applied: bool,
    ) {
        if let Some(handle) = self.servers.get(package_id).and_then(|s| s.handle.clone()) {
            handle.respond(request_id, Ok(serde_json::json!({ "applied": applied })));
        }
    }

    fn server_failed(&mut self, package_id: &str, reason: String, cx: &mut Context<Self>) {
        let Some(server) = self.servers.get_mut(package_id) else {
            return;
        };
        let now = Instant::now();
        server
            .crashes
            .retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        server.crashes.push(now);
        server.handle = None;
        server.progress.clear();
        let attempts = server.crashes.len();
        for document in self
            .documents
            .values_mut()
            .filter(|d| d.package_id == package_id)
        {
            document.opened_on = None;
        }
        if attempts > MAX_RESTARTS_PER_MINUTE {
            server.status = ServerStatus::Failed(format!(
                "{reason}. It crashed {attempts} times in a minute, so it won't be restarted"
            ));
            server._task = None;
        } else {
            server.status = ServerStatus::Failed(reason);
            let delay = Duration::from_millis(500 * (1 << attempts.min(4)));
            let package = package_id.to_string();
            server._task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                this.update(cx, |this, cx| {
                    this.start(&package, cx);
                })
                .ok();
            }));
        }
        cx.emit(LanguageServerEvent::StatusChanged(package_id.to_string()));
        cx.notify();
    }

    /// Restarts a server by hand, clearing its crash count.
    pub fn restart(&mut self, package_id: &str, cx: &mut Context<Self>) {
        if let Some(handle) = self.servers.remove(package_id).and_then(|s| s.handle) {
            let shutdown = cx.background_spawn(async move { handle.shutdown().await });
            shutdown.detach();
        }
        for document in self
            .documents
            .values_mut()
            .filter(|d| d.package_id == package_id)
        {
            document.opened_on = None;
        }
        self.start(package_id, cx);
    }

    fn notification(
        &mut self,
        package: &str,
        method: &str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        match method {
            "textDocument/publishDiagnostics" => {
                let Ok(params) = serde_json::from_value::<lsp::PublishDiagnosticsParams>(params)
                else {
                    return;
                };
                let Some(path) = uri_to_path(&params.uri) else {
                    return;
                };
                let entries: Vec<(lsp::Range, Severity, String)> = params
                    .diagnostics
                    .iter()
                    .map(|d| (d.range, severity(d.severity), d.message.clone()))
                    .collect();
                if entries.is_empty() {
                    self.diagnostics.remove(&path);
                } else {
                    self.diagnostics.insert(path.clone(), entries);
                }
                for document in self.documents.values().filter(|d| d.path == path) {
                    let Some(buffer) = document.buffer.upgrade() else {
                        continue;
                    };
                    buffer.update(cx, |model, cx| {
                        let converted = params
                            .diagnostics
                            .iter()
                            .map(|d| Diagnostic {
                                range: range_to_offsets(model.buffer(), &d.range),
                                severity: severity(d.severity),
                                message: d.message.clone(),
                                source: d.source.clone(),
                            })
                            .collect();
                        model.set_diagnostics(converted, cx);
                    });
                }
                cx.emit(LanguageServerEvent::DiagnosticsChanged(path));
            }
            "$/progress" => {
                let Ok(params) = serde_json::from_value::<lsp::ProgressParams>(params) else {
                    return;
                };
                let token = match params.token {
                    lsp::NumberOrString::Number(n) => n.to_string(),
                    lsp::NumberOrString::String(s) => s,
                };
                let Some(server) = self.servers.get_mut(package) else {
                    return;
                };
                let lsp::ProgressParamsValue::WorkDone(progress) = params.value;
                match progress {
                    lsp::WorkDoneProgress::Begin(begin) => {
                        server.progress.insert(token, begin.title);
                    }
                    lsp::WorkDoneProgress::Report(_) => {}
                    lsp::WorkDoneProgress::End(_) => {
                        server.progress.remove(&token);
                    }
                }
                if let ServerStatus::Running { busy } = &mut server.status {
                    *busy = server.progress.values().next().cloned();
                }
                cx.emit(LanguageServerEvent::StatusChanged(package.to_string()));
                cx.notify();
            }
            // rust-analyzer's own: busy until it's quiescent, meaning the project is loaded.
            "experimental/serverStatus" => {
                let Some(server) = self.servers.get_mut(package) else {
                    return;
                };
                let quiescent = params["quiescent"].as_bool().unwrap_or(true);
                if quiescent {
                    server.progress.remove(SERVER_STATUS_TOKEN);
                } else {
                    server.progress.insert(
                        SERVER_STATUS_TOKEN.to_string(),
                        "loading the project".to_string(),
                    );
                }
                if let ServerStatus::Running { busy } = &mut server.status {
                    *busy = server.progress.values().next().cloned();
                }
                cx.emit(LanguageServerEvent::StatusChanged(package.to_string()));
                cx.notify();
            }
            _ => {}
        }
    }

    /// Opens every registered document of a package on its server, if the server's ready.
    fn open_documents(&mut self, package_id: &str, cx: &mut Context<Self>) {
        let Some(handle) = self.servers.get(package_id).and_then(|s| s.handle.clone()) else {
            return;
        };
        if !matches!(
            self.servers[package_id].status,
            ServerStatus::Running { .. }
        ) {
            return;
        }
        for document in self
            .documents
            .values_mut()
            .filter(|d| d.package_id == package_id)
        {
            if document
                .opened_on
                .as_ref()
                .is_some_and(|h| Arc::ptr_eq(h, &handle))
            {
                continue;
            }
            let Some(buffer) = document.buffer.upgrade() else {
                continue;
            };
            document.version.set(document.version.get() + 1);
            handle.notify::<notif::DidOpenTextDocument>(lsp::DidOpenTextDocumentParams {
                text_document: lsp::TextDocumentItem {
                    uri: path_to_uri(&document.path),
                    language_id: document.language_id.to_string(),
                    version: document.version.get(),
                    text: buffer.read(cx).buffer().text(),
                },
            });
            document.synced.set(buffer.read(cx).buffer().version());
            document.opened_on = Some(handle.clone());
        }
        self.warm_up(package_id, &handle, cx);
    }

    /// For a server that loads the project on its first completion, asks for one now, with the
    /// server shown busy until it answers.
    fn warm_up(&mut self, package_id: &str, handle: &Arc<LanguageServer>, cx: &mut Context<Self>) {
        let wanted = registry()
            .package(package_id)
            .and_then(|p| p.manifest.server.as_ref())
            .is_some_and(|s| s.warm_up);
        let Some(document) = self.documents.values().find(|d| {
            d.package_id == package_id
                && d.opened_on.as_ref().is_some_and(|h| Arc::ptr_eq(h, handle))
        }) else {
            return;
        };
        let Some(server) = self.servers.get_mut(package_id) else {
            return;
        };
        if !wanted || server.warmed {
            return;
        }
        server.warmed = true;
        server
            .progress
            .insert(WARM_UP_TOKEN.to_string(), "loading the project".to_string());
        if let ServerStatus::Running { busy } = &mut server.status {
            *busy = server.progress.values().next().cloned();
        }
        let request = handle.request::<req::Completion>(lsp::CompletionParams {
            text_document_position: lsp::TextDocumentPositionParams {
                text_document: lsp::TextDocumentIdentifier {
                    uri: path_to_uri(&document.path),
                },
                position: lsp::Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: Some(lsp::CompletionContext {
                trigger_kind: lsp::CompletionTriggerKind::INVOKED,
                trigger_character: None,
            }),
        });
        let package = package_id.to_string();
        cx.emit(LanguageServerEvent::StatusChanged(package.clone()));
        cx.spawn(async move |this, cx| {
            // What it answers doesn't matter, only that it's done the work.
            request.await.ok();
            this.update(cx, |this, cx| {
                if let Some(server) = this.servers.get_mut(&package) {
                    server.progress.remove(WARM_UP_TOKEN);
                    if let ServerStatus::Running { busy } = &mut server.status {
                        *busy = server.progress.values().next().cloned();
                    }
                }
                cx.emit(LanguageServerEvent::StatusChanged(package));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn close_document(&mut self, id: EntityId) {
        let Some(document) = self.documents.remove(&id) else {
            return;
        };
        if let Some(handle) = document.opened_on {
            handle.notify::<notif::DidCloseTextDocument>(lsp::DidCloseTextDocumentParams {
                text_document: lsp::TextDocumentIdentifier {
                    uri: path_to_uri(&document.path),
                },
            });
        }
    }

    fn buffer_event(
        &mut self,
        buffer: &Entity<BufferModel>,
        event: &BufferEvent,
        cx: &mut Context<Self>,
    ) {
        let id = buffer.entity_id();
        match event {
            BufferEvent::Edited { edits, version, .. } => {
                let Some(document) = self.documents.get_mut(&id) else {
                    return;
                };
                let Some(handle) = document.opened_on.clone() else {
                    return;
                };
                // Already sent, as the whole text, to catch up before a request.
                if *version <= document.synced.get() {
                    return;
                }
                document.version.set(document.version.get() + 1);
                let full = matches!(
                    handle.capabilities().text_document_sync,
                    Some(lsp::TextDocumentSyncCapability::Kind(
                        lsp::TextDocumentSyncKind::FULL
                    ))
                ) || matches!(
                    handle.capabilities().text_document_sync,
                    Some(lsp::TextDocumentSyncCapability::Options(
                        lsp::TextDocumentSyncOptions {
                            change: Some(lsp::TextDocumentSyncKind::FULL),
                            ..
                        }
                    ))
                );
                // The edits are in the order applied, each valid against the document as it was
                // then, which is exactly how the protocol applies a list of changes.
                let content_changes = if full {
                    vec![lsp::TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: buffer.read(cx).buffer().text(),
                    }]
                } else {
                    edits
                        .iter()
                        .map(|edit| lsp::TextDocumentContentChangeEvent {
                            range: Some(lsp::Range::new(
                                to_lsp(edit.start_utf16),
                                to_lsp(edit.old_end_utf16),
                            )),
                            range_length: None,
                            text: edit.new_text.clone(),
                        })
                        .collect()
                };
                handle.notify::<notif::DidChangeTextDocument>(lsp::DidChangeTextDocumentParams {
                    text_document: lsp::VersionedTextDocumentIdentifier {
                        uri: path_to_uri(&document.path),
                        version: document.version.get(),
                    },
                    content_changes,
                });
                document.synced.set(if full {
                    buffer.read(cx).buffer().version()
                } else {
                    *version
                });
            }
            BufferEvent::Saved => {
                let Some(document) = self.documents.get(&id) else {
                    return;
                };
                if let Some(handle) = &document.opened_on {
                    handle.notify::<notif::DidSaveTextDocument>(lsp::DidSaveTextDocumentParams {
                        text_document: lsp::TextDocumentIdentifier {
                            uri: path_to_uri(&document.path),
                        },
                        text: None,
                    });
                }
            }
            BufferEvent::DiskStatusChanged => {
                // A rename shows up here: reopen under the new path, or with a new server if the
                // language changed with the extension.
                let new_path = buffer.read(cx).path().map(Path::to_path_buf);
                let moved = self
                    .documents
                    .get(&id)
                    .is_some_and(|d| Some(&d.path) != new_path.as_ref());
                if moved {
                    self.close_document(id);
                    self.register_buffer(buffer, cx);
                }
            }
        }
    }

    fn position(
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &App,
    ) -> (lsp::TextDocumentIdentifier, lsp::Position) {
        let model = buffer.read(cx);
        let uri = path_to_uri(model.path().unwrap_or(Path::new("/")));
        (
            lsp::TextDocumentIdentifier { uri },
            to_lsp(model.buffer().offset_to_utf16(offset)),
        )
    }

    /// Sends the whole text if the server's copy is behind the buffer. Edits reach the server
    /// through the buffer's events, which come after the update that made them, so a request made
    /// in that same update would otherwise be answered about the text as it was.
    fn catch_up(&self, buffer: &Entity<BufferModel>, cx: &App) {
        let Some(document) = self.documents.get(&buffer.entity_id()) else {
            return;
        };
        let Some(handle) = &document.opened_on else {
            return;
        };
        let text = buffer.read(cx).buffer();
        if text.version() <= document.synced.get() {
            return;
        }
        document.version.set(document.version.get() + 1);
        handle.notify::<notif::DidChangeTextDocument>(lsp::DidChangeTextDocumentParams {
            text_document: lsp::VersionedTextDocumentIdentifier {
                uri: path_to_uri(&document.path),
                version: document.version.get(),
            },
            content_changes: vec![lsp::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.text(),
            }],
        });
        document.synced.set(text.version());
    }

    fn server_or_error(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Result<Arc<LanguageServer>> {
        if let Some(server) = self.server_for(buffer, cx) {
            self.catch_up(buffer, cx);
            return Ok(server);
        }
        let package = Self::package_for(buffer, cx)
            .ok_or_else(|| anyhow!("no language server handles this file"))?;
        Err(match self.status(&package) {
            Some(ServerStatus::Starting) => anyhow!("the language server is still starting"),
            Some(ServerStatus::Installing) => anyhow!("the language server is being installed"),
            Some(ServerStatus::Missing(reason)) | Some(ServerStatus::Failed(reason)) => {
                anyhow!("{reason}")
            }
            _ => anyhow!("the language server isn't running"),
        })
    }

    pub fn goto(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        kind: GotoKind,
        cx: &App,
    ) -> Task<Result<Vec<Location>>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let (document, position) = Self::position(buffer, offset, cx);
        let params = lsp::GotoDefinitionParams {
            text_document_position_params: lsp::TextDocumentPositionParams {
                text_document: document,
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let request = match kind {
            GotoKind::Definition => {
                futures::future::Either::Left(server.request::<req::GotoDefinition>(params))
            }
            GotoKind::Implementation => futures::future::Either::Right(
                futures::future::Either::Left(server.request::<req::GotoImplementation>(params)),
            ),
            GotoKind::TypeDefinition => futures::future::Either::Right(
                futures::future::Either::Right(server.request::<req::GotoTypeDefinition>(params)),
            ),
        };
        cx.background_spawn(async move {
            let response = request.await.map_err(|e| anyhow!("{e}"))?;
            Ok(goto_locations(response))
        })
    }

    pub fn references(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &App,
    ) -> Task<Result<Vec<Location>>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let (document, position) = Self::position(buffer, offset, cx);
        let request = server.request::<req::References>(lsp::ReferenceParams {
            text_document_position: lsp::TextDocumentPositionParams {
                text_document: document,
                position,
            },
            // Usages, as IntelliJ lists them: the declaration is where you came from.
            context: lsp::ReferenceContext {
                include_declaration: false,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        cx.background_spawn(async move {
            let locations = request
                .await
                .map_err(|e| anyhow!("{e}"))?
                .unwrap_or_default();
            Ok(locations
                .into_iter()
                .filter_map(|l| location(&l.uri, l.range))
                .collect())
        })
    }

    pub fn hover(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &App,
    ) -> Task<Result<Option<lsp::Hover>>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let (document, position) = Self::position(buffer, offset, cx);
        let request = server.request::<req::HoverRequest>(lsp::HoverParams {
            text_document_position_params: lsp::TextDocumentPositionParams {
                text_document: document,
                position,
            },
            work_done_progress_params: Default::default(),
        });
        cx.background_spawn(async move { request.await.map_err(|e| anyhow!("{e}")) })
    }

    /// The uses of the name at `offset` in this file. Empty when the server doesn't do this.
    pub fn document_highlights(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &App,
    ) -> Task<Result<Vec<lsp::Range>>> {
        let Ok(server) = self.server_or_error(buffer, cx) else {
            return Task::ready(Ok(Vec::new()));
        };
        if server.capabilities().document_highlight_provider.is_none() {
            return Task::ready(Ok(Vec::new()));
        }
        let (document, position) = Self::position(buffer, offset, cx);
        let request =
            server.request::<req::DocumentHighlightRequest>(lsp::DocumentHighlightParams {
                text_document_position_params: lsp::TextDocumentPositionParams {
                    text_document: document,
                    position,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            });
        cx.background_spawn(async move {
            let found = request.await.map_err(|e| anyhow!("{e}"))?;
            Ok(found
                .unwrap_or_default()
                .into_iter()
                .map(|h| h.range)
                .collect())
        })
    }

    /// Inlay hints for the whole file. Empty when the server doesn't give them.
    pub fn inlay_hints(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Task<Result<Vec<lsp::InlayHint>>> {
        let Ok(server) = self.server_or_error(buffer, cx) else {
            return Task::ready(Ok(Vec::new()));
        };
        if server.capabilities().inlay_hint_provider.is_none() {
            return Task::ready(Ok(Vec::new()));
        }
        let model = buffer.read(cx);
        let (document, _) = Self::position(buffer, 0, cx);
        let end = to_lsp(model.buffer().offset_to_utf16(model.buffer().len()));
        let request = server.request::<req::InlayHintRequest>(lsp::InlayHintParams {
            text_document: document,
            range: lsp::Range::new(lsp::Position::new(0, 0), end),
            work_done_progress_params: Default::default(),
        });
        cx.background_spawn(async move {
            Ok(request
                .await
                .map_err(|e| anyhow!("{e}"))?
                .unwrap_or_default())
        })
    }

    /// The file's semantic tokens, with the names of their types from the server's legend.
    pub fn semantic_tokens(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Task<Result<(Vec<String>, Vec<lsp::SemanticToken>)>> {
        let Ok(server) = self.server_or_error(buffer, cx) else {
            return Task::ready(Ok(Default::default()));
        };
        let legend = match server.capabilities().semantic_tokens_provider {
            Some(lsp::SemanticTokensServerCapabilities::SemanticTokensOptions(options)) => {
                options.legend
            }
            Some(lsp::SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
                options,
            )) => options.semantic_tokens_options.legend,
            None => return Task::ready(Ok(Default::default())),
        };
        let types: Vec<String> = legend
            .token_types
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        let (document, _) = Self::position(buffer, 0, cx);
        let request = server.request::<req::SemanticTokensFullRequest>(lsp::SemanticTokensParams {
            text_document: document,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        cx.background_spawn(async move {
            let tokens = match request.await.map_err(|e| anyhow!("{e}"))? {
                Some(lsp::SemanticTokensResult::Tokens(tokens)) => tokens.data,
                Some(lsp::SemanticTokensResult::Partial(partial)) => partial.data,
                None => Vec::new(),
            };
            Ok((types, tokens))
        })
    }

    pub fn completions(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        trigger: Option<char>,
        cx: &App,
    ) -> Task<Result<lsp::CompletionResponse>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let (document, position) = Self::position(buffer, offset, cx);
        let context = Some(match trigger {
            Some(c) => lsp::CompletionContext {
                trigger_kind: lsp::CompletionTriggerKind::TRIGGER_CHARACTER,
                trigger_character: Some(c.to_string()),
            },
            None => lsp::CompletionContext {
                trigger_kind: lsp::CompletionTriggerKind::INVOKED,
                trigger_character: None,
            },
        });
        let request = server.request::<req::Completion>(lsp::CompletionParams {
            text_document_position: lsp::TextDocumentPositionParams {
                text_document: document,
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context,
        });
        cx.background_spawn(async move {
            Ok(request
                .await
                .map_err(|e| anyhow!("{e}"))?
                .unwrap_or(lsp::CompletionResponse::Array(Vec::new())))
        })
    }

    pub fn resolve_completion(
        &self,
        buffer: &Entity<BufferModel>,
        item: lsp::CompletionItem,
        cx: &App,
    ) -> Task<Result<lsp::CompletionItem>> {
        let Ok(server) = self.server_or_error(buffer, cx) else {
            return Task::ready(Ok(item));
        };
        let resolves = server
            .capabilities()
            .completion_provider
            .and_then(|c| c.resolve_provider)
            .unwrap_or(false);
        if !resolves {
            return Task::ready(Ok(item));
        }
        let request = server.request::<req::ResolveCompletionItem>(item.clone());
        cx.background_spawn(async move { Ok(request.await.unwrap_or(item)) })
    }

    pub fn signature_help(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &App,
    ) -> Task<Result<Option<lsp::SignatureHelp>>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let (document, position) = Self::position(buffer, offset, cx);
        let request = server.request::<req::SignatureHelpRequest>(lsp::SignatureHelpParams {
            text_document_position_params: lsp::TextDocumentPositionParams {
                text_document: document,
                position,
            },
            work_done_progress_params: Default::default(),
            context: None,
        });
        cx.background_spawn(async move { request.await.map_err(|e| anyhow!("{e}")) })
    }

    pub fn document_symbols(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Task<Result<Option<lsp::DocumentSymbolResponse>>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let (document, _) = Self::position(buffer, 0, cx);
        let request = server.request::<req::DocumentSymbolRequest>(lsp::DocumentSymbolParams {
            text_document: document,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        cx.background_spawn(async move { request.await.map_err(|e| anyhow!("{e}")) })
    }

    /// Searches every running server's workspace symbols.
    pub fn workspace_symbols(&self, query: &str, cx: &App) -> Task<Vec<SymbolHit>> {
        let requests: Vec<_> = self
            .servers
            .values()
            .filter_map(|s| {
                s.handle
                    .clone()
                    .filter(|h| h.is_alive() && matches!(s.status, ServerStatus::Running { .. }))
            })
            .map(|handle| {
                handle.request::<req::WorkspaceSymbolRequest>(lsp::WorkspaceSymbolParams {
                    query: query.to_string(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
            })
            .collect();
        cx.background_spawn(async move {
            let mut hits = Vec::new();
            for result in futures::future::join_all(requests).await {
                let Ok(Some(response)) = result else { continue };
                match response {
                    lsp::WorkspaceSymbolResponse::Flat(symbols) => {
                        for symbol in symbols {
                            if let Some(location) =
                                location(&symbol.location.uri, symbol.location.range)
                            {
                                hits.push(SymbolHit {
                                    name: symbol.name,
                                    kind: symbol.kind,
                                    container: symbol.container_name,
                                    location,
                                });
                            }
                        }
                    }
                    lsp::WorkspaceSymbolResponse::Nested(symbols) => {
                        for symbol in symbols {
                            let (uri, range) = match symbol.location {
                                lsp::OneOf::Left(l) => (l.uri, l.range),
                                lsp::OneOf::Right(l) => (l.uri, lsp::Range::default()),
                            };
                            if let Some(location) = location(&uri, range) {
                                hits.push(SymbolHit {
                                    name: symbol.name,
                                    kind: symbol.kind,
                                    container: symbol.container_name,
                                    location,
                                });
                            }
                        }
                    }
                }
            }
            hits
        })
    }

    pub fn rename(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        new_name: &str,
        cx: &App,
    ) -> Task<Result<lsp::WorkspaceEdit>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let (document, position) = Self::position(buffer, offset, cx);
        let request = server.request::<req::Rename>(lsp::RenameParams {
            text_document_position: lsp::TextDocumentPositionParams {
                text_document: document,
                position,
            },
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        });
        cx.background_spawn(async move {
            request
                .await
                .map_err(|e| anyhow!("{e}"))?
                .ok_or_else(|| anyhow!("nothing to rename here"))
        })
    }

    pub fn code_actions(
        &self,
        buffer: &Entity<BufferModel>,
        range: std::ops::Range<usize>,
        cx: &App,
    ) -> Task<Result<Vec<lsp::CodeActionOrCommand>>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let model = buffer.read(cx);
        let (document, _) = Self::position(buffer, 0, cx);
        let lsp_range = lsp::Range::new(
            to_lsp(model.buffer().offset_to_utf16(range.start)),
            to_lsp(model.buffer().offset_to_utf16(range.end)),
        );
        // The diagnostics there, which quick fixes are for.
        let diagnostics: Vec<lsp::Diagnostic> = model
            .diagnostics()
            .iter()
            .filter(|d| d.range.start <= range.end && range.start <= d.range.end)
            .map(|d| lsp::Diagnostic {
                range: lsp::Range::new(
                    to_lsp(model.buffer().offset_to_utf16(d.range.start)),
                    to_lsp(model.buffer().offset_to_utf16(d.range.end)),
                ),
                severity: None,
                message: d.message.clone(),
                source: d.source.clone(),
                ..Default::default()
            })
            .collect();
        let request = server.request::<req::CodeActionRequest>(lsp::CodeActionParams {
            text_document: document,
            range: lsp_range,
            context: lsp::CodeActionContext {
                diagnostics,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        cx.background_spawn(async move {
            Ok(request
                .await
                .map_err(|e| anyhow!("{e}"))?
                .unwrap_or_default())
        })
    }

    /// Finishes a code action that came without its edit, and runs its command if it has one.
    pub fn resolve_code_action(
        &self,
        buffer: &Entity<BufferModel>,
        action: lsp::CodeAction,
        cx: &App,
    ) -> Task<Result<lsp::CodeAction>> {
        let Ok(server) = self.server_or_error(buffer, cx) else {
            return Task::ready(Ok(action));
        };
        if action.edit.is_some() {
            return Task::ready(Ok(action));
        }
        let request = server.request::<req::CodeActionResolveRequest>(action.clone());
        cx.background_spawn(async move { Ok(request.await.unwrap_or(action)) })
    }

    pub fn execute_command(
        &self,
        buffer: &Entity<BufferModel>,
        command: lsp::Command,
        cx: &App,
    ) -> Task<Result<()>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let request = server.request::<req::ExecuteCommand>(lsp::ExecuteCommandParams {
            command: command.command,
            arguments: command.arguments.unwrap_or_default(),
            work_done_progress_params: Default::default(),
        });
        cx.background_spawn(async move {
            request.await.map_err(|e| anyhow!("{e}"))?;
            Ok(())
        })
    }

    pub fn format(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Task<Result<Vec<lsp::TextEdit>>> {
        let server = match self.server_or_error(buffer, cx) {
            Ok(server) => server,
            Err(error) => return Task::ready(Err(error)),
        };
        let model = buffer.read(cx);
        let config = model.edit_config();
        let (document, _) = Self::position(buffer, 0, cx);
        let request = server.request::<req::Formatting>(lsp::DocumentFormattingParams {
            text_document: document,
            options: lsp::FormattingOptions {
                tab_size: config.tab_size as u32,
                insert_spaces: !config.hard_tabs,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        });
        cx.background_spawn(async move {
            Ok(request
                .await
                .map_err(|e| anyhow!("{e}"))?
                .unwrap_or_default())
        })
    }
}

impl Server {
    fn new(status: ServerStatus) -> Self {
        Self {
            status,
            handle: None,
            crashes: Vec::new(),
            progress: BTreeMap::new(),
            warmed: false,
            _task: None,
        }
    }
}

fn location(uri: &lsp::Uri, range: lsp::Range) -> Option<Location> {
    Some(Location {
        path: uri_to_path(uri)?,
        start: from_lsp(range.start),
        end: from_lsp(range.end),
    })
}

fn goto_locations(response: Option<lsp::GotoDefinitionResponse>) -> Vec<Location> {
    match response {
        None => Vec::new(),
        Some(lsp::GotoDefinitionResponse::Scalar(l)) => {
            location(&l.uri, l.range).into_iter().collect()
        }
        Some(lsp::GotoDefinitionResponse::Array(list)) => list
            .into_iter()
            .filter_map(|l| location(&l.uri, l.range))
            .collect(),
        // Links carry the whole target and the part to select, which is the name.
        Some(lsp::GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .filter_map(|l| location(&l.target_uri, l.target_selection_range))
            .collect(),
    }
}

#[cfg(test)]
mod tests;
