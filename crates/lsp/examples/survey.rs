//! Measures what each language server really does, on a small sample project per language:
//! `cargo run -p gleipnir-lsp --example survey -- <managed-tools-dir>`.
//!
//! Advertising a capability and returning something useful aren't the same, so each feature is
//! actually tried. The output is a table for docs/LANGUAGE_SERVERS.md.

use futures::StreamExt as _;
use gleipnir_lsp::lsp_types::{self as lsp, request as req};
use gleipnir_lsp::{Incoming, LanguageServer, ServerOptions, path_to_uri};
use gleipnir_packages::install::InstallContext;
use gleipnir_packages::tools::{ResolveContext, resolve_tool};
use gleipnir_packages::{detect_sdks, env::user_env, registry};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Sample {
    package: &'static str,
    language_id: &'static str,
    files: &'static [(&'static str, &'static str)],
    /// The file the probes run in.
    main: &'static str,
    /// (probe, text to find, offset into it).
    usage: (&'static str, usize),
    declaration: (&'static str, usize),
    interface_method: (&'static str, usize),
    member_access: (&'static str, usize),
    call_args: (&'static str, usize),
    symbol_query: &'static str,
}

const SAMPLES: &[Sample] = &[
    Sample {
        package: "rust",
        language_id: "rust",
        files: &[
            (
                "Cargo.toml",
                "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "src/main.rs",
                "trait Shape {\n    fn area(&self) -> f64;\n}\n\nstruct Circle {\n    radius: f64,\n}\n\nimpl Shape for Circle {\n    fn area(&self) -> f64 {\n        3.14 * self.radius * self.radius\n    }\n}\n\nfn scale(value: f64, factor: f64) -> f64 {\n    value * factor\n}\n\nfn main() {\n    let circle = Circle { radius: 2.0 };\n    let total = scale(circle.area(), 2.0);\n    let broken: i32 = \"oops\";\n    println!(\"{total} {broken}\");\n}\n",
            ),
        ],
        main: "src/main.rs",
        usage: ("circle.area()", 7),
        declaration: ("fn area(&self) -> f64 {", 3),
        interface_method: ("fn area(&self) -> f64;", 3),
        member_access: ("circle.area()", 7),
        call_args: ("scale(circle", 6),
        symbol_query: "Circle",
    },
    Sample {
        package: "go",
        language_id: "go",
        files: &[
            ("go.mod", "module sample\n\ngo 1.21\n"),
            (
                "main.go",
                "package main\n\nimport \"fmt\"\n\ntype Shape interface {\n\tArea() float64\n}\n\ntype Circle struct {\n\tRadius float64\n}\n\nfunc (c Circle) Area() float64 {\n\treturn 3.14 * c.Radius * c.Radius\n}\n\nfunc scale(value float64, factor float64) float64 {\n\treturn value * factor\n}\n\nfunc main() {\n\tcircle := Circle{Radius: 2}\n\ttotal := scale(circle.Area(), 2)\n\tvar broken int = \"oops\"\n\tfmt.Println(total, broken)\n}\n",
            ),
        ],
        main: "main.go",
        usage: ("circle.Area()", 7),
        declaration: ("func (c Circle) Area()", 16),
        interface_method: ("\tArea() float64\n}", 1),
        member_access: ("circle.Area()", 7),
        call_args: ("scale(circle", 6),
        symbol_query: "Circle",
    },
    Sample {
        package: "typescript",
        language_id: "typescript",
        files: &[
            (
                "tsconfig.json",
                "{ \"compilerOptions\": { \"strict\": true, \"target\": \"es2022\" } }\n",
            ),
            (
                "main.ts",
                "interface Shape {\n  area(): number;\n}\n\nclass Circle implements Shape {\n  constructor(private radius: number) {}\n  area(): number {\n    return 3.14 * this.radius * this.radius;\n  }\n}\n\nfunction scale(value: number, factor: number): number {\n  return value * factor;\n}\n\nconst circle = new Circle(2);\nconst total = scale(circle.area(), 2);\nconst broken: number = \"oops\";\nconsole.log(total, broken);\n",
            ),
        ],
        main: "main.ts",
        usage: ("circle.area()", 7),
        declaration: ("  area(): number {", 2),
        interface_method: ("  area(): number;", 2),
        member_access: ("circle.area()", 7),
        call_args: ("scale(circle", 6),
        symbol_query: "Circle",
    },
    Sample {
        package: "odin",
        language_id: "odin",
        files: &[(
            "main.odin",
            "package main\n\nimport \"core:fmt\"\n\nCircle :: struct {\n\tradius: f64,\n}\n\narea :: proc(c: Circle) -> f64 {\n\treturn 3.14 * c.radius * c.radius\n}\n\nscale :: proc(value: f64, factor: f64) -> f64 {\n\treturn value * factor\n}\n\nmain :: proc() {\n\tcircle := Circle{radius = 2}\n\ttotal := scale(area(circle), 2)\n\tbroken: int = \"oops\"\n\tfmt.println(total, broken, circle.radius)\n}\n",
        )],
        main: "main.odin",
        usage: ("area(circle)", 1),
        declaration: ("area :: proc", 1),
        interface_method: ("area :: proc", 1),
        member_access: ("circle.radius)", 7),
        call_args: ("scale(area", 6),
        symbol_query: "Circle",
    },
];

fn position(text: &str, (needle, offset): (&str, usize)) -> lsp::Position {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in sample"))
        + offset;
    let line = text[..at].matches('\n').count() as u32;
    let column = (at - text[..at].rfind('\n').map_or(0, |i| i + 1)) as u32;
    lsp::Position::new(line, column)
}

fn non_empty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        _ => true,
    }
}

/// Tries a request until it returns something or time runs out. Servers answer emptily while
/// still indexing, so the first answer isn't always the real one.
async fn probe<R: req::Request>(
    server: &Arc<LanguageServer>,
    params: R::Params,
    patience: Duration,
) -> (bool, String)
where
    R::Params: serde::Serialize + Clone,
    R::Result: serde::de::DeserializeOwned + serde::Serialize,
{
    let started = Instant::now();
    loop {
        match server.request::<R>(params.clone()).await {
            Ok(result) => {
                let value = serde_json::to_value(&result).unwrap_or(Value::Null);
                if non_empty(&value) {
                    let text = value.to_string();
                    return (true, text.chars().take(90).collect());
                }
                if started.elapsed() > patience {
                    return (false, "empty".into());
                }
            }
            Err(error) => {
                if started.elapsed() > patience {
                    return (false, format!("error: {error}"));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn main() -> anyhow::Result<()> {
    let managed = PathBuf::from(std::env::args().nth(1).expect("the managed tools folder"));
    let base = user_env();
    let only = std::env::args().nth(2);
    for sample in SAMPLES {
        if only.as_deref().is_some_and(|o| o != sample.package) {
            continue;
        }
        let dir = tempfile::tempdir()?;
        let root = std::fs::canonicalize(dir.path())?;
        for (path, text) in sample.files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap())?;
            std::fs::write(full, text)?;
        }
        let package = registry().package(sample.package).unwrap();
        let sdk_spec = package.manifest.sdk.as_ref();
        let sdk = sdk_spec.and_then(|s| detect_sdks(s, &base).into_iter().next());
        let env = match (sdk_spec, &sdk) {
            (Some(spec), Some(sdk)) => sdk.env(spec, &base),
            _ => base.clone(),
        };
        let server_spec = package.manifest.server.as_ref().unwrap();
        let tool = &package.manifest.tools[&server_spec.tool];
        let resolution = resolve_tool(
            &server_spec.tool,
            tool,
            &ResolveContext {
                project_root: Some(&root),
                sdk: sdk.as_ref(),
                explicit: None,
                managed_dir: &managed,
                env: &env,
            },
        );
        let Some(program) = resolution.path() else {
            println!("## {}: {} not found", sample.package, server_spec.tool);
            continue;
        };
        let _ = InstallContext {
            managed_dir: &managed,
            env: &env,
            rust_toolchain: None,
        };
        let (server, mut incoming) = LanguageServer::spawn(
            sample.package,
            ServerOptions {
                program: program.to_path_buf(),
                args: server_spec.args.clone(),
                env: env
                    .vars()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                cwd: root.clone(),
            },
        )?;
        let diagnostics = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = diagnostics.clone();
        let responder = server.clone();
        std::thread::spawn(move || {
            futures::executor::block_on(async {
                while let Some(message) = incoming.next().await {
                    match message {
                        Incoming::Notification { method, params }
                            if method == "textDocument/publishDiagnostics" =>
                        {
                            for d in params["diagnostics"].as_array().into_iter().flatten() {
                                seen.lock().unwrap().push(
                                    d["message"]
                                        .as_str()
                                        .unwrap_or("")
                                        .lines()
                                        .next()
                                        .unwrap_or("")
                                        .to_string(),
                                );
                            }
                        }
                        Incoming::Request { id, .. } => responder.respond(id, Ok(Value::Null)),
                        Incoming::Exited { stderr } => {
                            println!("   (exited: {})", stderr.lines().last().unwrap_or(""));
                            break;
                        }
                        _ => {}
                    }
                }
            })
        });

        let started = Instant::now();
        let init = futures::executor::block_on(server.initialize(&root, None));
        let caps = match init {
            Ok(result) => result.capabilities,
            Err(error) => {
                println!("## {}: initialize failed: {error:#}", sample.package);
                continue;
            }
        };
        let main_path = root.join(sample.main);
        let text = std::fs::read_to_string(&main_path)?;
        let uri = path_to_uri(&main_path);
        server.notify::<lsp::notification::DidOpenTextDocument>(lsp::DidOpenTextDocumentParams {
            text_document: lsp::TextDocumentItem {
                uri: uri.clone(),
                language_id: sample.language_id.into(),
                version: 1,
                text: text.clone(),
            },
        });
        let doc = lsp::TextDocumentIdentifier { uri: uri.clone() };
        let at = |p: lsp::Position| lsp::TextDocumentPositionParams {
            text_document: doc.clone(),
            position: p,
        };
        let wd = lsp::WorkDoneProgressParams::default;
        let pr = lsp::PartialResultParams::default;
        // The first real answer can take a while as the server indexes, so it gets the patience.
        let warm = Duration::from_secs(90);
        let quick = Duration::from_secs(8);

        println!(
            "## {} ({}), started in {:?}",
            sample.package,
            program.display(),
            started.elapsed()
        );
        let results: Vec<(&str, bool, (bool, String))> = futures::executor::block_on(async {
            let mut results = Vec::new();
            results.push((
                "definition",
                caps.definition_provider.is_some(),
                probe::<req::GotoDefinition>(
                    &server,
                    lsp::GotoDefinitionParams {
                        text_document_position_params: at(position(&text, sample.usage)),
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                    },
                    warm,
                )
                .await,
            ));
            results.push((
                "implementation",
                caps.implementation_provider.is_some(),
                probe::<req::GotoImplementation>(
                    &server,
                    lsp::GotoDefinitionParams {
                        text_document_position_params: at(position(&text, sample.interface_method)),
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "type definition",
                caps.type_definition_provider.is_some(),
                probe::<req::GotoTypeDefinition>(
                    &server,
                    lsp::GotoDefinitionParams {
                        text_document_position_params: at(position(&text, ("circle", 0))),
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "references",
                caps.references_provider.is_some(),
                probe::<req::References>(
                    &server,
                    lsp::ReferenceParams {
                        text_document_position: at(position(&text, sample.declaration)),
                        context: lsp::ReferenceContext {
                            include_declaration: true,
                        },
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "hover",
                caps.hover_provider.is_some(),
                probe::<req::HoverRequest>(
                    &server,
                    lsp::HoverParams {
                        text_document_position_params: at(position(&text, sample.usage)),
                        work_done_progress_params: wd(),
                    },
                    quick,
                )
                .await,
            ));
            let mut completion_position = position(&text, sample.member_access);
            completion_position.character += 1;
            results.push((
                "completion",
                caps.completion_provider.is_some(),
                probe::<req::Completion>(
                    &server,
                    lsp::CompletionParams {
                        text_document_position: at(completion_position),
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                        context: None,
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "signature help",
                caps.signature_help_provider.is_some(),
                probe::<req::SignatureHelpRequest>(
                    &server,
                    lsp::SignatureHelpParams {
                        text_document_position_params: at(position(&text, sample.call_args)),
                        work_done_progress_params: wd(),
                        context: None,
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "document symbols",
                caps.document_symbol_provider.is_some(),
                probe::<req::DocumentSymbolRequest>(
                    &server,
                    lsp::DocumentSymbolParams {
                        text_document: doc.clone(),
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "workspace symbols",
                caps.workspace_symbol_provider.is_some(),
                probe::<req::WorkspaceSymbolRequest>(
                    &server,
                    lsp::WorkspaceSymbolParams {
                        query: sample.symbol_query.into(),
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "rename",
                caps.rename_provider.is_some(),
                probe::<req::Rename>(
                    &server,
                    lsp::RenameParams {
                        text_document_position: at(position(&text, sample.declaration)),
                        new_name: "size".into(),
                        work_done_progress_params: wd(),
                    },
                    quick,
                )
                .await,
            ));
            results.push((
                "formatting",
                caps.document_formatting_provider.is_some(),
                probe::<req::Formatting>(
                    &server,
                    lsp::DocumentFormattingParams {
                        text_document: doc.clone(),
                        options: lsp::FormattingOptions {
                            tab_size: 4,
                            insert_spaces: true,
                            ..Default::default()
                        },
                        work_done_progress_params: wd(),
                    },
                    quick,
                )
                .await,
            ));
            let broken = position(&text, ("broken", 0));
            results.push((
                "code actions",
                caps.code_action_provider.is_some(),
                probe::<req::CodeActionRequest>(
                    &server,
                    lsp::CodeActionParams {
                        text_document: doc.clone(),
                        range: lsp::Range::new(
                            broken,
                            lsp::Position::new(broken.line, broken.character + 6),
                        ),
                        context: lsp::CodeActionContext::default(),
                        work_done_progress_params: wd(),
                        partial_result_params: pr(),
                    },
                    quick,
                )
                .await,
            ));
            results
        });
        // Diagnostics are pushed, not asked for, and some servers only run checks on save.
        server.notify::<lsp::notification::DidSaveTextDocument>(lsp::DidSaveTextDocumentParams {
            text_document: doc.clone(),
            text: None,
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        while diagnostics.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
        println!("| feature | advertised | works | sample |");
        println!("|---|---|---|---|");
        for (name, advertised, (works, sample)) in results {
            println!(
                "| {name} | {} | {} | `{}` |",
                if advertised { "yes" } else { "no" },
                if works { "yes" } else { "no" },
                sample.replace('|', "\\|")
            );
        }
        let found = diagnostics.lock().unwrap().clone();
        println!(
            "| diagnostics | push | {} | `{}` |",
            if found.is_empty() { "no" } else { "yes" },
            found
                .first()
                .cloned()
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect::<String>()
        );
        println!(
            "encoding: {:?}\n",
            caps.position_encoding.map(|e| e.as_str().to_string())
        );
        futures::executor::block_on(server.shutdown());
    }
    Ok(())
}
