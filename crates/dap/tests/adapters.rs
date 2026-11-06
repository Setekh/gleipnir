//! The real adapters, headless: for each language, build a small program, stop at a breakpoint,
//! read a local, step over it and check where that lands. A test whose tools aren't installed
//! says so and passes, so the suite runs anywhere; `GLEIPNIR_TOOLS` points at a managed tools
//! folder other than the default.

use futures::StreamExt as _;
use futures::executor::block_on;
use gleipnir_dap::session::{self as s};
use gleipnir_dap::{
    AdapterCommand, DebugAdapter, FileBreakpoints, Incoming, LaunchRequest, SourceBreakpoint,
    StartDebugging, Step,
};
use gleipnir_packages::adapters::{TypeScriptAdapter, debugger_command};
use gleipnir_packages::env::user_env;
use gleipnir_packages::tools::{ResolveContext, resolve_tool};
use gleipnir_packages::{Env, RunTarget, adapter_for, detect_sdks, managed_tools_dir, registry};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// How long anything may take. Generous, as the first delve or cargo build can be slow.
const PATIENCE: Duration = Duration::from_secs(60);

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn project(files: &[(&str, &str)]) -> Project {
    // Not a dot-folder, which globs like js-debug's `outFiles` would skip.
    let dir = tempfile::Builder::new()
        .prefix("gleipnir-")
        .tempdir()
        .unwrap();
    // Canonical, as adapters report canonical paths and macOS's temp folder is a symlink.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (path, text) in files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    Project { _dir: dir, root }
}

/// A package's debugger, ready to start, and the environment its builds use.
struct Tools {
    command: AdapterCommand,
    adapter_id: String,
    env: Env,
}

fn tools(package_id: &str, root: &Path) -> Option<Tools> {
    let package = registry().package(package_id)?;
    let base = user_env();
    let sdk_spec = package.manifest.sdk.as_ref();
    let sdk = sdk_spec.and_then(|spec| detect_sdks(spec, &base).into_iter().next());
    let env = match (sdk_spec, &sdk) {
        (Some(spec), Some(sdk)) => sdk.env(spec, &base),
        _ => base.clone(),
    };
    let spec = package.manifest.debugger.as_ref()?;
    let managed = std::env::var_os("GLEIPNIR_TOOLS")
        .map(PathBuf::from)
        .unwrap_or_else(managed_tools_dir);
    let resolution = resolve_tool(
        &spec.tool,
        &package.manifest.tools[&spec.tool],
        &ResolveContext {
            project_root: Some(root),
            sdk: sdk.as_ref(),
            explicit: None,
            managed_dir: &managed,
            env: &env,
        },
    );
    let Some(tool) = resolution.path() else {
        eprintln!("skipped: {} isn't installed", spec.tool);
        return None;
    };
    let sdk_binary = match (sdk_spec, &sdk) {
        (Some(spec), Some(sdk)) => Some(sdk.root.join(&spec.binary)),
        _ => None,
    };
    let (program, args) = debugger_command(spec, tool, sdk_binary.as_deref()).ok()?;
    Some(Tools {
        command: AdapterCommand {
            program,
            args,
            env: env
                .vars()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            cwd: root.to_path_buf(),
            tcp: spec.transport == gleipnir_packages::manifest::Transport::Tcp,
        },
        adapter_id: spec.adapter_id().to_string(),
        env,
    })
}

