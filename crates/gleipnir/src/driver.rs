//! Runs a script against the real application: real window, real rendering, real key dispatch.
//! This is how end-to-end behaviour gets checked, on any machine with a display.
//!
//! Set `GLEIPNIR_SCRIPT` to a file with one command per line. Blank lines and `#` comments are
//! ignored. The process exits 0 if every command succeeded, and 1 at the first failure.
//!
//!     key <keystrokes>            one or more space-separated keystrokes, like: key cmd-s down.
//!                                 `mac|other` gives the macOS key and the Linux one, where the
//!                                 platforms differ by more than Cmd for Ctrl: key cmd-t|ctrl-shift-t
//!     do <EditorAction>           presses the key this platform binds to an editor action
//!     action <namespace::Name>    dispatches an action to whatever has focus
//!     double-shift                taps Shift twice, as real modifier events
//!     wait-idle                   waits for the project's scanning and indexing to finish
//!     expect-state <pointer> <v>  checks a value in the state, by JSON pointer; see `dump`
//!     wait-state <pointer> <v>    waits for a value, for anything asynchronous: ten seconds, or
//!                                 as set by `wait-timeout`
//!     wait-match <pointer> <text> waits for a string containing the text, or a list with an
//!                                 item containing it
//!     wait-timeout <seconds>      how long `wait-state` waits, as language servers can be slow
//!     cursor <line>:<column>      moves the cursor, one-based, as editors show positions
//!     type <text>                 types text, one keystroke per character. Text in any command
//!                                 can use the escapes \n, \t and \s (space, as lines are trimmed)
//!     wait <milliseconds>
//!     wait-parsed                 waits for background parsing to finish
//!     open <path>
//!     expect-text <text>          the whole document, with \n and \t escapes
//!     expect-contains <text>
//!     expect-cursors <count>
//!     expect-dirty <true|false>
//!     expect-disk <InSync|Conflict|Deleted>
//!     expect-language <name>
//!     expect-highlights           at least one highlight span on screen
//!     expect-file <path> <text>   the file's bytes on disk, with \n and \t escapes
//!     write-file <path> <text>    changes a file behind the application's back
//!     dump <path>                 writes the editor state as JSON
//!     screenshot <path>           writes a PNG, if built with `--features capture`
//!     scroll-bench <frames>       scrolls a frame at a time, reporting per-frame editor cost
//!     type-bench <count>          types and undoes, reporting per-keystroke cost
//!     mcp <tool> <json>           calls an MCP tool as an agent would, through `gleipnir mcp`
//!     expect-mcp <text>           the last tool's answer contains the text
//!     quit

use anyhow::{Context as _, Result, anyhow, bail};
use gleipnir_editor::EditorState;
use gleipnir_workspace::Workspace;
use gpui::{App, AppContext as _, AsyncApp, Keystroke, Window, WindowHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long `wait-state` waits, in seconds.
static WAIT_SECONDS: AtomicU64 = AtomicU64::new(10);

/// The last `mcp` command's answer: its text, after `error: ` if the tool failed.
static LAST_TOOL_ANSWER: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Calls one tool through `gleipnir mcp`, this same binary, as an agent would: it initializes,
/// calls, and closes its input, then reads every answer.
fn call_tool(socket: &Path, tool: &str, arguments: serde_json::Value) -> Result<String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("mcp")
        .env("GLEIPNIR_SOCKET", socket)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let requests = [
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "gleipnir-e2e" } } }),
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments } }),
    ];
    let mut stdin = child.stdin.take().context("no stdin")?;
    for request in requests {
        writeln!(stdin, "{request}")?;
    }
    drop(stdin);
    let output = child.wait_with_output()?;
    let text = String::from_utf8(output.stdout)?;
    let answer: serde_json::Value = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|message| message["id"] == 2)
        .with_context(|| format!("no answer to the call in {text:?}"))?;
    let content: Vec<&str> = answer["result"]["content"]
        .as_array()
        .context("the answer has no content")?
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect();
    let failed = answer["result"]["isError"] == true;
    Ok(format!(
        "{}{}",
        if failed { "error: " } else { "" },
        content.join("\n")
    ))
}

