//! How agents started in Gleipnir's terminal get its tools: the window's MCP server, as each
//! agent takes one, and guidance on when to use them: for Claude Code a skill, in a plugin loaded
//! for that session only, with a line saying where it's running; for Codex the same text as that
//! session's developer instructions. Nothing is written to the person's own agent configuration.

use crate::{mcp_config_json, socket_path};
use gleipnir_terminal::{Agent, AgentSetup, CLAUDE_CODE, CODEX};
use gleipnir_workspace::Workspace;
use gpui::App;
use std::path::{Path, PathBuf};

const SKILL: &str = include_str!("../assets/SKILL.md");

/// Told to Claude Code at the start, so it knows to reach for the skill.
const CLAUDE_NOTE: &str = "This Claude Code session runs in a terminal tab of Gleipnir, the IDE \
the person is using. The gleipnir MCP tools see and change what they have open in it, and the \
gleipnir skill says when to use them: read it before working on their code.";

/// The skill without its front matter: the guidance itself.
fn guidance() -> &'static str {
    SKILL
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(SKILL, |(_, body)| body.trim_start())
}

/// Codex's own developer instructions, if the person has set some in its config, which a
/// session's override would otherwise replace.
fn codex_developer_instructions() -> Option<String> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))?;
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let config: toml::Table = text.parse().ok()?;
    config
        .get("developer_instructions")?
        .as_str()
        .map(str::to_string)
}

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
    setup_for(
        agent,
        &binary,
        &socket,
        plugin_dir().as_deref(),
        codex_developer_instructions().as_deref(),
    )
}

/// The arguments that give `agent` the server at `socket`, started as `binary mcp`, and the
/// guidance: for Claude Code the plugin at `plugin`; for Codex its developer instructions, after
/// any the person has of their own.
pub fn setup_for(
    agent: &Agent,
    binary: &Path,
    socket: &Path,
    plugin: Option<&Path>,
    codex_own: Option<&str>,
) -> AgentSetup {
    let mut args = Vec::new();
    if agent.id == CLAUDE_CODE.id {
        args.push("--mcp-config".into());
        args.push(mcp_config_json(binary, socket).to_string());
        if let Some(plugin) = plugin {
            args.push("--plugin-dir".into());
            args.push(plugin.to_string_lossy().into_owned());
        }
        args.push("--append-system-prompt".into());
        args.push(CLAUDE_NOTE.into());
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
        let instructions = match codex_own {
            Some(own) => format!("{own}\n\n{}", guidance()),
            None => guidance().to_string(),
        };
        args.extend([
            "-c".into(),
            format!(
                "developer_instructions={}",
                serde_json::Value::from(instructions)
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
            None,
        );
        assert_eq!(setup.args[0], "--mcp-config");
        let config: serde_json::Value = serde_json::from_str(&setup.args[1]).unwrap();
        let server = &config["mcpServers"]["gleipnir"];
        assert_eq!(server["args"][0], "mcp");
        assert_eq!(
            server["env"]["GLEIPNIR_SOCKET"],
            "/tmp/gleipnir-501/1-1.sock"
        );
        assert_eq!(setup.args[2..4], ["--plugin-dir", "/state/claude-plugin"]);
        assert_eq!(setup.args[4], "--append-system-prompt");
        assert!(setup.args[5].contains("gleipnir skill"));
    }

    #[test]
    fn codex_gets_the_server_as_config_overrides() {
        let setup = setup_for(
            &CODEX,
            Path::new("/opt/My Apps/gleipnir"),
            Path::new("/tmp/gleipnir-501/1-1.sock"),
            None,
            None,
        );
        assert_eq!(
            setup.args[..6],
            [
                "-c",
                "mcp_servers.gleipnir.command=\"/opt/My Apps/gleipnir\"",
                "-c",
                "mcp_servers.gleipnir.args=[\"mcp\"]",
                "-c",
                "mcp_servers.gleipnir.env={GLEIPNIR_SOCKET=\"/tmp/gleipnir-501/1-1.sock\"}",
            ]
        );
        // The guidance as developer instructions, written so Codex's TOML reads it back whole.
        assert_eq!(setup.args[6], "-c");
        let (key, value) = setup.args[7].split_once('=').unwrap();
        assert_eq!(key, "developer_instructions");
        let parsed: toml::Table = format!("v = {value}").parse().unwrap();
        let text = parsed["v"].as_str().unwrap();
        assert!(text.starts_with("# Working in Gleipnir"), "{text}");
        assert!(text.contains("propose_edit"));

        // After the person's own, which the override would otherwise replace.
        let setup = setup_for(
            &CODEX,
            Path::new("/g"),
            Path::new("/s"),
            None,
            Some("Be terse."),
        );
        let (_, value) = setup.args[7].split_once('=').unwrap();
        let parsed: toml::Table = format!("v = {value}").parse().unwrap();
        assert!(
            parsed["v"]
                .as_str()
                .unwrap()
                .starts_with("Be terse.\n\n# Working in Gleipnir")
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
