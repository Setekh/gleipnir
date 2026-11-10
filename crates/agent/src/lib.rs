//! Gleipnir's MCP server, which lets coding agents like Claude Code and Codex see and work in the
//! IDE: what's open and selected, the problems, tests and Git status, definitions and usages, and
//! edits made in open files or proposed for review.
//!
//! Each window listens on its own Unix socket, speaking MCP as its stdio transport does: JSON-RPC
//! 2.0, one message per line. An agent starts `gleipnir mcp`, which is only a pipe from its stdin
//! and stdout to the socket named by `GLEIPNIR_SOCKET`. Agents are the person's own installed
//! programs, signed in by them: Gleipnir only offers them tools, and never sees their traffic or
//! credentials.

mod bridge;
mod ide;
pub mod launch;
mod proposals;
pub mod remote;
pub mod rpc;
mod socket;
pub mod tools;

pub use bridge::{main as mcp_main, pipe};
pub use socket::socket_dir;

use futures::StreamExt as _;
use gleipnir_workspace::Workspace;
use gpui::{App, AppContext as _, Context, Entity, EntityId, Window};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What an agent is told about Gleipnir when it connects, to know which tool to use when.
pub const INSTRUCTIONS: &str = "Gleipnir is the IDE this session runs in: an IntelliJ-like editor \
for Rust, Go, Odin, TypeScript and JavaScript, with the person's project open. These tools see and \
act on what they have open in it.

- When the person says \"this\", \"here\", \"the selection\" or \"this file\", call get_selection \
first; get_open_files lists their tabs.
- read_file shows a file as the editor has it, including edits the person hasn't saved, which the \
file on disk doesn't have yet.
- To change a file that's open in Gleipnir (see get_open_files), use edit_file rather than writing \
it on disk: the change lands in the open editor as one step the person can undo, keeping their own \
unsaved edits, and stays unsaved until save_file or the person saves. Writing an open file with \
unsaved edits on disk instead would conflict with them.
- To offer a change for the person to approve first, use propose_edit: they see a diff and accept \
or reject each change. It waits for them a while, then answers \"pending\" with an id for \
get_proposal.
- get_problems has the language servers' errors and warnings; run_tests runs tests with the IDE's \
test runner, and get_test_results has the latest run's failures with their output; get_git_status \
has the branch and changed files; run_configuration lists and runs the project's programs.
- find_definition and find_usages ask the language server, which is more precise than searching \
the text.
- open_file shows the person a place in a file.

Paths are relative to the project root, and only files inside it can be used. Lines and columns \
count from 1; columns count characters.";

/// One window's server: its socket, and the edits it's proposed.
pub struct Agent {
    server: Option<socket::Server>,
    proposals: proposals::Proposals,
}

/// The agent side of every window, by its workspace.
#[derive(Default)]
struct Agents(HashMap<EntityId, Entity<Agent>>);

impl gpui::Global for Agents {}

pub fn add_to_workspace(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    // Tools work on a project's files; a window without a project has none to offer.
    let Some(root) = workspace.root().map(PathBuf::from) else {
        return;
    };
    let (calls_sender, mut calls) = futures::channel::mpsc::unbounded::<socket::Call>();
    let server = match socket::Server::start(&socket_dir(), calls_sender) {
        Ok(server) => Some(server),
        Err(error) => {
            workspace.show_error(format!("Agents can't connect to this window: {error}"), cx);
            None
        }
    };
    let agent = cx.new(|_| Agent {
        server,
        proposals: Default::default(),
    });
    let id = cx.entity_id();
    cx.default_global::<Agents>().0.insert(id, agent.clone());
    // The socket goes with the window, and with the app however it quits.
    let window_id = window.window_handle().window_id();
    cx.on_window_closed(move |cx, closed| {
        if closed == window_id {
            stop(id, cx);
        }
    })
    .detach();
    cx.on_release(move |_, cx| stop(id, cx)).detach();
    cx.on_app_quit(move |_, cx| {
        stop(id, cx);
        async {}
    })
    .detach();

    let agent = agent.downgrade();
    cx.spawn_in(window, async move |workspace, cx| {
        while let Some(call) = calls.next().await {
            let (workspace, agent, root) = (workspace.clone(), agent.clone(), root.clone());
            // Each call runs on its own, so a slow one, like waiting for a review, holds up no
            // other.
            cx.spawn(async move |cx| {
                let outcome =
                    ide::run(&workspace, &agent, &root, &call.name, &call.arguments, cx).await;
                call.answer(outcome);
            })
            .detach();
        }
    })
    .detach();
}

fn stop(id: EntityId, cx: &mut App) {
    let agent = cx
        .try_global::<Agents>()
        .and_then(|agents| agents.0.get(&id).cloned());
    if let Some(agent) = agent {
        cx.global_mut::<Agents>().0.remove(&id);
        agent.update(cx, |agent, _| agent.server.take());
    }
}

/// The socket a window's agents connect to, to give them in `GLEIPNIR_SOCKET`. `None` for a window
/// without a project, or if listening failed.
pub fn socket_path(workspace: &Workspace, cx: &App) -> Option<PathBuf> {
    let id = workspace.weak_handle().entity_id();
    let agent = cx.try_global::<Agents>()?.0.get(&id)?;
    Some(agent.read(cx).server.as_ref()?.path().to_path_buf())
}

/// The MCP configuration that starts `gleipnir mcp` for a window's socket, as Claude Code's
/// `--mcp-config` takes it.
pub fn mcp_config_json(gleipnir_binary: &Path, socket: &Path) -> Value {
    json!({
        "mcpServers": {
            "gleipnir": {
                "command": gleipnir_binary.to_string_lossy(),
                "args": ["mcp"],
                "env": { "GLEIPNIR_SOCKET": socket.to_string_lossy() },
            }
        }
    })
}

#[cfg(test)]
mod tests;
