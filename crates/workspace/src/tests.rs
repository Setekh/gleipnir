use crate::*;
use gleipnir_core::BufferStore;
use gleipnir_test_support::{isolate_state, project, resolve, run_until};
use gleipnir_ui::Icon;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, Modifiers, MouseButton,
    Render, TestAppContext, VisualTestContext, Window, div, point, prelude::*, px, size,
};
use std::fs;
use std::path::{Path, PathBuf};

const MOD: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "ctrl"
};

fn keys(text: &str) -> String {
    text.replace("mod-", &format!("{MOD}-"))
}

fn init(cx: &mut TestAppContext) {
    isolate_state();
    // A workspace on a folder watches it, from a thread of its own.
    cx.executor().allow_parking();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_editor::bind_keys(cx);
        crate::bind_keys(cx);
    });
}

/// A panel with nothing in it, standing in for real ones.
struct TestPanel {
    focus_handle: FocusHandle,
    id: &'static str,
    position: DockPosition,
}

impl Focusable for TestPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TestPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(self.id)
    }
}

impl Panel for TestPanel {
    fn id(&self) -> &'static str {
        self.id
    }
    fn title(&self) -> &'static str {
        self.id
    }
    fn icon(&self) -> Icon {
        Icon::Tree
    }
    fn position(&self) -> DockPosition {
        self.position
    }
}

fn workspace_for<'a>(
    root: Option<&Path>,
    cx: &'a mut TestAppContext,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    init(cx);
    let root = root.map(Path::to_path_buf);
    let store = cx.new(|_| BufferStore::new());
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(store, root, window, cx);
        for (id, position) in [
            ("left", DockPosition::Left),
            ("bottom", DockPosition::Bottom),
        ] {
            let panel = cx.new(|cx| TestPanel {
                focus_handle: cx.focus_handle(),
                id,
                position,
            });
            workspace.add_panel(panel, cx);
        }
        workspace
    });
    cx.simulate_resize(size(px(1200.), px(800.)));
    (workspace, cx)
}

fn open(workspace: &Entity<Workspace>, path: PathBuf, cx: &mut VisualTestContext) {
    let task = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| workspace.open_path(path, window, cx))
    });
    resolve(cx, task).expect("the file should open");
    cx.run_until_parked();
}

fn tabs(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<Vec<String>> {
    cx.update(|window, cx| {
        workspace
            .read(cx)
            .snapshot(window, cx)
            .panes
            .into_iter()
            .map(|p| p.tabs)
            .collect()
    })
}

fn active_text(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| workspace.read(cx).editor_state(cx).expect("an editor").text)
}

#[gpui::test]
fn opening_a_file_adds_a_tab_and_opening_it_again_reuses_it(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "fn a() {}"), ("b.rs", "fn b() {}")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    open(&workspace, root.join("b.rs"), cx);
    open(&workspace, root.join("a.rs"), cx);
    assert_eq!(tabs(&workspace, cx), [["a.rs", "b.rs"]]);
    assert_eq!(active_text(&workspace, cx), "fn a() {}");
}

#[gpui::test]
fn typing_goes_to_the_opened_file(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_input("x");
    assert_eq!(active_text(&workspace, cx), "x");
}

#[gpui::test]
fn splitting_shows_the_same_buffer_in_both_panes(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_keystrokes(&keys("mod-\\"));
    assert_eq!(tabs(&workspace, cx), [["a.rs"], ["a.rs"]]);

    // Typing in the new pane shows in the old one.
    cx.simulate_input("shared");
    let first = cx.update(|_, cx| {
        let pane = workspace.read(cx).panes()[0].clone();
        let editor = pane.read(cx).active_item().unwrap().clone();
        editor.read(cx).buffer().read(cx).buffer().text()
    });
    assert_eq!(first, "shared");
}

