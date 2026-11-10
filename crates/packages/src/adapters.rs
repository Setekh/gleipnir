//! What can't be declared in a manifest: finding what a project can run, and how to debug it.
//! One adapter per package, behind one interface, so the run and debug UI doesn't care which
//! language it's dealing with.

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::testing::{
    Suite, TestPick, go_locate, go_select, has_odin_tests, is_js_test_file, js_select,
    js_test_chain, js_test_target, odin_locate, odin_select, odin_test_name, odin_test_target,
    rust_locate, rust_module_path, rust_select,
};

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
    /// A package's tests, or one test.
    Test,
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
    /// The file with the entry point, or for Go and Odin the package's folder, so the editor can
    /// offer to run it from there.
    pub source: Option<PathBuf>,
    /// Arguments for the program when it's debugged, like the test to run.
    pub args: Vec<String>,
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
    /// Adjusts a launch with what can only be found by asking the project's tools, such as where
    /// the toolchain keeps its debugger scripts. Runs off the UI thread.
    fn prepare_debug_launch(
        &self,
        _: &RunTarget,
        _: &mut DebugLaunch,
        _: &crate::Env,
    ) -> Result<()> {
        Ok(())
    }
    /// The `attach` request for debugging a process that's already running, if the debugger can.
    fn attach_arguments(&self, _pid: u32) -> Option<Value> {
        None
    }
    /// The test declared on a line, to run from the gutter, given the file's text.
    fn test_at(&self, _root: &Path, _file: &Path, _text: &str, _line: usize) -> Option<RunTarget> {
        None
    }
    /// A test target narrowed to some of its tests, as the results tree names them.
    fn select_tests(&self, _target: &RunTarget, _picks: &[TestPick]) -> Option<RunTarget> {
        None
    }
    /// Where tests of a suite are declared: a file and a line, from 0, for each name given.
    fn locate_tests(
        &self,
        _target: &RunTarget,
        _suite: &Suite,
        names: &[String],
    ) -> Vec<Option<(PathBuf, u32)>> {
        vec![None; names.len()]
    }
}

