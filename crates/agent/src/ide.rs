//! The tools, run in the window: each reads or changes what the workspace has, and answers with
//! paths from the project root, and lines and columns from 1 with columns counting characters.

use crate::Agent;
use crate::proposals;
use crate::rpc::ToolOutput;
use crate::tools::{self, Edit, FilePath, Position, Target, TestChoice, ToolCall, relative};
use gleipnir_core::{
    BufferModel, ConsoleLine, Debugger, DiskStatus, GotoKind, LanguageServers, Launch, LaunchId,
    LaunchStatus, Location, NodeKind, ServerStatus, Severity, TEST_ROOT, TestState,
};
use gleipnir_index::GitState;
use gleipnir_packages::{JS_NAME_SEPARATOR, RunTarget, RunTargetKind, registry};
use gleipnir_text::{Buffer, Selection, SelectionSet, TextEdit, Utf16Point};
use gleipnir_workspace::Workspace;
use gpui::{App, AsyncWindowContext, Context, Entity, Focusable as _, WeakEntity, Window};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

pub(crate) const CLOSED: &str = "The Gleipnir window closed";
/// How often a tool that waits looks again.
const POLL: Duration = Duration::from_millis(100);
/// How long a lookup waits for its language server to start.
const SERVER_START_WAIT: Duration = Duration::from_secs(30);
/// The most items a list gives, so a huge answer doesn't swamp the agent.
const MAX_RESULTS: usize = 200;
/// How much of a test's or program's output is given: its end, where failures usually are.
const OUTPUT_LINES: usize = 80;
const OUTPUT_BYTES: usize = 8000;
/// How many lines read_file gives without being asked for a range.
const READ_LINES: usize = 2000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

type Outcome = Result<ToolOutput, String>;

/// Runs one tool call.
pub(crate) async fn run(
    workspace: &WeakEntity<Workspace>,
    agent: &WeakEntity<Agent>,
    root: &Path,
    name: &str,
    arguments: &Value,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let call = tools::parse(name, arguments, root)?;
    let json = ToolOutput::Json;
    match call {
        ToolCall::GetOpenFiles => {
            update(workspace, cx, |w, _, cx| open_files(w, root, cx)).map(json)
        }
        ToolCall::GetSelection => {
            update(workspace, cx, |w, _, cx| selection(w, root, cx)).map(json)
        }
        ToolCall::GetProblems { path, errors_only } => update(workspace, cx, |w, _, cx| {
            problems(w, root, path.as_ref(), errors_only, cx)
        })
        .map(json),
        ToolCall::GetTestResults => {
            update(workspace, cx, |w, _, cx| test_results(w, root, cx))?.map(json)
        }
        ToolCall::GetGitStatus => git_status(root, cx).await.map(json),
        ToolCall::FindDefinition(target) => lookup(workspace, root, target, false, cx).await,
        ToolCall::FindUsages(target) => lookup(workspace, root, target, true, cx).await,
        ToolCall::ReadFile {
            path,
            start_line,
            end_line,
        } => read_file(workspace, &path, start_line, end_line, cx).await,
        ToolCall::OpenFile {
            path,
            at,
            select_to,
        } => open_file(workspace, &path, at, select_to, cx).await,
        ToolCall::EditFile(edit) => edit_file(workspace, &edit, cx).await,
        ToolCall::SaveFile(path) => save_file(workspace, &path, cx).await,
        ToolCall::RunTests { path, choice, wait } => {
            run_tests(workspace, root, &path, &choice, wait, cx).await
        }
        ToolCall::RunConfiguration { name, wait } => {
            run_configuration(workspace, root, name, wait, cx).await
        }
        ToolCall::ProposeEdit { edit, wait } => {
            proposals::propose(workspace, agent, edit, wait, cx).await
        }
        ToolCall::GetProposal { id, wait } => proposals::answer(agent, id, wait, cx).await,
    }
}

fn update<R>(
    workspace: &WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
) -> Result<R, String> {
    workspace.update_in(cx, f).map_err(|_| CLOSED.to_string())
}

/// Waits until `done` says so, or `wait` has passed, looking again every so often. Returns
/// whether it's done.
pub(crate) async fn wait_until(
    cx: &mut AsyncWindowContext,
    wait: Duration,
    mut done: impl FnMut(&mut AsyncWindowContext) -> Result<bool, String>,
) -> Result<bool, String> {
    let executor = cx.background_executor().clone();
    let deadline = executor.now() + wait;
    loop {
        if done(cx)? {
            return Ok(true);
        }
        if executor.now() >= deadline {
            return Ok(false);
        }
        executor.timer(POLL).await;
    }
}

