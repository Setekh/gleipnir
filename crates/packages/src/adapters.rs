//! What can't be declared in a manifest: finding what a project can run, and how to debug it.
//! One adapter per package, behind one interface, so the run and debug UI doesn't care which
//! language it's dealing with.

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A process to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunTargetKind {
    Binary,
    Script,
    File,
}

/// Something the project can run, and debug.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunTarget {
    pub package_id: String,
    pub name: String,
    pub kind: RunTargetKind,
    /// What to build first, if anything, as program and arguments, run in `cwd`.
    pub build: Option<Vec<String>>,
    /// What to run, as program and arguments.
    pub run: Vec<String>,
    /// The executable a debugger should launch, for compiled languages.
    pub program: Option<PathBuf>,
    pub cwd: PathBuf,
}

/// How to start a debug session: the arguments of DAP's `launch` request.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugLaunch {
    pub launch_arguments: Value,
}

pub trait LanguageAdapter: Send + Sync {
    fn package_id(&self) -> &'static str;
    /// Everything the project can run, found from its files.
    fn run_targets(&self, root: &Path) -> Vec<RunTarget>;
    /// The `launch` request for debugging a target.
    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch>;
}

pub fn adapter_for(package_id: &str) -> Option<&'static dyn LanguageAdapter> {
    match package_id {
        "rust" => Some(&RustAdapter),
        "go" => Some(&GoAdapter),
        "odin" => Some(&OdinAdapter),
        "typescript" => Some(&TypeScriptAdapter),
        _ => None,
    }
}

/// Folders that never hold a project's own entry points, and can be enormous.
const SKIPPED: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".zig-cache",
];

/// Folders under `root`, breadth first, up to a depth, skipping dependency and build output.
fn folders(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut found = vec![root.to_path_buf()];
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for dir in &frontier {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            let mut children: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    !name.starts_with('.') && !SKIPPED.contains(&name.as_str())
                })
                .map(|e| e.path())
                .collect();
            children.sort();
            next.extend(children);
        }
        found.extend(next.iter().cloned());
        frontier = next;
    }
    found
}

fn lldb_launch(target: &RunTarget) -> Result<DebugLaunch> {
    let Some(program) = &target.program else {
        bail!("{} has no program to debug", target.name)
    };
    Ok(DebugLaunch {
        launch_arguments: json!({
            "program": program,
            "args": [],
            "cwd": target.cwd,
            "stopOnEntry": false,
        }),
    })
}

pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn package_id(&self) -> &'static str {
        "rust"
    }

    /// Binaries of the package, or of each workspace member: `src/main.rs`, `src/bin/*.rs` and
    /// `[[bin]]` entries.
    fn run_targets(&self, root: &Path) -> Vec<RunTarget> {
        let mut manifests = vec![root.join("Cargo.toml")];
        if let Ok(text) = fs::read_to_string(root.join("Cargo.toml"))
            && let Ok(value) = text.parse::<toml::Table>()
            && let Some(members) = value
                .get("workspace")
                .and_then(|w| w.get("members"))
                .and_then(|m| m.as_array())
        {
            for member in members.iter().filter_map(|m| m.as_str()) {
                match member.strip_suffix("/*") {
                    Some(parent) => {
                        if let Ok(entries) = fs::read_dir(root.join(parent)) {
                            let mut dirs: Vec<PathBuf> =
                                entries.filter_map(Result::ok).map(|e| e.path()).collect();
                            dirs.sort();
                            manifests.extend(dirs.into_iter().map(|d| d.join("Cargo.toml")));
                        }
                    }
                    None => manifests.push(root.join(member).join("Cargo.toml")),
                }
            }
        }

        let mut targets = Vec::new();
        for manifest in manifests {
            let Ok(text) = fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(value) = text.parse::<toml::Table>() else {
                continue;
            };
            let Some(package) = value
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            else {
                continue;
            };
            let dir = manifest.parent().unwrap_or(root);
            let mut bins: Vec<String> = Vec::new();
            if dir.join("src/main.rs").is_file() {
                bins.push(package.to_string());
            }
            if let Ok(entries) = fs::read_dir(dir.join("src/bin")) {
                let mut names: Vec<String> = entries
                    .filter_map(Result::ok)
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                    })
                    .collect();
                names.sort();
                bins.extend(names);
            }
            if let Some(declared) = value.get("bin").and_then(|b| b.as_array()) {
                bins.extend(
                    declared
                        .iter()
                        .filter_map(|b| b.get("name")?.as_str().map(String::from)),
                );
            }
            bins.dedup();
            for bin in bins {
                targets.push(RunTarget {
                    package_id: "rust".into(),
                    name: bin.clone(),
                    kind: RunTargetKind::Binary,
                    build: Some(vec![
                        "cargo".into(),
                        "build".into(),
                        "-p".into(),
                        package.into(),
                        "--bin".into(),
                        bin.clone(),
                    ]),
                    run: vec![
                        root.join("target/debug")
                            .join(&bin)
                            .to_string_lossy()
                            .into_owned(),
                    ],
                    program: Some(root.join("target/debug").join(&bin)),
                    cwd: root.to_path_buf(),
                });
            }
        }
        targets
    }

    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch> {
        lldb_launch(target)
    }
}