/// The program and arguments that start a package's debug adapter, given where its tool is and,
/// for adapters that are scripts, the SDK binary that runs them.
pub fn debugger_command(
    spec: &crate::manifest::DebuggerSpec,
    tool: &Path,
    sdk_binary: Option<&Path>,
) -> Result<(PathBuf, Vec<String>)> {
    match spec.runtime.as_deref() {
        Some("sdk") => {
            let Some(runtime) = sdk_binary else {
                bail!("{} needs the SDK to run, and there isn't one", spec.tool)
            };
            let mut args = vec![tool.to_string_lossy().into_owned()];
            args.extend(spec.args.iter().cloned());
            Ok((runtime.to_path_buf(), args))
        }
        _ => Ok((tool.to_path_buf(), spec.args.clone())),
    }
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
    // A test's program is found when the launch is prepared.
    if target.program.is_none() && target.kind != RunTargetKind::Test {
        bail!("{} has no program to debug", target.name)
    }
    Ok(DebugLaunch {
        launch_arguments: json!({
            "program": target.program,
            "args": target.args,
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
        let mut tests = Vec::new();
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
            let mut bins: Vec<(String, PathBuf)> = Vec::new();
            if dir.join("src/main.rs").is_file() {
                bins.push((package.to_string(), dir.join("src/main.rs")));
            }
            if let Ok(entries) = fs::read_dir(dir.join("src/bin")) {
                let mut names: Vec<(String, PathBuf)> = entries
                    .filter_map(Result::ok)
                    .filter_map(|e| {
                        let path = e.path();
                        let stem = path.file_stem()?.to_string_lossy().into_owned();
                        Some((stem, path))
                    })
                    .collect();
                names.sort();
                bins.extend(names);
            }
            if let Some(declared) = value.get("bin").and_then(|b| b.as_array()) {
                bins.extend(declared.iter().filter_map(|b| {
                    let name = b.get("name")?.as_str()?.to_string();
                    let path = b
                        .get("path")
                        .and_then(|p| p.as_str())
                        .map_or_else(|| dir.join("src/main.rs"), |p| dir.join(p));
                    Some((name, path))
                }));
            }
            bins.dedup_by(|a, b| a.0 == b.0);
            tests.push(rust_test_target(root, package, None, false, &[], None));
            for (bin, source) in bins {
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
                    source: Some(source),
                    args: Vec::new(),
                });
            }
        }
        // Tests after what runs, as they're less often what's wanted.
        if tests.len() > 1 {
            targets.push(RunTarget {
                package_id: "rust".into(),
                name: "all tests".into(),
                kind: RunTargetKind::Test,
                build: Some(
                    ["cargo", "test", "--no-run", "--workspace"]
                        .map(String::from)
                        .to_vec(),
                ),
                run: ["cargo", "test", "--workspace", "--no-fail-fast"]
                    .map(String::from)
                    .to_vec(),
                program: None,
                cwd: root.to_path_buf(),
                source: None,
                args: Vec::new(),
            });
        }
        targets.extend(tests);
        targets
    }

    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch> {
        lldb_launch(target)
    }

    fn select_tests(&self, target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
        rust_select(target, picks)
    }

    fn locate_tests(
        &self,
        target: &RunTarget,
        suite: &Suite,
        names: &[String],
    ) -> Vec<Option<(PathBuf, u32)>> {
        rust_locate(target, suite, names)
    }

    /// Loads the toolchain's own formatters into lldb, as `rust-lldb` does, so strings, vectors
    /// and enums read as Rust rather than as their C layout.
    fn prepare_debug_launch(
        &self,
        target: &RunTarget,
        launch: &mut DebugLaunch,
        env: &crate::Env,
    ) -> Result<()> {
        // A test binary's name has a hash in it, which only Cargo knows.
        if target.kind == RunTargetKind::Test {
            let program = rust_test_binary(target, env)?;
            launch.launch_arguments["program"] = json!(program);
        }
        if let Some(script) = rust_lldb_script(&target.cwd, env) {
            let import = format!("command script import \"{}\"", script.display());
            launch.launch_arguments["initCommands"] = json!([import]);
        }
        Ok(())
    }

    fn attach_arguments(&self, pid: u32) -> Option<Value> {
        Some(json!({ "pid": pid }))
    }

    /// A `#[test]` function, the line with `fn name(` under the attribute, or a `mod` under
    /// `#[cfg(test)]`, for every test in it. Named by their full path, to run exactly them.
    fn test_at(&self, root: &Path, file: &Path, text: &str, line: usize) -> Option<RunTarget> {
        let lines: Vec<&str> = text.lines().collect();
        let declared = lines.get(line)?;
        let attributes = || {
            lines[..line].iter().rev().take_while(|l| {
                let l = l.trim();
                l.starts_with("#[") || l.starts_with("//") || l.is_empty()
            })
        };
        let prefix = || {
            let mut prefix = rust_module_path(file).unwrap_or_default();
            for module in inline_modules(&lines, line) {
                prefix.push_str(&module);
                prefix.push_str("::");
            }
            prefix
        };
        let package = || cargo_package_of(file);
        if let Some(module) = rust_inline_mod_name(declared) {
            if !attributes().any(|l| l.contains("cfg(test)")) {
                return None;
            }
            let filter = format!("{}{module}::", prefix());
            let selector = rust_selector_for_file(file);
            return Some(rust_test_target(
                root,
                &package()?,
                Some(&filter),
                false,
                &selector,
                Some(file),
            ));
        }
        let name = rust_fn_name(declared)?;
        if !attributes().any(|l| l.trim().ends_with("test]")) {
            return None;
        }
        let filter = format!("{}{name}", prefix());
        let selector = rust_selector_for_file(file);
        Some(rust_test_target(
            root,
            &package()?,
            Some(&filter),
            true,
            &selector,
            Some(file),
        ))
    }
}