fn build(target: &RunTarget, env: &Env) {
    let Some(build) = &target.build else { return };
    if let Some(folder) = target.program.as_deref().and_then(Path::parent) {
        std::fs::create_dir_all(folder).unwrap();
    }
    let mut command = Command::new(&build[0]);
    command.args(&build[1..]).current_dir(&target.cwd);
    env.apply(&mut command);
    let output = command.output().expect("the build starts");
    assert!(
        output.status.success(),
        "{build:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

enum Message {
    Stopped(Arc<DebugAdapter>, serde_json::Value),
    Output(String),
    Ended,
}

/// Forwards one connection's events, and starts child sessions as js-debug asks for them.
fn pump(
    adapter: Arc<DebugAdapter>,
    mut incoming: futures::channel::mpsc::UnboundedReceiver<Incoming>,
    adapter_id: String,
    breakpoints: Vec<FileBreakpoints>,
    tx: mpsc::Sender<Message>,
) {
    std::thread::spawn(move || {
        while let Some(message) = block_on(incoming.next()) {
            if std::env::var_os("DAP_TRACE").is_some() {
                eprintln!(
                    "<- {}: {}",
                    adapter.name(),
                    format!("{message:?}").chars().take(300).collect::<String>()
                );
            }
            match message {
                Incoming::Event { event, body } => match event.as_str() {
                    "stopped" => drop(tx.send(Message::Stopped(adapter.clone(), body))),
                    "output" => drop(tx.send(Message::Output(
                        body["output"].as_str().unwrap_or("").to_string(),
                    ))),
                    "terminated" => drop(tx.send(Message::Ended)),
                    _ => {}
                },
                Incoming::Request {
                    seq,
                    command,
                    arguments,
                } if command == "startDebugging" => {
                    adapter.respond(seq, &command, Ok(serde_json::json!({})));
                    let start: StartDebugging = serde_json::from_value(arguments).unwrap();
                    let (child, child_incoming) = adapter.connect_again("child").unwrap();
                    pump(
                        child.clone(),
                        child_incoming,
                        adapter_id.clone(),
                        breakpoints.clone(),
                        tx.clone(),
                    );
                    let request = LaunchRequest {
                        adapter_id: adapter_id.clone(),
                        request: start.request,
                        arguments: start.configuration,
                    };
                    let breakpoints = breakpoints.clone();
                    std::thread::spawn(move || {
                        block_on(s::start(&child, &request, &breakpoints)).unwrap();
                    });
                }
                Incoming::Request { seq, command, .. } => {
                    adapter.respond(seq, &command, Err("not supported".into()))
                }
                Incoming::Exited { .. } => {
                    tx.send(Message::Ended).ok();
                    return;
                }
            }
        }
    });
}

/// What the test expects at the breakpoint.
struct Expect {
    file: &'static str,
    line: u32,
    local: &'static str,
    value: &'static str,
}

/// Debugs a target: stop at the breakpoint, check the frame and a local, step over, check the line
/// moved on, then let the program finish. Returns the program's output.
fn debug(project: &Project, package_id: &str, target: RunTarget, expect: Expect) -> String {
    let Some(tools) = tools(package_id, &project.root) else {
        return String::new();
    };
    build(&target, &tools.env);
    let launch = adapter_for(package_id)
        .unwrap()
        .debug_launch(&target)
        .unwrap();
    let source = project.root.join(expect.file);
    let breakpoints = vec![FileBreakpoints {
        path: source.clone(),
        breakpoints: vec![SourceBreakpoint {
            line: expect.line,
            ..Default::default()
        }],
    }];

    let (adapter, incoming) = DebugAdapter::start(package_id, &tools.command).unwrap();
    let (tx, rx) = mpsc::channel();
    pump(
        adapter.clone(),
        incoming,
        tools.adapter_id.clone(),
        breakpoints.clone(),
        tx,
    );
    let request = LaunchRequest {
        adapter_id: tools.adapter_id.clone(),
        request: "launch".into(),
        arguments: launch.launch_arguments,
    };
    if let Err(error) = block_on(s::start(&adapter, &request, &breakpoints)) {
        panic!("{error}\n{}", adapter.output());
    }

    let mut output = String::new();
    let next_stop = |output: &mut String| loop {
        match rx.recv_timeout(PATIENCE) {
            Ok(Message::Stopped(adapter, body)) => return (adapter, body),
            Ok(Message::Output(text)) => output.push_str(&text),
            Ok(Message::Ended) => {}
            Err(_) => panic!("never stopped; output so far:\n{output}"),
        }
    };

    let (session, stopped) = next_stop(&mut output);
    assert_eq!(stopped["reason"], "breakpoint", "{stopped}");
    let thread = stopped["threadId"].as_i64().unwrap();
    let frames = block_on(s::stack_trace(&session, thread, 20)).unwrap();
    let top = &frames[0];
    assert_eq!(top.line, expect.line, "{frames:?}");
    assert_eq!(
        top.source.as_ref().and_then(|s| s.path.clone()),
        Some(source.clone()),
        "{frames:?}"
    );
    let scopes = block_on(s::scopes(&session, top.id)).unwrap();
    let locals = block_on(s::variables(&session, scopes[0].variables_reference)).unwrap();
    let local = locals
        .iter()
        .find(|v| v.name == expect.local)
        .unwrap_or_else(|| panic!("no {} in {locals:?}", expect.local));
    assert_eq!(local.value, expect.value, "{local:?}");
    let evaluated = block_on(s::evaluate(&session, expect.local, Some(top.id), "watch")).unwrap();
    assert_eq!(evaluated.result, expect.value);

    block_on(s::resume(&session, thread, Step::Over)).unwrap();
    let (session, stopped) = next_stop(&mut output);
    assert_eq!(stopped["reason"], "step", "{stopped}");
    let frames = block_on(s::stack_trace(&session, thread, 20)).unwrap();
    assert_eq!(frames[0].line, expect.line + 1, "{frames:?}");

    block_on(s::resume(&session, thread, Step::Continue)).unwrap();
    // Output may trail the end of the program a little.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while let Ok(message) =
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        if let Message::Output(text) = message {
            output.push_str(&text);
        }
    }
    // js-debug doesn't answer once its sessions have ended, so this can't wait forever.
    let (done, waited) = mpsc::channel();
    let disconnecting = adapter.clone();
    std::thread::spawn(move || {
        block_on(s::disconnect(&disconnecting, true)).ok();
        done.send(()).ok();
    });
    waited.recv_timeout(Duration::from_secs(3)).ok();
    output
}

#[test]
fn rust_through_lldb_dap() {
    let project = project(&[
        (
            "Cargo.toml",
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "src/main.rs",
            "struct Point { x: i32, y: i32 }\n\nfn main() {\n    let count = 3;\n    let point = Point { x: 1, y: 2 };\n    let total = count + point.x + point.y;\n    println!(\"total {total}\");\n}\n",
        ),
    ]);
    let target = adapter_for("rust")
        .unwrap()
        .run_targets(&project.root)
        .remove(0);
    let output = debug(
        &project,
        "rust",
        target,
        Expect {
            file: "src/main.rs",
            line: 6,
            local: "count",
            value: "3",
        },
    );
    eprintln!("rust output: {output:?}");
}

#[test]
fn go_through_delve() {
    let project = project(&[
        ("go.mod", "module sample\n\ngo 1.21\n"),
        (
            "main.go",
            "package main\n\nimport \"fmt\"\n\ntype Point struct{ X, Y int }\n\nfunc main() {\n\tcount := 3\n\tpoint := Point{X: 1, Y: 2}\n\ttotal := count + point.X + point.Y\n\tfmt.Println(\"total\", total)\n}\n",
        ),
    ]);
    let target = adapter_for("go")
        .unwrap()
        .run_targets(&project.root)
        .remove(0);
    let output = debug(
        &project,
        "go",
        target,
        Expect {
            file: "main.go",
            line: 10,
            local: "count",
            value: "3",
        },
    );
    eprintln!("go output: {output:?}");
}

#[test]
fn odin_through_lldb_dap() {
    let project = project(&[(
        "app/main.odin",
        "package main\n\nimport \"core:fmt\"\n\nPoint :: struct { x, y: int }\n\nmain :: proc() {\n\tcount := 3\n\tpoint := Point{1, 2}\n\ttotal := count + point.x + point.y\n\tfmt.println(\"total\", total)\n}\n",
    )]);
    let Some(target) = adapter_for("odin")
        .unwrap()
        .run_targets(&project.root)
        .into_iter()
        .next()
    else {
        panic!("no Odin target")
    };
    if which("odin").is_none() {
        eprintln!("skipped: odin isn't installed");
        return;
    }
    let output = debug(
        &project,
        "odin",
        target,
        Expect {
            file: "app/main.odin",
            line: 10,
            local: "count",
            value: "3",
        },
    );
    eprintln!("odin output: {output:?}");
}

const TS_PROGRAM: &str = "interface Point { x: number; y: number }\n\nfunction main(): void {\n  const count: number = 3;\n  const point: Point = { x: 1, y: 2 };\n  const total = count + point.x + point.y;\n  console.log(\"total\", total);\n}\n\nmain();\n";

#[test]
fn javascript_through_js_debug() {
    let project = project(&[(
        "main.js",
        "function main() {\n  const count = 3;\n  const point = { x: 1, y: 2 };\n  const total = count + point.x + point.y;\n  console.log(\"total\", total);\n}\n\nmain();\n",
    )]);
    let target = TypeScriptAdapter.file_target(&project.root, &project.root.join("main.js"));
    let output = debug(
        &project,
        "typescript",
        target,
        Expect {
            file: "main.js",
            line: 4,
            local: "count",
            value: "3",
        },
    );
    if !output.is_empty() {
        assert!(output.contains("total 6"), "{output:?}");
    }
}

#[test]
fn typescript_run_directly_by_node() {
    // Node strips the types itself, so lines are the TypeScript's own.
    let project = project(&[("main.ts", TS_PROGRAM)]);
    let target = TypeScriptAdapter.file_target(&project.root, &project.root.join("main.ts"));
    debug(
        &project,
        "typescript",
        target,
        Expect {
            file: "main.ts",
            line: 6,
            local: "count",
            value: "3",
        },
    );
}

#[test]
fn typescript_compiled_with_source_maps() {
    let Some(tsc) = tsc() else {
        eprintln!("skipped: no tsc");
        return;
    };
    let project = project(&[
        ("src/main.ts", TS_PROGRAM),
        (
            "tsconfig.json",
            "{ \"compilerOptions\": { \"target\": \"es2022\", \"module\": \"commonjs\", \"sourceMap\": true, \"outDir\": \"out\", \"rootDir\": \"src\" } }\n",
        ),
    ]);
    let status = Command::new(&tsc)
        .current_dir(&project.root)
        .envs(user_env().vars())
        .status()
        .unwrap();
    assert!(status.success());
    // The compiled file runs; the breakpoint is in the TypeScript, found through the map.
    let target = TypeScriptAdapter.file_target(&project.root, &project.root.join("out/main.js"));
    debug(
        &project,
        "typescript",
        target,
        Expect {
            file: "src/main.ts",
            line: 6,
            local: "count",
            value: "3",
        },
    );
}

fn which(binary: &str) -> Option<PathBuf> {
    user_env().which(binary)
}

/// TypeScript's compiler, from the managed language server install if there is one.
fn tsc() -> Option<PathBuf> {
    let managed = std::env::var_os("GLEIPNIR_TOOLS")
        .map(PathBuf::from)
        .unwrap_or_else(managed_tools_dir);
    let dir = managed.join("typescript-language-server");
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join("node_modules/.bin/tsc"))
        .find(|p| p.exists())
        .or_else(|| which("tsc"))
}
