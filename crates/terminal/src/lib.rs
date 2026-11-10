//! The terminal: IntelliJ's Terminal tool window, with tabs of shells, and of programs started in
//! one, like a coding agent. Programs run on a pseudo-terminal, and alacritty_terminal keeps what
//! they draw.

mod agents;
mod colors;
mod keys;
mod mouse;
mod panel;
mod session;
mod view;

pub use agents::{AGENTS, Agent, AgentSetup, CLAUDE_CODE, CODEX, add_agent_setup, agent_launch};
pub use keys::{paste_bytes, to_bytes};
pub use panel::{AGENTS_PANEL, AgentsPanel, PanelKind, TERMINAL_PANEL, TerminalPanel};
pub use session::{Launch, Session, SessionEvent};
pub use view::{Clear, Copy, OpenLocation, Paste, ScrollPageDown, ScrollPageUp, TerminalView};

use gleipnir_workspace::Workspace;
use gpui::{App, AppContext as _, Context, KeyBinding, NoAction, Window, actions};

actions!(
    terminal,
    [
        /// Shows the terminal, starting one if there's none, or hides it.
        ToggleTerminal,
        /// Shows the Agents tool window, starting Claude Code if there's no agent, or hides it.
        ToggleAgents,
        /// Opens another terminal tab.
        NewTerminal,
        /// Closes the terminal tab in front.
        CloseTerminal,
        /// Shows Claude Code's tab, starting it in the project folder if it isn't running.
        OpenClaudeCode,
        /// Shows Codex's tab, starting it in the project folder if it isn't running.
        OpenCodex,
        /// Types a reference to the editor's file and selected lines into the agent's tab, as
        /// `@src/main.rs#L10-20`, for the next thing asked of it.
        SendToAgent,
    ]
);

/// Keys that are the program's in a terminal, though the IDE has them elsewhere: Ctrl with a
/// letter, Tab, Escape, the arrows and the like.
fn shell_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = vec![
        "tab",
        "shift-tab",
        "escape",
        "enter",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "pageup",
        "pagedown",
        "backspace",
        "delete",
        "alt-left",
        "alt-right",
        "alt-backspace",
        "ctrl-left",
        "ctrl-right",
        "ctrl-space",
    ];
    // Bound once, at startup, so these live as long as the app.
    let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    for letter in 'a'..='z' {
        keys.push(leak(format!("ctrl-{letter}")));
        if cfg!(not(target_os = "macos")) {
            keys.push(leak(format!("alt-{letter}")));
        }
    }
    keys
}

