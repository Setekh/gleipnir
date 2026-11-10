//! Finding a tool, and saying where it came from.

use crate::env::{Env, is_executable};
use crate::manifest::{InstallSpec, ToolSpec};
use crate::sdk::Sdk;
use std::path::{Path, PathBuf};

/// Where a tool was found, in the order they're tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum ToolSource {
    /// A path set in the settings, which always wins.
    Explicit,
    /// The project's own copy, like `node_modules/.bin`.
    Project,
    /// Inside the project's SDK.
    Sdk,
    /// Installed by Gleipnir.
    Managed,
    /// A fixed system location, like inside Xcode.
    System,
    /// Found on `PATH`.
    Path,
}

impl ToolSource {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Explicit => "set in settings",
            Self::Project => "the project's own copy",
            Self::Sdk => "from the SDK",
            Self::Managed => "installed by Gleipnir",
            Self::System => "from the system",
            Self::Path => "found on PATH",
        }
    }
}

/// What the resolver found, and everywhere it looked, so the UI can explain a missing tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub tool: String,
    pub found: Option<(PathBuf, ToolSource)>,
    pub looked_in: Vec<(ToolSource, PathBuf)>,
}

impl Resolution {
    pub fn path(&self) -> Option<&Path> {
        self.found.as_ref().map(|(p, _)| p.as_path())
    }
}

pub struct ResolveContext<'a> {
    pub project_root: Option<&'a Path>,
    pub sdk: Option<&'a Sdk>,
    pub explicit: Option<&'a Path>,
    pub managed_dir: &'a Path,
    pub env: &'a Env,
}

/// A system path, with a leading `~/` meaning the home folder, as for tools installed per user.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Where an installed copy of a tool lives, given how it's installed.
pub fn managed_binary(name: &str, spec: &ToolSpec, managed_dir: &Path) -> Option<PathBuf> {
    let install = spec.install.as_ref()?;
    let dir = managed_dir.join(name).join(install.version()?);
    Some(match install {
        InstallSpec::GoInstall { .. } => dir.join("bin").join(&spec.binary),
        InstallSpec::Npm { .. } => dir.join("node_modules/.bin").join(&spec.binary),
        InstallSpec::Download { binary, .. } => dir.join(platform_binary(binary)),
        InstallSpec::DotnetTool { .. } => dir.join(&spec.binary),
        InstallSpec::RustupComponent { .. } | InstallSpec::Manual { .. } => return None,
    })
}

/// Versions of a tool that we installed earlier, other than the one the manifest pins now, as
/// (version, folder).
pub fn older_installs(name: &str, spec: &ToolSpec, managed_dir: &Path) -> Vec<(String, PathBuf)> {
    let Some(current) = spec.install.as_ref().and_then(|i| i.version()) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(managed_dir.join(name)) else {
        return Vec::new();
    };
    let mut older: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let version = e.file_name().to_string_lossy().into_owned();
            // Dot-folders are installs in progress.
            (version != current && !version.starts_with('.')).then(|| (version, e.path()))
        })
        .collect();
    older.sort();
    older
}

/// Fills in `{os}` and `{arch}` in a binary's name, as release assets name them.
pub fn platform_binary(template: &str) -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "unknown-linux-gnu"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    template.replace("{os}", os).replace("{arch}", arch)
}

