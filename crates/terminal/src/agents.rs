//! Coding agents, run in terminal tabs: each the person's own installed command-line tool,
//! started in the project folder and signed in however it signs in, with nothing of Gleipnir's
//! between it and its service. Gleipnir only finds the tool, and lets other parts of the IDE add
//! to how it starts, as the MCP server that gives an agent the IDE's tools does.

use crate::session::Launch;
use gleipnir_workspace::Workspace;
use gpui::{App, Global};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

/// An agent Gleipnir knows how to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agent {
    /// Stable, for settings and the MCP server's setup to refer to.
    pub id: &'static str,
    pub name: &'static str,
    /// The command it's run as, looked for on the login shell's `PATH`.
    pub command: &'static str,
    /// How its makers say to install it, shown when it isn't there. Gleipnir doesn't run it.
    pub install: &'static str,
}

pub const CLAUDE_CODE: Agent = Agent {
    id: "claude-code",
    name: "Claude Code",
    command: "claude",
    install: "curl -fsSL https://claude.ai/install.sh | bash",
};

pub const CODEX: Agent = Agent {
    id: "codex",
    name: "Codex",
    command: "codex",
    install: "npm install -g @openai/codex",
};

pub const AGENTS: [Agent; 2] = [CLAUDE_CODE, CODEX];

/// What another part of the IDE adds when an agent starts: arguments, and environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSetup {
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

type SetupFn = Rc<dyn Fn(&Agent, &Workspace, &App) -> AgentSetup>;

/// The ways the IDE adds to agents' starts, in the order they were added.
#[derive(Default, Clone)]
pub struct AgentSetups(Vec<SetupFn>);

impl Global for AgentSetups {}

/// Lets a part of the IDE add arguments and environment to every agent started from now on.
pub fn add_agent_setup(
    cx: &mut App,
    setup: impl Fn(&Agent, &Workspace, &App) -> AgentSetup + 'static,
) {
    cx.default_global::<AgentSetups>().0.push(Rc::new(setup));
}

/// How to start an agent in a workspace: through the person's own shell, as an interactive login
/// shell, so it's started as typing its name would start it, with their `PATH`, and through any
/// function or alias they've wrapped it in, such as one choosing an account per folder. Where it
/// isn't installed, the tab says how to install it instead.
pub fn agent_launch(agent: &Agent, workspace: &Workspace, cx: &App) -> Launch {
    let user = gleipnir_packages::env::user_env();
    let cwd = workspace
        .root()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| user.get("HOME").unwrap_or("/").into());
    let mut args = Vec::new();
    let mut env: HashMap<String, String> = HashMap::new();
    if let Some(setups) = cx.try_global::<AgentSetups>() {
        for setup in &setups.0 {
            let added = setup(agent, workspace, cx);
            args.extend(added.args);
            env.extend(added.env);
        }
    }
    let shell = user.get("SHELL").unwrap_or("/bin/zsh").to_string();
    launch_in_shell(agent, &shell, cwd, args, env, |command| user.which(command))
}

/// An agent's start through `shell`, with `args` after its own name and `env` added.
fn launch_in_shell(
    agent: &Agent,
    shell: &str,
    cwd: std::path::PathBuf,
    args: Vec<String>,
    mut env: HashMap<String, String>,
    which: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> Launch {
    env.insert("GLEIPNIR_AGENT_NOTE".into(), missing_note(agent));
    let posix = matches!(
        Path::new(shell).file_name().and_then(|n| n.to_str()),
        Some("zsh" | "bash" | "sh" | "dash" | "ksh")
    );
    let program = if posix {
        // `$0` is the command, and `$@` what the IDE adds, passed through untouched. Gleipnir
        // itself may have been started from an agent's session, whose markers would make this
        // one think it's that session's child.
        let script = "unset CLAUDECODE CLAUDE_CODE_ENTRYPOINT CLAUDE_CODE_CHILD_SESSION \
                      CLAUDE_CODE_SSE_PORT; \
                      if command -v \"$0\" >/dev/null 2>&1; then \"$0\" \"$@\"; \
                      else printf '%s' \"$GLEIPNIR_AGENT_NOTE\"; fi";
        let mut shell_args = vec![
            "-l".to_string(),
            "-i".to_string(),
            "-c".to_string(),
            script.to_string(),
            agent.command.to_string(),
        ];
        shell_args.extend(args);
        (shell.to_string(), shell_args)
    } else {
        // A shell that isn't sh-like, as fish: the tool itself, found on its PATH.
        match which(agent.command) {
            Some(binary) => (binary.to_string_lossy().into_owned(), args),
            None => (
                "/bin/sh".into(),
                vec!["-c".into(), "printf '%s' \"$GLEIPNIR_AGENT_NOTE\"".into()],
            ),
        }
    };
    Launch {
        program: Some(program),
        cwd,
        env,
        title: agent.name.into(),
    }
}

/// What the tab says when the agent isn't installed.
fn missing_note(agent: &Agent) -> String {
    format!(
        "{name} isn't installed, or isn't on your shell's PATH.\r\n\r\n\
         To install it, as its makers say, run this in a terminal tab (Alt+F12):\r\n\r\n    \
         {install}\r\n\r\n\
         Then open {name} again. It signs in on its own the first time.\r\n",
        name = agent.name,
        install = agent.install,
    )
}

#[cfg(test)]
pub(crate) fn test_launch(agent: &Agent, args: &[&str]) -> Launch {
    launch_in_shell(
        agent,
        "/bin/bash",
        std::env::temp_dir(),
        args.iter().map(|a| a.to_string()).collect(),
        HashMap::new(),
        |_| None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_says_how_to_install_without_running_it() {
        let note = missing_note(&CLAUDE_CODE);
        assert!(note.contains("curl -fsSL https://claude.ai/install.sh | bash"));
        assert!(note.contains("Claude Code isn't installed"));
    }
}