pub fn bind_keys(cx: &mut App) {
    let terminal = Some("Terminal");
    let mut bindings: Vec<KeyBinding> = shell_keys()
        .into_iter()
        .map(|keys| KeyBinding::new(keys, NoAction, terminal))
        .collect();
    bindings.extend([
        KeyBinding::new("alt-f12", ToggleTerminal, None),
        KeyBinding::new("shift-pageup", ScrollPageUp, terminal),
        KeyBinding::new("shift-pagedown", ScrollPageDown, terminal),
    ]);
    #[cfg(target_os = "macos")]
    bindings.extend([
        KeyBinding::new("cmd-c", Copy, terminal),
        KeyBinding::new("cmd-v", Paste, terminal),
        KeyBinding::new("cmd-k", Clear, terminal),
        KeyBinding::new("cmd-t", NewTerminal, terminal),
        KeyBinding::new("cmd-w", CloseTerminal, terminal),
        // As Claude Code's own IntelliJ plugin opens it, and sends it a file reference.
        KeyBinding::new("cmd-escape", OpenClaudeCode, None),
        KeyBinding::new("cmd-alt-k", SendToAgent, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    bindings.extend([
        KeyBinding::new("ctrl-shift-c", Copy, terminal),
        KeyBinding::new("ctrl-shift-v", Paste, terminal),
        KeyBinding::new("ctrl-shift-t", NewTerminal, terminal),
        KeyBinding::new("ctrl-shift-w", CloseTerminal, terminal),
        KeyBinding::new("ctrl-escape", OpenClaudeCode, None),
        KeyBinding::new("ctrl-alt-k", SendToAgent, None),
    ]);
    cx.bind_keys(bindings);
}

pub fn add_to_workspace(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    let panel = cx.new(|cx| TerminalPanel::new(workspace, PanelKind::Terminal, cx));
    workspace.add_panel(panel, cx);
    let agents = cx.new(|cx| AgentsPanel::new(workspace, cx));
    workspace.add_panel(agents, cx);
    workspace.register_action(|workspace, _: &ToggleAgents, window, cx| {
        workspace.toggle_panel(AGENTS_PANEL, window, cx);
    });
    workspace.register_action(|workspace, _: &ToggleTerminal, window, cx| {
        workspace.toggle_panel(TERMINAL_PANEL, window, cx);
    });
    workspace.register_action(|workspace, _: &NewTerminal, window, cx| {
        workspace.show_panel(TERMINAL_PANEL, cx);
        if let Some(panel) = workspace.panel::<TerminalPanel>(cx) {
            panel.update(cx, |panel, cx| panel.new_shell(window, cx));
        }
    });
    workspace.register_action(|workspace, _: &OpenClaudeCode, window, cx| {
        open_agent(workspace, &CLAUDE_CODE, window, cx)
    });
    workspace.register_action(|workspace, _: &OpenCodex, window, cx| {
        open_agent(workspace, &CODEX, window, cx)
    });
    workspace.register_action(|workspace, _: &SendToAgent, window, cx| {
        let Some(reference) = editor_reference(workspace, cx) else {
            workspace.show_message("Send to Agent needs a file open in the editor", cx);
            return;
        };
        workspace.show_panel(AGENTS_PANEL, cx);
        let launch = agent_launch(&CLAUDE_CODE, workspace, cx);
        if let Some(panel) = agents_tabs(workspace, cx) {
            panel.update(cx, |panel, cx| {
                panel.send_to_agent(&reference, launch, window, cx)
            });
        }
    });
    workspace.register_action(|workspace, _: &CloseTerminal, window, cx| {
        if let Some(panel) = workspace.panel::<TerminalPanel>(cx) {
            panel.update(cx, |panel, cx| panel.close_active(window, cx));
        }
    });
}

/// The active editor's file, relative to the project, with its selected lines: `@path#L3-7`, or
/// `@path#L3` for one line, or `@path` with nothing selected.
fn editor_reference(workspace: &Workspace, cx: &App) -> Option<String> {
    let editor = workspace.active_editor(cx)?;
    let editor = editor.read(cx);
    let model = editor.buffer().read(cx);
    let path = model.path()?;
    let shown = workspace
        .root()
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path);
    let selection = editor.selections().primary();
    let mut reference = format!("@{}", shown.display());
    if !selection.is_empty() {
        let range = selection.range();
        let buffer = model.buffer();
        let start = buffer.offset_to_point(range.start);
        let end = buffer.offset_to_point(range.end);
        reference.push_str(&lines_suffix(start.line, end.line, end.col));
    }
    Some(reference)
}

/// `#L3` or `#L3-7` for a selection from line `start` to `end` (from 0) that ends at column
/// `end_col`; one ending at the start of a line doesn't take that line in.
fn lines_suffix(start: usize, end: usize, end_col: usize) -> String {
    let first = start + 1;
    let last = if end_col == 0 && end > start {
        end
    } else {
        end + 1
    };
    if first == last {
        format!("#L{first}")
    } else {
        format!("#L{first}-{last}")
    }
}

/// The Agents tool window's tabs.
fn agents_tabs(workspace: &Workspace, cx: &App) -> Option<gpui::Entity<TerminalPanel>> {
    Some(workspace.panel::<AgentsPanel>(cx)?.read(cx).tabs().clone())
}

fn open_agent(
    workspace: &mut Workspace,
    agent: &Agent,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.show_panel(AGENTS_PANEL, cx);
    let launch = agent_launch(agent, workspace, cx);
    if let Some(panel) = agents_tabs(workspace, cx) {
        panel.update(cx, |panel, cx| {
            panel.open_agent(agent, Some(launch), window, cx)
        });
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reference_tests {
    #[test]
    fn selected_lines_are_referenced_as_claude_code_writes_them() {
        assert_eq!(super::lines_suffix(2, 2, 5), "#L3");
        assert_eq!(super::lines_suffix(2, 6, 3), "#L3-7");
        // Whole lines selected end at the start of the next.
        assert_eq!(super::lines_suffix(2, 7, 0), "#L3-7");
    }
}