/// A place in a buffer as agents count it: line and column from 1, columns in characters.
fn point(text: &Buffer, offset: usize) -> Value {
    let p = text.offset_to_point(offset);
    let line = text.line(p.line);
    let column = line
        .get(..p.col.min(line.len()))
        .map_or(p.col, |before| before.chars().count());
    json!({ "line": p.line + 1, "column": column + 1 })
}

/// How many lines a text has, not counting the empty one after a final line break.
fn line_count(text: &Buffer) -> usize {
    let lines = text.line_count();
    if !text.is_empty() && text.char_before(text.len()) == Some('\n') {
        lines - 1
    } else {
        lines
    }
}

/// The offset of a line and column, a column past the line's end meaning its end.
fn offset_at(text: &Buffer, position: Position, path: &str) -> Result<usize, String> {
    let lines = line_count(text).max(1);
    if position.line > lines {
        return Err(format!(
            "{path} has {lines} line(s), so there's no line {}",
            position.line
        ));
    }
    let line = position.line - 1;
    let content = text.line(line);
    let byte = content
        .char_indices()
        .nth(position.column - 1)
        .map_or(content.len(), |(byte, _)| byte);
    Ok(text.line_start(line) + byte)
}

/// The end of a text, at most so many lines and bytes of it, cut at a character.
fn tail(lines: &[ConsoleLine]) -> String {
    let start = lines.len().saturating_sub(OUTPUT_LINES);
    let mut text: String = lines[start..]
        .iter()
        .map(|line| format!("{}\n", line.text))
        .collect();
    if text.len() > OUTPUT_BYTES {
        let mut cut = text.len() - OUTPUT_BYTES;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        text = format!("…{}", &text[cut..]);
    }
    text
}

// Reading.

fn open_files(workspace: &Workspace, root: &Path, cx: &App) -> Value {
    let active = workspace
        .active_editor(cx)
        .map(|e| e.read(cx).buffer().clone());
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    for pane in workspace.panes() {
        for item in pane.read(cx).editors() {
            let buffer = item.read(cx).buffer();
            if !seen.insert(buffer.entity_id()) {
                continue;
            }
            let model = buffer.read(cx);
            files.push(json!({
                "path": model.path().map_or("(untitled)".into(), |p| relative(root, p)),
                "active": Some(buffer) == active.as_ref(),
                "dirty": model.buffer().is_dirty(),
                "language": model.language().map(|l| l.name()),
            }));
        }
    }
    let active = active.and_then(|b| b.read(cx).path().map(|p| relative(root, p)));
    json!({ "files": files, "active": active })
}

fn selection(workspace: &Workspace, root: &Path, cx: &App) -> Value {
    let Some(editor) = workspace.active_editor(cx) else {
        return json!({ "path": null, "message": "No file is open in the editor" });
    };
    let view = editor.read(cx);
    let model = view.buffer().read(cx);
    let text = model.buffer();
    let primary = view.selections().primary();
    let mut answer = json!({
        "path": model.path().map_or("(untitled)".into(), |p| relative(root, p)),
        "language": model.language().map(|l| l.name()),
        "dirty": text.is_dirty(),
        "cursor": point(text, primary.head),
    });
    if primary.is_empty() {
        let line = text.offset_to_point(primary.head).line;
        answer["selection"] = Value::Null;
        answer["current_line"] = json!({ "line": line + 1, "text": text.line(line) });
    } else {
        let range = primary.range();
        answer["selection"] = json!({
            "start": point(text, range.start),
            "end": point(text, range.end),
            "text": text.slice(range),
        });
    }
    let others: Vec<Value> = view
        .selections()
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != view.selections().primary_index())
        .take(MAX_RESULTS)
        .map(|(_, s)| {
            json!({
                "start": point(text, s.range().start),
                "end": point(text, s.range().end),
                "text": text.slice(s.range()),
            })
        })
        .collect();
    if !others.is_empty() {
        answer["other_cursors"] = json!(others);
    }
    answer
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Information => "information",
        Severity::Hint => "hint",
    }
}

