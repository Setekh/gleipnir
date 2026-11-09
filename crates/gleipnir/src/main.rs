//! Gleipnir: an IDE for TypeScript, JavaScript, Odin, Rust and Go.

mod driver;

use gleipnir_core::BufferStore;
use gleipnir_workspace::{
    GlobalStore, NewFile, OpenFile, OpenFolder, Quit, SaveAs, SplitDown, SplitRight, WorkspaceInit,
    last_project, load_user_keymap, open_project_window, project_root_for,
};
use gpui::{App, AppContext as _, Menu, MenuItem};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// How often to check open files for external changes. The project watcher only reports on files
/// inside the project, and open files can be anywhere.
/// A safety net only: open files' folders are watched, so changes normally show at once.
const DISK_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// What the command line asked for.
struct LaunchTarget {
    root: Option<PathBuf>,
    file: Option<PathBuf>,
}

/// A folder opens as a project. A file opens in the project it belongs to. With neither, the last
/// project comes back.
fn launch_target(argument: Option<PathBuf>) -> LaunchTarget {
    match argument {
        Some(path) if path.is_dir() => LaunchTarget {
            root: Some(path),
            file: None,
        },
        Some(path) => LaunchTarget {
            root: project_root_for(&absolute(&path)),
            file: Some(path),
        },
        None => LaunchTarget {
            root: last_project(),
            file: None,
        },
    }
}

fn absolute(path: &std::path::Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Makes the theme the settings choose the one in use.
fn apply_theme(cx: &mut App) {
    let theme = gleipnir_ui::Theme::from_settings(
        gleipnir_core::settings(cx),
        cx.global::<gleipnir_ui::Themes>(),
    );
    cx.set_global(theme);
}

/// How often the themes folder and the settings file are looked at for changes made outside.
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// Reads themes and settings again when their files change, so a theme made or switched by hand
/// or by an AI agent shows at once. Looking costs a few file times a second.
fn watch_settings_and_themes(themes_dir: PathBuf, cx: &mut App) {
    let settings_path = gleipnir_packages::settings::settings_path();
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut themes_seen = gleipnir_ui::themes::fingerprint(&themes_dir);
    let mut settings_seen = modified(&settings_path);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(CONFIG_POLL).await;
            let themes_now = gleipnir_ui::themes::fingerprint(&themes_dir);
            if themes_now != themes_seen {
                themes_seen = themes_now;
                let themes = gleipnir_ui::Themes::load(&themes_dir);
                cx.update(|cx| cx.set_global(themes));
            }
            let settings_now = modified(&settings_path);
            if settings_now != settings_seen {
                settings_seen = settings_now;
                cx.update(gleipnir_core::reload_settings);
            }
        }
    })
    .detach();
}

/// Binds every feature's default keys, then the person's own, which win. Returns problems with
/// the person's keymap file.
fn bind_all_keys(cx: &mut App) -> Vec<String> {
    gleipnir_ui::bind_text_input_keys(cx);
    gleipnir_ui::bind_context_menu_keys(cx);
    gleipnir_editor::bind_keys(cx);
    gleipnir_workspace::bind_keys(cx);
    gleipnir_project_panel::bind_keys(cx);
    gleipnir_search::bind_keys(cx);
    gleipnir_settings_ui::bind_keys(cx);
    gleipnir_code_nav::bind_keys(cx);
    gleipnir_debugger_ui::bind_keys(cx);
    gleipnir_git_ui::bind_keys(cx);
    gleipnir_terminal::bind_keys(cx);
    // Loaded last, so that it overrides the defaults.
    load_user_keymap(&gleipnir_workspace::user_keymap_path(), cx)
}