/// Finds a tool: settings, then the project, the SDK, our installs, system locations and `PATH`.
pub fn resolve_tool(name: &str, spec: &ToolSpec, cx: &ResolveContext) -> Resolution {
    let mut looked_in = Vec::new();
    let mut candidates: Vec<(ToolSource, PathBuf)> = Vec::new();
    if let Some(explicit) = cx.explicit {
        candidates.push((ToolSource::Explicit, explicit.to_path_buf()));
    }
    if let Some(root) = cx.project_root {
        candidates.extend(
            spec.project_paths
                .iter()
                .map(|p| (ToolSource::Project, root.join(p))),
        );
    }
    if let Some(sdk) = cx.sdk {
        candidates.extend(
            spec.sdk_paths
                .iter()
                .map(|p| (ToolSource::Sdk, sdk.root.join(p))),
        );
    }
    if let Some(path) = managed_binary(name, spec, cx.managed_dir) {
        candidates.push((ToolSource::Managed, path));
    }
    candidates.extend(
        spec.system_paths
            .iter()
            .map(|p| (ToolSource::System, expand_home(p))),
    );

    for (source, path) in candidates {
        // Scripts like js-debug's entry point are run by another program, so they needn't be
        // executable themselves.
        let usable = if path.extension().is_some_and(|e| e == "js") {
            path.is_file()
        } else {
            is_executable(&path)
        };
        if usable {
            return Resolution {
                tool: name.to_string(),
                found: Some((path, source)),
                looked_in,
            };
        }
        looked_in.push((source, path));
    }
    if let Some(path) = cx.env.which(&spec.binary) {
        return Resolution {
            tool: name.to_string(),
            found: Some((path, ToolSource::Path)),
            looked_in,
        };
    }
    looked_in.push((ToolSource::Path, PathBuf::from(&spec.binary)));
    Resolution {
        tool: name.to_string(),
        found: None,
        looked_in,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::SdkSource;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    fn executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn spec() -> ToolSpec {
        ToolSpec {
            binary: "tool".into(),
            project_paths: vec!["node_modules/.bin/tool".into()],
            sdk_paths: vec!["bin/tool".into()],
            system_paths: vec![],
            install: Some(InstallSpec::GoInstall {
                module: "x/tool".into(),
                version: "v1".into(),
            }),
        }
    }

    struct Places {
        _dir: tempfile::TempDir,
        explicit: PathBuf,
        project: PathBuf,
        sdk: Sdk,
        managed: PathBuf,
        path_dir: PathBuf,
    }

    /// A copy of the tool in every place the resolver looks.
    fn everywhere() -> Places {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let places = Places {
            explicit: base.join("explicit/tool"),
            project: base.join("project"),
            sdk: Sdk {
                kind: "go".into(),
                root: base.join("sdk"),
                version: None,
                source: SdkSource::Known,
            },
            managed: base.join("managed"),
            path_dir: base.join("on-path"),
            _dir: dir,
        };
        executable(&places.explicit);
        executable(&places.project.join("node_modules/.bin/tool"));
        executable(&places.sdk.root.join("bin/tool"));
        executable(&places.managed.join("tool/v1/bin/tool"));
        executable(&places.path_dir.join("tool"));
        places
    }

    fn resolve(places: &Places, explicit: bool) -> Resolution {
        let env = Env::from_pairs([("PATH", places.path_dir.to_str().unwrap())]);
        let cx = ResolveContext {
            project_root: Some(&places.project),
            sdk: Some(&places.sdk),
            explicit: explicit.then_some(places.explicit.as_path()),
            managed_dir: &places.managed,
            env: &env,
        };
        resolve_tool("tool", &spec(), &cx)
    }

    fn source(resolution: &Resolution) -> Option<ToolSource> {
        resolution.found.as_ref().map(|(_, s)| *s)
    }

    #[test]
    fn each_place_is_tried_in_order() {
        let places = everywhere();
        assert_eq!(source(&resolve(&places, true)), Some(ToolSource::Explicit));
        assert_eq!(source(&resolve(&places, false)), Some(ToolSource::Project));
        fs::remove_file(places.project.join("node_modules/.bin/tool")).unwrap();
        assert_eq!(source(&resolve(&places, false)), Some(ToolSource::Sdk));
        fs::remove_file(places.sdk.root.join("bin/tool")).unwrap();
        assert_eq!(source(&resolve(&places, false)), Some(ToolSource::Managed));
        fs::remove_file(places.managed.join("tool/v1/bin/tool")).unwrap();
        assert_eq!(source(&resolve(&places, false)), Some(ToolSource::Path));
        fs::remove_file(places.path_dir.join("tool")).unwrap();
        let missing = resolve(&places, false);
        assert_eq!(source(&missing), None);
        // Everywhere it looked is reported, to explain why it's missing.
        assert_eq!(missing.looked_in.len(), 4);
    }

    #[test]
    fn a_broken_explicit_path_falls_through_and_is_reported() {
        let places = everywhere();
        fs::remove_file(&places.explicit).unwrap();
        let resolution = resolve(&places, true);
        assert_eq!(source(&resolution), Some(ToolSource::Project));
        assert_eq!(
            resolution.looked_in[0],
            (ToolSource::Explicit, places.explicit.clone())
        );
    }

    #[test]
    fn a_non_executable_file_does_not_count() {
        let places = everywhere();
        let project_copy = places.project.join("node_modules/.bin/tool");
        fs::set_permissions(&project_copy, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(source(&resolve(&places, false)), Some(ToolSource::Sdk));
    }

    #[test]
    fn managed_installs_have_a_folder_per_version() {
        let spec = spec();
        assert_eq!(
            managed_binary("tool", &spec, Path::new("/m")),
            Some(PathBuf::from("/m/tool/v1/bin/tool"))
        );
        let npm = ToolSpec {
            install: Some(InstallSpec::Npm {
                packages: vec!["tool@2.0.0".into()],
            }),
            ..spec
        };
        assert_eq!(
            managed_binary("tool", &npm, Path::new("/m")),
            Some(PathBuf::from("/m/tool/2.0.0/node_modules/.bin/tool"))
        );
    }

    #[test]
    fn platform_names_are_filled_in() {
        let name = platform_binary("ols-{arch}-{os}");
        assert!(!name.contains('{'), "{name}");
    }

    #[test]
    fn older_installs_are_the_other_versions_but_not_installs_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        for folder in ["v1.0.0", "v2.0.0", "v3.0.0", ".v3.0.0.installing-7"] {
            std::fs::create_dir_all(dir.path().join("gopls").join(folder)).unwrap();
        }
        let spec: ToolSpec = toml::from_str(
            "binary = \"gopls\"\ninstall = { kind = \"go-install\", module = \"x\", version = \"v3.0.0\" }",
        )
        .unwrap();
        let older: Vec<String> = older_installs("gopls", &spec, dir.path())
            .into_iter()
            .map(|(v, _)| v)
            .collect();
        assert_eq!(older, ["v1.0.0", "v2.0.0"]);
    }
}