/// The toolchain's lldb formatter script, for the toolchain the project uses.
pub fn rust_lldb_script(cwd: &Path, env: &crate::Env) -> Option<PathBuf> {
    let rustc = env.which("rustc")?;
    let mut command = std::process::Command::new(rustc);
    command.args(["--print", "sysroot"]).current_dir(cwd);
    env.apply(&mut command);
    let output = command.output().ok().filter(|o| o.status.success())?;
    let sysroot = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    let script = sysroot.join("lib/rustlib/etc/lldb_lookup.py");
    script.is_file().then_some(script)
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

/// The name of a function declared on a line, if one is.
fn rust_fn_name(line: &str) -> Option<String> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix("pub ").unwrap_or(rest);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The package a Rust file belongs to: the nearest `Cargo.toml` above it with a `[package]`.
fn cargo_package_of(file: &Path) -> Option<String> {
    file.ancestors().skip(1).find_map(|dir| {
        let text = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
        let value = text.parse::<toml::Table>().ok()?;
        value
            .get("package")?
            .get("name")?
            .as_str()
            .map(String::from)
    })
}

/// The name of an inline module declared on a line: `mod tests {`.
fn rust_inline_mod_name(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    for visibility in ["pub(crate) ", "pub(super) ", "pub "] {
        rest = rest.strip_prefix(visibility).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("mod ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let after = rest[name.len()..].trim_start();
    (!name.is_empty() && after.starts_with('{')).then_some(name)
}

/// The inline modules a line is inside, outermost first, found by counting braces above it.
fn inline_modules(lines: &[&str], line: usize) -> Vec<String> {
    let mut open: Vec<(String, i32)> = Vec::new();
    let mut depth = 0i32;
    for text in &lines[..line.min(lines.len())] {
        let code = text.split("//").next().unwrap_or("");
        if let Some(name) = rust_inline_mod_name(code) {
            open.push((name, depth));
        }
        let mut in_string = false;
        let mut previous = ' ';
        for c in code.chars() {
            match c {
                '"' if previous != '\\' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => depth -= 1,
                _ => {}
            }
            previous = c;
        }
        while open.last().is_some_and(|(_, at)| depth <= *at) {
            open.pop();
        }
    }
    open.into_iter().map(|(name, _)| name).collect()
}

/// The Cargo flags for the test binary a file's tests are built into, when it's clear which.
fn rust_selector_for_file(file: &Path) -> Vec<String> {
    let Some(dir) = file.ancestors().skip(1).find(|dir| {
        fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|t| t.contains("[package]"))
    }) else {
        return Vec::new();
    };
    let Ok(relative) = file.strip_prefix(dir) else {
        return Vec::new();
    };
    let parts: Vec<String> = relative
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let stem = |name: &str| name.trim_end_matches(".rs").to_string();
    match parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["tests", file] => vec!["--test".into(), stem(file)],
        ["tests", folder, "main.rs"] => vec!["--test".into(), folder.to_string()],
        ["tests", ..] => Vec::new(),
        ["src", "bin", file] => vec!["--bin".into(), stem(file)],
        ["src", "bin", folder, ..] => vec!["--bin".into(), folder.to_string()],
        ["src", "main.rs"] => match cargo_package_of(file) {
            Some(package) => vec!["--bin".into(), package],
            None => Vec::new(),
        },
        ["src", ..] if dir.join("src/lib.rs").is_file() => vec!["--lib".into()],
        _ => Vec::new(),
    }
}

/// A package's tests, or those named by a filter: one test when `exact`, else every test whose
/// name contains it. `selector` picks the test binary, like `--lib`.
fn rust_test_target(
    root: &Path,
    package: &str,
    filter: Option<&str>,
    exact: bool,
    selector: &[String],
    source: Option<&Path>,
) -> RunTarget {
    let mut base: Vec<String> = ["cargo", "test", "-p", package]
        .into_iter()
        .map(String::from)
        .collect();
    let mut build = base.clone();
    build.insert(2, "--no-run".into());
    build.extend(selector.iter().cloned());
    base.push("--no-fail-fast".into());
    base.extend(selector.iter().cloned());
    let mut args = Vec::new();
    if let Some(filter) = filter {
        if exact {
            args.push("--exact".to_string());
        }
        args.push(filter.to_string());
    }
    let mut run = base;
    if !args.is_empty() {
        run.push("--".into());
        run.extend(args.iter().cloned());
    }
    RunTarget {
        package_id: "rust".into(),
        name: match filter {
            Some(test) if exact => format!("test {}", test.rsplit("::").next().unwrap_or(test)),
            Some(group) => format!("tests in {package}::{}", group.trim_end_matches("::")),
            None => format!("tests in {package}"),
        },
        kind: RunTargetKind::Test,
        build: Some(build),
        run,
        program: None,
        cwd: root.to_path_buf(),
        source: source.map(Path::to_path_buf),
        args,
    }
}

/// The test binary Cargo builds for a test target, asking Cargo where it put it: the library's,
/// unless the target picks another.
fn rust_test_binary(target: &RunTarget, env: &crate::Env) -> Result<PathBuf> {
    let build = target
        .build
        .clone()
        .ok_or_else(|| anyhow::anyhow!("{} has no test binary to debug", target.name))?;
    let package = build
        .iter()
        .skip_while(|a| *a != "-p")
        .nth(1)
        .cloned()
        .unwrap_or_default();
    let cargo = env
        .which("cargo")
        .ok_or_else(|| anyhow::anyhow!("cargo isn't on the PATH"))?;
    let mut command = std::process::Command::new(cargo);
    command
        .args(&build[1..])
        .arg("--message-format=json")
        .current_dir(&target.cwd);
    env.apply(&mut command);
    let output = command.output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // The library's own tests first, then a binary's, then any other.
    let mut found: Vec<(u8, PathBuf)> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["profile"]["test"] == true)
        .filter(|m| {
            package.is_empty()
                || m["package_id"]
                    .as_str()
                    .is_some_and(|id| id.contains(package.as_str()))
                || m["target"]["name"].as_str().map(|n| n.replace('-', "_"))
                    == Some(package.replace('-', "_"))
        })
        .filter_map(|m| {
            let path = PathBuf::from(m["executable"].as_str()?);
            let kinds = m["target"]["kind"].as_array()?;
            let rank = if kinds.iter().any(|k| k == "lib") {
                0
            } else if kinds.iter().any(|k| k == "bin") {
                1
            } else {
                2
            };
            Some((rank, path))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow::anyhow!("Cargo built no test binary for {package}"))
}

/// Whether a folder has Go tests.
fn has_go_tests(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with("_test.go"))
    })
}