pub fn run(script: PathBuf, window: WindowHandle<Workspace>, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let code = match run_script(&script, window, cx).await {
            Ok(()) => {
                println!("script passed");
                0
            }
            Err(error) => {
                eprintln!("script failed: {error:#}");
                1
            }
        };
        std::process::exit(code);
    })
    .detach();
}

async fn run_script(
    script: &Path,
    window: WindowHandle<Workspace>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let source = std::fs::read_to_string(script)
        .with_context(|| format!("can't read {}", script.display()))?;
    sleep(Duration::from_millis(100), cx).await;
    draw(window, cx)?;

    for (number, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        run_command(line, window, cx)
            .await
            .with_context(|| format!("line {}: {line}", number + 1))?;
        // Let work this command started run on the main thread, then show the result.
        sleep(Duration::from_millis(2), cx).await;
        draw(window, cx)?;
    }
    Ok(())
}

/// Draws a frame now. The platform stops sending frames to a window that's covered or not in
/// front, and scripts mustn't depend on what else is on the screen.
fn draw(window: WindowHandle<Workspace>, cx: &mut AsyncApp) -> Result<()> {
    with_window(window, cx, |window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    })
}

/// Runs something against the window itself. `WindowHandle::update` would hold the root view for
/// the duration, and drawing it or dispatching an action to it would then be a double borrow.
fn with_window<R>(
    window: WindowHandle<Workspace>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> Result<R> {
    cx.update_window(window.into(), |_, window, cx| f(window, cx))
        .map_err(|e| anyhow!("{e}"))
}

/// Runs something against the workspace with its window, for operations that need both.
fn with_workspace<R>(
    window: WindowHandle<Workspace>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut gpui::Context<Workspace>) -> R,
) -> Result<R> {
    window.update(cx, f).map_err(|e| anyhow!("{e}"))
}

async fn sleep(duration: Duration, cx: &mut AsyncApp) {
    cx.background_executor().timer(duration).await;
}

fn unescape(text: &str) -> String {
    text.replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\s", " ")
}

fn state(window: WindowHandle<Workspace>, cx: &mut AsyncApp) -> Result<EditorState> {
    window
        .update(cx, |workspace, _, cx| workspace.editor_state(cx))
        .map_err(|e| anyhow!("{e}"))?
        .context("no editor is open")
}

/// The workspace and active editor as JSON, for `dump` and the `-state` commands.
fn snapshot(window: WindowHandle<Workspace>, cx: &mut AsyncApp) -> Result<serde_json::Value> {
    window
        .update(cx, |workspace, window, cx| {
            let mut value = serde_json::to_value(workspace.snapshot(window, cx))?;
            value["editor"] = serde_json::to_value(workspace.editor_state(cx))?;
            Ok::<_, serde_json::Error>(value)
        })
        .map_err(|e| anyhow!("{e}"))?
        .map_err(Into::into)
}

/// Compares a value at a JSON pointer with text. Numbers and booleans compare by how they print.
fn state_matches(state: &serde_json::Value, pointer: &str, expected: &str) -> bool {
    match state.pointer(pointer) {
        Some(serde_json::Value::String(actual)) => actual == expected,
        // Not `actual == expected`: a JSON number never equals a string, even "3" and 3.
        Some(actual) => {
            let printed = actual.to_string();
            printed == expected
        }
        None => expected == "null",
    }
}

fn press(keystroke: &str, window: WindowHandle<Workspace>, cx: &mut AsyncApp) -> Result<()> {
    // Parsing wants a static string, as keymaps are normally compiled in. This is a test tool
    // running one short script, so leaking these is fine.
    let leaked: &'static str = Box::leak(keystroke.to_string().into_boxed_str());
    let parsed = Keystroke::parse(leaked).map_err(|e| anyhow!("bad keystroke: {e}"))?;
    with_window(window, cx, |window, cx| {
        window.dispatch_keystroke(parsed, cx);
    })
}

