//! The package manifest format. Unknown fields are errors rather than being ignored, so a typo in a
//! manifest is reported instead of silently doing nothing.

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub package: PackageInfo,
    #[serde(default, rename = "language")]
    pub languages: Vec<LanguageSpec>,
    pub server: Option<ServerSpec>,
    pub debugger: Option<DebuggerSpec>,
    #[serde(default, rename = "tool")]
    pub tools: BTreeMap<String, ToolSpec>,
    pub sdk: Option<SdkSpec>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PackageInfo {
    pub id: String,
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LanguageSpec {
    pub name: String,
    /// One of the grammars compiled in: rust, go, odin, typescript, tsx, javascript.
    pub grammar: String,
    pub extensions: Vec<String>,
    pub line_comment: Option<String>,
    #[serde(default = "default_tab_size")]
    pub tab_size: usize,
    #[serde(default)]
    pub hard_tabs: bool,
    /// Characters that auto-close, each written as the opening and closing character together.
    #[serde(default)]
    pub pairs: Vec<String>,
}

fn default_tab_size() -> usize {
    4
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    pub tool: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Tcp,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DebuggerSpec {
    pub tool: String,
    /// `{port}` is replaced by a free port when the transport is TCP.
    #[serde(default)]
    pub args: Vec<String>,
    pub transport: Transport,
    /// `sdk` runs the tool with the SDK's binary, for adapters that are scripts, like js-debug.
    pub runtime: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// The executable's name, as found on `PATH`.
    pub binary: String,
    /// Where a project may have its own copy, relative to its root. Checked first.
    #[serde(default)]
    pub project_paths: Vec<String>,
    /// Where the SDK may have it, relative to the SDK's root.
    #[serde(default)]
    pub sdk_paths: Vec<String>,
    /// Fixed places the system puts it, like inside Xcode.
    #[serde(default)]
    pub system_paths: Vec<String>,
    pub install: Option<InstallSpec>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum InstallSpec {
    /// `go install module@version`, using the project's Go. Go's checksum database verifies it.
    GoInstall {
        module: String,
        version: String,
    },
    /// npm packages at exact versions. npm verifies them against the registry's integrity hashes.
    Npm {
        packages: Vec<String>,
    },
    /// A release archive, checked against a pinned sha256 before anything is unpacked.
    Download {
        version: String,
        binary: String,
        assets: Vec<Asset>,
    },
    RustupComponent {
        component: String,
    },
    /// Can't be installed by us; the hint says how.
    Manual {
        hint: String,
    },
}

impl InstallSpec {
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::GoInstall { version, .. } | Self::Download { version, .. } => Some(version),
            Self::Npm { packages } => packages
                .first()
                .and_then(|p| p.rsplit_once('@'))
                .map(|(_, v)| v),
            Self::RustupComponent { .. } | Self::Manual { .. } => None,
        }
    }

    pub fn can_install(&self) -> bool {
        !matches!(self, Self::Manual { .. })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    /// `macos-aarch64`, `linux-x86_64` and so on, or `any`.
    pub platform: String,
    pub url: String,
    pub sha256: String,
    pub format: ArchiveFormat,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum ArchiveFormat {
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "tar.gz")]
    TarGz,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SdkSpec {
    pub kind: String,
    pub name: String,
    /// The SDK's main executable, relative to its root. How a folder is recognized as one.
    pub binary: String,
    #[serde(default)]
    pub version_args: Vec<String>,
    /// Where to look, in order: `env:VAR`, `path:binary`, `dir:path` or `glob:path/*`.
    #[serde(default)]
    pub detect: Vec<String>,
    /// Environment for processes using this SDK. `{sdk}` is its root, `{PATH}` the existing PATH.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

pub const GRAMMARS: &[&str] = &["rust", "go", "odin", "typescript", "tsx", "javascript"];

/// Parses a manifest and checks that it makes sense, naming the problem if it doesn't.
pub fn parse_manifest(source: &str, origin: &str) -> Result<Manifest> {
    let manifest: Manifest = toml::from_str(source)
        .with_context(|| format!("{origin} is not a valid package manifest"))?;
    validate(&manifest).with_context(|| format!("in {origin}"))?;
    Ok(manifest)
}

fn validate(manifest: &Manifest) -> Result<()> {
    if manifest.package.id.is_empty() {
        bail!("the package needs an id");
    }
    for language in &manifest.languages {
        if !GRAMMARS.contains(&language.grammar.as_str()) {
            bail!(
                "language {} uses an unknown grammar {:?}",
                language.name,
                language.grammar
            );
        }
        if language.extensions.is_empty() {
            bail!("language {} has no file extensions", language.name);
        }
        for pair in &language.pairs {
            if pair.chars().count() != 2 {
                bail!(
                    "in language {}, {pair:?} isn't an opening and closing character",
                    language.name
                );
            }
        }
    }
    let tool_refs = manifest
        .server
        .iter()
        .map(|s| &s.tool)
        .chain(manifest.debugger.iter().map(|d| &d.tool));
    for tool in tool_refs {
        if !manifest.tools.contains_key(tool) {
            bail!("it refers to a tool {tool:?} that isn't described under [tool]");
        }
    }
    if let Some(runtime) = manifest
        .debugger
        .as_ref()
        .and_then(|d| d.runtime.as_deref())
    {
        if runtime != "sdk" {
            bail!("the debugger's runtime can only be \"sdk\"");
        }
        if manifest.sdk.is_none() {
            bail!("the debugger runs with the SDK, but there's no [sdk]");
        }
    }
    for (name, tool) in &manifest.tools {
        if let Some(InstallSpec::Download { assets, .. }) = &tool.install {
            for asset in assets {
                if asset.sha256.len() != 64 || !asset.sha256.chars().all(|c| c.is_ascii_hexdigit())
                {
                    bail!("tool {name} has a download without a valid sha256");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [package]
        id = "x"
        name = "X"
        version = "1"
    "#;

    #[test]
    fn a_minimal_manifest_parses() {
        let manifest = parse_manifest(MINIMAL, "x").unwrap();
        assert_eq!(manifest.package.id, "x");
        assert!(manifest.languages.is_empty());
    }

    fn error(source: &str) -> String {
        format!("{:#}", parse_manifest(source, "test.toml").unwrap_err())
    }

    #[test]
    fn a_misspelt_field_is_an_error_not_ignored() {
        let message = error(&format!(
            "{MINIMAL}\n[[language]]\nname = \"L\"\ngrammar = \"rust\"\nextensions = [\"l\"]\ntab_sise = 2\n"
        ));
        assert!(message.contains("tab_sise"), "{message}");
        assert!(message.contains("test.toml"), "{message}");
    }

    #[test]
    fn an_unknown_grammar_is_named() {
        let message = error(&format!(
            "{MINIMAL}\n[[language]]\nname = \"L\"\ngrammar = \"cobol\"\nextensions = [\"cbl\"]\n"
        ));
        assert!(message.contains("cobol"), "{message}");
    }

    #[test]
    fn a_server_must_refer_to_a_described_tool() {
        let message = error(&format!("{MINIMAL}\n[server]\ntool = \"nope\"\n"));
        assert!(message.contains("nope"), "{message}");
    }

    #[test]
    fn a_download_needs_a_real_checksum() {
        let message = error(&format!(
            "{MINIMAL}\n[tool.t]\nbinary = \"t\"\n[tool.t.install]\nkind = \"download\"\nversion = \"1\"\nbinary = \"t\"\nassets = [{{ platform = \"any\", format = \"zip\", url = \"https://x\", sha256 = \"abc\" }}]\n"
        ));
        assert!(message.contains("sha256"), "{message}");
    }

    #[test]
    fn pairs_must_be_two_characters() {
        let message = error(&format!(
            "{MINIMAL}\n[[language]]\nname = \"L\"\ngrammar = \"rust\"\nextensions = [\"l\"]\npairs = [\"(\"]\n"
        ));
        assert!(message.contains("opening and closing"), "{message}");
    }

    #[test]
    fn install_versions_are_read_from_each_kind() {
        let npm = InstallSpec::Npm {
            packages: vec!["a@1.2.3".into()],
        };
        assert_eq!(npm.version(), Some("1.2.3"));
        let manual = InstallSpec::Manual { hint: "h".into() };
        assert_eq!(manual.version(), None);
        assert!(!manual.can_install());
    }
}