/// Whether any `.go` file in a folder declares `package main`.
fn is_go_main(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|e| {
        let path = e.path();
        path.extension().is_some_and(|x| x == "go")
            && !path.to_string_lossy().ends_with("_test.go")
            && fs::read_to_string(&path)
                .is_ok_and(|text| text.lines().any(|l| l.trim() == "package main"))
    })
}

pub struct GoAdapter;

impl LanguageAdapter for GoAdapter {
    fn package_id(&self) -> &'static str {
        "go"
    }

    fn run_targets(&self, root: &Path) -> Vec<RunTarget> {
        folders(root, 4)
            .into_iter()
            .filter(|dir| is_go_main(dir))
            .map(|dir| {
                let relative = dir.strip_prefix(root).unwrap_or(&dir);
                let package = if relative.as_os_str().is_empty() {
                    ".".to_string()
                } else {
                    format!("./{}", relative.display())
                };
                RunTarget {
                    package_id: "go".into(),
                    name: package.clone(),
                    kind: RunTargetKind::Binary,
                    build: None,
                    run: vec!["go".into(), "run".into(), package],
                    program: Some(dir.clone()),
                    cwd: root.to_path_buf(),
                }
            })
            .collect()
    }

    /// Delve builds the program itself, with the optimizations off that would confuse debugging.
    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch> {
        let Some(program) = &target.program else {
            bail!("{} has no package to debug", target.name)
        };
        Ok(DebugLaunch {
            launch_arguments: json!({
                "mode": "debug",
                "program": program,
                "args": [],
                "cwd": target.cwd,
            }),
        })
    }
}

/// Whether any `.odin` file in a folder declares a `main` procedure.
fn is_odin_main(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|e| {
        let path = e.path();
        path.extension().is_some_and(|x| x == "odin")
            && fs::read_to_string(&path).is_ok_and(|text| {
                text.lines().any(|l| {
                    let l = l.trim_start();
                    l.starts_with("main") && l.contains("::") && l.contains("proc")
                })
            })
    })
}

pub struct OdinAdapter;

impl LanguageAdapter for OdinAdapter {
    fn package_id(&self) -> &'static str {
        "odin"
    }

    fn run_targets(&self, root: &Path) -> Vec<RunTarget> {
        folders(root, 3)
            .into_iter()
            .filter(|dir| is_odin_main(dir))
            .map(|dir| {
                let name = dir
                    .file_name()
                    .map_or_else(|| "main".into(), |n| n.to_string_lossy().into_owned());
                // Built into the project's own output folder, out of the way of its sources.
                let program = root.join("build/gleipnir").join(&name);
                RunTarget {
                    package_id: "odin".into(),
                    name: name.clone(),
                    kind: RunTargetKind::Binary,
                    build: Some(vec![
                        "odin".into(),
                        "build".into(),
                        dir.to_string_lossy().into_owned(),
                        "-debug".into(),
                        format!("-out:{}", program.display()),
                    ]),
                    run: vec![program.to_string_lossy().into_owned()],
                    program: Some(program),
                    cwd: root.to_path_buf(),
                }
            })
            .collect()
    }

    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch> {
        lldb_launch(target)
    }
}

pub struct TypeScriptAdapter;

impl TypeScriptAdapter {
    /// Running one file directly, which is what "run this" means for a script.
    pub fn file_target(&self, root: &Path, file: &Path) -> RunTarget {
        let name = file
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        RunTarget {
            package_id: "typescript".into(),
            name,
            kind: RunTargetKind::File,
            build: None,
            run: vec!["node".into(), file.to_string_lossy().into_owned()],
            program: Some(file.to_path_buf()),
            cwd: root.to_path_buf(),
        }
    }
}

