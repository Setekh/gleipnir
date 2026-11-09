//! Running and debugging for a window: what the project can run, each launch with its console,
//! the debug sessions inside a launch, and the project's breakpoints.
//!
//! A *launch* is one press of Run or Debug: an optional build, then the program, either on its own
//! or under a debug adapter. A debug launch holds a tree of *sessions*, as js-debug starts the
//! real one as a child of the first. Whichever session stopped last has the focus: stepping, the
//! frames and the variables all refer to it.

mod breakpoints;
mod process;
mod test_runs;
#[cfg(test)]
mod tests;

pub use breakpoints::{Breakpoint, Breakpoints};
pub use process::{ProcessCommand, ProcessHandle, ProcessInfo, ProcessOutput, list_processes};

use crate::buffer::{BufferEvent, BufferModel};
use crate::test_results::TestRun;
use crate::toolchains::Toolchains;
use anyhow::{Result, anyhow};
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gleipnir_dap::session::{self as dap_session};
use gleipnir_dap::{
    AdapterCommand, Capabilities, Change, Connected, DebugAdapter, EvaluateResult, FileBreakpoints,
    Incoming, LaunchRequest, Scope, SessionState, StackFrame, StartDebugging, Step, Thread,
    Variable,
};
use gleipnir_packages::adapters::{TypeScriptAdapter, debugger_command};
use gleipnir_packages::{Env, RunTarget, adapter_for, registry};
use gpui::{App, Context, Entity, EntityId, EventEmitter, Subscription, Task};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

pub type LaunchId = u64;

/// What a run configuration changes about its target: the program's arguments, environment and
/// working folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunSettings {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

impl RunSettings {
    pub fn is_empty(&self) -> bool {
        self.args.is_empty() && self.env.is_empty() && self.cwd.is_none()
    }

    /// A target as these settings change it.
    pub fn apply(&self, mut target: RunTarget) -> RunTarget {
        if !self.args.is_empty() {
            // npm passes on what comes after `--`.
            if target.run.first().is_some_and(|p| p == "npm") && !target.run.contains(&"--".into())
            {
                target.run.push("--".into());
            }
            target.run.extend(self.args.iter().cloned());
            target.args.extend(self.args.iter().cloned());
        }
        if let Some(cwd) = &self.cwd {
            target.cwd = cwd.clone();
        }
        target
    }
}

/// Splits arguments as a shell would, keeping quoted parts together.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (_, '\\') if quote != Some('\'') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    started = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (_, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// How many stack frames to fetch at a stop. Deeper ones are rarely looked at.
const FRAME_LEVELS: u32 = 200;
/// A console keeps at most this many lines, dropping the oldest.
const CONSOLE_LINES: usize = 20_000;
/// Finished launches kept around as tabs.
const KEPT_LAUNCHES: usize = 8;
/// How long a launch waits for the project's toolchains to be found.
const TOOLCHAIN_WAIT: Duration = Duration::from_secs(30);
/// How long a debug session gets to end politely before it's dropped.
const STOP_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchStatus {
    Building,
    Starting,
    Running,
    /// Stopped in the debugger.
    Paused,
    /// The exit code, if the program reported one.
    Finished(Option<i64>),
    Failed(String),
}