fn problems(
    workspace: &Workspace,
    root: &Path,
    only: Option<&FilePath>,
    errors_only: bool,
    cx: &App,
) -> Value {
    let servers = workspace.language_servers().read(cx);
    let store = workspace.store().read(cx);
    let mut found = Vec::new();
    for (path, diagnostics) in servers.all_diagnostics() {
        if only.is_some_and(|only| &only.absolute != path) {
            continue;
        }
        let shown = relative(root, path);
        // An open file's problems follow its unsaved edits.
        if let Some(buffer) = store.get(path) {
            let model = buffer.read(cx);
            for d in model.diagnostics() {
                let start = point(model.buffer(), d.range.start);
                let end = point(model.buffer(), d.range.end);
                found.push((
                    d.severity,
                    json!({
                        "path": shown,
                        "line": start["line"],
                        "column": start["column"],
                        "end_line": end["line"],
                        "end_column": end["column"],
                        "severity": severity_name(d.severity),
                        "message": d.message,
                        "source": d.source,
                    }),
                ));
            }
        } else {
            for (range, severity, message) in diagnostics {
                found.push((
                    *severity,
                    json!({
                        "path": shown,
                        "line": range.start.line + 1,
                        "column": range.start.character + 1,
                        "end_line": range.end.line + 1,
                        "end_column": range.end.character + 1,
                        "severity": severity_name(*severity),
                        "message": message,
                    }),
                ));
            }
        }
    }
    let errors = found.iter().filter(|(s, _)| *s == Severity::Error).count();
    let warnings = found
        .iter()
        .filter(|(s, _)| *s == Severity::Warning)
        .count();
    if errors_only {
        found.retain(|(s, _)| *s == Severity::Error);
    }
    // Errors first, then by file.
    found.sort_by_key(|(severity, _)| *severity);
    let none = found.is_empty();
    let listed: Vec<Value> = found
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, v)| v)
        .collect();
    let mut answer = json!({ "problems": listed, "errors": errors, "warnings": warnings });
    if none {
        answer["message"] = json!(
            "No problems reported. Language servers report on files they've looked at, which \
             are mostly the open ones."
        );
    }
    answer
}

fn state_name(state: TestState) -> &'static str {
    match state {
        TestState::Running => "running",
        TestState::Passed => "passed",
        TestState::Failed => "failed",
        TestState::Skipped => "skipped",
        TestState::Stopped => "stopped",
    }
}

fn status_name(status: &LaunchStatus) -> &'static str {
    match status {
        LaunchStatus::Building => "building",
        LaunchStatus::Starting => "starting",
        LaunchStatus::Running => "running",
        LaunchStatus::Paused => "paused in the debugger",
        LaunchStatus::Finished(_) => "finished",
        LaunchStatus::Failed(_) => "failed to start",
    }
}

/// A test's name as people read it: a JavaScript test's titles joined with `›`.
fn test_name(name: &str) -> String {
    name.trim_end_matches(JS_NAME_SEPARATOR)
        .replace(JS_NAME_SEPARATOR, " › ")
}

/// A launch's state, and for a test run, its results.
fn launch_json(launch: &Launch, root: &Path) -> Value {
    let finished = !launch.status.is_live();
    let mut answer = json!({
        "configuration": launch.name,
        "status": status_name(&launch.status),
        "finished": finished,
    });
    match &launch.status {
        LaunchStatus::Finished(Some(code)) => answer["exit_code"] = json!(code),
        LaunchStatus::Failed(why) => answer["error"] = json!(why),
        _ => {}
    }
    let Some(tests) = &launch.tests else {
        answer["output"] = json!(tail(&launch.console));
        return answer;
    };
    let counts = tests.node(TEST_ROOT).map(|n| n.counts).unwrap_or_default();
    answer["result"] = json!(if finished {
        state_name(tests.state(TEST_ROOT))
    } else {
        "running"
    });
    answer["counts"] = json!({
        "total": counts.total,
        "passed": counts.passed,
        "failed": counts.failed,
        "skipped": counts.skipped,
        "running": counts.running,
    });
    let failures: Vec<Value> = tests
        .failed_tests()
        .into_iter()
        .take(MAX_RESULTS)
        .filter_map(|index| {
            let node = tests.node(index)?;
            let mut failure = json!({
                "name": test_name(&node.name),
                "output": tail(&tests.output_of(index)),
            });
            if let Some((path, line)) = &node.location {
                failure["path"] = json!(relative(root, path));
                failure["line"] = json!(line + 1);
            }
            Some(failure)
        })
        .collect();
    answer["failures"] = json!(failures);
    if counts.total <= MAX_RESULTS {
        let all: Vec<Value> = tests
            .nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| n.kind == NodeKind::Test)
            .map(|(i, n)| json!({ "name": test_name(&n.name), "result": state_name(tests.state(i)) }))
            .collect();
        answer["tests"] = json!(all);
    }
    // No tests at all usually means the build failed, which the console says.
    if counts.total == 0 && finished {
        answer["output"] = json!(tail(&launch.console));
    }
    answer
}

fn debugger(workspace: &Workspace, cx: &App) -> Result<Entity<Debugger>, String> {
    gleipnir_debugger_ui::debugger(workspace, cx)
        .ok_or_else(|| "Gleipnir's runner isn't available in this window".to_string())
}

fn test_results(workspace: &Workspace, root: &Path, cx: &App) -> Result<Value, String> {
    let debugger = debugger(workspace, cx)?;
    let debugger = debugger.read(cx);
    Ok(
        match debugger.launches().iter().rev().find(|l| l.tests.is_some()) {
            Some(launch) => launch_json(launch, root),
            None => json!({
                "result": "none",
                "message": "No tests have run in this window yet; run_tests runs some",
            }),
        },
    )
}