fn main() {
    // `gleipnir mcp` is what coding agents start as Gleipnir's MCP server: a pipe to a window's
    // socket, which must start at once, so it's handled before anything else is set up.
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|first| first == "mcp") {
        std::process::exit(gleipnir_agent::mcp_main(args.collect()));
    }
    let target = launch_target(std::env::args_os().nth(1).map(PathBuf::from));
    let script = std::env::var_os("GLEIPNIR_SCRIPT").map(PathBuf::from);
    // Asking the login shell for its environment takes about a second, so start it now. An app
    // started from the Dock has a bare PATH without it, and wouldn't find any tools.
    gleipnir_packages::env::warm_up();

    gpui_platform::application()
        .with_assets(gleipnir_ui::Assets)
        .run(move |cx: &mut App| {
            gleipnir_core::init_settings(cx);
            let themes_dir = gleipnir_ui::themes_dir();
            // The folder says how to make a theme, for people and AI agents alike.
            gleipnir_ui::themes::write_guide(&themes_dir).ok();
            cx.set_global(gleipnir_ui::Themes::load(&themes_dir));
            apply_theme(cx);
            watch_settings_and_themes(themes_dir, cx);
            let keymap_errors = bind_all_keys(cx);
            let keymap_path = gleipnir_workspace::user_keymap_path();
            cx.set_global(gleipnir_workspace::KeymapReload(Rc::new(|cx| {
                cx.clear_key_bindings();
                let errors = bind_all_keys(cx);
                set_menus(cx);
                errors
            })));
            set_menus(cx);
            // Agents started in the terminal get the window's tools.
            gleipnir_agent::launch::init(cx);

            cx.on_action(|_: &Quit, cx| cx.quit());
            // The local model's server goes with the app.
            cx.on_app_quit(|_| async { gleipnir_suggest::llama::shutdown() })
                .detach();
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            // This takes tens of milliseconds, which the first highlighted frame shouldn't pay for.
            cx.background_spawn(async { gleipnir_syntax::precompile_queries() })
                .detach();

            let store = cx.new(|cx| {
                let mut store = BufferStore::new();
                store.start_watching(cx);
                store.start_polling(DISK_POLL_INTERVAL, cx);
                store
            });
            cx.set_global(GlobalStore(store.clone()));
            // Settings take effect as they change: the theme and fonts, and open files'
            // indentation.
            cx.observe_global::<gleipnir_core::AppSettings>(move |cx| {
                apply_theme(cx);
                store.update(cx, |store, cx| store.apply_code_style(cx));
                cx.refresh_windows();
            })
            .detach();
            cx.observe_global::<gleipnir_ui::Themes>(|cx| {
                apply_theme(cx);
                cx.refresh_windows();
            })
            .detach();
            cx.set_global(WorkspaceInit(Rc::new(|workspace, window, cx| {
                gleipnir_project_panel::add_to_workspace(workspace, window, cx);
                gleipnir_search::add_to_workspace(workspace, window, cx);
                gleipnir_settings_ui::add_to_workspace(workspace, window, cx);
                gleipnir_code_nav::add_to_workspace(workspace, window, cx);
                gleipnir_debugger_ui::add_to_workspace(workspace, window, cx);
                gleipnir_git_ui::add_to_workspace(workspace, window, cx);
                gleipnir_suggest::add_to_workspace(workspace, window, cx);
                gleipnir_agent::add_to_workspace(workspace, window, cx);
                gleipnir_terminal::add_to_workspace(workspace, window, cx);
            })));

            let window = open_project_window(target.root.clone(), false, cx);
            if let Some(file) = target.file.clone() {
                window
                    .update(cx, |workspace, window, cx| {
                        workspace.open_path(file, window, cx).detach();
                    })
                    .ok();
            }
            if !keymap_errors.is_empty() {
                let message = format!("{}: {}", keymap_path.display(), keymap_errors.join("; "));
                eprintln!("{message}");
                window
                    .update(cx, |workspace, _, cx| workspace.show_error(message, cx))
                    .ok();
            }

            if let Some(script) = script.clone() {
                driver::run(script, window, cx);
            } else {
                cx.activate(true);
            }
        });
}