/// A Go package's tests, or one of them by name.
fn go_test_target(root: &Path, dir: &Path, test: Option<&str>) -> RunTarget {
    let relative = dir.strip_prefix(root).unwrap_or(dir);
    let package = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        format!("./{}", relative.display())
    };
    let mut run = vec!["go".to_string(), "test".into(), "-json".into()];
    let mut args = Vec::new();
    if let Some(test) = test {
        run.extend(["-run".to_string(), format!("^{test}$")]);
        args = vec!["-test.run".to_string(), format!("^{test}$")];
    }
    // Under delve, results are read from the verbose output.
    args.push("-test.v".into());
    run.push(package.clone());
    RunTarget {
        package_id: "go".into(),
        name: match test {
            Some(test) => format!("test {test}"),
            None => format!("tests in {package}"),
        },
        kind: RunTargetKind::Test,
        build: None,
        run,
        program: Some(dir.to_path_buf()),
        cwd: root.to_path_buf(),
        // A package's tests run from its files, where nothing else does.
        source: test.is_none().then(|| dir.to_path_buf()),
        args,
    }
}

pub struct GoAdapter;

impl LanguageAdapter for GoAdapter {
    fn package_id(&self) -> &'static str {
        "go"
    }

    fn run_targets(&self, root: &Path) -> Vec<RunTarget> {
        let folders = folders(root, 4);
        let mut tests: Vec<RunTarget> = folders
            .iter()
            .filter(|dir| has_go_tests(dir))
            .map(|dir| go_test_target(root, dir, None))
            .collect();
        if tests.len() > 1 {
            let mut all = go_test_target(root, root, None);
            all.name = "all tests".into();
            *all.run.last_mut().unwrap() = "./...".into();
            all.program = None;
            tests.insert(0, all);
        }
        let mut targets: Vec<RunTarget> = folders
            .into_iter()
            .filter(|dir| is_go_main(dir))
            .map(|dir| {
                let relative = dir.strip_prefix(root).unwrap_or(&dir);
                let package = if relative.as_os_str().is_empty() {
                    ".".to_string()
                } else {
                    format!("./{}", relative.display())
                };
                // Named for people: the folder, rather than Go's `.` for the root package.
                let name = if relative.as_os_str().is_empty() {
                    root.file_name()
                        .map_or_else(|| "main".into(), |n| n.to_string_lossy().into_owned())
                } else {
                    relative.display().to_string()
                };
                RunTarget {
                    package_id: "go".into(),
                    name,
                    kind: RunTargetKind::Binary,
                    build: None,
                    run: vec!["go".into(), "run".into(), package],
                    program: Some(dir.clone()),
                    cwd: root.to_path_buf(),
                    source: Some(dir.clone()),
                    args: Vec::new(),
                }
            })
            .collect();
        targets.extend(tests);
        targets
    }

    fn attach_arguments(&self, pid: u32) -> Option<Value> {
        Some(json!({ "mode": "local", "processId": pid }))
    }

    fn select_tests(&self, target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
        go_select(target, picks)
    }

    fn locate_tests(
        &self,
        target: &RunTarget,
        suite: &Suite,
        names: &[String],
    ) -> Vec<Option<(PathBuf, u32)>> {
        go_locate(target, suite, names)
    }

    /// A `func TestX(t *testing.T)` in a `_test.go` file.
    fn test_at(&self, root: &Path, file: &Path, text: &str, line: usize) -> Option<RunTarget> {
        if !file.to_string_lossy().ends_with("_test.go") {
            return None;
        }
        let declared = text.lines().nth(line)?.trim_start();
        let rest = declared.strip_prefix("func Test")?;
        let name: String = "Test"
            .chars()
            .chain(
                rest.chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_'),
            )
            .collect();
        if !declared.contains("*testing.T") {
            return None;
        }
        let mut target = go_test_target(root, file.parent()?, Some(&name));
        target.source = Some(file.to_path_buf());
        Some(target)
    }

    /// Delve builds the program itself, with the optimizations off that would confuse debugging.
    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch> {
        let Some(program) = &target.program else {
            bail!("{} has no package to debug", target.name)
        };
        let mode = if target.kind == RunTargetKind::Test {
            "test"
        } else {
            "debug"
        };
        Ok(DebugLaunch {
            launch_arguments: json!({
                "mode": mode,
                "program": program,
                "args": target.args,
                "cwd": target.cwd,
                // The program's output comes to us as events, rather than to delve's own stdout.
                "outputMode": "remote",
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
        let folders = folders(root, 3);
        let tests: Vec<RunTarget> = folders
            .iter()
            .filter(|dir| has_odin_tests(dir))
            .map(|dir| odin_test_target(root, dir, &[]))
            .collect();
        let mut targets: Vec<RunTarget> = folders
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
                    source: Some(dir.clone()),
                    args: Vec::new(),
                }
            })
            .collect();
        targets.extend(tests);
        targets
    }

    fn debug_launch(&self, target: &RunTarget) -> Result<DebugLaunch> {
        lldb_launch(target)
    }

    fn attach_arguments(&self, pid: u32) -> Option<Value> {
        Some(json!({ "pid": pid }))
    }

    /// An `@(test)` procedure, run by its package and name.
    fn test_at(&self, root: &Path, file: &Path, text: &str, line: usize) -> Option<RunTarget> {
        let lines: Vec<&str> = text.lines().collect();
        let name = odin_test_name(&lines, line)?;
        let package = lines
            .iter()
            .find_map(|l| l.trim().strip_prefix("package "))?
            .trim();
        let mut target = odin_test_target(root, file.parent()?, &[format!("{package}.{name}")]);
        target.source = Some(file.to_path_buf());
        Some(target)
    }

    fn select_tests(&self, target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
        odin_select(target, picks)
    }

    fn locate_tests(
        &self,
        target: &RunTarget,
        suite: &Suite,
        names: &[String],
    ) -> Vec<Option<(PathBuf, u32)>> {
        odin_locate(target, suite, names)
    }
}