fn git_state_name(state: GitState) -> &'static str {
    match state {
        GitState::Added => "added",
        GitState::Untracked => "untracked",
        GitState::Renamed => "renamed",
        GitState::Modified => "modified",
        GitState::Deleted => "deleted",
        GitState::Conflicted => "conflicted",
    }
}

async fn git_status(root: &Path, cx: &mut AsyncWindowContext) -> Result<Value, String> {
    let dir = root.to_path_buf();
    let (branch, changes, operation) = cx
        .background_executor()
        .spawn(async move {
            (
                gleipnir_workspace::current_branch(&dir),
                gleipnir_index::git_ops::changed_files(&dir),
                gleipnir_index::git_ops::operation_in_progress(&dir),
            )
        })
        .await;
    let files = match changes {
        Ok(files) => files,
        Err(error) => {
            return Ok(json!({
                "branch": branch,
                "message": format!("Git can't say what changed here: {error:#}"),
            }));
        }
    };
    let changed: Vec<Value> = files
        .iter()
        .take(MAX_RESULTS)
        .map(|f| json!({ "path": relative(root, &f.path), "status": git_state_name(f.state) }))
        .collect();
    let mut answer = json!({
        "branch": branch,
        "changes": changed,
        "changed_files": files.len(),
    });
    if let Some(operation) = operation {
        answer["in_progress"] = json!(format!("{operation:?}").to_lowercase());
    }
    Ok(answer)
}

/// A file's buffer, opened if it isn't, without a tab: for reading and asking about.
async fn buffer_for(
    workspace: &WeakEntity<Workspace>,
    path: &FilePath,
    cx: &mut AsyncWindowContext,
) -> Result<Entity<BufferModel>, String> {
    let open = update(workspace, cx, |w, _, cx| {
        w.store()
            .update(cx, |store, cx| store.open(&path.absolute, cx))
    })?;
    open.await
        .map_err(|e| format!("Can't open {}: {e:#}", path.relative))
}

/// A file's buffer, shown in a tab if it isn't already, for edits that stay unsaved: the person
/// sees them there, and can save or undo them.
pub(crate) async fn open_in_background(
    workspace: &WeakEntity<Workspace>,
    path: &FilePath,
    cx: &mut AsyncWindowContext,
) -> Result<Entity<BufferModel>, String> {
    let open = update(workspace, cx, |w, window, cx| {
        w.open_in_background(path.absolute.clone(), window, cx)
    })?;
    open.await
        .map_err(|e| format!("Can't open {}: {e:#}", path.relative))
}

/// Declarations with a name: from the project's own index, which is always there, or else the
/// language servers'.
async fn declarations(
    workspace: &WeakEntity<Workspace>,
    root: &Path,
    name: &str,
    cx: &mut AsyncWindowContext,
) -> Result<(Vec<Location>, bool), String> {
    let (symbols, busy) = update(workspace, cx, |w, _, cx| {
        w.project()
            .map(|p| p.update(cx, |p, _| (p.symbols(), p.is_busy())))
    })?
    .unwrap_or_default();
    let mut found: Vec<Location> = symbols
        .iter()
        .filter(|entry| entry.symbol.name == name)
        .map(|entry| {
            let path = Path::new(&*entry.path);
            let start = Utf16Point::new(entry.symbol.line, entry.symbol.column);
            Location {
                path: if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    root.join(path)
                },
                start,
                end: start,
            }
        })
        .collect();
    if found.is_empty() {
        let search = update(workspace, cx, |w, _, cx| {
            w.language_servers().read(cx).workspace_symbols(name, cx)
        })?;
        found = search
            .await
            .into_iter()
            .filter(|hit| hit.name == name)
            .map(|hit| hit.location)
            .collect();
    }
    found.sort_by(|a, b| (&a.path, a.start).cmp(&(&b.path, b.start)));
    found.dedup();
    Ok((found, busy))
}

/// Whether a buffer's language server can answer yet; an error if it never will.
fn server_ready(
    workspace: &Workspace,
    buffer: &Entity<BufferModel>,
    cx: &App,
) -> Result<bool, String> {
    let Some((package, server)) = LanguageServers::server_name_for(buffer, cx) else {
        return Err("No language server handles this kind of file".into());
    };
    // Tools are only known missing once the project's toolchains have been looked for.
    let looking = workspace.toolchains().read(cx).packages().is_empty();
    match workspace.language_servers().read(cx).status(&package) {
        Some(ServerStatus::Running { .. }) => Ok(true),
        Some(ServerStatus::Missing(_)) if looking => Ok(false),
        Some(ServerStatus::Missing(why) | ServerStatus::Failed(why)) => Err(format!(
            "The language server, {server}, isn't running: {why}"
        )),
        _ => Ok(false),
    }
}