fn type_text(text: &str, window: WindowHandle<Workspace>, cx: &mut AsyncApp) -> Result<()> {
    for c in text.chars() {
        match c {
            '\n' => press("enter", window, cx)?,
            '\t' => press("tab", window, cx)?,
            ' ' => press("space", window, cx)?,
            '-' => press("-", window, cx)?,
            c => press(&c.to_string(), window, cx)?,
        }
    }
    Ok(())
}

fn report(label: &str, mut times: Vec<Duration>) {
    if times.is_empty() {
        println!("{label}: no samples");
        return;
    }
    times.sort();
    let total: Duration = times.iter().sum();
    let at = |p: f32| times[((times.len() - 1) as f32 * p) as usize];
    println!(
        "{label}: {} samples, mean {:?}, median {:?}, p99 {:?}, max {:?}",
        times.len(),
        total / times.len() as u32,
        at(0.5),
        at(0.99),
        times[times.len() - 1],
    );
}

#[cfg(feature = "capture")]
fn screenshot(path: &str, window: WindowHandle<Workspace>, cx: &mut AsyncApp) -> Result<()> {
    let image = with_window(window, cx, |window, _| window.render_to_image())?;
    match image {
        Ok(image) => image.save(path)?,
        // Not every platform's renderer can read frames back.
        Err(error) => println!("screenshot unavailable on this platform: {error:#}"),
    }
    Ok(())
}

#[cfg(not(feature = "capture"))]
fn screenshot(_: &str, _: WindowHandle<Workspace>, _: &mut AsyncApp) -> Result<()> {
    println!("screenshot skipped: build with --features capture");
    Ok(())
}

