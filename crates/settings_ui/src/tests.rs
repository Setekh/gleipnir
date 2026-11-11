use super::*;
use gleipnir_core::BufferStore;
use gleipnir_test_support::{isolate_state, project, run_until};
use gpui::{Modifiers, TestAppContext, VisualTestContext, size};

/// What a test holds until it ends: its project, and its turn with the settings file, which every
/// test in this process shares, so one test's save can't land between another's save and its read.
struct Held {
    _project: tempfile::TempDir,
    _settings: std::sync::MutexGuard<'static, ()>,
}

fn setup(cx: &mut TestAppContext) -> (Held, Entity<Workspace>, &mut VisualTestContext) {
    static SETTINGS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A test that failed while holding it leaves it poisoned, which says nothing about the next.
    let settings = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    isolate_state();
    cx.executor().allow_parking();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        cx.set_global(gleipnir_ui::Themes::load(&gleipnir_ui::themes_dir()));
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_ui::bind_context_menu_keys(cx);
        gleipnir_workspace::bind_keys(cx);
        bind_keys(cx);
    });
    let (dir, root) = project(&[("main.go", "package main\n")]);
    let store = cx.new(|_| BufferStore::new());
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(store, Some(root), window, cx);
        add_to_workspace(&mut workspace, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1200.), px(800.)));
    let toolchains = cx.update(|_, cx| workspace.read(cx).toolchains().clone());
    run_until(cx, |cx| {
        cx.update(|_, cx| !toolchains.read(cx).packages().is_empty())
    });
    let held = Held {
        _project: dir,
        _settings: settings,
    };
    (held, workspace, cx)
}

/// Opens Settings at Languages & Tools, as Project Structure's shortcut does.
fn open(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<LanguageSettings> {
    let open = if cfg!(target_os = "macos") {
        "cmd-;"
    } else {
        "ctrl-alt-shift-s"
    };
    cx.simulate_keystrokes(open);
    let view = settings_view(workspace, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).page()), Page::Languages);
    cx.update(|_, cx| view.read(cx).languages().clone())
}

fn settings_view(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Entity<SettingsView> {
    cx.update(|_, cx| workspace.read(cx).active_modal::<SettingsView>())
        .expect("Settings open")
}

fn open_settings(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Entity<SettingsView> {
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-,"
    } else {
        "ctrl-alt-s"
    });
    settings_view(workspace, cx)
}

#[gpui::test]
fn the_shortcut_opens_it_and_escape_closes_it(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    open(&workspace, cx);
    cx.simulate_keystrokes("escape");
    assert!(
        cx.update(|_, cx| workspace.read(cx).active_modal::<SettingsView>())
            .is_none()
    );
}

#[gpui::test]
fn a_search_narrows_the_pages_and_a_switch_saves_its_setting(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    let view = open_settings(&workspace, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).page()), Page::Appearance);
    // Typing goes straight into the search.
    cx.simulate_input("inlay");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).visible_pages(cx)),
        [Page::Editor]
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).page()), Page::Editor);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).debug_rows(cx)),
        ["Inlay hints: true"]
    );
    cx.run_until_parked();
    // The page's only row; its switch is the first control drawn.
    let index = 8;
    let switch = cx
        .debug_bounds(Box::leak(format!("setting-{index}").into_boxed_str()))
        .expect("the switch");
    cx.simulate_click(switch.center(), Modifiers::none());
    assert!(!cx.update(|_, cx| gleipnir_core::settings(cx).editor.inlay_hints));
    let saved = std::fs::read_to_string(gleipnir_packages::settings::settings_path()).unwrap();
    assert!(saved.contains("\"inlay_hints\": false"), "{saved}");
    cx.update(|_, cx| view.update(cx, |v, cx| v.set_value("Inlay hints", "true", cx)))
        .unwrap();
}

#[gpui::test]
fn the_theme_and_font_sizes_are_chosen_and_kept(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    let view = open_settings(&workspace, cx);
    cx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_value("Theme", "Gleipnir Light", cx)?;
            v.set_value("Font size", "15", cx)?;
            // Out of range, so held to the most there is.
            v.set_value("Interface font size", "99", cx)
        })
    })
    .unwrap();
    let rows = cx.update(|_, cx| view.read(cx).debug_rows(cx));
    assert_eq!(
        rows,
        [
            "Theme: Gleipnir Light",
            "Interface font size: 20",
            "Update automatically: true"
        ]
    );
    let settings = cx.update(|_, cx| gleipnir_core::settings(cx).clone());
    assert_eq!(settings.editor.font_size, 15.);
    let themes = gleipnir_ui::Themes::load(&gleipnir_ui::themes_dir());
    let theme = gleipnir_ui::Theme::from_settings(&settings, &themes);
    assert!(theme.is_light());
    assert_eq!(theme.editor_font_size, 15.);
    let (reread, problem) = gleipnir_packages::settings::Settings::load();
    assert_eq!(problem, None);
    assert_eq!(reread, settings);
    cx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_value("Theme", "Gleipnir Dark", cx)?;
            v.set_value("Font size", "13", cx)?;
            v.set_value("Interface font size", "13", cx)
        })
    })
    .unwrap();
}