async fn lookup(
    workspace: &WeakEntity<Workspace>,
    root: &Path,
    target: Target,
    usages: bool,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let key = if usages { "usages" } else { "definitions" };
    let mut note = None;
    let (buffer, offset) = match target {
        Target::At(path, position) => {
            let buffer = buffer_for(workspace, &path, cx).await?;
            let offset =
                buffer.read_with(cx, |b, _| offset_at(b.buffer(), position, &path.relative))?;
            (buffer, offset)
        }
        Target::Symbol(name) => {
            let (found, busy) = declarations(workspace, root, &name, cx).await?;
            let Some(first) = found.first().cloned() else {
                let mut message = format!("Nothing named {name} is declared in the project");
                if busy {
                    message.push_str(" that Gleipnir has indexed so far; it's still indexing");
                }
                return Err(message);
            };
            if !usages {
                let previews = update(workspace, cx, |w, _, cx| {
                    gleipnir_code_nav::previews(w, found, cx)
                })?
                .await;
                return Ok(ToolOutput::Json(json!({
                    "symbol": name,
                    "definitions": usage_list(root, &previews),
                })));
            }
            if found.len() > 1 {
                note = Some(format!(
                    "{} declarations are named {name}; these are the usages of the one in {}:{}. \
                     Give a path, line and column for another.",
                    found.len(),
                    relative(root, &first.path),
                    first.start.line + 1
                ));
            }
            let path = tools::resolve_path(root, &first.path.to_string_lossy())?;
            let buffer = buffer_for(workspace, &path, cx).await?;
            let offset = buffer.read_with(cx, |b, _| b.buffer().utf16_to_offset(first.start));
            (buffer, offset)
        }
    };
    update(workspace, cx, |w, _, cx| {
        w.language_servers()
            .update(cx, |servers, cx| servers.register_buffer(&buffer, cx))
    })?;
    let ready = wait_until(cx, SERVER_START_WAIT, |cx| {
        update(workspace, cx, |w, _, cx| server_ready(w, &buffer, cx))?
    })
    .await?;
    if !ready {
        return Err("The language server is still starting; try again in a moment".into());
    }
    let request = update(workspace, cx, |w, _, cx| {
        let servers = w.language_servers().read(cx);
        if usages {
            servers.references(&buffer, offset, cx)
        } else {
            servers.goto(&buffer, offset, GotoKind::Definition, cx)
        }
    })?;
    let locations = request
        .await
        .map_err(|e| format!("The language server couldn't answer: {e:#}"))?;
    let previews = update(workspace, cx, |w, _, cx| {
        gleipnir_code_nav::previews(w, locations, cx)
    })?
    .await;
    let mut answer = json!({ key: usage_list(root, &previews), "count": previews.len() });
    if previews.is_empty() {
        let busy = update(workspace, cx, |w, _, cx| {
            let package = LanguageServers::server_name_for(&buffer, cx)?.0;
            match w.language_servers().read(cx).status(&package)? {
                ServerStatus::Running { busy: Some(task) } => Some(task.clone()),
                _ => None,
            }
        })?;
        if let Some(task) = busy {
            note = Some(format!(
                "The language server is still busy ({task}), so it may find more once it's done"
            ));
        }
    }
    if let Some(note) = note {
        answer["message"] = json!(note);
    }
    Ok(ToolOutput::Json(answer))
}

fn usage_list(root: &Path, usages: &[gleipnir_code_nav::Usage]) -> Vec<Value> {
    usages
        .iter()
        .take(MAX_RESULTS)
        .map(|u| {
            json!({
                "path": relative(root, &u.path),
                "line": u.line + 1,
                "column": u.start.character + 1,
                "text": u.text,
            })
        })
        .collect()
}

fn read_text(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_BYTES {
        return Err(format!("it's {} MB, too big to read whole", size >> 20));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "it isn't text".to_string())?;
    Ok(text.replace("\r\n", "\n"))
}