impl LaunchStatus {
    pub fn is_live(&self) -> bool {
        !matches!(self, Self::Finished(_) | Self::Failed(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    Stdout,
    Stderr,
    /// Said by the IDE or the debugger, like the build command or the exit code.
    Info,
    /// An expression typed into the console.
    Input,
    /// What it evaluated to.
    Result,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleLine {
    pub kind: ConsoleKind,
    pub text: String,
    /// Whether the line has ended, or more of it may still come.
    pub complete: bool,
}

/// A watch expression's value in the focused frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchValue {
    Pending,
    Value(EvaluateResult),
    Error(String),
}

/// What a breakpoint turned out to be in the running sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BreakpointStatus {
    pub verified: bool,
    pub message: Option<String>,
}

pub struct Session {
    pub name: String,
    adapter: Arc<DebugAdapter>,
    /// What the adapter was called in `initialize`, like `lldb-dap`.
    adapter_id: String,
    pub parent: Option<usize>,
    pub state: SessionState,
    pub capabilities: Capabilities,
    pub threads: Vec<Thread>,
    /// The thread shown: the one that stopped, unless another was chosen.
    pub thread: Option<i64>,
    pub frames: Vec<StackFrame>,
    pub frame: usize,
    pub scopes: Vec<Scope>,
    /// Children fetched so far, by variables reference.
    pub variables: HashMap<i64, Vec<Variable>>,
    /// One per watch expression, in order.
    pub watches: Vec<WatchValue>,
    /// Run to cursor's breakpoint, removed at the next stop.
    temporary: Option<(PathBuf, u32)>,
    ended: bool,
    _events: Option<Task<()>>,
}

impl Session {
    fn new(
        name: String,
        adapter: Arc<DebugAdapter>,
        adapter_id: String,
        parent: Option<usize>,
    ) -> Self {
        Self {
            name,
            adapter,
            adapter_id,
            parent,
            state: SessionState::new(),
            capabilities: Capabilities::default(),
            threads: Vec::new(),
            thread: None,
            frames: Vec::new(),
            frame: 0,
            scopes: Vec::new(),
            variables: HashMap::new(),
            watches: Vec::new(),
            temporary: None,
            ended: false,
            _events: None,
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.state.stopped().is_some()
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    pub fn selected_frame(&self) -> Option<&StackFrame> {
        self.frames.get(self.frame)
    }

    fn clear_stop(&mut self) {
        self.frames.clear();
        self.frame = 0;
        self.scopes.clear();
        self.variables.clear();
        self.watches.clear();
    }
}

pub struct Launch {
    pub id: LaunchId,
    pub name: String,
    pub target: RunTarget,
    pub debug: bool,
    pub status: LaunchStatus,
    pub console: Vec<ConsoleLine>,
    pub sessions: Vec<Session>,
    /// The session that stopped last.
    pub focus: Option<usize>,
    process: Option<ProcessHandle>,
    /// The environment the program runs with, once the toolchain's been found.
    env: Option<Env>,
    /// Forwards the output of a program the debugger asked us to start.
    _program_output: Option<Task<()>>,
    /// For a test target, its results as they come in.
    pub tests: Option<TestRun>,

    stopping: bool,
    _task: Option<Task<()>>,
}

impl Launch {
    /// The live lldb session, whose program's terminal can be typed into through lldb.
    fn lldb_session(&self) -> Option<&Session> {
        self.sessions
            .iter()
            .find(|s| !s.ended && s.adapter_id == "lldb-dap")
    }

    pub fn focused(&self) -> Option<&Session> {
        self.focus.and_then(|i| self.sessions.get(i))
    }

    /// The console as plain text, for tests and the driver.
    pub fn console_text(&self) -> String {
        let mut text = String::new();
        for line in &self.console {
            text.push_str(&line.text);
            if line.complete {
                text.push('\n');
            }
        }
        text
    }

    fn print(&mut self, kind: ConsoleKind, text: &str) {
        for piece in text.split_inclusive('\n') {
            let (body, complete) = match piece.strip_suffix('\n') {
                Some(body) => (body.strip_suffix('\r').unwrap_or(body), true),
                None => (piece, false),
            };
            match self.console.last_mut() {
                Some(last) if !last.complete && last.kind == kind => {
                    last.text.push_str(body);
                    last.complete = complete;
                }
                _ => self.console.push(ConsoleLine {
                    kind,
                    text: body.to_string(),
                    complete,
                }),
            }
        }
        if self.console.len() > CONSOLE_LINES {
            let excess = self.console.len() - CONSOLE_LINES;
            self.console.drain(..excess);
        }
    }

    /// The program's own output, which a test run reads its results from.
    fn program_output(&mut self, kind: ConsoleKind, text: &str) {
        let shown = match &mut self.tests {
            Some(tests) => tests.feed(kind, text),
            None => vec![(kind, text.to_string())],
        };
        for (kind, text) in shown {
            self.print(kind, &text);
        }
    }

    fn println(&mut self, kind: ConsoleKind, text: &str) {
        if self.console.last().is_some_and(|l| !l.complete) {
            self.print(kind, "\n");
        }
        self.print(kind, &format!("{text}\n"));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebuggerEvent {
    BreakpointsChanged,
    LaunchesChanged,
    /// Execution stopped, or another frame was chosen: show this line.
    ShowLocation {
        path: PathBuf,
        line: u32,
    },
    /// A launch stopped in the debugger.
    Stopped(LaunchId),
    /// A launch started, so its panel should show.
    Started {
        id: LaunchId,
        debug: bool,
    },
    /// What editors show changed: breakpoint statuses, the execution line, values or Run icons.
    MarksChanged,
    /// Watches or the chosen run configuration changed, which are saved with the project.
    SettingsChanged,
}

/// An adapter about to be started: what to call it, and how to connect. Connecting can block, so
/// it runs off the UI thread.
pub struct PreparedAdapter {
    pub adapter_id: String,
    pub connect: Box<dyn FnOnce() -> Result<Connected> + Send>,
}

/// Prepares a package's debug adapter. Swappable so tests can use an in-process fake.
pub type AdapterLauncher = Rc<dyn Fn(&str, &Path, &App) -> Result<PreparedAdapter>>;

/// Starts the real adapter a package's manifest names, with the project's SDK.
pub fn toolchain_adapter_launcher(toolchains: Entity<Toolchains>) -> AdapterLauncher {
    Rc::new(move |package_id, cwd, cx| {
        let package = registry()
            .package(package_id)
            .ok_or_else(|| anyhow!("no package {package_id}"))?;
        let spec = package
            .manifest
            .debugger
            .as_ref()
            .ok_or_else(|| anyhow!("{} has no debugger", package.name()))?;
        let toolchains = toolchains.read(cx);
        let tool = toolchains
            .tool_path(package_id, &spec.tool)
            .ok_or_else(|| anyhow!("{} isn't installed", spec.tool))?;
        let sdk_binary = package.manifest.sdk.as_ref().and_then(|sdk_spec| {
            let chosen = toolchains.package(package_id)?.chosen.as_ref()?;
            Some(chosen.root.join(&sdk_spec.binary))
        });
        let (program, args) = debugger_command(spec, &tool, sdk_binary.as_deref())?;
        let env = toolchains.env_for(package_id).unwrap_or_default();
        let command = AdapterCommand {
            program,
            args,
            env: env_pairs(&env),
            cwd: cwd.to_path_buf(),
            tcp: spec.transport == gleipnir_packages::manifest::Transport::Tcp,
        };
        let name = spec.tool.clone();
        Ok(PreparedAdapter {
            adapter_id: spec.adapter_id().to_string(),
            connect: Box::new(move || DebugAdapter::start(&name, &command)),
        })
    })
}

fn env_pairs(env: &Env) -> Vec<(String, String)> {
    env.vars()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub struct Debugger {
    root: Option<PathBuf>,
    toolchains: Entity<Toolchains>,
    launcher: AdapterLauncher,
    breakpoints: Breakpoints,
    /// For the live sessions, what each breakpoint turned out to be, by file and requested line.
    statuses: HashMap<PathBuf, BTreeMap<u32, BreakpointStatus>>,
    /// Adapters' ids for breakpoints, for their later `breakpoint` events.
    breakpoint_ids: HashMap<i64, (PathBuf, u32)>,
    targets: Vec<RunTarget>,
    selected_target: Option<String>,
    launches: Vec<Launch>,
    active: Option<LaunchId>,
    next_id: LaunchId,
    watches: Vec<String>,
    /// Arguments, environment and folder set for run configurations, by name.
    settings: BTreeMap<String, RunSettings>,
    /// Each adapter's exception filters, as last seen, so they can be chosen before a session.
    exception_filters: BTreeMap<String, Vec<gleipnir_dap::ExceptionFilter>>,
    /// Filters turned on or off by hand, by adapter and filter id; the rest follow the adapter.
    exception_choices: BTreeMap<String, bool>,
    buffers: HashMap<EntityId, (gpui::WeakEntity<BufferModel>, Subscription)>,
    /// The last text seen of each file with breakpoints, so they can follow a change made while
    /// the file isn't open, like a branch switch.
    known_text: HashMap<PathBuf, String>,
    /// Each test's last result, by file and function name, for the gutter.
    test_marks: HashMap<PathBuf, HashMap<String, crate::TestState>>,
    _targets_task: Option<Task<()>>,
}

impl EventEmitter<DebuggerEvent> for Debugger {}

impl Debugger {
    pub fn new(
        root: Option<PathBuf>,
        toolchains: Entity<Toolchains>,
        cx: &mut Context<Self>,
    ) -> Self {
        let launcher = toolchain_adapter_launcher(toolchains.clone());
        Self::with_launcher(root, toolchains, launcher, cx)
    }

    pub fn with_launcher(
        root: Option<PathBuf>,
        toolchains: Entity<Toolchains>,
        launcher: AdapterLauncher,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            root,
            toolchains,
            launcher,
            breakpoints: Breakpoints::default(),
            statuses: HashMap::new(),
            breakpoint_ids: HashMap::new(),
            targets: Vec::new(),
            selected_target: None,
            launches: Vec::new(),
            active: None,
            next_id: 1,
            watches: Vec::new(),
            settings: BTreeMap::new(),
            exception_filters: BTreeMap::new(),
            exception_choices: BTreeMap::new(),
            buffers: HashMap::new(),
            known_text: HashMap::new(),
            test_marks: HashMap::new(),
            _targets_task: None,
        };
        this.refresh_targets(cx);
        this
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    // Breakpoints.

    pub fn breakpoints(&self) -> &Breakpoints {
        &self.breakpoints
    }

    pub fn breakpoint_status(&self, path: &Path, line: u32) -> Option<&BreakpointStatus> {
        self.statuses.get(path)?.get(&line)
    }

    /// Replaces every breakpoint, as when restoring a saved project.
    pub fn set_all_breakpoints(&mut self, breakpoints: Breakpoints, cx: &mut Context<Self>) {
        let mut paths: Vec<PathBuf> = self.breakpoints.files().map(|(p, _)| p.into()).collect();
        paths.extend(breakpoints.files().map(|(p, _)| p.to_path_buf()));
        self.breakpoints = breakpoints;
        paths.sort();
        paths.dedup();
        for path in paths {
            self.send_breakpoints(&path, cx);
        }
        cx.emit(DebuggerEvent::BreakpointsChanged);
        cx.notify();
    }

    pub fn toggle_breakpoint(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        self.breakpoints.toggle(path, line);
        self.breakpoints_changed(path, cx);
    }

    /// Adds or replaces the breakpoint on its line.
    pub fn set_breakpoint(&mut self, path: &Path, breakpoint: Breakpoint, cx: &mut Context<Self>) {
        self.breakpoints.set(path, breakpoint);
        self.breakpoints_changed(path, cx);
    }

    pub fn remove_breakpoint(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        if self.breakpoints.remove(path, line).is_some() {
            self.breakpoints_changed(path, cx);
        }
    }

    pub fn remove_all_breakpoints(&mut self, cx: &mut Context<Self>) {
        self.set_all_breakpoints(Breakpoints::default(), cx);
    }

    fn breakpoints_changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.known_text.contains_key(path)
            && !self.breakpoints.in_file(path).is_empty()
            && let Ok(text) = std::fs::read_to_string(path)
        {
            self.known_text
                .insert(path.to_path_buf(), text.replace("\r\n", "\n"));
        }
        self.send_breakpoints(path, cx);
        cx.emit(DebuggerEvent::BreakpointsChanged);
        cx.notify();
    }

    /// Keeps breakpoints in a buffer's file on their lines as it's edited.
    pub fn track_buffer(&mut self, buffer: &Entity<BufferModel>, cx: &mut Context<Self>) {
        if self.buffers.contains_key(&buffer.entity_id()) {
            return;
        }
        let subscription = cx.subscribe(buffer, |this, buffer, event, cx| {
            if matches!(event, BufferEvent::Saved) {
                let model = buffer.read(cx);
                if let Some(path) = model.path().filter(|p| this.known_text.contains_key(*p)) {
                    this.known_text
                        .insert(path.to_path_buf(), model.buffer().text());
                }
                return;
            }
            let BufferEvent::Edited { edits, .. } = event else {
                return;
            };
            let Some(path) = buffer.read(cx).path().map(Path::to_path_buf) else {
                return;
            };
            let text = buffer.read(cx).buffer();
            let blank_before = |edit: &gleipnir_text::AppliedEdit| {
                let line = text.line(edit.start.line);
                line.get(..edit.start.col)
                    .is_some_and(|before| before.trim().is_empty())
            };
            if this.breakpoints.apply_edits(&path, edits, blank_before) {
                // Statuses were for the old lines; the adapter's answer to the resend replaces them.
                this.statuses.remove(&path);
                this.breakpoints_changed(&path, cx);
            }
        });
        self.buffers
            .insert(buffer.entity_id(), (buffer.downgrade(), subscription));
        let id = buffer.entity_id();
        cx.observe_release(buffer, move |this, _, _| {
            this.buffers.remove(&id);
        })
        .detach();
    }

    /// Files changed on disk, perhaps by another program. Breakpoints in an open file follow its
    /// buffer as it reloads; in a file that isn't open, they follow a diff of the old text and new.
    pub fn files_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        for path in paths {
            if self.breakpoints.in_file(path).is_empty() {
                continue;
            }
            let Ok(new) = std::fs::read_to_string(path) else {
                continue;
            };
            let new = new.replace("\r\n", "\n");
            let open = self
                .buffers
                .values()
                .filter_map(|(buffer, _)| buffer.upgrade())
                .any(|b| b.read(cx).path() == Some(path.as_path()));
            let old = self.known_text.insert(path.clone(), new.clone());
            let (false, Some(old)) = (open, old) else {
                continue;
            };
            let hunks = gleipnir_text::diff::diff_lines(&old, &new);
            if hunks.is_empty() {
                continue;
            }
            let moved: Vec<Breakpoint> = self
                .breakpoints
                .in_file(path)
                .iter()
                .map(|b| Breakpoint {
                    line: gleipnir_text::diff::map_line(&hunks, b.line as usize) as u32,
                    ..b.clone()
                })
                .collect();
            for breakpoint in self.breakpoints.in_file(path).to_vec() {
                self.breakpoints.remove(path, breakpoint.line);
            }
            for breakpoint in moved {
                // Two landing on one line keep the first.
                if self.breakpoints.get(path, breakpoint.line).is_none() {
                    self.breakpoints.set(path, breakpoint);
                }
            }
            self.statuses.remove(path);
            self.breakpoints_changed(path, cx);
        }
    }

    /// Tells every live session about a file's breakpoints.
    fn send_breakpoints(&mut self, path: &Path, cx: &mut Context<Self>) {
        let mut adapters = Vec::new();
        for launch in &self.launches {
            if !launch.status.is_live() {
                continue;
            }
            for (index, session) in launch.sessions.iter().enumerate() {
                if session.ended || session.state.run == gleipnir_dap::RunState::Starting {
                    continue;
                }
                let mut list = self.breakpoints.for_adapter(path);
                if let Some((temp_path, line)) = &session.temporary
                    && temp_path == path
                {
                    list.push(Breakpoint::at(*line).to_source());
                }
                adapters.push((launch.id, index, session.adapter.clone(), list));
            }
        }
        if adapters.is_empty() {
            return;
        }
        self.statuses.remove(path);
        for (launch, session, adapter, list) in adapters {
            let path = path.to_path_buf();
            cx.spawn(async move |this, cx| {
                let placed = dap_session::set_breakpoints(&adapter, &path, &list).await;
                this.update(cx, |this, cx| match placed {
                    Ok(placed) => {
                        this.record_placed(&path, &list, &placed);
                        cx.emit(DebuggerEvent::MarksChanged);
                        cx.notify();
                    }
                    Err(error) => {
                        this.with_launch(launch, |launch| {
                            let name = launch.sessions[session].name.clone();
                            launch.println(
                                ConsoleKind::Error,
                                &format!("{name} refused the breakpoints: {error}"),
                            )
                        });
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn record_placed(
        &mut self,
        path: &Path,
        sent: &[gleipnir_dap::SourceBreakpoint],
        placed: &[gleipnir_dap::Breakpoint],
    ) {
        let statuses = self.statuses.entry(path.to_path_buf()).or_default();
        for (asked, answer) in sent.iter().zip(placed) {
            let line = asked.line - 1;
            if let Some(id) = answer.id {
                self.breakpoint_ids.insert(id, (path.to_path_buf(), line));
            }
            let status = statuses.entry(line).or_default();
            // Verified by any session counts, as js-debug's parent session never is.
            status.verified |= answer.verified;
            if !answer.verified && answer.message.is_some() {
                status.message = answer.message.clone();
            }
        }
    }

    // Run targets.

    pub fn targets(&self) -> &[RunTarget] {
        &self.targets
    }

    /// Finds what the project can run again, in the background.
    pub fn refresh_targets(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let task = cx.background_executor().spawn(async move {
            let mut targets = Vec::new();
            for package in registry().packages() {
                if let Some(adapter) = adapter_for(package.id()) {
                    targets.extend(adapter.run_targets(&root));
                }
            }
            targets
        });
        self._targets_task = Some(cx.spawn(async move |this, cx| {
            let targets = task.await;
            this.update(cx, |this, cx| {
                this.targets = targets;
                cx.emit(DebuggerEvent::LaunchesChanged);
                cx.emit(DebuggerEvent::MarksChanged);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Replaces the targets, for tests that don't want to depend on finding them.
    pub fn set_targets(&mut self, targets: Vec<RunTarget>, cx: &mut Context<Self>) {
        self._targets_task = None;
        self.targets = targets;
        cx.emit(DebuggerEvent::MarksChanged);
        cx.notify();
    }

    /// The chosen run configuration, or the first there is.
    pub fn selected_target(&self) -> Option<&RunTarget> {
        self.selected_target
            .as_ref()
            .and_then(|name| self.targets.iter().find(|t| &t.name == name))
            .or_else(|| {
                // A file chosen by running it isn't among the project's targets.
                self.selected_target.as_ref().and_then(|name| {
                    self.launches
                        .iter()
                        .rev()
                        .find(|l| &l.target.name == name)
                        .map(|l| &l.target)
                })
            })
            .or_else(|| self.targets.first())
    }

    pub fn selected_target_name(&self) -> Option<&str> {
        self.selected_target.as_deref()
    }

    pub fn select_target(&mut self, name: &str, cx: &mut Context<Self>) {
        self.selected_target = Some(name.to_string());
        cx.emit(DebuggerEvent::SettingsChanged);
        cx.notify();
    }

    /// Restores the chosen configuration, as saved.
    pub fn set_selected_target(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.selected_target = name;
        cx.notify();
    }

    /// What running a file means: the target it's the entry point of, or for a script, itself.
    pub fn target_for_file(&self, path: &Path) -> Option<RunTarget> {
        let exact = self
            .targets
            .iter()
            .find(|t| t.source.as_deref() == Some(path));
        // A Go or Odin package is a folder, which claims only files in its own language.
        let package = registry()
            .language_for_path(path)
            .map(|language| language.package_id.clone());
        let folder = || {
            self.targets.iter().find(|t| {
                Some(&t.package_id) == package.as_ref()
                    && t.source
                        .as_deref()
                        .is_some_and(|s| path.parent() == Some(s) && s.is_dir())
            })
        };
        if let Some(target) = exact.or_else(folder) {
            return Some(target.clone());
        }
        let extension = path.extension()?.to_str()?;
        let script = matches!(extension, "js" | "mjs" | "cjs" | "ts" | "mts" | "cts");
        script.then(|| {
            let root = self
                .root
                .clone()
                .unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_default());
            TypeScriptAdapter.file_target(&root, path)
        })
    }

    /// The test declared on a line of a file, given its text, to run from the gutter.
    pub fn test_at(&self, path: &Path, text: &str, line: usize) -> Option<RunTarget> {
        let package = registry().language_for_path(path)?.package_id.clone();
        let root = self.root.clone()?;
        adapter_for(&package)?.test_at(&root, path, text, line)
    }

    // Launches.

    pub fn launches(&self) -> &[Launch] {
        &self.launches
    }

    pub fn launch(&self, id: LaunchId) -> Option<&Launch> {
        self.launches.iter().find(|l| l.id == id)
    }

    /// The launch the toolbar and panels act on: the one chosen, or the latest.
    pub fn active_launch(&self) -> Option<&Launch> {
        self.active
            .and_then(|id| self.launch(id))
            .or_else(|| self.launches.last())
    }

    /// The active debug launch, which the Debug panel and stepping act on.
    pub fn active_debug_launch(&self) -> Option<&Launch> {
        match self.active_launch() {
            Some(launch) if launch.debug => Some(launch),
            _ => self.launches.iter().rev().find(|l| l.debug),
        }
    }

    pub fn set_active(&mut self, id: LaunchId, cx: &mut Context<Self>) {
        self.active = Some(id);
        cx.emit(DebuggerEvent::LaunchesChanged);
        cx.notify();
    }

    pub fn is_running(&self) -> bool {
        self.launches.iter().any(|l| l.status.is_live())
    }

    fn with_launch<R>(&mut self, id: LaunchId, f: impl FnOnce(&mut Launch) -> R) -> Option<R> {
        self.launches.iter_mut().find(|l| l.id == id).map(f)
    }

    /// Runs a target, building it first if it needs that, with or without the debugger.
    /// What's been set for a run configuration.
    pub fn run_settings(&self, name: &str) -> RunSettings {
        self.settings.get(name).cloned().unwrap_or_default()
    }

    pub fn all_run_settings(&self) -> &BTreeMap<String, RunSettings> {
        &self.settings
    }

    pub fn set_run_settings(&mut self, name: &str, settings: RunSettings, cx: &mut Context<Self>) {
        if settings.is_empty() {
            self.settings.remove(name);
        } else {
            self.settings.insert(name.to_string(), settings);
        }
        cx.emit(DebuggerEvent::SettingsChanged);
        cx.notify();
    }

    /// Restores every configuration's settings, as saved.
    pub fn restore_run_settings(&mut self, settings: BTreeMap<String, RunSettings>) {
        self.settings = settings;
    }

    pub fn start(&mut self, target: RunTarget, debug: bool, cx: &mut Context<Self>) -> LaunchId {
        let settings = self.run_settings(&target.name);
        let target = settings.apply(target);
        // Running the same thing again replaces its tab, stopping it if it's still going.
        let previous: Vec<LaunchId> = self
            .launches
            .iter()
            .filter(|l| l.target.name == target.name && l.debug == debug)
            .map(|l| l.id)
            .collect();
        for id in previous {
            self.stop(id, cx);
            self.launches.retain(|l| l.id != id || l.status.is_live());
        }
        while self.launches.len() >= KEPT_LAUNCHES {
            match self.launches.iter().position(|l| !l.status.is_live()) {
                Some(index) => drop(self.launches.remove(index)),
                None => break,
            }
        }

        let id = self.next_id;
        self.next_id += 1;
        self.selected_target = Some(target.name.clone());
        let mut launch = Launch {
            id,
            name: target.name.clone(),
            target: target.clone(),
            debug,
            status: LaunchStatus::Starting,
            console: Vec::new(),
            sessions: Vec::new(),
            focus: None,
            process: None,
            env: None,
            _program_output: None,
            tests: None,
            stopping: false,
            _task: None,
        };
        let toolchains = self.toolchains.clone();
        launch._task = Some(cx.spawn(async move |this, cx| {
            // Right after opening a project, its SDKs and tools may still be being found.
            let mut waited = Duration::ZERO;
            while waited < TOOLCHAIN_WAIT && toolchains.read_with(cx, |t, _| t.is_detecting()) {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                waited += Duration::from_millis(50);
            }
            let mut env = toolchains.read_with(cx, |t, _| {
                t.env_for(&target.package_id)
                    .unwrap_or_else(gleipnir_packages::env::user_env)
            });
            for (key, value) in &settings.env {
                env.set(key.clone(), value.clone());
            }
            this.update(cx, |this, _| {
                this.with_launch(id, |l| l.env = Some(env.clone()));
            })
            .ok();
            if let Some(build) = target.build.clone() {
                // Compilers that write where they're told expect the folder to be there.
                if let Some(folder) = target.program.as_deref().and_then(Path::parent) {
                    std::fs::create_dir_all(folder).ok();
                }
                let built = run_build(&this, id, &build, &target.cwd, &env, cx).await;
                if !built {
                    return;
                }
            }
            if debug {
                if !install_adapter(&this, id, &target.package_id, &toolchains, cx).await {
                    return;
                }
                start_debugging(&this, id, &target, &env, cx).await;
            } else {
                run_program(&this, id, &target, &env, cx).await;
            }
        }));
        self.launches.push(launch);
        self.active = Some(id);
        cx.emit(DebuggerEvent::Started { id, debug });
        cx.emit(DebuggerEvent::LaunchesChanged);
        cx.notify();
        id
    }

    /// Debugs a process that's already running, with a language's debugger.
    pub fn attach(
        &mut self,
        package_id: &str,
        pid: u32,
        label: &str,
        cx: &mut Context<Self>,
    ) -> Option<LaunchId> {
        let language = adapter_for(package_id)?;
        let target = RunTarget {
            package_id: package_id.to_string(),
            name: format!("{label} ({pid})"),
            kind: gleipnir_packages::RunTargetKind::Binary,
            build: None,
            run: Vec::new(),
            program: None,
            cwd: self.root.clone().unwrap_or_else(|| PathBuf::from("/")),
            source: None,
            args: Vec::new(),
        };
        let id = self.next_id;
        self.next_id += 1;
        let toolchains = self.toolchains.clone();
        let task_target = target.clone();
        let task = cx.spawn(async move |this, cx| {
            // Getting ready to attach can mean signalling the process and waiting for it.
            let arguments = cx
                .background_executor()
                .spawn(async move { language.attach_arguments(pid) })
                .await;
            let Some(arguments) = arguments else {
                this.update(cx, |this, cx| {
                    this.fail(id, format!("Can't attach to process {pid}"), cx)
                })
                .ok();
                return;
            };
            let env = toolchains.read_with(cx, |t, _| {
                t.env_for(&task_target.package_id)
                    .unwrap_or_else(gleipnir_packages::env::user_env)
            });
            start_debugging_with(&this, id, &task_target, &env, Some(arguments), cx).await;
        });
        self.launches.push(Launch {
            id,
            name: target.name.clone(),
            target,
            debug: true,
            status: LaunchStatus::Starting,
            console: Vec::new(),
            sessions: Vec::new(),
            focus: None,
            process: None,
            env: None,
            _program_output: None,
            tests: None,
            stopping: false,
            _task: Some(task),
        });
        self.active = Some(id);
        cx.emit(DebuggerEvent::Started { id, debug: true });
        cx.emit(DebuggerEvent::LaunchesChanged);
        cx.notify();
        Some(id)
    }

    /// Starts a launch's target again, as it was started.
    pub fn rerun(&mut self, id: LaunchId, cx: &mut Context<Self>) -> Option<LaunchId> {
        let launch = self.launch(id)?;
        let (target, debug) = (launch.target.clone(), launch.debug);
        Some(self.start(target, debug, cx))
    }

    pub fn stop(&mut self, id: LaunchId, cx: &mut Context<Self>) {
        let Some(launch) = self.launches.iter_mut().find(|l| l.id == id) else {
            return;
        };
        if !launch.status.is_live() || launch.stopping {
            return;
        }
        launch.stopping = true;
        if let Some(process) = &launch.process {
            process.kill();
        }
        let root = launch
            .sessions
            .first()
            .filter(|s| !s.ended)
            .map(|s| s.adapter.clone());
        let early = matches!(
            launch.status,
            LaunchStatus::Building | LaunchStatus::Starting
        );
        if early && root.is_none() && launch.process.is_none() {
            self.finish(id, None, cx);
            return;
        }
        if let Some(adapter) = root {
            cx.spawn(async move |this, cx| {
                let polite = dap_session::disconnect(&adapter, true);
                let timer = cx.background_executor().timer(STOP_GRACE);
                futures::future::select(Box::pin(polite), Box::pin(timer)).await;
                this.update(cx, |this, cx| this.finish(id, None, cx)).ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// Marks a launch finished, dropping its sessions, which stops their adapters.
    fn finish(&mut self, id: LaunchId, code: Option<i64>, cx: &mut Context<Self>) {
        let Some(launch) = self.launches.iter_mut().find(|l| l.id == id) else {
            return;
        };
        if !launch.status.is_live() {
            return;
        }
        let code = code.or_else(|| launch.sessions.iter().find_map(|s| s.state.exit_code));
        let message = match code {
            Some(code) => format!("Process finished with exit code {code}"),
            None => "Process finished".to_string(),
        };
        launch.println(ConsoleKind::Info, &format!("\n{message}"));
        let stopped = launch.stopping;
        let problem = launch
            .tests
            .as_mut()
            .and_then(|tests| load_test_report(tests, &launch.target));
        if let Some(problem) = problem {
            launch.println(ConsoleKind::Stderr, &problem);
        }
        if let Some(tests) = &mut launch.tests {
            tests.finish(code, stopped);
        }
        launch.status = LaunchStatus::Finished(code);
        launch.process = None;
        for session in &mut launch.sessions {
            session.ended = true;
            session.clear_stop();
            session._events = None;
        }
        launch._task = None;
        if !self.launches.iter().any(|l| l.debug && l.status.is_live()) {
            self.statuses.clear();
            self.breakpoint_ids.clear();
        }
        self.locate_tests(id, cx);
        cx.emit(DebuggerEvent::LaunchesChanged);
        cx.emit(DebuggerEvent::MarksChanged);
        cx.notify();
    }

    fn fail(&mut self, id: LaunchId, message: String, cx: &mut Context<Self>) {
        let Some(launch) = self.launches.iter_mut().find(|l| l.id == id) else {
            return;
        };
        launch.println(ConsoleKind::Error, &message);
        let stopped = launch.stopping;
        if let Some(tests) = &mut launch.tests {
            tests.finish(Some(-1), stopped);
        }
        launch.status = LaunchStatus::Failed(message);
        launch.process = None;
        for session in &mut launch.sessions {
            session.ended = true;
            session._events = None;
        }
        cx.emit(DebuggerEvent::LaunchesChanged);
        cx.emit(DebuggerEvent::MarksChanged);
        cx.notify();
    }

    // Stepping. These act on the focused session of the active debug launch.

    fn focused_adapter(&self) -> Option<(LaunchId, usize, Arc<DebugAdapter>, Option<i64>)> {
        let launch = self.active_debug_launch()?;
        let index = launch.focus?;
        let session = launch.sessions.get(index)?;
        (!session.ended).then(|| (launch.id, index, session.adapter.clone(), session.thread))
    }

    /// Whether the focused session is stopped, so stepping makes sense.
    pub fn is_paused(&self) -> bool {
        self.active_debug_launch()
            .and_then(Launch::focused)
            .is_some_and(|s| s.is_stopped() && !s.ended)
    }

    pub fn resume(&mut self, step: Step, cx: &mut Context<Self>) {
        if !self.is_paused() {
            return;
        }
        let Some((id, index, adapter, Some(thread))) = self.focused_adapter() else {
            return;
        };
        self.with_launch(id, |launch| {
            let session = &mut launch.sessions[index];
            session.state.resumed();
            session.clear_stop();
            if !launch.sessions.iter().any(Session::is_stopped) {
                launch.status = LaunchStatus::Running;
            }
        });
        cx.emit(DebuggerEvent::MarksChanged);
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = dap_session::resume(&adapter, thread, step).await {
                this.update(cx, |this, cx| {
                    this.with_launch(id, |l| l.println(ConsoleKind::Error, &error.to_string()));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        let Some(launch) = self.active_debug_launch() else {
            return;
        };
        if launch.status != LaunchStatus::Running {
            return;
        }
        // The session doing the work: the deepest one still going.
        let Some(session) = launch.sessions.iter().rev().find(|s| !s.ended) else {
            return;
        };
        let (id, adapter, known) = (launch.id, session.adapter.clone(), session.thread);
        cx.spawn(async move |this, cx| {
            let thread = match known {
                Some(thread) => Some(thread),
                None => dap_session::threads(&adapter)
                    .await
                    .ok()
                    .and_then(|t| t.first().map(|t| t.id)),
            };
            let Some(thread) = thread else { return };
            if let Err(error) = dap_session::pause(&adapter, thread).await {
                this.update(cx, |this, cx| {
                    this.with_launch(id, |l| {
                        l.println(ConsoleKind::Error, &format!("Couldn't pause: {error}"))
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Continues to a line, as if it had a breakpoint that's used once.
    pub fn run_to_line(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        let Some((id, index, _, _)) = self.focused_adapter() else {
            return;
        };
        if !self.is_paused() {
            return;
        }
        self.with_launch(id, |launch| {
            launch.sessions[index].temporary = Some((path.to_path_buf(), line));
        });
        let list = {
            let mut list = self.breakpoints.for_adapter(path);
            list.push(Breakpoint::at(line).to_source());
            list
        };
        let adapter = self.focused_adapter().map(|f| f.2);
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            if let Some(adapter) = adapter {
                dap_session::set_breakpoints(&adapter, &path, &list)
                    .await
                    .ok();
            }
            this.update(cx, |this, cx| this.resume(Step::Continue, cx))
                .ok();
        })
        .detach();
    }

    // What's shown at a stop.

    pub fn select_thread(&mut self, thread: i64, cx: &mut Context<Self>) {
        let Some((id, index, _, _)) = self.focused_adapter() else {
            return;
        };
        self.with_launch(id, |launch| {
            let session = &mut launch.sessions[index];
            session.thread = Some(thread);
            session.clear_stop();
        });
        self.load_stack(id, index, cx);
    }

    pub fn select_frame(&mut self, frame: usize, cx: &mut Context<Self>) {
        let Some((id, index, _, _)) = self.focused_adapter() else {
            return;
        };
        let changed = self
            .with_launch(id, |launch| {
                let session = &mut launch.sessions[index];
                if frame >= session.frames.len() {
                    return false;
                }
                session.frame = frame;
                session.scopes.clear();
                session.variables.clear();
                session.watches.clear();
                true
            })
            .unwrap_or(false);
        if changed {
            self.load_frame(id, index, true, cx);
            cx.emit(DebuggerEvent::MarksChanged);
            cx.notify();
        }
    }

    /// Fetches a variable's children, if they haven't been.
    pub fn load_variables(&mut self, reference: i64, cx: &mut Context<Self>) {
        let Some((id, index, adapter, _)) = self.focused_adapter() else {
            return;
        };
        let loaded = self
            .launch(id)
            .is_some_and(|l| l.sessions[index].variables.contains_key(&reference));
        if loaded || reference == 0 {
            return;
        }
        cx.spawn(async move |this, cx| {
            let variables = dap_session::variables(&adapter, reference).await;
            this.update(cx, |this, cx| {
                this.with_launch(id, |launch| {
                    let session = &mut launch.sessions[index];
                    session
                        .variables
                        .insert(reference, variables.unwrap_or_default());
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether the stopped program's variables can be changed.
    pub fn can_set_values(&self) -> bool {
        self.is_paused()
            && self
                .active_debug_launch()
                .and_then(Launch::focused)
                .is_some_and(|s| s.capabilities.supports_set_variable)
    }

    /// Changes a variable in the selected frame, then fetches the frame's values again, as other
    /// values may follow from it.
    pub fn set_variable(
        &mut self,
        container: i64,
        name: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<String, String>> {
        let Some((id, index, adapter, _)) = self.focused_adapter() else {
            return Task::ready(Err("Nothing is being debugged".into()));
        };
        let (name, value) = (name.to_string(), value.to_string());
        cx.spawn(async move |this, cx| {
            let result = dap_session::set_variable(&adapter, container, &name, &value)
                .await
                .map_err(|e| e.to_string());
            this.update(cx, |this, cx| {
                this.with_launch(id, |l| {
                    let session = &mut l.sessions[index];
                    session.variables.clear();
                    session.watches.clear();
                });
                this.load_frame(id, index, false, cx);
            })
            .ok();
            result
        })
    }

    /// Where the focused session is stopped, in the frame shown: file, line, and whether it's the
    /// top frame rather than a caller.
    pub fn execution_point(&self) -> Option<(PathBuf, u32, bool)> {
        let session = self.active_debug_launch()?.focused()?;
        if !session.is_stopped() {
            return None;
        }
        let frame = session.selected_frame()?;
        let path = frame.source.as_ref()?.path.clone()?;
        Some((path, frame.line.saturating_sub(1), session.frame == 0))
    }

    /// The selected frame's local variables, for showing their values beside the code.
    pub fn frame_values(&self) -> Option<(PathBuf, Vec<(String, String)>)> {
        let session = self.active_debug_launch()?.focused()?;
        let frame = session.selected_frame()?;
        let path = frame.source.as_ref()?.path.clone()?;
        let mut values: Vec<(String, String)> = Vec::new();
        let mut any = false;
        for scope in session.scopes.iter().filter(|s| is_local_scope(s)) {
            let Some(variables) = session.variables.get(&scope.variables_reference) else {
                continue;
            };
            any = true;
            for variable in variables {
                // An inner scope's name hides an outer one's.
                if !values.iter().any(|(name, _)| *name == variable.name) {
                    values.push((variable.name.clone(), variable.value.clone()));
                }
            }
        }
        any.then_some((path, values))
    }

    // Exceptions.

    /// Each adapter's exception filters, with whether each is on.
    pub fn exception_filters(&self) -> Vec<(String, gleipnir_dap::ExceptionFilter, bool)> {
        self.exception_filters
            .iter()
            .flat_map(|(adapter, filters)| {
                filters.iter().map(move |f| {
                    let on = self
                        .exception_choices
                        .get(&format!("{adapter}/{}", f.filter))
                        .copied()
                        .unwrap_or(f.default);
                    (adapter.clone(), f.clone(), on)
                })
            })
            .collect()
    }

    /// Turns stopping on a kind of exception on or off, now and in later sessions.
    pub fn set_exception_filter(
        &mut self,
        adapter_id: &str,
        filter: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.exception_choices
            .insert(format!("{adapter_id}/{filter}"), on);
        let Some(filters) = self.exception_filters.get(adapter_id) else {
            return;
        };
        let chosen: Vec<String> = filters
            .iter()
            .filter(|f| {
                self.exception_choices
                    .get(&format!("{adapter_id}/{}", f.filter))
                    .copied()
                    .unwrap_or(f.default)
            })
            .map(|f| f.filter.clone())
            .collect();
        for launch in self.launches.iter().filter(|l| l.status.is_live()) {
            for session in launch
                .sessions
                .iter()
                .filter(|s| !s.ended && s.adapter_id == adapter_id)
            {
                let (adapter, chosen) = (session.adapter.clone(), chosen.clone());
                cx.spawn(async move |_, _| {
                    dap_session::set_exception_breakpoints(&adapter, &chosen)
                        .await
                        .ok();
                })
                .detach();
            }
        }
        cx.emit(DebuggerEvent::SettingsChanged);
        cx.notify();
    }

    /// The exception filters and choices to save with the project.
    pub fn exception_settings(
        &self,
    ) -> (
        BTreeMap<String, Vec<gleipnir_dap::ExceptionFilter>>,
        BTreeMap<String, bool>,
    ) {
        (
            self.exception_filters.clone(),
            self.exception_choices.clone(),
        )
    }

    pub fn set_exception_settings(
        &mut self,
        filters: BTreeMap<String, Vec<gleipnir_dap::ExceptionFilter>>,
        choices: BTreeMap<String, bool>,
    ) {
        self.exception_filters = filters;
        self.exception_choices = choices;
    }

    // Watches and evaluation.

    pub fn watches(&self) -> &[String] {
        &self.watches
    }

    pub fn add_watch(&mut self, expression: String, cx: &mut Context<Self>) {
        let expression = expression.trim().to_string();
        if expression.is_empty() {
            return;
        }
        self.watches.push(expression);
        self.evaluate_watches(cx);
        cx.emit(DebuggerEvent::SettingsChanged);
        cx.notify();
    }

    pub fn remove_watch(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.watches.len() {
            self.watches.remove(index);
            if let Some((id, session, _, _)) = self.focused_adapter() {
                self.with_launch(id, |l| {
                    let watches = &mut l.sessions[session].watches;
                    if index < watches.len() {
                        watches.remove(index);
                    }
                });
            }
            cx.emit(DebuggerEvent::SettingsChanged);
            cx.notify();
        }
    }

    pub fn set_watches(&mut self, watches: Vec<String>, cx: &mut Context<Self>) {
        self.watches = watches;
        cx.notify();
    }

    /// Evaluates an expression in the selected frame, as typed into Evaluate or the console.
    pub fn evaluate(
        &mut self,
        expression: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<EvaluateResult, String>> {
        let Some((id, index, adapter, _)) = self.focused_adapter() else {
            return Task::ready(Err("Nothing is being debugged".into()));
        };
        let frame = self
            .launch(id)
            .and_then(|l| l.sessions[index].selected_frame().map(|f| f.id));
        let expression = expression.to_string();
        cx.spawn(async move |_, _| {
            dap_session::evaluate(&adapter, &expression, frame, "repl")
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// Evaluates in the console: the expression and its value go into the launch's console.
    pub fn evaluate_in_console(&mut self, expression: &str, cx: &mut Context<Self>) {
        let Some(id) = self.active_debug_launch().map(|l| l.id) else {
            return;
        };
        self.with_launch(id, |l| {
            l.println(ConsoleKind::Input, &format!("> {expression}"))
        });
        let task = self.evaluate(expression, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.with_launch(id, |l| match result {
                    Ok(value) => l.println(ConsoleKind::Result, &value.result),
                    Err(error) => l.println(ConsoleKind::Error, &error),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn evaluate_watches(&mut self, cx: &mut Context<Self>) {
        let Some((id, index, adapter, _)) = self.focused_adapter() else {
            return;
        };
        let Some(frame) = self
            .launch(id)
            .and_then(|l| l.sessions[index].selected_frame().map(|f| f.id))
        else {
            return;
        };
        let expressions = self.watches.clone();
        self.with_launch(id, |l| {
            l.sessions[index].watches = vec![WatchValue::Pending; expressions.len()];
        });
        cx.spawn(async move |this, cx| {
            let mut values = Vec::new();
            for expression in &expressions {
                values.push(
                    match dap_session::evaluate(&adapter, expression, Some(frame), "watch").await {
                        Ok(result) => WatchValue::Value(result),
                        Err(error) => WatchValue::Error(error.to_string()),
                    },
                );
            }
            this.update(cx, |this, cx| {
                // Only if nothing moved on meanwhile.
                let current = this.watches == expressions;
                this.with_launch(id, |l| {
                    let session = &mut l.sessions[index];
                    if current && session.selected_frame().map(|f| f.id) == Some(frame) {
                        session.watches = values;
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // Sessions and their events.

    fn add_session(
        &mut self,
        id: LaunchId,
        name: String,
        connected: Connected,
        parent: Option<usize>,
        adapter_id: String,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let (adapter, incoming) = connected;
        let index = self.with_launch(id, |launch| {
            launch
                .sessions
                .push(Session::new(name, adapter, adapter_id.clone(), parent));
            launch.sessions.len() - 1
        })?;
        let events = cx.spawn(async move |this, cx| {
            pump_events(this, id, index, incoming, adapter_id, cx).await;
        });
        self.with_launch(id, |launch| launch.sessions[index]._events = Some(events));
        Some(index)
    }

    fn handle_incoming(
        &mut self,
        id: LaunchId,
        index: usize,
        message: Incoming,
        adapter_id: &str,
        cx: &mut Context<Self>,
    ) {
        match message {
            Incoming::Event { event, body } => self.handle_event(id, index, &event, &body, cx),
            Incoming::Request {
                seq,
                command,
                arguments,
            } => {
                let Some(adapter) = self
                    .launch(id)
                    .and_then(|l| l.sessions.get(index))
                    .map(|s| s.adapter.clone())
                else {
                    return;
                };
                if command == "startDebugging" {
                    adapter.respond(seq, &command, Ok(serde_json::json!({})));
                    match serde_json::from_value::<StartDebugging>(arguments) {
                        Ok(start) => self.start_child(id, index, adapter, start, adapter_id, cx),
                        Err(error) => self
                            .with_launch(id, |l| {
                                l.println(
                                    ConsoleKind::Error,
                                    &format!("A bad child session: {error}"),
                                )
                            })
                            .unwrap_or_default(),
                    }
                } else if command == "runInTerminal" {
                    let result = self.run_in_terminal(id, &arguments, cx);
                    adapter.respond(seq, &command, result);
                } else {
                    adapter.respond(seq, &command, Err(format!("{command} isn't supported")));
                }
            }
            Incoming::Exited { stderr } => {
                let Some(launch) = self.launch(id) else {
                    return;
                };
                let stopping = launch.stopping;
                let graceful = launch.sessions[index].ended;
                let root = launch.sessions[index].parent.is_none();
                let live = launch.status.is_live();
                self.with_launch(id, |l| l.sessions[index].ended = true);
                if root && !graceful && !stopping && live {
                    let reason = stderr
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("the debugger stopped unexpectedly")
                        .to_string();
                    if !stderr.trim().is_empty() {
                        self.with_launch(id, |l| l.println(ConsoleKind::Stderr, stderr.trim_end()));
                    }
                    self.fail(id, format!("Debugger stopped: {reason}"), cx);
                } else if root {
                    self.finish(id, None, cx);
                }
            }
        }
    }

    fn handle_event(
        &mut self,
        id: LaunchId,
        index: usize,
        event: &str,
        body: &serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(change) = self.with_launch(id, |l| l.sessions[index].state.apply(event, body))
        else {
            return;
        };
        match change {
            Change::Stopped => {
                let temporary = self.with_launch(id, |launch| {
                    launch.status = LaunchStatus::Paused;
                    launch.focus = Some(index);
                    let session = &mut launch.sessions[index];
                    let stopped = session.state.stopped().cloned();
                    session.thread = stopped
                        .and_then(|s| s.thread_id)
                        .or(session.thread)
                        .or_else(|| session.threads.first().map(|t| t.id));
                    session.clear_stop();
                    session.temporary.take()
                });
                if let Some(Some((path, _))) = temporary {
                    self.send_breakpoints(&path, cx);
                }
                self.active = Some(id);
                self.load_stack(id, index, cx);
            }
            Change::Continued => self
                .with_launch(id, |launch| {
                    launch.sessions[index].clear_stop();
                    if launch.status == LaunchStatus::Starting
                        || !launch.sessions.iter().any(Session::is_stopped)
                    {
                        launch.status = LaunchStatus::Running;
                    }
                })
                .unwrap_or_default(),
            Change::Output(output) => {
                let kind = match output.category.as_deref() {
                    Some("stdout") => ConsoleKind::Stdout,
                    Some("stderr") => ConsoleKind::Stderr,
                    Some("important") => ConsoleKind::Error,
                    Some("telemetry") => return,
                    _ => ConsoleKind::Info,
                };
                self.with_launch(id, |l| match kind {
                    ConsoleKind::Stdout | ConsoleKind::Stderr => {
                        l.program_output(kind, &output.output)
                    }
                    _ => l.print(kind, &output.output),
                });
                cx.notify();
                return;
            }
            Change::Breakpoint(breakpoint) => {
                if let Some(bp_id) = breakpoint.id
                    && let Some((path, line)) = self.breakpoint_ids.get(&bp_id).cloned()
                {
                    let status = self
                        .statuses
                        .entry(path)
                        .or_default()
                        .entry(line)
                        .or_default();
                    status.verified |= breakpoint.verified;
                    if breakpoint.verified {
                        status.message = None;
                    }
                }
            }
            Change::Exited(_) | Change::Threads | Change::Nothing => {
                cx.notify();
                return;
            }
            Change::Ended => {
                let root = self.with_launch(id, |launch| {
                    let session = &mut launch.sessions[index];
                    session.ended = true;
                    session.clear_stop();
                    if launch.focus == Some(index) {
                        launch.focus = None;
                    }
                    session.parent.is_none()
                });
                if root == Some(true) {
                    // Let the adapter close the connection itself: dropping it now could lose
                    // output still on its way.
                    if let Some(adapter) =
                        self.launch(id).map(|l| l.sessions[index].adapter.clone())
                    {
                        cx.spawn(async move |this, cx| {
                            let polite = dap_session::disconnect(&adapter, false);
                            let timer = cx.background_executor().timer(Duration::from_secs(1));
                            futures::future::select(Box::pin(polite), Box::pin(timer)).await;
                            this.update(cx, |this, cx| this.finish(id, None, cx)).ok();
                        })
                        .detach();
                    }
                }
            }
        }
        cx.emit(DebuggerEvent::MarksChanged);
        cx.notify();
    }

    /// Starts the program for an adapter that asks us to, so that its input and output go
    /// through the console. Answers with its process id.
    fn run_in_terminal(
        &mut self,
        id: LaunchId,
        arguments: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, String> {
        let argv: Vec<String> = arguments["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let Some((program, args)) = argv.split_first() else {
            return Err("nothing to run".into());
        };
        let Some(launch) = self.launch(id) else {
            return Err("the launch has ended".into());
        };
        let mut env = launch
            .env
            .clone()
            .unwrap_or_else(gleipnir_packages::env::user_env);
        if let Some(extra) = arguments["env"].as_object() {
            for (key, value) in extra {
                match value.as_str() {
                    Some(value) => env.set(key.clone(), value.to_string()),
                    None => env.set(key.clone(), String::new()),
                }
            }
        }
        let cwd = arguments["cwd"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| launch.target.cwd.clone());
        let command = ProcessCommand {
            program: resolve_program(program, &env),
            args: args.to_vec(),
            env: env_pairs(&env),
            cwd,
        };
        let (handle, mut output) = process::spawn(&command).map_err(|e| format!("{e:#}"))?;
        let pid = handle.pid();
        let forward = cx.spawn(async move |this, cx| {
            while let Some(item) = output.next().await {
                let (kind, text) = match item {
                    ProcessOutput::Stdout(text) => (ConsoleKind::Stdout, text),
                    ProcessOutput::Stderr(text) => (ConsoleKind::Stderr, text),
                    ProcessOutput::Exited(_) => break,
                };
                let alive = this.update(cx, |this, cx| {
                    this.with_launch(id, |l| l.program_output(kind, &text));
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        self.with_launch(id, |l| {
            l.process = Some(handle);
            l._program_output = Some(forward);
        });
        Ok(serde_json::json!({ "processId": pid }))
    }

    /// Whether the active launch's program can be typed to.
    pub fn accepts_input(&self, id: LaunchId) -> bool {
        self.launch(id).is_some_and(|l| {
            l.status.is_live() && (l.process.is_some() || l.lldb_session().is_some())
        })
    }

    /// Types a line into a launch's program, echoing it in the console.
    pub fn send_input(&mut self, id: LaunchId, line: &str, cx: &mut Context<Self>) {
        let Some(launch) = self.launches.iter_mut().find(|l| l.id == id) else {
            return;
        };
        let text = format!("{line}\n");
        if let Some(process) = launch.process.clone() {
            launch.print(ConsoleKind::Input, &text);
            if let Err(error) = process.write_input(&text) {
                launch.println(ConsoleKind::Error, &format!("{error:#}"));
            }
            cx.notify();
            return;
        }
        // lldb runs the program on a terminal of its own, which only it can type into. Its
        // terminal echoes what's typed, so it isn't echoed here.
        let Some(adapter) = launch.lldb_session().map(|s| s.adapter.clone()) else {
            launch.println(ConsoleKind::Error, "The program isn't taking input");
            cx.notify();
            return;
        };
        let literal = serde_json::to_string(&text).unwrap_or_default();
        let command = format!("`script lldb.process.PutSTDIN({literal}) and None");
        cx.spawn(async move |this, cx| {
            if let Err(error) = dap_session::evaluate(&adapter, &command, None, "repl").await {
                this.update(cx, |this, cx| {
                    this.with_launch(id, |l| {
                        l.println(
                            ConsoleKind::Error,
                            &format!("Couldn't type to the program: {error}"),
                        )
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn start_child(
        &mut self,
        id: LaunchId,
        parent: usize,
        adapter: Arc<DebugAdapter>,
        start: StartDebugging,
        adapter_id: &str,
        cx: &mut Context<Self>,
    ) {
        let name = start
            .configuration
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("child")
            .to_string();
        let adapter_id = adapter_id.to_string();
        cx.spawn(async move |this, cx| {
            let child_name = name.clone();
            let connected = cx
                .background_executor()
                .spawn(async move { adapter.connect_again(&child_name) })
                .await;
            let connected = match connected {
                Ok(connected) => connected,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.with_launch(id, |l| {
                            l.println(ConsoleKind::Error, &format!("{error:#}"))
                        });
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let child = connected.0.clone();
            let Ok(Some(index)) = this.update(cx, |this, cx| {
                this.add_session(id, name, connected, Some(parent), adapter_id.clone(), cx)
            }) else {
                return;
            };
            let request = LaunchRequest {
                adapter_id,
                request: start.request,
                arguments: start.configuration,
            };
            handshake(&this, id, index, &child, &request, cx).await;
        })
        .detach();
    }

    /// Threads and the stack for the session's thread, then the top frame's variables.
    fn load_stack(&mut self, id: LaunchId, index: usize, cx: &mut Context<Self>) {
        let Some(session) = self.launch(id).and_then(|l| l.sessions.get(index)) else {
            return;
        };
        let adapter = session.adapter.clone();
        let wanted = session.thread;
        cx.spawn(async move |this, cx| {
            let threads = dap_session::threads(&adapter).await.unwrap_or_default();
            let thread = wanted.or_else(|| threads.first().map(|t| t.id));
            let frames = match thread {
                Some(thread) => dap_session::stack_trace(&adapter, thread, FRAME_LEVELS)
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            this.update(cx, |this, cx| {
                let current = this.with_launch(id, |launch| {
                    let session = &mut launch.sessions[index];
                    if !session.is_stopped() || session.thread.is_some() && session.thread != thread
                    {
                        return false;
                    }
                    session.threads = threads;
                    session.thread = thread;
                    session.frames = frames.into_iter().map(canonical_frame).collect();
                    // Library code without a source isn't worth landing in.
                    session.frame = session
                        .frames
                        .iter()
                        .position(|f| f.source.as_ref().is_some_and(|s| s.path.is_some()))
                        .unwrap_or(0);
                    true
                });
                if current == Some(true) {
                    this.load_frame(id, index, true, cx);
                    cx.emit(DebuggerEvent::Stopped(id));
                    cx.emit(DebuggerEvent::MarksChanged);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The selected frame's scopes and first scope's variables, then the watches.
    fn load_frame(&mut self, id: LaunchId, index: usize, show: bool, cx: &mut Context<Self>) {
        let Some(session) = self.launch(id).and_then(|l| l.sessions.get(index)) else {
            return;
        };
        let Some(frame) = session.selected_frame().cloned() else {
            return;
        };
        if show && let Some(path) = frame.source.as_ref().and_then(|s| s.path.clone()) {
            cx.emit(DebuggerEvent::ShowLocation {
                path,
                line: frame.line.saturating_sub(1),
            });
        }
        let adapter = session.adapter.clone();
        cx.spawn(async move |this, cx| {
            let scopes = dap_session::scopes(&adapter, frame.id)
                .await
                .unwrap_or_default();
            let mut loaded = HashMap::new();
            for scope in scopes.iter().filter(|s| is_local_scope(s)) {
                if let Ok(variables) =
                    dap_session::variables(&adapter, scope.variables_reference).await
                {
                    loaded.insert(scope.variables_reference, variables);
                }
            }
            this.update(cx, |this, cx| {
                let current = this.with_launch(id, |launch| {
                    let session = &mut launch.sessions[index];
                    if session.selected_frame().map(|f| f.id) != Some(frame.id) {
                        return false;
                    }
                    session.scopes = scopes;
                    session.variables.extend(loaded);
                    true
                });
                if current == Some(true) {
                    this.evaluate_watches(cx);
                    cx.emit(DebuggerEvent::MarksChanged);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// For the end-to-end driver and tests.
    pub fn debug_state(&self, cx: &App) -> serde_json::Value {
        let _ = cx;
        let breakpoints: Vec<serde_json::Value> = self
            .breakpoints
            .files()
            .flat_map(|(path, list)| {
                list.iter().map(move |b| {
                    let status = self.breakpoint_status(path, b.line);
                    serde_json::json!({
                        "path": path,
                        "line": b.line + 1,
                        "enabled": b.enabled,
                        "condition": b.condition,
                        "log": b.log_message,
                        "verified": status.map(|s| s.verified),
                    })
                })
            })
            .collect();
        let launch = self.active_launch().map(|launch| {
            let session = launch.focused();
            serde_json::json!({
                "name": launch.name,
                "debug": launch.debug,
                "status": format!("{:?}", launch.status),
                "console": launch.console_text(),
                "sessions": launch.sessions.len(),
                "frames": session.map(|s| s.frames.iter().map(|f| format!("{}:{}", f.name, f.line)).collect::<Vec<_>>()),
                "frame": session.map(|s| s.frame),
                "variables": self.frame_values().map(|(_, values)| {
                    values.iter().map(|(name, value)| format!("{name} = {value}")).collect::<Vec<_>>()
                }),
                "watches": session.map(|s| s.watches.iter().map(|w| match w {
                    WatchValue::Pending => "…".to_string(),
                    WatchValue::Value(v) => v.result.clone(),
                    WatchValue::Error(e) => format!("error: {e}"),
                }).collect::<Vec<_>>()),
            })
        });
        serde_json::json!({
            "breakpoints": breakpoints,
            "targets": self.targets.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            "selected": self.selected_target().map(|t| t.name.clone()),
            "launches": self.launches.len(),
            "launch": launch,
            "execution": self.execution_point().map(|(p, line, top)| serde_json::json!({ "path": p, "line": line + 1, "top": top })),
            "watches": self.watches,
        })
    }
}

/// Scopes worth opening at a stop: the cheap ones that hold the frame's own variables, not globals
/// or registers, which are many and rarely what's wanted.
pub fn is_local_scope(scope: &Scope) -> bool {
    let name = scope.name.to_lowercase();
    !scope.expensive && !name.contains("global") && !name.contains("register")
}

/// Adapters report paths as the program saw them, which may go through a symlink, like macOS's
/// `/var` into `/private/var`. The editor uses canonical paths.
fn canonical_frame(mut frame: StackFrame) -> StackFrame {
    if let Some(source) = &mut frame.source
        && let Some(path) = &source.path
        && let Ok(canonical) = std::fs::canonicalize(path)
    {
        source.path = Some(canonical);
    }
    frame
}

fn resolve_program(program: &str, env: &Env) -> PathBuf {
    let path = Path::new(program);
    if path.is_absolute() || program.contains('/') {
        return path.to_path_buf();
    }
    env.which(program).unwrap_or_else(|| path.to_path_buf())
}

/// Runs a process into a launch's console. Returns its exit code, or `None` if it couldn't start
/// or was killed.
async fn run_into_console(
    this: &gpui::WeakEntity<Debugger>,
    id: LaunchId,
    argv: &[String],
    cwd: &Path,
    env: &Env,
    cx: &mut gpui::AsyncApp,
) -> Result<Option<i32>, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("there's nothing to run".into());
    };
    let command = ProcessCommand {
        program: resolve_program(program, env),
        args: args.to_vec(),
        env: env_pairs(env),
        cwd: cwd.to_path_buf(),
    };
    let (handle, mut output) = process::spawn(&command).map_err(|e| format!("{e:#}"))?;
    let stopped = this
        .update(cx, |this, _| {
            this.with_launch(id, |l| {
                l.process = Some(handle.clone());
                l.stopping
            })
        })
        .ok()
        .flatten()
        .unwrap_or(true);
    if stopped {
        handle.kill();
    }
    let mut code = None;
    while let Some(item) = output.next().await {
        let (kind, text) = match item {
            ProcessOutput::Stdout(text) => (ConsoleKind::Stdout, text),
            ProcessOutput::Stderr(text) => (ConsoleKind::Stderr, text),
            ProcessOutput::Exited(exit) => {
                code = exit;
                break;
            }
        };
        let alive = this.update(cx, |this, cx| {
            this.with_launch(id, |l| l.program_output(kind, &text));
            cx.notify();
        });
        if alive.is_err() {
            handle.kill();
            return Ok(None);
        }
    }
    this.update(cx, |this, _| this.with_launch(id, |l| l.process = None))
        .ok();
    Ok(code)
}

async fn run_build(
    this: &gpui::WeakEntity<Debugger>,
    id: LaunchId,
    build: &[String],
    cwd: &Path,
    env: &Env,
    cx: &mut gpui::AsyncApp,
) -> bool {
    this.update(cx, |this, cx| {
        this.with_launch(id, |l| {
            l.status = LaunchStatus::Building;
            l.println(ConsoleKind::Info, &build.join(" "));
        });
        cx.notify();
    })
    .ok();
    let result = run_into_console(this, id, build, cwd, env, cx).await;
    this.update(cx, |this, cx| {
        let stopping = this.launch(id).is_some_and(|l| l.stopping);
        match result {
            Ok(Some(0)) if !stopping => true,
            _ if stopping => {
                this.finish(id, None, cx);
                false
            }
            Ok(code) => {
                let code = code.map_or_else(|| "a signal".into(), |c| format!("exit code {c}"));
                this.fail(id, format!("Build failed with {code}"), cx);
                false
            }
            Err(error) => {
                this.fail(id, format!("The build couldn't start: {error}"), cx);
                false
            }
        }
    })
    .unwrap_or(false)
}

/// Installs the debugger a launch needs if it's missing and can be, saying so in its console.
/// Returns whether to carry on.
async fn install_adapter(
    this: &gpui::WeakEntity<Debugger>,
    id: LaunchId,
    package_id: &str,
    toolchains: &Entity<Toolchains>,
    cx: &mut gpui::AsyncApp,
) -> bool {
    let Some(tool) = registry()
        .package(package_id)
        .and_then(|p| p.manifest.debugger.as_ref())
        .map(|d| d.tool.clone())
    else {
        return true;
    };
    if !toolchains.update(cx, |t, cx| t.install_missing(package_id, &tool, cx)) {
        return true;
    }
    this.update(cx, |this, cx| {
        this.with_launch(id, |l| {
            l.println(ConsoleKind::Info, &format!("Installing {tool}…"))
        });
        cx.notify();
    })
    .ok();
    while toolchains.read_with(cx, |t, _| t.is_installing(&tool)) {
        let live = this
            .read_with(cx, |this, _| {
                this.launch(id).is_some_and(|l| l.status.is_live())
            })
            .unwrap_or(false);
        if !live {
            return false;
        }
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
    }
    let error = toolchains.read_with(cx, |t, _| t.install_error(&tool).map(String::from));
    match error {
        Some(error) => {
            this.update(cx, |this, cx| {
                this.fail(id, format!("Couldn't install {tool}: {error}"), cx)
            })
            .ok();
            false
        }
        None => true,
    }
}

/// A test run's results to fill. A report from an earlier run is cleared away first, so a run
/// that dies before writing its own isn't shown the old one.
fn new_test_run(format: gleipnir_packages::TestFormat, target: &RunTarget) -> TestRun {
    if let Some(report) = gleipnir_packages::js_report_of(target) {
        std::fs::remove_file(&report).ok();
        if let Some(folder) = report.parent() {
            std::fs::create_dir_all(folder).ok();
        }
    }
    TestRun::new(format, &target.name)
}

/// Reads the report a run writes when it ends, for runners that report that way. Says what went
/// wrong if there's one that can't be read; a run that wrote none has said why itself.
fn load_test_report(tests: &mut TestRun, target: &RunTarget) -> Option<String> {
    let report = gleipnir_packages::js_report_of(target)?;
    let json = std::fs::read_to_string(&report).ok()?;
    let filtered = target.run.iter().any(|arg| arg == "-t");
    tests
        .load_jest_report(&json, &target.cwd, filtered)
        .err()
        .map(|error| {
            format!(
                "Couldn't read the test report {}: {error:#}",
                report.display()
            )
        })
}

async fn run_program(
    this: &gpui::WeakEntity<Debugger>,
    id: LaunchId,
    target: &RunTarget,
    env: &Env,
    cx: &mut gpui::AsyncApp,
) {
    this.update(cx, |this, cx| {
        this.with_launch(id, |l| {
            l.status = LaunchStatus::Running;
            l.println(ConsoleKind::Info, &target.run.join(" "));
            l.tests = target
                .test_format(false)
                .map(|format| new_test_run(format, target));
        });
        cx.notify();
    })
    .ok();
    let result = run_into_console(this, id, &target.run, &target.cwd, env, cx).await;
    this.update(cx, |this, cx| match result {
        Ok(code) => this.finish(id, code.map(i64::from), cx),
        Err(error) => this.fail(id, format!("Couldn't run {}: {error}", target.name), cx),
    })
    .ok();
}

async fn start_debugging(
    this: &gpui::WeakEntity<Debugger>,
    id: LaunchId,
    target: &RunTarget,
    env: &Env,
    cx: &mut gpui::AsyncApp,
) {
    start_debugging_with(this, id, target, env, None, cx).await
}

/// Starts debugging a target, or with `attach` arguments, a process that's already running.
async fn start_debugging_with(
    this: &gpui::WeakEntity<Debugger>,
    id: LaunchId,
    target: &RunTarget,
    env: &Env,
    attach: Option<serde_json::Value>,
    cx: &mut gpui::AsyncApp,
) {
    let request_kind = if attach.is_some() { "attach" } else { "launch" };
    let prepared = this.update(cx, |this, cx| {
        this.with_launch(id, |l| {
            l.status = LaunchStatus::Starting;
            if attach.is_none() {
                l.tests = target
                    .test_format(true)
                    .map(|format| new_test_run(format, target));
            }
        });
        cx.notify();
        let launch = match &attach {
            Some(arguments) => Ok(gleipnir_packages::DebugLaunch {
                launch_arguments: arguments.clone(),
            }),
            None => adapter_for(&target.package_id)
                .ok_or_else(|| anyhow!("{} can't be debugged", target.package_id))
                .and_then(|a| a.debug_launch(target)),
        };
        let prepared = (this.launcher)(&target.package_id, &target.cwd, cx);
        launch.and_then(|launch| prepared.map(|p| (launch, p)))
    });
    let Ok(prepared) = prepared else { return };
    let (launch, prepared) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            this.update(cx, |this, cx| {
                this.fail(id, format!("Can't debug: {error:#}"), cx)
            })
            .ok();
            return;
        }
    };
    // What only the project's tools can say, like where Rust's lldb formatters are.
    let launch = match adapter_for(&target.package_id) {
        Some(language) => {
            let (target, env) = (target.clone(), env.clone());
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    let mut launch = launch;
                    language
                        .prepare_debug_launch(&target, &mut launch, &env)
                        .map(|()| launch)
                })
                .await;
            match prepared {
                Ok(launch) => launch,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.fail(id, format!("Can't debug: {error:#}"), cx)
                    })
                    .ok();
                    return;
                }
            }
        }
        None => launch,
    };
    // The configuration's environment, in the form each adapter takes.
    let mut launch = launch;
    let extra = this
        .read_with(cx, |this, _| this.run_settings(&target.name).env)
        .unwrap_or_default();
    if !extra.is_empty()
        && let Some(arguments) = launch.launch_arguments.as_object_mut()
    {
        arguments.insert(
            "env".into(),
            if prepared.adapter_id == "lldb-dap" {
                serde_json::json!(
                    extra
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                )
            } else {
                serde_json::json!(extra)
            },
        );
    }
    let adapter_id = prepared.adapter_id.clone();
    let connect = prepared.connect;
    let connected = cx
        .background_executor()
        .spawn(async move { connect() })
        .await;
    let connected = match connected {
        Ok(connected) => connected,
        Err(error) => {
            this.update(cx, |this, cx| {
                this.fail(id, format!("The debugger didn't start: {error:#}"), cx)
            })
            .ok();
            return;
        }
    };
    let adapter = connected.0.clone();
    let name = target.name.clone();
    let Ok(Some(index)) = this.update(cx, |this, cx| {
        this.add_session(id, name, connected, None, adapter_id.clone(), cx)
    }) else {
        return;
    };
    let request = LaunchRequest {
        adapter_id,
        request: request_kind.into(),
        arguments: launch.launch_arguments,
    };
    handshake(this, id, index, &adapter, &request, cx).await;
}

/// Starts a session with the project's breakpoints, and records what came of them.
async fn handshake(
    this: &gpui::WeakEntity<Debugger>,
    id: LaunchId,
    index: usize,
    adapter: &DebugAdapter,
    request: &LaunchRequest,
    cx: &mut gpui::AsyncApp,
) {
    let Ok(breakpoints) = this.update(cx, |this, _| {
        this.breakpoints
            .files()
            .map(|(path, _)| FileBreakpoints {
                path: path.to_path_buf(),
                breakpoints: this.breakpoints.for_adapter(path),
            })
            .filter(|f| !f.breakpoints.is_empty())
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    let Ok(choices) = this.update(cx, |this, _| this.exception_choices.clone()) else {
        return;
    };
    let adapter_id = request.adapter_id.clone();
    let choose = move |filter: &gleipnir_dap::ExceptionFilter| {
        choices
            .get(&format!("{adapter_id}/{}", filter.filter))
            .copied()
            .unwrap_or(filter.default)
    };
    let started = dap_session::start_with(adapter, request, &breakpoints, &choose).await;
    this.update(cx, |this, cx| match started {
        Ok(started) => {
            for (file, (path, placed)) in breakpoints.iter().zip(&started.breakpoints) {
                this.record_placed(path, &file.breakpoints, placed);
            }
            if !started.capabilities.exception_breakpoint_filters.is_empty() {
                this.exception_filters.insert(
                    request.adapter_id.clone(),
                    started.capabilities.exception_breakpoint_filters.clone(),
                );
                cx.emit(DebuggerEvent::SettingsChanged);
            }
            this.with_launch(id, |launch| {
                let session = &mut launch.sessions[index];
                session.capabilities = started.capabilities;
                if session.state.run == gleipnir_dap::RunState::Starting {
                    session.state.run = gleipnir_dap::RunState::Running;
                }
                if launch.status == LaunchStatus::Starting {
                    launch.status = LaunchStatus::Running;
                }
            });
            cx.emit(DebuggerEvent::MarksChanged);
            cx.notify();
        }
        Err(error) => {
            let root = this
                .launch(id)
                .is_some_and(|l| l.sessions.get(index).is_some_and(|s| s.parent.is_none()));
            let output = adapter.output();
            if root {
                if !output.trim().is_empty() {
                    this.with_launch(id, |l| l.println(ConsoleKind::Stderr, output.trim_end()));
                }
                this.fail(id, format!("Couldn't start debugging: {error}"), cx);
            } else {
                this.with_launch(id, |l| {
                    l.println(
                        ConsoleKind::Error,
                        &format!("A child session failed: {error}"),
                    )
                });
                cx.notify();
            }
        }
    })
    .ok();
}

async fn pump_events(
    this: gpui::WeakEntity<Debugger>,
    id: LaunchId,
    index: usize,
    mut incoming: UnboundedReceiver<Incoming>,
    adapter_id: String,
    cx: &mut gpui::AsyncApp,
) {
    while let Some(message) = incoming.next().await {
        let exited = matches!(message, Incoming::Exited { .. });
        let alive = this.update(cx, |this, cx| {
            this.handle_incoming(id, index, message, &adapter_id, cx)
        });
        if alive.is_err() || exited {
            return;
        }
    }
}