#[gpui::test]
fn closing_the_last_tab_in_a_split_removes_the_pane(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_keystrokes(&keys("mod-\\"));
    cx.simulate_keystrokes(&keys("mod-w"));
    assert_eq!(tabs(&workspace, cx), [["a.rs"]]);
    // Focus went back to the remaining editor, so typing still works.
    cx.simulate_input("x");
    assert_eq!(active_text(&workspace, cx), "x");
}

#[gpui::test]
fn the_last_pane_stays_when_emptied(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_keystrokes(&keys("mod-w"));
    assert_eq!(tabs(&workspace, cx), [Vec::<String>::new()]);
}

#[gpui::test]
fn closing_a_dirty_tab_asks_first(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "old")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_input("new ");

    cx.simulate_keystrokes(&keys("mod-w"));
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert_eq!(tabs(&workspace, cx), [["a.rs"]]);

    cx.simulate_keystrokes(&keys("mod-w"));
    cx.simulate_prompt_answer("Save");
    run_until(cx, |cx| tabs(&workspace, cx) == [Vec::<String>::new()]);
    assert_eq!(fs::read_to_string(root.join("a.rs")).unwrap(), "new old");
}

#[gpui::test]
fn dont_save_closes_and_leaves_the_file_alone(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "old")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_input("new ");
    cx.simulate_keystrokes(&keys("mod-w"));
    cx.simulate_prompt_answer("Don't Save");
    cx.run_until_parked();
    assert_eq!(tabs(&workspace, cx), [Vec::<String>::new()]);
    assert_eq!(fs::read_to_string(root.join("a.rs")).unwrap(), "old");
}

#[gpui::test]
fn closing_one_of_two_views_of_a_dirty_file_does_not_ask(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_input("unsaved");
    cx.simulate_keystrokes(&keys("mod-\\"));
    cx.simulate_keystrokes(&keys("mod-w"));
    assert!(!cx.has_pending_prompt());
    assert_eq!(tabs(&workspace, cx), [["a.rs"]]);
}

#[gpui::test]
fn closing_the_window_with_unsaved_work_asks_and_can_save_it_all(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", ""), ("b.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_input("a");
    open(&workspace, root.join("b.rs"), cx);
    cx.simulate_input("b");

    let decision = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| workspace.prepare_to_close(window, cx))
    });
    cx.run_until_parked();
    cx.simulate_prompt_answer("Save All");
    assert!(resolve(cx, decision));
    assert_eq!(fs::read_to_string(root.join("a.rs")).unwrap(), "a");
    assert_eq!(fs::read_to_string(root.join("b.rs")).unwrap(), "b");
}

#[gpui::test]
fn cancelling_the_close_keeps_the_window(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_input("a");
    let decision = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| workspace.prepare_to_close(window, cx))
    });
    cx.run_until_parked();
    cx.simulate_prompt_answer("Cancel");
    assert!(!resolve(cx, decision));
}

#[gpui::test]
fn closing_with_nothing_unsaved_does_not_ask(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    let decision = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| workspace.prepare_to_close(window, cx))
    });
    assert!(!cx.has_pending_prompt());
    assert!(resolve(cx, decision));
}

#[gpui::test]
fn tab_navigation_wraps_around(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "a"), ("b.rs", "b"), ("c.rs", "c")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    for name in ["a.rs", "b.rs", "c.rs"] {
        open(&workspace, root.join(name), cx);
    }
    cx.simulate_keystrokes("ctrl-tab");
    assert_eq!(active_text(&workspace, cx), "a");
    cx.simulate_keystrokes("ctrl-shift-tab");
    assert_eq!(active_text(&workspace, cx), "c");
}

#[gpui::test]
fn focus_moves_between_panes(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_keystrokes(&keys("mod-\\"));
    let active = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).active_pane)
    };
    assert_eq!(active(cx), 1);
    cx.simulate_keystrokes("alt-tab");
    assert_eq!(active(cx), 0);
    cx.simulate_keystrokes("alt-tab");
    assert_eq!(active(cx), 1);
}