async fn read_file(
    workspace: &WeakEntity<Workspace>,
    path: &FilePath,
    start_line: Option<usize>,
    end_line: Option<usize>,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let open = update(workspace, cx, |w, _, cx| {
        let buffer = w.store().read(cx).get(&path.absolute)?;
        let text = buffer.read(cx).buffer();
        Some((text.text(), text.is_dirty()))
    })?;
    let (text, from_editor, dirty) = match open {
        Some((text, dirty)) => (text, true, dirty),
        None => {
            let absolute = path.absolute.clone();
            let text = cx
                .background_executor()
                .spawn(async move { read_text(&absolute) })
                .await
                .map_err(|why| format!("Can't read {}: {why}", path.relative))?;
            (text, false, false)
        }
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    let start = start_line.unwrap_or(1);
    if start > total.max(1) {
        return Err(format!(
            "{} has {total} line(s), so there's no line {start}",
            path.relative
        ));
    }
    let mut end = end_line.unwrap_or(total).min(total);
    let truncated = end_line.is_none() && (end + 1).saturating_sub(start) > READ_LINES;
    if truncated {
        end = start + READ_LINES - 1;
    }
    let body = if total == 0 {
        String::new()
    } else {
        lines[start - 1..end].concat()
    };
    let mut about = json!({
        "path": path.relative,
        "start_line": start,
        "end_line": end,
        "total_lines": total,
        "from": if from_editor { "editor" } else { "disk" },
        "unsaved_edits": dirty,
    });
    if truncated {
        about["message"] = json!(format!(
            "Only lines {start} to {end} of {total} are here; ask for more with start_line and end_line"
        ));
    }
    Ok(ToolOutput::JsonAndText(about, body))
}

// Acting.

async fn open_file(
    workspace: &WeakEntity<Workspace>,
    path: &FilePath,
    at: Option<Position>,
    select_to: Option<(usize, Option<usize>)>,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let (open, focus) = update(workspace, cx, |w, window, cx| {
        // Showing a file mustn't take the keyboard from where the person is typing, as to the
        // agent in the terminal. In the editor, it's natural for it to follow.
        let in_editor = w
            .panes()
            .iter()
            .any(|pane| pane.focus_handle(cx).contains_focused(window, cx));
        let focus = if in_editor { None } else { window.focused(cx) };
        w.record_position(cx);
        (w.open_path(path.absolute.clone(), window, cx), focus)
    })?;
    let editor = match open.await {
        Ok(editor) => editor,
        // Shown, but in a preview, so there's no text to place the cursor in.
        Err(e)
            if e.downcast_ref::<gleipnir_workspace::ShownAsImage>()
                .is_some() =>
        {
            update(workspace, cx, |_, window, cx| {
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
            })?;
            return Ok(ToolOutput::Json(
                json!({ "path": path.relative, "shown_as": "image" }),
            ));
        }
        Err(e) => return Err(format!("Can't open {}: {e:#}", path.relative)),
    };
    update(workspace, cx, |_, window, cx| {
        let placed = editor.update(cx, |editor, cx| -> Result<Value, String> {
            let Some(at) = at else {
                return Ok(json!({ "path": path.relative }));
            };
            let (start, end, start_point) = {
                let text = editor.buffer().read(cx).buffer();
                let start = offset_at(text, at, &path.relative)?;
                let end = match select_to {
                    None => None,
                    Some((line, column)) => Some(match column {
                        Some(column) => offset_at(text, Position { line, column }, &path.relative)?,
                        None => {
                            let lines = line_count(text).max(1);
                            text.line_end(line.min(lines) - 1)
                        }
                    }),
                };
                (start, end, text.offset_to_point(start))
            };
            editor.jump_to(start_point.line, start_point.col, cx);
            let mut answer = json!({ "path": path.relative, "line": at.line, "column": at.column });
            if let Some(end) = end.filter(|&end| end > start) {
                editor.set_selections(SelectionSet::single(Selection::new(start, end)), cx);
                answer["selected"] = json!(editor.buffer().read(cx).buffer().slice(start..end));
            }
            Ok(answer)
        });
        if let Some(focus) = focus {
            window.focus(&focus, cx);
        }
        placed
    })?
    .map(ToolOutput::Json)
}

/// Where `old_text` is in a text, as edits replacing it: an error unless it's there, and there
/// once, or `replace_all` is set.
pub(crate) fn find_edits(text: &str, edit: &Edit) -> Result<Vec<TextEdit>, String> {
    let found: Vec<usize> = text
        .match_indices(&edit.old_text)
        .map(|(at, _)| at)
        .collect();
    let line_of = |at: usize| text[..at].matches('\n').count() + 1;
    match found.len() {
        0 => Err(not_found(text, edit)),
        count if count > 1 && !edit.replace_all => {
            let lines: Vec<String> = found
                .iter()
                .take(10)
                .map(|&at| line_of(at).to_string())
                .collect();
            Err(format!(
                "old_text is in {} {count} times, on lines {}{}; include more of the lines around \
                 it so it's found once, or set replace_all to change every one",
                edit.path.relative,
                lines.join(", "),
                if count > 10 { " and more" } else { "" }
            ))
        }
        _ => Ok(found
            .into_iter()
            .map(|at| TextEdit::new(at..at + edit.old_text.len(), edit.new_text.clone()))
            .collect()),
    }
}

fn not_found(text: &str, edit: &Edit) -> String {
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let squashed = squash(&edit.old_text);
    if !squashed.is_empty() && squash(text).contains(&squashed) {
        format!(
            "old_text wasn't found in {} as given, but it is with different spacing: check its \
             indentation (tabs or spaces) and line breaks against read_file",
            edit.path.relative
        )
    } else {
        format!(
            "old_text wasn't found in {}. read_file shows the file as the editor has it, which \
             differs from the disk while it has unsaved edits",
            edit.path.relative
        )
    }
}

/// A text with non-overlapping edits made, as `find_edits` gives them.
pub(crate) fn apply_edits(text: &str, edits: &[TextEdit]) -> String {
    let mut out = text.to_string();
    for edit in edits.iter().rev() {
        out.replace_range(edit.range.clone(), &edit.text);
    }
    out
}

async fn edit_file(
    workspace: &WeakEntity<Workspace>,
    edit: &Edit,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let buffer = open_in_background(workspace, &edit.path, cx).await?;
    buffer.update(cx, |buffer, cx| {
        let text = buffer.buffer().text();
        let edits = find_edits(&text, edit)?;
        let count = edits.len();
        let line = text[..edits[0].range.start].matches('\n').count() + 1;
        buffer.apply_external_edits(edits, cx);
        Ok(ToolOutput::Json(json!({
            "path": edit.path.relative,
            "replacements": count,
            "line": line,
            "saved": false,
            "message": "Changed in the editor, as one step the person can undo. It's unsaved \
                        until save_file, or the person saves.",
        })))
    })
}

async fn save_file(
    workspace: &WeakEntity<Workspace>,
    path: &FilePath,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let buffer = update(workspace, cx, |w, _, cx| {
        w.store().read(cx).get(&path.absolute)
    })?;
    let Some(buffer) = buffer else {
        return Ok(ToolOutput::Json(json!({
            "path": path.relative,
            "saved": false,
            "message": "It isn't open in Gleipnir, so there's nothing unsaved: the file on disk is current",
        })));
    };
    let (dirty, status) = buffer.read_with(cx, |b, _| (b.buffer().is_dirty(), b.disk_status()));
    if status == DiskStatus::Conflict {
        return Err(format!(
            "{} changed on disk while it had unsaved edits in Gleipnir; saving would overwrite \
             that change, so ask the person which to keep",
            path.relative
        ));
    }
    if !dirty {
        return Ok(ToolOutput::Json(json!({
            "path": path.relative,
            "saved": true,
            "message": "It had no unsaved edits",
        })));
    }
    let save = buffer.update(cx, |b, cx| b.save(cx));
    save.await
        .map_err(|e| format!("Couldn't save {}: {e:#}", path.relative))?;
    Ok(ToolOutput::Json(
        json!({ "path": path.relative, "saved": true }),
    ))
}

/// The names a test target goes by: its own, as the gutter shows it, and the full one it's run
/// with, like `tests::parses` for Rust's `parses`.
fn test_names(target: &RunTarget) -> Vec<String> {
    let Some(own) = target.single_test() else {
        return Vec::new();
    };
    let mut names = vec![test_name(own)];
    names.extend(
        target
            .args
            .iter()
            .filter(|arg| !arg.starts_with('-'))
            .map(|arg| test_name(arg)),
    );
    names
}

/// Whether a test target is the one a name means: exactly, or failing that, by its last part.
fn names_test(target: &RunTarget, name: &str, exactly: bool) -> bool {
    test_names(target).iter().any(|known| {
        known == name
            || (!exactly
                && [
                    known.rsplit("::").next(),
                    known.rsplit('/').next(),
                    known.rsplit(" › ").next(),
                ]
                .contains(&Some(name)))
    })
}

/// The tests a choice in a file means, as the gutter's Run icons would run them.
pub(crate) fn find_tests(
    debugger: &Debugger,
    path: &FilePath,
    text: &str,
    choice: &TestChoice,
) -> Result<RunTarget, String> {
    let file = &path.absolute;
    let language = registry()
        .language_for_path(file)
        .ok_or_else(|| format!("Gleipnir can't run tests in {}", path.relative))?
        .grammar;
    let declared: Vec<(usize, RunTarget)> =
        gleipnir_debugger_ui::test_candidates(language, file, text)
            .into_iter()
            .filter_map(|line| Some((line, debugger.test_at(file, text, line)?)))
            .collect();
    let names = || {
        let names: Vec<String> = declared
            .iter()
            .filter_map(|(_, t)| t.single_test().map(test_name))
            .take(40)
            .collect();
        if names.is_empty() {
            format!("Gleipnir finds no tests in {}", path.relative)
        } else {
            format!("Its tests are: {}", names.join(", "))
        }
    };
    match choice {
        TestChoice::Line(line) => debugger.test_at(file, text, line - 1).ok_or_else(|| {
            let lines: Vec<String> = declared.iter().map(|(l, _)| (l + 1).to_string()).collect();
            format!(
                "No test is declared on line {line} of {}{}",
                path.relative,
                if lines.is_empty() {
                    String::new()
                } else {
                    format!("; tests are declared on lines {}", lines.join(", "))
                }
            )
        }),
        TestChoice::Named(name) => [true, false]
            .into_iter()
            .find_map(|exactly| declared.iter().find(|(_, t)| names_test(t, name, exactly)))
            .map(|(_, t)| t.clone())
            .ok_or_else(|| format!("No test named {name} in {}. {}", path.relative, names())),
        TestChoice::File => declared
            .iter()
            .find(|(_, t)| t.single_test().is_none())
            .map(|(_, t)| t.clone())
            .or_else(|| {
                // A Go or Odin package's tests run together, from its folder.
                debugger
                    .target_for_file(file)
                    .filter(|t| t.kind == RunTargetKind::Test)
            })
            .or_else(|| {
                debugger
                    .targets()
                    .iter()
                    .find(|t| {
                        t.kind == RunTargetKind::Test
                            && t.source
                                .as_deref()
                                .is_some_and(|s| s == file || Some(s) == file.parent())
                    })
                    .cloned()
            })
            .ok_or_else(|| {
                format!(
                    "Gleipnir finds no way to run {}'s tests as a whole; name one with test. {}",
                    path.relative,
                    names()
                )
            }),
    }
}

/// Starts a target, waits up to `wait` for it to end, and says how it went.
async fn start_and_wait(
    workspace: &WeakEntity<Workspace>,
    root: &Path,
    target: RunTarget,
    wait: Duration,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let id: LaunchId = update(workspace, cx, |w, _, cx| {
        debugger(w, cx).map(|d| d.update(cx, |d, cx| d.start(target, false, cx)))
    })??;
    let launch_ended = |w: &Workspace, cx: &App| -> Result<bool, String> {
        let debugger = debugger(w, cx)?;
        Ok(debugger.read(cx).launch(id).is_none_or(|l| {
            !l.status.is_live() && l.tests.as_ref().is_none_or(|t| t.is_finished())
        }))
    };
    let ended = wait_until(cx, wait, |cx| {
        update(workspace, cx, |w, _, cx| launch_ended(w, cx))?
    })
    .await?;
    let mut answer = update(workspace, cx, |w, _, cx| {
        let debugger = debugger(w, cx)?;
        let debugger = debugger.read(cx);
        debugger
            .launch(id)
            .map(|launch| launch_json(launch, root))
            .ok_or_else(|| {
                "The run was replaced by another of the same before it could be read".to_string()
            })
    })??;
    if !ended {
        answer["message"] = json!(
            "It's still running, in Gleipnir's Run panel; get_test_results has the results \
             once it ends."
        );
    }
    Ok(ToolOutput::Json(answer))
}

async fn run_tests(
    workspace: &WeakEntity<Workspace>,
    root: &Path,
    path: &FilePath,
    choice: &TestChoice,
    wait: Duration,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let open = update(workspace, cx, |w, _, cx| {
        w.store()
            .read(cx)
            .get(&path.absolute)
            .map(|b| b.read(cx).buffer().text())
    })?;
    let text = match open {
        Some(text) => text,
        None => read_text(&path.absolute)
            .map_err(|why| format!("Can't read {}: {why}", path.relative))?,
    };
    let target = update(workspace, cx, |w, _, cx| {
        find_tests(debugger(w, cx)?.read(cx), path, &text, choice)
    })??;
    start_and_wait(workspace, root, target, wait, cx).await
}

async fn run_configuration(
    workspace: &WeakEntity<Workspace>,
    root: &Path,
    name: Option<String>,
    wait: Duration,
    cx: &mut AsyncWindowContext,
) -> Outcome {
    let targets = update(workspace, cx, |w, _, cx| {
        debugger(w, cx).map(|d| d.read(cx).targets().to_vec())
    })??;
    let Some(name) = name else {
        let listed: Vec<Value> = targets
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "kind": format!("{:?}", t.kind).to_lowercase(),
                    "source": t.source.as_deref().map(|s| relative(root, s)),
                })
            })
            .collect();
        let mut answer = json!({ "configurations": listed });
        if targets.is_empty() {
            answer["message"] = json!(
                "No run configurations were found: Cargo binaries, Go and Odin main packages and \
                 package.json scripts are, once the project's been looked through"
            );
        }
        return Ok(ToolOutput::Json(answer));
    };
    let target = targets
        .iter()
        .find(|t| t.name == name)
        .or_else(|| targets.iter().find(|t| t.name.eq_ignore_ascii_case(&name)))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
            format!(
                "There's no run configuration named {name}; there are: {}",
                names.join(", ")
            )
        })?;
    start_and_wait(workspace, root, target, wait, cx).await
}