pub struct TypeScriptAdapter;

impl TypeScriptAdapter {
    /// Running one file directly, which is what "run this" means for a script.
    pub fn file_target(&self, root: &Path, file: &Path) -> RunTarget {
        // A test file runs its tests, with the project's runner.
        if is_js_test_file(file)
            && let Some(target) = js_test_target(file, None)
        {
            return target;
        }
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
            source: Some(file.to_path_buf()),
            args: Vec::new(),
        }
    }
}

impl LanguageAdapter for TypeScriptAdapter {
    fn package_id(&self) -> &'static str {
        "typescript"
    }

    /// A running Node opens its inspector when sent SIGUSR1, which this does before js-debug
    /// connects to it.
    fn attach_arguments(&self, pid: u32) -> Option<Value> {
        let signalled = std::process::Command::new("kill")
            .args(["-USR1", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success());
        if !signalled {
            return None;
        }
        // Node opens the port in its own time.
        std::thread::sleep(std::time::Duration::from_millis(400));
        Some(json!({
            "type": "pwa-node",
            "request": "attach",
            // Node's inspector listens on IPv4, and `localhost` can mean IPv6 first.
            "address": "127.0.0.1",
            "port": 9229,
        }))
    }

    /// A `describe`, `it` or `test` on a line of a test file.
    fn test_at(&self, _root: &Path, file: &Path, text: &str, line: usize) -> Option<RunTarget> {
        if !is_js_test_file(file) {
            return None;
        }
        js_test_target(file, Some(js_test_chain(text, line)?))
    }

    fn select_tests(&self, target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
        js_select(target, picks)
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
                        source: None,
                        args: Vec::new(),
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
            // Where compiled files are, so breakpoints in TypeScript resolve through source maps
            // before the compiled file has loaded.
            "outFiles": [
                format!("{}/**/*.(m|c|)js", target.cwd.display()),
                "!**/node_modules/**",
            ],
            // The IDE starts Node, so the program's input and output go through its console.
            "console": "integratedTerminal",
            // What VS Code calls the workspace folder. js-debug builds its defaults from it, like
            // where source maps may be, and without it can't resolve breakpoints through them.
            "__workspaceFolder": target.cwd,
        });
        match (&target.program, target.kind) {
            (Some(program), _) => {
                launch["program"] = json!(program);
                if !target.args.is_empty() {
                    launch["args"] = json!(target.args);
                }
            }
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
        assert_eq!(
            names(&targets),
            [
                "app",
                "extra",
                "generate",
                "all tests",
                "tests in app",
                "tests in lib",
                "tests in gen"
            ]
        );
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
        let root_name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            names(&targets),
            [root_name.as_str(), "cmd/server", "tests in ./cmd/tool"]
        );
        assert_eq!(targets[1].run, ["go", "run", "./cmd/server"]);
        assert_eq!(targets[2].run, ["go", "test", "-json", "./cmd/tool"]);
        let test = GoAdapter
            .debug_launch(&targets[2])
            .unwrap()
            .launch_arguments;
        assert_eq!(test["mode"], "test");
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

    #[test]
    fn a_rust_test_function_is_found_from_its_line() {
        let dir = project(&[
            ("Cargo.toml", "[package]\nname = \"calc\"\n"),
            (
                "src/lib.rs",
                "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(1, 2), 3);\n    }\n}\n",
            ),
        ]);
        let file = dir.path().join("src/lib.rs");
        let text = fs::read_to_string(&file).unwrap();
        assert!(RustAdapter.test_at(dir.path(), &file, &text, 0).is_none());
        let test = RustAdapter.test_at(dir.path(), &file, &text, 5).unwrap();
        assert_eq!(test.name, "test adds");
        assert_eq!(
            test.run.join(" "),
            "cargo test -p calc --no-fail-fast --lib -- --exact tests::adds"
        );
        assert_eq!(test.args, ["--exact", "tests::adds"]);
        let launch = RustAdapter.debug_launch(&test).unwrap().launch_arguments;
        assert_eq!(launch["args"], json!(["--exact", "tests::adds"]));
        let module = RustAdapter.test_at(dir.path(), &file, &text, 3).unwrap();
        assert_eq!(module.name, "tests in calc::tests");
        assert_eq!(
            module.run.join(" "),
            "cargo test -p calc --no-fail-fast --lib -- tests::"
        );
    }

    #[test]
    fn a_go_test_function_is_found_from_its_line() {
        let dir = project(&[
            ("go.mod", "module m"),
            (
                "calc/calc_test.go",
                "package calc\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n}\n",
            ),
        ]);
        let file = dir.path().join("calc/calc_test.go");
        let text = fs::read_to_string(&file).unwrap();
        let test = GoAdapter.test_at(dir.path(), &file, &text, 4).unwrap();
        assert_eq!(test.name, "test TestAdd");
        assert_eq!(
            test.run,
            ["go", "test", "-json", "-run", "^TestAdd$", "./calc"]
        );
        assert_eq!(test.args, ["-test.run", "^TestAdd$", "-test.v"]);
        assert!(GoAdapter.test_at(dir.path(), &file, &text, 0).is_none());
    }
}