impl LanguageAdapter for TypeScriptAdapter {
    fn package_id(&self) -> &'static str {
        "typescript"
    }

    /// The `scripts` in package.json, and its `main` file.
    fn run_targets(&self, root: &Path) -> Vec<RunTarget> {
        let Ok(text) = fs::read_to_string(root.join("package.json")) else {
            return Vec::new();
        };
        let Ok(package) = serde_json::from_str::<Value>(&text) else {
            return Vec::new();
        };
        let mut targets: Vec<RunTarget> = package
            .get("scripts")
            .and_then(Value::as_object)
            .map(|scripts| {
                scripts
                    .keys()
                    .map(|name| RunTarget {
                        package_id: "typescript".into(),
                        name: format!("npm run {name}"),
                        kind: RunTargetKind::Script,
                        build: None,
                        run: vec!["npm".into(), "run".into(), name.clone()],
                        program: None,
                        cwd: root.to_path_buf(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(main) = package.get("main").and_then(Value::as_str) {
            let file = root.join(main);
            if file.is_file() {
                targets.insert(0, self.file_target(root, &file));
            }
        }
        targets
    }

    /// js-debug's Node launch. Source maps are on, so breakpoints in TypeScript land.
    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch> {
        let mut launch = json!({
            "type": "pwa-node",
            "request": "launch",
            "name": target.name,
            "cwd": target.cwd,
            "sourceMaps": true,
            "console": "internalConsole",
        });
        match (&target.program, target.kind) {
            (Some(program), _) => launch["program"] = json!(program),
            (None, RunTargetKind::Script) => {
                launch["runtimeExecutable"] = json!("npm");
                launch["runtimeArgs"] = json!(target.run[1..]);
            }
            _ => bail!("{} has nothing to debug", target.name),
        }
        Ok(DebugLaunch {
            launch_arguments: launch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        dir
    }

    fn names(targets: &[RunTarget]) -> Vec<&str> {
        targets.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn every_builtin_package_has_an_adapter() {
        for package in crate::registry().packages() {
            assert!(adapter_for(package.id()).is_some(), "{}", package.id());
        }
    }

    #[test]
    fn rust_binaries_are_found_across_a_workspace() {
        let dir = project(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\", \"tools/gen\"]\n",
            ),
            ("crates/app/Cargo.toml", "[package]\nname = \"app\"\n"),
            ("crates/app/src/main.rs", "fn main() {}"),
            ("crates/app/src/bin/extra.rs", "fn main() {}"),
            ("crates/lib/Cargo.toml", "[package]\nname = \"lib\"\n"),
            ("crates/lib/src/lib.rs", ""),
            (
                "tools/gen/Cargo.toml",
                "[package]\nname = \"gen\"\n[[bin]]\nname = \"generate\"\npath = \"main.rs\"\n",
            ),
        ]);
        let targets = RustAdapter.run_targets(dir.path());
        assert_eq!(names(&targets), ["app", "extra", "generate"]);
        assert_eq!(
            targets[1].build.as_ref().unwrap(),
            &["cargo", "build", "-p", "app", "--bin", "extra"]
        );
        assert_eq!(
            targets[0].program.as_ref().unwrap(),
            &dir.path().join("target/debug/app")
        );
    }

    #[test]
    fn go_main_packages_are_found_and_tests_and_vendor_skipped() {
        let dir = project(&[
            ("go.mod", "module m"),
            ("cmd/server/main.go", "package main\nfunc main() {}"),
            ("cmd/tool/x_test.go", "package main"),
            ("internal/lib/lib.go", "package lib"),
            ("vendor/dep/main.go", "package main"),
            ("main.go", "// root\npackage main\n"),
        ]);
        let targets = GoAdapter.run_targets(dir.path());
        assert_eq!(names(&targets), [".", "./cmd/server"]);
        let launch = GoAdapter
            .debug_launch(&targets[1])
            .unwrap()
            .launch_arguments;
        assert_eq!(launch["mode"], "debug");
    }

    #[test]
    fn odin_programs_are_folders_with_a_main_procedure() {
        let dir = project(&[
            ("game/main.odin", "package game\n\nmain :: proc() {\n}\n"),
            ("lib/util.odin", "package lib\nhelper :: proc() {}\n"),
        ]);
        let targets = OdinAdapter.run_targets(dir.path());
        assert_eq!(names(&targets), ["game"]);
        let build = targets[0].build.as_ref().unwrap();
        assert!(build.contains(&"-debug".to_string()));
        let launch = OdinAdapter
            .debug_launch(&targets[0])
            .unwrap()
            .launch_arguments;
        assert!(
            launch["program"]
                .as_str()
                .unwrap()
                .ends_with("build/gleipnir/game")
        );
    }

    #[test]
    fn javascript_scripts_and_main_are_found() {
        let dir = project(&[
            (
                "package.json",
                r#"{"main": "index.js", "scripts": {"dev": "vite", "test": "vitest"}}"#,
            ),
            ("index.js", ""),
        ]);
        let targets = TypeScriptAdapter.run_targets(dir.path());
        assert_eq!(names(&targets), ["index.js", "npm run dev", "npm run test"]);
        let script = TypeScriptAdapter
            .debug_launch(&targets[1])
            .unwrap()
            .launch_arguments;
        assert_eq!(script["runtimeExecutable"], "npm");
        assert_eq!(script["runtimeArgs"], json!(["run", "dev"]));
        let file = TypeScriptAdapter
            .debug_launch(&targets[0])
            .unwrap()
            .launch_arguments;
        assert_eq!(file["type"], "pwa-node");
        assert_eq!(file["sourceMaps"], true);
    }

    #[test]
    fn a_project_without_any_of_these_has_no_targets() {
        let dir = project(&[("notes.txt", "")]);
        for package in crate::registry().packages() {
            assert!(
                adapter_for(package.id())
                    .unwrap()
                    .run_targets(dir.path())
                    .is_empty()
            );
        }
    }

    #[test]
    fn rust_targets_in_this_repository() {
        // Gleipnir itself: a workspace whose one binary is the app.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let targets = RustAdapter.run_targets(&root);
        assert!(
            names(&targets).contains(&"gleipnir"),
            "{:?}",
            names(&targets)
        );
    }
}
