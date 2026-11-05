use super::*;
use gleipnir_core::BufferStore;
use gleipnir_test_support::{isolate_state, project, run_until};
use gpui::{Modifiers, TestAppContext, VisualTestContext, size};

fn setup(
    cx: &mut TestAppContext,
) -> (tempfile::TempDir, Entity<Workspace>, &mut VisualTestContext) {
    isolate_state();
    cx.executor().allow_parking();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
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
    (dir, workspace, cx)
}

fn open(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<LanguageSettings> {
    let open = if cfg!(target_os = "macos") {
        "cmd-;"
    } else {
        "ctrl-alt-shift-s"
    };
    cx.simulate_keystrokes(open);
    cx.update(|_, cx| workspace.read(cx).active_modal::<LanguageSettings>())
        .expect("it opens")
}

#[gpui::test]
fn the_shortcut_opens_it_and_escape_closes_it(cx: &mut TestAppContext) {
    let (_dir, workspace, cx) = setup(cx);
    open(&workspace, cx);
    cx.simulate_keystrokes("escape");
    assert!(
        cx.update(|_, cx| workspace.read(cx).active_modal::<LanguageSettings>())
            .is_none()
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
    assert_eq!(ids, ["rust", "go", "odin", "typescript"]);
    assert!(listed[1].1.contains(&"gopls".to_string()) && listed[1].1.contains(&"dlv".to_string()));
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
