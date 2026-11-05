//! Gleipnir: an IDE for TypeScript, JavaScript, Odin, Rust and Go.

mod driver;

use gleipnir_core::BufferStore;
use gleipnir_workspace::{
    GlobalStore, NewFile, OpenFile, OpenFolder, Quit, SaveAs, SplitDown, SplitRight, WorkspaceInit,
    config_dir, last_project, load_user_keymap, open_project_window, project_root_for,
};
use gpui::{App, AppContext as _, Menu, MenuItem};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// How often to check open files for external changes. The project watcher only reports on files
/// inside the project, and open files can be anywhere.
const DISK_POLL_INTERVAL: Duration = Duration::from_secs(1);

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

fn main() {
    let target = launch_target(std::env::args_os().nth(1).map(PathBuf::from));
    let script = std::env::var_os("GLEIPNIR_SCRIPT").map(PathBuf::from);

    gpui_platform::application()
        .with_assets(gleipnir_ui::Assets)
        .run(move |cx: &mut App| {
            gleipnir_ui::init_theme(cx);
            gleipnir_ui::bind_text_input_keys(cx);
            gleipnir_ui::bind_context_menu_keys(cx);
            gleipnir_editor::bind_keys(cx);
            gleipnir_workspace::bind_keys(cx);
            gleipnir_project_panel::bind_keys(cx);
            gleipnir_search::bind_keys(cx);
            set_menus(cx);

            // Loaded last, so that it overrides the defaults.
            let keymap_path = config_dir().join("keymap.conf");
            let keymap_errors = load_user_keymap(&keymap_path, cx);

            cx.on_action(|_: &Quit, cx| cx.quit());
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
                store.start_polling(DISK_POLL_INTERVAL, cx);
                store
            });
            cx.set_global(GlobalStore(store));
            cx.set_global(WorkspaceInit(Rc::new(|workspace, window, cx| {
                gleipnir_project_panel::add_to_workspace(workspace, window, cx);
                gleipnir_search::add_to_workspace(workspace, window, cx);
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
            items: vec![MenuItem::action("Quit", Quit)],
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
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Project", gleipnir_project_panel::Toggle),
                MenuItem::separator(),
                MenuItem::action("Split Right", SplitRight),
                MenuItem::action("Split Down", SplitDown),
            ],
            disabled: false,
        },
    ]);
}
