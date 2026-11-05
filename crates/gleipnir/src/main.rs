//! Gleipnir: an IDE for TypeScript, JavaScript, Odin, Rust and Go.

mod driver;
mod root;

use gleipnir_core::BufferStore;
use gpui::{
    App, AppContext as _, Bounds, KeyBinding, Menu, MenuItem, WindowBounds, WindowOptions, actions,
    px, size,
};
use root::Root;
use std::path::PathBuf;
use std::time::Duration;

actions!(gleipnir, [Quit, OpenFile, NewFile, SaveAs]);

/// How often to check open files for external changes, until file watching exists.
const DISK_POLL_INTERVAL: Duration = Duration::from_secs(1);

fn main() {
    // Every path argument after the program name; the first is what we open.
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let script = std::env::var_os("GLEIPNIR_SCRIPT").map(PathBuf::from);

    gpui_platform::application().run(move |cx: &mut App| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_editor::bind_keys(cx);
        bind_app_keys(cx);
        set_menus(cx);
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

        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        let scripted = script.is_some();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // Lets window manager rules, such as Hyprland's, target this app.
                    app_id: Some("gleipnir".to_string()),
                    // A scripted run shouldn't take focus from whatever else is going on.
                    focus: !scripted,
                    show: true,
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Root::new(store, path, window, cx)),
            )
            .expect("failed to open a window");

        if let Some(script) = script {
            driver::run(script, window, cx);
        } else {
            cx.activate(true);
        }
    });
}

fn bind_app_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-n", NewFile, None),
        KeyBinding::new("cmd-shift-s", SaveAs, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-o", OpenFile, None),
        KeyBinding::new("ctrl-n", NewFile, None),
        KeyBinding::new("ctrl-shift-s", SaveAs, None),
    ]);
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
                MenuItem::action("Open…", OpenFile),
                MenuItem::separator(),
                MenuItem::action("Save", gleipnir_editor::Save),
                MenuItem::action("Save As…", SaveAs),
            ],
            disabled: false,
        },
    ]);
}
