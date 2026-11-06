//! Driving a session: the launch handshake, the requests the IDE makes, and the state that events
//! move between. Nothing here knows about the UI, so it runs the same under test.

use crate::client::{DebugAdapter, RequestError};
use crate::types::*;
use futures::future::{Either, select};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// What to send after `initialize`: a `launch` or an `attach`, with the adapter's own arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchRequest {
    pub adapter_id: String,
    /// `launch` or `attach`.
    pub request: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBreakpoints {
    pub path: PathBuf,
    pub breakpoints: Vec<SourceBreakpoint>,
}

/// A session that's past the handshake.
#[derive(Clone, Debug, PartialEq)]
pub struct Started {
    pub capabilities: Capabilities,
    /// What the adapter made of each file's breakpoints, in the order they were sent.
    pub breakpoints: Vec<(PathBuf, Vec<Breakpoint>)>,
}

/// The `initialize` request, saying what this client supports.
pub async fn initialize(
    adapter: &DebugAdapter,
    adapter_id: &str,
) -> Result<Capabilities, RequestError> {
    let body = adapter
        .request(
            "initialize",
            json!({
                "clientID": "gleipnir",
                "clientName": "Gleipnir",
                "adapterID": adapter_id,
                "locale": "en",
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsVariableType": true,
                "supportsVariablePaging": false,
                "supportsRunInTerminalRequest": false,
                "supportsStartDebuggingRequest": true,
                "supportsProgressReporting": false,
                "supportsInvalidatedEvent": false,
                "supportsMemoryReferences": false,
                "supportsArgsCanBeInterpretedByShell": false,
            }),
        )
        .await?;
    if body.is_null() {
        return Ok(Capabilities::default());
    }
    serde_json::from_value(body).map_err(|e| RequestError::BadResponse(e.to_string()))
}

/// The whole handshake: `initialize`, then `launch` (or `attach`), then breakpoints and
/// `configurationDone` once the adapter says it's ready.
///
/// Adapters order this differently. lldb-dap and js-debug answer `launch` only after
/// `configurationDone`; delve answers it first and sends `initialized` after. So the launch is sent
/// without waiting, and whichever comes first is handled.
pub async fn start(
    adapter: &DebugAdapter,
    request: &LaunchRequest,
    breakpoints: &[FileBreakpoints],
) -> Result<Started, RequestError> {
    let capabilities = initialize(adapter, &request.adapter_id).await?;
    let launch = adapter.request(&request.request, request.arguments.clone());
    let initialized = adapter.initialized();
    let launch = match select(Box::pin(initialized), launch).await {
        Either::Left((ready, launch)) => {
            ready?;
            Some(launch)
        }
        Either::Right((launched, initialized)) => {
            launched?;
            initialized.await?;
            None
        }
    };
    let placed = configure(adapter, &capabilities, breakpoints).await?;
    if let Some(launch) = launch {
        launch.await?;
    }
    Ok(Started {
        capabilities,
        breakpoints: placed,
    })
}

/// Breakpoints, exception filters, then `configurationDone`.
async fn configure(
    adapter: &DebugAdapter,
    capabilities: &Capabilities,
    breakpoints: &[FileBreakpoints],
) -> Result<Vec<(PathBuf, Vec<Breakpoint>)>, RequestError> {
    let mut placed = Vec::new();
    for file in breakpoints {
        let result = set_breakpoints(adapter, &file.path, &file.breakpoints).await?;
        placed.push((file.path.clone(), result));
    }
    if !capabilities.exception_breakpoint_filters.is_empty() {
        let filters: Vec<&str> = capabilities
            .exception_breakpoint_filters
            .iter()
            .filter(|f| f.default)
            .map(|f| f.filter.as_str())
            .collect();
        adapter
            .request("setExceptionBreakpoints", json!({ "filters": filters }))
            .await?;
    }
    if capabilities.supports_configuration_done_request {
        adapter.request("configurationDone", json!({})).await?;
    }
    Ok(placed)
}

/// Replaces every breakpoint in a file.
pub async fn set_breakpoints(
    adapter: &DebugAdapter,
    path: &Path,
    breakpoints: &[SourceBreakpoint],
) -> Result<Vec<Breakpoint>, RequestError> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        breakpoints: Vec<Breakpoint>,
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let body: Body = adapter
        .request_as(
            "setBreakpoints",
            json!({
                "source": { "name": name, "path": path },
                "breakpoints": breakpoints,
                "lines": breakpoints.iter().map(|b| b.line).collect::<Vec<_>>(),
                "sourceModified": false,
            }),
        )
        .await?;
    Ok(body.breakpoints)
}

pub async fn threads(adapter: &DebugAdapter) -> Result<Vec<Thread>, RequestError> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        threads: Vec<Thread>,
    }
    let body: Body = adapter.request_as("threads", json!({})).await?;
    Ok(body.threads)
}