#[gpui::test]
fn a_recorded_shortcut_runs_its_action_and_is_kept(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    let view = open_settings(&workspace, cx);
    cx.update(|_, cx| view.update(cx, |v, cx| v.show_page(Page::Keymap, cx)));
    cx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.record_shortcut("workspace::SplitRight", window, cx)
        })
    });
    cx.simulate_keystrokes("ctrl-alt-shift-9");
    let keymap = std::fs::read_to_string(gleipnir_workspace::user_keymap_path()).unwrap();
    assert!(
        keymap.contains("ctrl-alt-shift-9 = workspace::SplitRight"),
        "{keymap}"
    );
    cx.update(|_, cx| view.update(cx, |v, cx| v.search_for("split right", cx)));
    let rows = cx.update(|_, cx| view.read(cx).debug_rows(cx));
    assert!(
        rows.iter()
            .any(|r| r.starts_with("Split Right: ctrl-alt-shift-9")),
        "{rows:?}"
    );
    // It works at once: closing Settings and pressing it splits the editor.
    cx.simulate_keystrokes("escape");
    let panes = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).panes.len())
    };
    let before = panes(cx);
    cx.simulate_keystrokes("ctrl-alt-shift-9");
    assert_eq!(panes(cx), before + 1);
    std::fs::remove_file(gleipnir_workspace::user_keymap_path()).ok();
}

#[gpui::test]
fn indentation_is_set_per_language(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    let view = open_settings(&workspace, cx);
    cx.update(|_, cx| view.update(cx, |v, cx| v.show_page(Page::CodeStyle, cx)));
    cx.run_until_parked();
    let go = cx.update(|_, _| {
        gleipnir_packages::registry()
            .languages()
            .iter()
            .map(|l| l.name.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .position(|name| name == "Go")
            .unwrap()
    });
    let wider = cx
        .debug_bounds(Box::leak(format!("indent-more-{go}").into_boxed_str()))
        .expect("Go's indent");
    cx.simulate_click(wider.center(), Modifiers::none());
    let style = cx.update(|_, cx| gleipnir_core::settings(cx).code_style.get("Go").copied());
    let own = gleipnir_packages::registry()
        .languages()
        .iter()
        .find(|l| l.name == "Go")
        .unwrap()
        .edit_config
        .clone();
    assert_eq!(style.map(|s| s.tab_size), Some(own.tab_size + 1));
    // Back to the language's own, and the override is gone.
    let narrower = cx
        .debug_bounds(Box::leak(format!("indent-less-{go}").into_boxed_str()))
        .unwrap();
    cx.simulate_click(narrower.center(), Modifiers::none());
    assert_eq!(
        cx.update(|_, cx| gleipnir_core::settings(cx).code_style.get("Go").copied()),
        None
    );
}

#[gpui::test]
fn every_package_is_listed_with_its_tools(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    open(&workspace, cx);
    let toolchains = cx.update(|_, cx| workspace.read(cx).toolchains().clone());
    let listed: Vec<(String, Vec<String>)> = cx.update(|_, cx| {
        toolchains
            .read(cx)
            .packages()
            .iter()
            .map(|p| {
                (
                    p.package_id.clone(),
                    p.tools.iter().map(|t| t.name.clone()).collect(),
                )
            })
            .collect()
    });
    let ids: Vec<&str> = listed.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "rust",
            "go",
            "odin",
            "typescript",
            "csharp",
            "json",
            "html",
            "css",
            "markdown"
        ]
    );
    assert!(listed[1].1.contains(&"gopls".to_string()) && listed[1].1.contains(&"dlv".to_string()));
    assert_eq!(listed[4].1, ["csharp-ls"]);
    assert_eq!(listed[8].1, ["marksman"]);
}

#[gpui::test]
fn clicking_an_sdk_chooses_it_and_the_choice_is_saved(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    open(&workspace, cx);
    let toolchains = cx.update(|_, cx| workspace.read(cx).toolchains().clone());
    // Use whichever package has more than one SDK on this machine; skip if none does.
    let pick = cx.update(|_, cx| {
        toolchains
            .read(cx)
            .packages()
            .iter()
            .find(|p| p.candidates.len() > 1)
            .map(|p| {
                let other = p
                    .candidates
                    .iter()
                    .position(|c| Some(&c.root) != p.chosen.as_ref().map(|s| &s.root))
                    .unwrap();
                (
                    p.package_id.clone(),
                    p.sdk_kind.clone().unwrap(),
                    other,
                    p.candidates[other].root.clone(),
                )
            })
    });
    let Some((package_id, kind, index, root)) = pick else {
        return;
    };
    cx.run_until_parked();
    let row = cx
        .debug_bounds(Box::leak(
            format!("sdk-{package_id}-{index}").into_boxed_str(),
        ))
        .expect("the row is drawn");
    cx.simulate_click(row.center(), Modifiers::none());
    let chosen = cx.update(|_, cx| {
        toolchains
            .read(cx)
            .package(&package_id)
            .unwrap()
            .chosen
            .clone()
    });
    assert_eq!(chosen.unwrap().root, root);
    let saved = cx.update(|_, cx| workspace.read(cx).to_saved(cx).sdks);
    assert_eq!(saved.get(&kind), Some(&root));
}

#[gpui::test]
fn installing_asks_first_and_cancelling_does_nothing(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    let settings = open(&workspace, cx);
    let toolchains = cx.update(|_, cx| workspace.read(cx).toolchains().clone());
    // A missing, installable tool, wherever this machine has one.
    let missing = cx.update(|_, cx| {
        toolchains.read(cx).packages().iter().find_map(|p| {
            p.tools
                .iter()
                .find(|t| t.resolution.found.is_none() && t.can_install)
                .map(|t| (p.package_id.clone(), t.clone()))
        })
    });
    let Some((package_id, tool)) = missing else {
        return;
    };
    cx.update(|window, cx| {
        settings.update(cx, |s, cx| {
            s.install(package_id.clone(), tool.clone(), window, cx)
        })
    });
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    let installing = cx.update(|_, cx| {
        toolchains
            .read(cx)
            .package(&package_id)
            .unwrap()
            .tools
            .iter()
            .any(|t| t.installing)
    });
    assert!(!installing);
}