#[gpui::test]
fn clicking_a_tab_activates_it_and_its_close_button_closes_it(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "a"), ("b.rs", "b")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    open(&workspace, root.join("b.rs"), cx);

    let tab = cx.debug_bounds("tab-0").expect("the first tab is drawn");
    cx.simulate_click(tab.center(), Modifiers::none());
    assert_eq!(active_text(&workspace, cx), "a");

    let close = cx.debug_bounds("tab-close-1").expect("a close button");
    cx.simulate_click(close.center(), Modifiers::none());
    assert_eq!(tabs(&workspace, cx), [["a.rs"]]);
}

#[gpui::test]
fn tabs_with_the_same_name_show_their_folder(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("one/mod.rs", ""), ("two/mod.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("one/mod.rs"), cx);
    open(&workspace, root.join("two/mod.rs"), cx);
    let titles = cx.update(|_, cx| {
        let pane = workspace.read(cx).active_pane().clone();
        let pane = pane.read(cx);
        pane.items()
            .iter()
            .map(|item| {
                let path = item.read(cx).buffer().read(cx).path().map(PathBuf::from);
                pane.tab_title(path.as_deref(), cx).to_string()
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, ["mod.rs — one", "mod.rs — two"]);
}

#[gpui::test]
fn a_panel_toggles_with_focus_and_hides_when_toggled_again(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_for(None, cx);
    let visible = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| workspace.read(cx).is_dock_visible(DockPosition::Left))
    };
    assert!(!visible(cx));
    cx.update(|window, cx| workspace.update(cx, |w, cx| w.toggle_panel("left", window, cx)));
    assert!(visible(cx));
    cx.update(|window, cx| workspace.update(cx, |w, cx| w.toggle_panel("left", window, cx)));
    assert!(!visible(cx));
}

#[gpui::test]
fn shift_escape_hides_the_open_dock(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_for(None, cx);
    cx.update(|window, cx| workspace.update(cx, |w, cx| w.toggle_panel("left", window, cx)));
    cx.simulate_keystrokes("shift-escape");
    assert!(!cx.update(|_, cx| workspace.read(cx).is_dock_visible(DockPosition::Left)));
}

#[gpui::test]
fn the_rail_buttons_toggle_their_panels(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_for(None, cx);
    let button = cx.debug_bounds("rail-left").expect("a rail button");
    cx.simulate_click(button.center(), Modifiers::none());
    assert!(cx.update(|_, cx| workspace.read(cx).is_dock_visible(DockPosition::Left)));
    // Bottom-dock panels are on the same rail, further down.
    let bottom = cx
        .debug_bounds("rail-bottom")
        .expect("a rail button for the bottom panel");
    assert!(bottom.top() > button.top());
    cx.simulate_click(bottom.center(), Modifiers::none());
    assert!(cx.update(|_, cx| workspace.read(cx).is_dock_visible(DockPosition::Bottom)));
}

#[gpui::test]
fn docks_resize_by_dragging_their_edge_within_limits(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_for(None, cx);
    cx.update(|window, cx| workspace.update(cx, |w, cx| w.toggle_panel("left", window, cx)));
    cx.run_until_parked();
    let handle = cx
        .debug_bounds("dock-handle-Left")
        .expect("a resize handle");
    let start = handle.center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(start.x + px(100.), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        point(start.x + px(100.), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    let size = cx.update(|_, cx| workspace.read(cx).dock_size(DockPosition::Left));
    assert!((size - px(380.)).abs() < px(2.), "{size:?}");

    // Dragging far past either end stops at the limits.
    let handle = cx.debug_bounds("dock-handle-Left").unwrap();
    cx.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(px(2.), handle.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    assert_eq!(
        cx.update(|_, cx| workspace.read(cx).dock_size(DockPosition::Left)),
        px(dock::MIN_DOCK_SIZE)
    );
    cx.simulate_mouse_move(
        point(px(5000.), handle.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        point(px(5000.), handle.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    let size = cx.update(|_, cx| workspace.read(cx).dock_size(DockPosition::Left));
    assert!(
        size <= px(1200. - 2. * 40. - dock::MIN_CENTER_SIZE),
        "{size:?}"
    );
}

#[gpui::test]
fn splits_resize_by_dragging_the_divider(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_keystrokes(&keys("mod-\\"));
    cx.run_until_parked();
    let handle = cx.debug_bounds("split-handle-0").expect("a divider");
    let start = handle.center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(start.x + px(200.), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        point(start.x + px(200.), start.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    let ratios = cx.update(|_, cx| match &workspace.read(cx).to_saved(cx).center {
        persistence::SavedTree::Split { ratios, .. } => ratios.clone(),
        other => panic!("expected a split, got {other:?}"),
    });
    assert!(ratios[0] > 0.6, "{ratios:?}");
    assert!((ratios.iter().sum::<f32>() - 1.).abs() < 0.001);
}

#[gpui::test]
fn the_layout_comes_back_when_the_project_reopens(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "0123456789"), ("b.rs", "b"), ("gone.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_keystrokes("right right right");
    open(&workspace, root.join("gone.rs"), cx);
    cx.simulate_keystrokes(&keys("mod-\\"));
    open(&workspace, root.join("b.rs"), cx);
    cx.update(|window, cx| workspace.update(cx, |w, cx| w.toggle_panel("left", window, cx)));
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.save_now(cx)));
    fs::remove_file(root.join("gone.rs")).unwrap();

    let store = cx.new(|_| BufferStore::new());
    let reopened = cx.update(|window, cx| {
        cx.new(|cx| {
            let mut workspace = Workspace::new(store, Some(root.clone()), window, cx);
            let panel = cx.new(|cx| TestPanel {
                focus_handle: cx.focus_handle(),
                id: "left",
                position: DockPosition::Left,
            });
            workspace.add_panel(panel, cx);
            assert!(workspace.restore_layout(window, cx));
            workspace
        })
    });
    run_until(cx, |cx| {
        !cx.update(|_, cx| reopened.read(cx).is_restoring())
    });

    // The deleted file is skipped, and the split survives.
    assert_eq!(
        tabs(&reopened, cx),
        [vec!["a.rs"], vec!["gone.rs", "b.rs"][1..].to_vec()]
    );
    assert!(cx.update(|_, cx| reopened.read(cx).is_dock_visible(DockPosition::Left)));
    // The cursor is back where it was.
    let cursor = cx.update(|_, cx| {
        let pane = reopened.read(cx).panes()[0].clone();
        let editor = pane.read(cx).active_item().unwrap().clone();
        editor.read(cx).selections().primary().head
    });
    assert_eq!(cursor, 3);
}

#[gpui::test]
fn a_damaged_state_file_is_ignored(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    init(cx);
    let path = persistence::state_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{ this is not json").unwrap();
    let store = cx.new(|_| BufferStore::new());
    let (workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::new(store, Some(root), window, cx));
    let restored =
        cx.update(|window, cx| workspace.update(cx, |w, cx| w.restore_layout(window, cx)));
    assert!(!restored);
}

#[gpui::test]
fn saving_a_file_changed_on_disk_asks_which_to_keep(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "original")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_input("mine ");
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(root.join("a.rs"), "theirs").unwrap();
    let store = cx.update(|_, cx| workspace.read(cx).store().clone());
    cx.update(|_, cx| store.update(cx, |s, cx| s.check_disk(cx)));
    run_until(cx, |cx| {
        cx.update(|_, cx| workspace.read(cx).editor_state(cx).unwrap().disk_status == "Conflict")
    });

    cx.simulate_keystrokes(&keys("mod-s"));
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Overwrite");
    run_until(cx, |_| {
        fs::read_to_string(root.join("a.rs")).unwrap() == "mine original"
    });
}

#[gpui::test]
fn deleting_closes_clean_tabs_but_keeps_unsaved_ones(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("dir/clean.rs", ""), ("dir/dirty.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("dir/clean.rs"), cx);
    open(&workspace, root.join("dir/dirty.rs"), cx);
    cx.simulate_input("unsaved");
    let dir = root.join("dir");
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| w.close_clean_tabs_under(&dir, window, cx));
    });
    assert_eq!(tabs(&workspace, cx), [["dirty.rs"]]);
}

#[gpui::test]
fn every_command_is_listed_with_a_readable_title_and_its_shortcut(cx: &mut TestAppContext) {
    let (_workspace, cx) = workspace_for(None, cx);
    let actions = cx.update(|window, cx| list_actions(window, cx));
    let find = |name: &str| actions.iter().find(|a| a.name == name).cloned();
    let duplicate = find("editor::DuplicateLine").expect("editor actions are listed");
    assert_eq!(duplicate.title, "Duplicate Line");
    let split = find("workspace::SplitRight").expect("workspace actions are listed");
    assert!(split.shortcut.is_some());
    // Widget internals aren't commands someone would search for.
    assert!(actions.iter().all(|a| !a.name.starts_with("text_input::")));
}

#[gpui::test]
fn shortcuts_bound_in_a_context_show_when_focus_is_there(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    let actions = cx.update(|window, cx| list_actions(window, cx));
    let duplicate = actions
        .iter()
        .find(|a| a.name == "editor::DuplicateLine")
        .unwrap();
    let expected = if cfg!(target_os = "macos") {
        "cmd-d"
    } else {
        "ctrl-d"
    };
    assert_eq!(duplicate.shortcut.as_deref(), Some(expected));
}

#[gpui::test]
fn a_user_keymap_overrides_the_defaults(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "line"), ("keymap.conf", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    let file = root.join("keymap.conf");
    fs::write(
        &file,
        "# mine\nctrl-k = editor::DuplicateLine in Editor\nctrl-q = no::Such\n",
    )
    .unwrap();
    let errors = cx.update(|_, cx| load_user_keymap(&file, cx));
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains("line 3") && errors[0].contains("no::Such"),
        "{errors:?}"
    );
    cx.simulate_keystrokes("ctrl-k");
    assert_eq!(active_text(&workspace, cx), "line\nline");
}

#[test]
fn humanized_action_names() {
    assert_eq!(
        humanize_action_name("editor::DuplicateLine"),
        "Duplicate Line"
    );
    assert_eq!(humanize_action_name("workspace::Quit"), "Quit");
}

#[test]
fn breadcrumbs_run_from_the_project_root() {
    let crumbs = breadcrumb_segments(Some(Path::new("/p/app")), Path::new("/p/app/src/main.rs"));
    let labels: Vec<_> = crumbs.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(labels, ["app", "src", "main.rs"]);
    assert_eq!(crumbs[1].1, PathBuf::from("/p/app/src"));

    let outside = breadcrumb_segments(Some(Path::new("/p/app")), Path::new("/etc/hosts"));
    assert_eq!(
        outside,
        [("hosts".to_string(), PathBuf::from("/etc/hosts"))]
    );
}

#[gpui::test]
fn symbols_typed_but_not_saved_are_searchable(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "fn saved() {}\n")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    cx.simulate_keystrokes(&keys("mod-end"));
    cx.simulate_input("fn typed_here() {}\n");
    let project = cx.update(|_, cx| workspace.read(cx).project().cloned().unwrap());
    run_until(cx, |cx| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.update(|_, cx| {
            project.update(cx, |p, _| {
                p.symbols().iter().any(|e| e.symbol.name == "typed_here")
            })
        })
    });
}