pub async fn stack_trace(
    adapter: &DebugAdapter,
    thread_id: i64,
    levels: u32,
) -> Result<Vec<StackFrame>, RequestError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        #[serde(default)]
        stack_frames: Vec<StackFrame>,
    }
    let body: Body = adapter
        .request_as(
            "stackTrace",
            json!({ "threadId": thread_id, "startFrame": 0, "levels": levels }),
        )
        .await?;
    Ok(body.stack_frames)
}

pub async fn scopes(adapter: &DebugAdapter, frame_id: i64) -> Result<Vec<Scope>, RequestError> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        scopes: Vec<Scope>,
    }
    let body: Body = adapter
        .request_as("scopes", json!({ "frameId": frame_id }))
        .await?;
    Ok(body.scopes)
}

pub async fn variables(
    adapter: &DebugAdapter,
    reference: i64,
) -> Result<Vec<Variable>, RequestError> {
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        variables: Vec<Variable>,
    }
    let body: Body = adapter
        .request_as("variables", json!({ "variablesReference": reference }))
        .await?;
    Ok(body.variables)
}

/// Evaluates an expression. `context` is `watch`, `repl`, `hover` or `clipboard`, which some
/// adapters treat differently: js-debug runs side effects only for `repl`.
pub async fn evaluate(
    adapter: &DebugAdapter,
    expression: &str,
    frame_id: Option<i64>,
    context: &str,
) -> Result<EvaluateResult, RequestError> {
    let mut arguments = json!({ "expression": expression, "context": context });
    if let Some(frame) = frame_id {
        arguments["frameId"] = json!(frame);
    }
    adapter.request_as("evaluate", arguments).await
}

/// How execution moves on from a stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    Over,
    Into,
    Out,
}

impl Step {
    fn command(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Over => "next",
            Self::Into => "stepIn",
            Self::Out => "stepOut",
        }
    }
}

pub async fn resume(
    adapter: &DebugAdapter,
    thread_id: i64,
    step: Step,
) -> Result<(), RequestError> {
    adapter
        .request(step.command(), json!({ "threadId": thread_id }))
        .await
        .map(drop)
}

pub async fn pause(adapter: &DebugAdapter, thread_id: i64) -> Result<(), RequestError> {
    adapter
        .request("pause", json!({ "threadId": thread_id }))
        .await
        .map(drop)
}

/// Ends the session. A launched program is stopped with it.
pub async fn disconnect(adapter: &DebugAdapter, terminate: bool) -> Result<(), RequestError> {
    adapter
        .request(
            "disconnect",
            json!({ "restart": false, "terminateDebuggee": terminate }),
        )
        .await
        .map(drop)
}

/// Where a session is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Starting,
    Running,
    Stopped(StoppedEvent),
    /// The program finished, or the session ended.
    Ended,
}

/// What an event changed, so the owner knows what to refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Stopped,
    Continued,
    Threads,
    Output(OutputEvent),
    Breakpoint(Breakpoint),
    Exited(i64),
    Ended,
    Nothing,
}

/// A session's state, moved along by the adapter's events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    pub run: RunState,
    pub exit_code: Option<i64>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            run: RunState::Starting,
            exit_code: None,
        }
    }

    pub fn stopped(&self) -> Option<&StoppedEvent> {
        match &self.run {
            RunState::Stopped(stopped) => Some(stopped),
            _ => None,
        }
    }

    /// For requests we make that resume the program: adapters needn't send `continued` for them.
    pub fn resumed(&mut self) {
        if self.run != RunState::Ended {
            self.run = RunState::Running;
        }
    }

    pub fn apply(&mut self, event: &str, body: &Value) -> Change {
        fn parse<T: serde::de::DeserializeOwned>(body: &Value) -> Option<T> {
            serde_json::from_value(body.clone()).ok()
        }
        match event {
            "stopped" => {
                let Some(stopped) = parse(body) else {
                    return Change::Nothing;
                };
                self.run = RunState::Stopped(stopped);
                Change::Stopped
            }
            "continued" => {
                if self.run == RunState::Ended {
                    return Change::Nothing;
                }
                self.run = RunState::Running;
                Change::Continued
            }
            "process" | "initialized" if self.run == RunState::Starting => {
                self.run = RunState::Running;
                Change::Continued
            }
            "thread" => Change::Threads,
            "output" => match parse(body) {
                Some(output) => Change::Output(output),
                None => Change::Nothing,
            },
            "breakpoint" => match parse::<BreakpointEvent>(body) {
                Some(event) => Change::Breakpoint(event.breakpoint),
                None => Change::Nothing,
            },
            "exited" => match parse::<ExitedEvent>(body) {
                Some(exited) => {
                    self.exit_code = Some(exited.exit_code);
                    Change::Exited(exited.exit_code)
                }
                None => Change::Nothing,
            },
            "terminated" => {
                self.run = RunState::Ended;
                Change::Ended
            }
            _ => Change::Nothing,
        }
    }
}
