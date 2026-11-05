//! Runs a script against the real application: real window, real rendering, real key dispatch.
//! This is how end-to-end behaviour gets checked, on any machine with a display.
//!
//! Set `GLEIPNIR_SCRIPT` to a file with one command per line. Blank lines and `#` comments are
//! ignored. The process exits 0 if every command succeeded, and 1 at the first failure.
//!
//!     key <keystrokes>            one or more space-separated keystrokes, like: key cmd-s down
//!     do <EditorAction>           presses the key this platform binds to an editor action
//!     action <namespace::Name>    dispatches an action to whatever has focus
//!     expect-state <pointer> <v>  checks a value in the state, by JSON pointer; see `dump`
//!     wait-state <pointer> <v>    waits up to ten seconds for a value, for anything asynchronous
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
//!     quit

use anyhow::{Context as _, Result, anyhow, bail};
use gleipnir_editor::EditorState;
use gleipnir_workspace::Workspace;
use gpui::{App, AppContext as _, AsyncApp, Keystroke, Window, WindowHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
        "wait" => sleep(Duration::from_millis(rest.parse()?), cx).await,
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
            open.await?;
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
                if started.elapsed() > Duration::from_secs(10) {
                    bail!("{pointer} stayed {:?}", state.pointer(pointer));
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
        "quit" => std::process::exit(0),
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