async fn run_command(line: &str, window: WindowHandle<Workspace>, cx: &mut AsyncApp) -> Result<()> {
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    match command {
        "key" => {
            for keystroke in rest.split_whitespace() {
                let keystroke = match keystroke.split_once('|') {
                    Some((mac, _)) if cfg!(target_os = "macos") => mac,
                    Some((_, other)) => other,
                    None => keystroke,
                };
                press(keystroke, window, cx)?;
            }
        }
        "do" => {
            let (keystroke, _) = gleipnir_editor::keymap(cfg!(target_os = "macos"))
                .find(|(_, action)| *action == rest)
                .with_context(|| format!("no key is bound to {rest}"))?;
            press(keystroke, window, cx)?;
        }
        "type" => type_text(&unescape(rest), window, cx)?,
        "double-shift" => {
            for shift in [true, false, true, false] {
                let modifiers = gpui::Modifiers {
                    shift,
                    ..gpui::Modifiers::none()
                };
                let event = gpui::PlatformInput::ModifiersChanged(gpui::ModifiersChangedEvent {
                    modifiers,
                    capslock: gpui::Capslock { on: false },
                });
                with_window(window, cx, |window, cx| {
                    window.dispatch_event(event, cx);
                })?;
                sleep(Duration::from_millis(30), cx).await;
            }
        }
        "wait-idle" => {
            let started = Instant::now();
            loop {
                let busy = with_workspace(window, cx, |workspace, _, cx| {
                    workspace.project().is_some_and(|p| p.read(cx).is_busy())
                })?;
                if !busy {
                    break;
                }
                if started.elapsed() > Duration::from_secs(60) {
                    bail!("the project was still busy after a minute");
                }
                sleep(Duration::from_millis(20), cx).await;
            }
        }
        "wait" => sleep(Duration::from_millis(rest.parse()?), cx).await,
        "wait-timeout" => WAIT_SECONDS.store(rest.parse()?, Ordering::Relaxed),
        "cursor" => {
            let (line, column) = rest.split_once(':').context("expected line:column")?;
            let (line, column): (usize, usize) = (line.parse()?, column.parse()?);
            with_workspace(window, cx, |workspace, _, cx| {
                if let Some(editor) = workspace.active_editor(cx) {
                    editor.update(cx, |editor, cx| editor.jump_to(line - 1, column - 1, cx));
                }
            })?;
        }
        "wait-parsed" => {
            let started = Instant::now();
            while state(window, cx)?.parse_pending {
                if started.elapsed() > Duration::from_secs(30) {
                    bail!("still parsing after 30 seconds");
                }
                sleep(Duration::from_millis(5), cx).await;
            }
        }
        "open" => {
            let path = PathBuf::from(rest);
            let open = with_workspace(window, cx, |workspace, window, cx| {
                workspace.open_path(path, window, cx)
            })?;
            match open.await {
                Err(e)
                    if e.downcast_ref::<gleipnir_workspace::ShownAsImage>()
                        .is_none() =>
                {
                    return Err(e);
                }
                _ => {}
            }
        }
        "console" => {
            // `console run|debug <text>`: types a line into that panel's console and presses Enter.
            let (panel, text) = rest.split_once(' ').unwrap_or((rest, ""));
            let text = unescape(text);
            let panel = panel.to_string();
            with_workspace(window, cx, move |workspace, _, cx| -> Result<()> {
                let console = match panel.as_str() {
                    "run" => workspace
                        .panel::<gleipnir_debugger_ui::RunPanel>(cx)
                        .map(|p| p.read(cx).console().clone()),
                    "debug" => workspace
                        .panel::<gleipnir_debugger_ui::DebugPanel>(cx)
                        .map(|p| p.read(cx).console().clone()),
                    other => bail!("no console in {other}"),
                };
                let console = console.ok_or_else(|| anyhow!("no {panel} panel"))?;
                console.update(cx, |c, cx| c.enter(&text, cx));
                Ok(())
            })??;
        }
        "select-test" => {
            // `select-test <name>`: chooses a test in the Run panel's results, by its full name.
            // A JavaScript test's titles are written joined by ` › `.
            let name = rest.replace(" › ", &gleipnir_packages::JS_NAME_SEPARATOR.to_string());
            with_workspace(window, cx, move |workspace, _, cx| -> Result<()> {
                let panel = workspace
                    .panel::<gleipnir_debugger_ui::RunPanel>(cx)
                    .ok_or_else(|| anyhow!("no run panel"))?;
                let view = panel.read(cx).tests().clone();
                let node = workspace
                    .panel::<gleipnir_debugger_ui::DebugPanel>(cx)
                    .and_then(|p| {
                        let debugger = p.read(cx).debugger().read(cx);
                        let launch = debugger.launches().iter().rev().find(|l| !l.debug)?;
                        let tests = launch.tests.as_ref()?;
                        tests.nodes().iter().position(|n| n.name == name)
                    })
                    .ok_or_else(|| anyhow!("no test called {name}"))?;
                view.update(cx, |v, cx| v.select(node, cx));
                Ok(())
            })??;
        }
        "action" => {
            let name = rest.to_string();
            with_window(window, cx, move |window, cx| -> Result<()> {
                let action = cx
                    .build_action(&name, None)
                    .map_err(|_| anyhow!("no action called {name}"))?;
                window.dispatch_action(action, cx);
                Ok(())
            })??;
        }
        "expect-state" => {
            let (pointer, expected) = rest.split_once(' ').unwrap_or((rest, ""));
            let state = snapshot(window, cx)?;
            if !state_matches(&state, pointer, &unescape(expected)) {
                bail!("{pointer} is {:?}", state.pointer(pointer));
            }
        }
        "wait-state" => {
            let (pointer, expected) = rest.split_once(' ').unwrap_or((rest, ""));
            let expected = unescape(expected);
            let started = Instant::now();
            loop {
                let state = snapshot(window, cx)?;
                if state_matches(&state, pointer, &expected) {
                    break;
                }
                if started.elapsed() > Duration::from_secs(WAIT_SECONDS.load(Ordering::Relaxed)) {
                    bail!("{pointer} stayed {:?}", state.pointer(pointer));
                }
                sleep(Duration::from_millis(10), cx).await;
                draw(window, cx)?;
            }
        }
        "wait-match" => {
            let (pointer, expected) = rest.split_once(' ').unwrap_or((rest, ""));
            let expected = unescape(expected);
            let contains = |value: &serde_json::Value| match value {
                serde_json::Value::String(s) => s.contains(&expected),
                serde_json::Value::Array(items) => items
                    .iter()
                    .any(|i| i.as_str().is_some_and(|s| s.contains(&expected))),
                // `>N` for a count that should grow past N.
                serde_json::Value::Number(n) => expected
                    .strip_prefix('>')
                    .and_then(|m| m.trim().parse::<f64>().ok())
                    .zip(n.as_f64())
                    .is_some_and(|(min, n)| n > min),
                _ => false,
            };
            let started = Instant::now();
            loop {
                let state = snapshot(window, cx)?;
                if state.pointer(pointer).is_some_and(contains) {
                    break;
                }
                if started.elapsed() > Duration::from_secs(WAIT_SECONDS.load(Ordering::Relaxed)) {
                    bail!(
                        "{pointer} never contained {expected:?}: {:?}",
                        state.pointer(pointer)
                    );
                }
                sleep(Duration::from_millis(10), cx).await;
                draw(window, cx)?;
            }
        }
        "expect-text" => {
            let actual = state(window, cx)?.text;
            let expected = unescape(rest);
            if actual != expected {
                bail!("expected {expected:?}, found {actual:?}");
            }
        }
        "expect-contains" => {
            let actual = state(window, cx)?.text;
            let expected = unescape(rest);
            if !actual.contains(&expected) {
                bail!("{expected:?} not in {actual:?}");
            }
        }
        "expect-cursors" => {
            let actual = state(window, cx)?.selections.len();
            let expected: usize = rest.parse()?;
            if actual != expected {
                bail!("expected {expected} cursors, found {actual}");
            }
        }
        "expect-dirty" => {
            let actual = state(window, cx)?.dirty;
            let expected: bool = rest.parse()?;
            if actual != expected {
                bail!("expected dirty to be {expected}, found {actual}");
            }
        }
        "expect-disk" => {
            let actual = state(window, cx)?.disk_status;
            if actual != rest {
                bail!("expected disk status {rest}, found {actual}");
            }
        }
        "expect-language" => {
            let actual = state(window, cx)?.language;
            if actual.as_deref() != Some(rest) {
                bail!("expected language {rest}, found {actual:?}");
            }
        }
        "expect-highlights" => {
            // Highlights are computed while drawing, so it takes a frame for them to show up.
            let started = Instant::now();
            while state(window, cx)?.highlight_spans == 0 {
                if started.elapsed() > Duration::from_secs(3) {
                    bail!("nothing on screen is highlighted");
                }
                sleep(Duration::from_millis(5), cx).await;
                draw(window, cx)?;
            }
        }
        "expect-exists" => {
            if !Path::new(rest).exists() {
                bail!("{rest} doesn't exist");
            }
        }
        "expect-file" => {
            let (path, expected) = rest.split_once(' ').context("expected a path and text")?;
            let actual = std::fs::read_to_string(path)?;
            let expected = unescape(expected);
            if actual != expected {
                bail!("expected {path} to hold {expected:?}, found {actual:?}");
            }
        }
        "write-file" => {
            let (path, text) = rest.split_once(' ').context("expected a path and text")?;
            if let Some(folder) = Path::new(path).parent() {
                std::fs::create_dir_all(folder)?;
            }
            std::fs::write(path, unescape(text))?;
        }
        "dump" => {
            let json = serde_json::to_string_pretty(&snapshot(window, cx)?)?;
            std::fs::write(rest, json)?;
        }
        "screenshot" => screenshot(rest, window, cx)?,
        "scroll-bench" => {
            let frames: usize = rest.parse()?;
            let editor = window
                .update(cx, |workspace, _, cx| workspace.active_editor(cx))
                .map_err(|e| anyhow!("{e}"))?
                .context("no editor is open")?;
            cx.update(|cx| editor.update(cx, |e, _| e.start_measuring()));
            for _ in 0..frames {
                cx.update(|cx| editor.update(cx, |e, cx| e.scroll_lines(7., cx)));
                draw(window, cx)?;
                sleep(Duration::from_millis(1), cx).await;
            }
            let times = cx.update(|cx| editor.update(cx, |e, _| e.take_frame_times()));
            report("editor layout+paint per frame while scrolling", times);
        }
        "type-bench" => {
            let count: usize = rest.parse()?;
            let mut times = Vec::with_capacity(count);
            for _ in 0..count {
                let started = Instant::now();
                press("x", window, cx)?;
                draw(window, cx)?;
                times.push(started.elapsed());
                sleep(Duration::from_millis(1), cx).await;
            }
            report("keystroke through to a drawn frame", times);
        }
        // `type-timed <ms between keys> <text>`: types like a person, timing each keystroke
        // through to a drawn frame, then how long the completion list took to show.
        "type-timed" => {
            let (pause, text) = rest.split_once(' ').unwrap_or((rest, ""));
            let pause = Duration::from_millis(pause.parse()?);
            let mut times = Vec::new();
            let started = Instant::now();
            let mut shown_after = None;
            for c in unescape(text).chars() {
                let key = Instant::now();
                type_text(&c.to_string(), window, cx)?;
                draw(window, cx)?;
                times.push(key.elapsed());
                sleep(pause, cx).await;
                let state = snapshot(window, cx)?;
                let showing = state
                    .pointer("/editor/completions")
                    .and_then(|c| c.as_array())
                    .is_some_and(|c| !c.is_empty());
                if showing && shown_after.is_none() {
                    shown_after = Some(started.elapsed());
                }
            }
            // Any key slower than a frame, to find what made it so.
            let typed_text: Vec<char> = unescape(text).chars().collect();
            for (index, time) in times.iter().enumerate() {
                if *time > Duration::from_millis(16) {
                    println!(
                        "slow key {index} ({:?}): {time:?}",
                        typed_text.get(index).copied().unwrap_or(' ')
                    );
                }
            }
            report("keystroke through to a drawn frame while typing", times);
            let typed = started.elapsed();
            while shown_after.is_none() && started.elapsed() < typed + Duration::from_secs(10) {
                sleep(Duration::from_millis(5), cx).await;
                draw(window, cx)?;
                let state = snapshot(window, cx)?;
                if state
                    .pointer("/editor/completions")
                    .and_then(|c| c.as_array())
                    .is_some_and(|c| !c.is_empty())
                {
                    shown_after = Some(started.elapsed());
                }
            }
            match shown_after {
                Some(after) => println!(
                    "completions shown {after:?} after the first key, typing took {typed:?}"
                ),
                None => println!("completions never showed"),
            }
        }
        "mcp" => {
            // `mcp <tool> <arguments>`: calls a tool as an agent would, by starting `gleipnir mcp`
            // on the window's socket, and keeps the answer for `expect-mcp`.
            let (tool, arguments) = rest.split_once(' ').unwrap_or((rest, "{}"));
            let arguments: serde_json::Value =
                serde_json::from_str(arguments).context("the arguments aren't JSON")?;
            let socket = with_workspace(window, cx, |workspace, _, cx| {
                gleipnir_agent::socket_path(workspace, cx)
            })?
            .context("the window has no socket for agents")?;
            let tool = tool.to_string();
            // In the background, as the window answers the call meanwhile.
            let answer = cx
                .background_executor()
                .spawn(async move { call_tool(&socket, &tool, arguments) })
                .await?;
            *LAST_TOOL_ANSWER.lock().unwrap() = answer;
        }
        "expect-mcp" => {
            let answer = LAST_TOOL_ANSWER.lock().unwrap().clone();
            let expected = unescape(rest);
            if !answer.contains(&expected) {
                bail!("{expected:?} not in the tool's answer {answer:?}");
            }
        }
        "quit" => std::process::exit(0),
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
