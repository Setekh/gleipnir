//! How agents started in Gleipnir's terminal get its tools: the window's MCP server, as each
//! agent takes one, and for Claude Code a skill saying when to use them, in a plugin loaded for
//! that session only. Nothing is written to the person's own agent configuration.

use crate::{mcp_config_json, socket_path};
use gleipnir_terminal::{Agent, AgentSetup, CLAUDE_CODE, CODEX};
use gleipnir_workspace::Workspace;
use gpui::App;
use std::path::{Path, PathBuf};

const SKILL: &str = include_str!("../assets/SKILL.md");

const PLUGIN: &str = r#"{
  "name": "gleipnir",
  "description": "Working in the Gleipnir IDE: its tools, and when to use them.",
  "version": "0.1.0"
}
"#;

/// Gives agents started from now on the window's tools.
pub fn init(cx: &mut App) {
    gleipnir_terminal::add_agent_setup(cx, setup);
}

fn setup(agent: &Agent, workspace: &Workspace, cx: &App) -> AgentSetup {
    let (Some(socket), Ok(binary)) = (socket_path(workspace, cx), std::env::current_exe()) else {
        return AgentSetup::default();
    };
    setup_for(agent, &binary, &socket, plugin_dir().as_deref())
}

/// The arguments that give `agent` the server at `socket`, started as `binary mcp`.
pub fn setup_for(agent: &Agent, binary: &Path, socket: &Path, plugin: Option<&Path>) -> AgentSetup {
    let mut args = Vec::new();
    if agent.id == CLAUDE_CODE.id {
        args.push("--mcp-config".into());
        args.push(mcp_config_json(binary, socket).to_string());
        if let Some(plugin) = plugin {
            args.push("--plugin-dir".into());
            args.push(plugin.to_string_lossy().into_owned());
        }
    } else if agent.id == CODEX.id {
        // Codex reads its servers from config.toml; `-c` sets them for this run only. Values
        // are TOML, whose basic strings are written as JSON writes them.
        let quoted = |path: &Path| serde_json::Value::from(path.to_string_lossy()).to_string();
        args.extend([
            "-c".into(),
            format!("mcp_servers.gleipnir.command={}", quoted(binary)),
            "-c".into(),
            "mcp_servers.gleipnir.args=[\"mcp\"]".into(),
            "-c".into(),
            format!(
                "mcp_servers.gleipnir.env={{GLEIPNIR_SOCKET={}}}",
                quoted(socket)
            ),
        ]);
    }
    AgentSetup {
        args,
        env: Default::default(),
    }
}

/// The plugin holding the skill, written into Gleipnir's state folder, kept current with this
/// version. None if it can't be written.
fn plugin_dir() -> Option<PathBuf> {
    let dir = gleipnir_packages::state_dir().join("claude-plugin");
    write_plugin(&dir).ok()?;
    Some(dir)
}

pub fn write_plugin(dir: &Path) -> std::io::Result<()> {
    for (path, text) in [
        (".claude-plugin/plugin.json", PLUGIN),
        ("skills/gleipnir/SKILL.md", SKILL),
    ] {
        let path = dir.join(path);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            std::fs::create_dir_all(path.parent().expect("a folder"))?;
            std::fs::write(&path, text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_code_gets_the_server_and_the_skill_for_its_session() {
        let setup = setup_for(
            &CLAUDE_CODE,
            Path::new("/Applications/Gleipnir.app/gleipnir"),
            Path::new("/tmp/gleipnir-501/1-1.sock"),
            Some(Path::new("/state/claude-plugin")),
        );
        assert_eq!(setup.args[0], "--mcp-config");
        let config: serde_json::Value = serde_json::from_str(&setup.args[1]).unwrap();
        let server = &config["mcpServers"]["gleipnir"];
        assert_eq!(server["args"][0], "mcp");
        assert_eq!(
            server["env"]["GLEIPNIR_SOCKET"],
            "/tmp/gleipnir-501/1-1.sock"
        );
        assert_eq!(setup.args[2..], ["--plugin-dir", "/state/claude-plugin"]);
    }

    #[test]
    fn codex_gets_the_server_as_config_overrides() {
        let setup = setup_for(
            &CODEX,
            Path::new("/opt/My Apps/gleipnir"),
            Path::new("/tmp/gleipnir-501/1-1.sock"),
            None,
        );
        assert_eq!(
            setup.args,
            [
                "-c",
                "mcp_servers.gleipnir.command=\"/opt/My Apps/gleipnir\"",
                "-c",
                "mcp_servers.gleipnir.args=[\"mcp\"]",
                "-c",
                "mcp_servers.gleipnir.env={GLEIPNIR_SOCKET=\"/tmp/gleipnir-501/1-1.sock\"}",
            ]
        );
    }

    #[test]
    fn the_plugin_has_the_skill_and_is_rewritten_only_when_it_differs() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path()).unwrap();
        let skill = std::fs::read_to_string(dir.path().join("skills/gleipnir/SKILL.md")).unwrap();
        assert!(skill.starts_with("---\nname: gleipnir\ndescription: "));
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["name"], "gleipnir");
        let written = std::fs::metadata(dir.path().join("skills/gleipnir/SKILL.md"))
            .unwrap()
            .modified()
            .unwrap();
        write_plugin(dir.path()).unwrap();
        let again = std::fs::metadata(dir.path().join("skills/gleipnir/SKILL.md"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(written, again);
    }
}