fn set_menus(cx: &mut App) {
    cx.set_menus([
        Menu {
            name: "Gleipnir".into(),
            items: vec![
                MenuItem::action("Settings…", gleipnir_settings_ui::OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New", NewFile),
                MenuItem::action("Open File…", OpenFile),
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::separator(),
                MenuItem::action("Save", gleipnir_editor::Save),
                MenuItem::action("Save As…", SaveAs),
                MenuItem::separator(),
                MenuItem::action("Languages & Tools…", gleipnir_settings_ui::OpenLanguages),
                MenuItem::action("Install Package…", gleipnir_settings_ui::InstallPackage),
            ],
            disabled: false,
        },
        Menu {
            name: "Navigate".into(),
            items: vec![
                MenuItem::action("Search Everywhere", gleipnir_search::SearchEverywhere),
                MenuItem::action("Go to File…", gleipnir_search::GoToFile),
                MenuItem::action("Go to Class…", gleipnir_search::GoToClass),
                MenuItem::action("Go to Symbol…", gleipnir_search::GoToSymbol),
                MenuItem::action("Find Action…", gleipnir_search::FindAction),
                MenuItem::action("Recent Files", gleipnir_search::RecentFiles),
                MenuItem::separator(),
                MenuItem::action("Back", gleipnir_workspace::NavigateBack),
                MenuItem::action("Forward", gleipnir_workspace::NavigateForward),
                MenuItem::separator(),
                MenuItem::action(
                    "Declaration or Usages",
                    gleipnir_code_nav::GoToDeclarationOrUsages,
                ),
                MenuItem::action("Implementation(s)", gleipnir_code_nav::GoToImplementation),
                MenuItem::action("Type Declaration", gleipnir_code_nav::GoToTypeDeclaration),
                MenuItem::action("File Structure", gleipnir_code_nav::FileStructure),
                MenuItem::action("Next Problem", gleipnir_editor::NextProblem),
                MenuItem::action("Previous Problem", gleipnir_editor::PreviousProblem),
            ],
            disabled: false,
        },
        Menu {
            name: "Code".into(),
            items: vec![
                MenuItem::action("Completion", gleipnir_editor::ShowCompletions),
                MenuItem::action("Quick Documentation", gleipnir_editor::ShowHover),
                MenuItem::action("Parameter Info", gleipnir_editor::ShowSignatureHelp),
                MenuItem::action("Show Intentions", gleipnir_code_nav::ShowIntentions),
                MenuItem::separator(),
                MenuItem::action("Find Usages", gleipnir_code_nav::FindUsages),
                MenuItem::action("Show Usages", gleipnir_code_nav::ShowUsages),
                MenuItem::action("Rename…", gleipnir_code_nav::Rename),
                MenuItem::action("Reformat Code", gleipnir_code_nav::Reformat),
                MenuItem::separator(),
                MenuItem::action(
                    "Restart Language Server",
                    gleipnir_code_nav::RestartLanguageServer,
                ),
            ],
            disabled: false,
        },
        Menu {
            name: "Git".into(),
            items: {
                use gleipnir_git_ui as g;
                vec![
                    MenuItem::action("Commit…", g::Commit),
                    MenuItem::action("Push…", g::Push),
                    MenuItem::action("Update Project…", g::UpdateProject),
                    MenuItem::separator(),
                    MenuItem::action("Show Diff", g::ShowDiff),
                    MenuItem::action("Rollback Lines", g::RollbackLines),
                    MenuItem::action("Annotate with Git Blame", g::ToggleBlame),
                    MenuItem::separator(),
                    MenuItem::action("Continue Rebase or Merge", g::ContinueOperation),
                    MenuItem::action("Abort Rebase or Merge", g::AbortOperation),
                    MenuItem::separator(),
                    MenuItem::action("Stash Changes…", g::StashChanges),
                    MenuItem::action("Unstash Changes…", g::UnstashChanges),
                    MenuItem::separator(),
                    MenuItem::action("Show Log", g::ShowLog),
                ]
            },
            disabled: false,
        },
        Menu {
            name: "Run".into(),
            items: {
                use gleipnir_debugger_ui as d;
                vec![
                    MenuItem::action("Run", d::Run),
                    MenuItem::action("Debug", d::Debug),
                    MenuItem::action("Run…", d::RunConfiguration),
                    MenuItem::action("Debug…", d::DebugConfiguration),
                    MenuItem::action("Run Context Configuration", d::RunFile),
                    MenuItem::action("Debug Context Configuration", d::DebugFile),
                    MenuItem::action("Rerun", d::Rerun),
                    MenuItem::action("Rerun Failed Tests", d::RerunFailedTests),
                    MenuItem::action("Stop", d::Stop),
                    MenuItem::separator(),
                    MenuItem::action("Edit Configuration…", d::EditConfiguration),
                    MenuItem::action("Attach to Process…", d::AttachToProcess),
                    MenuItem::separator(),
                    MenuItem::action("Step Over", d::StepOver),
                    MenuItem::action("Step Into", d::StepInto),
                    MenuItem::action("Step Out", d::StepOut),
                    MenuItem::action("Run to Cursor", d::RunToCursor),
                    MenuItem::action("Resume", d::Resume),
                    MenuItem::action("Pause", d::Pause),
                    MenuItem::action("Evaluate Expression…", d::EvaluateExpression),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Breakpoint", d::ToggleBreakpoint),
                    MenuItem::action("View Breakpoints…", d::ViewBreakpoints),
                    MenuItem::action("Remove All Breakpoints", d::RemoveAllBreakpoints),
                ]
            },
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Project", gleipnir_project_panel::Toggle),
                MenuItem::action("Problems", gleipnir_code_nav::ShowProblems),
                MenuItem::action("Run", gleipnir_debugger_ui::ShowRunPanel),
                MenuItem::action("Debug", gleipnir_debugger_ui::ShowDebugPanel),
                MenuItem::action("Terminal", gleipnir_terminal::ToggleTerminal),
                MenuItem::action("New Terminal Tab", gleipnir_terminal::NewTerminal),
                MenuItem::action("Agents", gleipnir_terminal::ToggleAgents),
                MenuItem::action("Claude Code", gleipnir_terminal::OpenClaudeCode),
                MenuItem::action("Codex", gleipnir_terminal::OpenCodex),
                MenuItem::separator(),
                MenuItem::action("Split Right", SplitRight),
                MenuItem::action("Split Down", SplitDown),
            ],
            disabled: false,
        },
    ]);
}
