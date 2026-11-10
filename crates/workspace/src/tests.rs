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
        gleipnir_ui::bind_context_menu_keys(cx);
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
    /// Stands in for one whose contents don't come back, like the Agents window.
    fn reopens(&self) -> bool {
        self.id != "right"
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
        let editor = pane.read(cx).active_editor().unwrap().clone();
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
                let path = item.path(cx);
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
fn a_rail_button_hides_its_panel_even_when_the_panel_isnt_focused(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_for(None, cx);
    // Like Find Usages showing its results, which leaves focus in the editor.
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.show_panel("bottom", cx)));
    cx.run_until_parked();
    let visible = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| workspace.read(cx).is_dock_visible(DockPosition::Bottom))
    };
    assert!(visible(cx));
    let button = cx.debug_bounds("rail-bottom").expect("a rail button");
    cx.simulate_click(button.center(), Modifiers::none());
    assert!(!visible(cx));
    cx.simulate_click(button.center(), Modifiers::none());
    assert!(visible(cx));
    cx.simulate_click(button.center(), Modifiers::none());
    assert!(!visible(cx));
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
    let right = |cx: &mut Context<Workspace>| {
        cx.new(|cx| TestPanel {
            focus_handle: cx.focus_handle(),
            id: "right",
            position: DockPosition::Right,
        })
    };
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| {
            let panel = right(cx);
            w.add_panel(panel, cx);
            w.toggle_panel("right", window, cx);
        })
    });
    assert!(cx.update(|_, cx| workspace.read(cx).is_dock_visible(DockPosition::Right)));
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
            let panel = right(cx);
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
    // A panel that would come back empty stays closed.
    assert!(!cx.update(|_, cx| reopened.read(cx).is_dock_visible(DockPosition::Right)));
    // The cursor is back where it was.
    let cursor = cx.update(|_, cx| {
        let pane = reopened.read(cx).panes()[0].clone();
        let editor = pane.read(cx).active_editor().unwrap().clone();
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

fn panes(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<Entity<Pane>> {
    cx.update(|_, cx| workspace.read(cx).panes())
}

#[gpui::test]
fn tabs_move_within_a_pane_and_between_panes(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", ""), ("b.rs", ""), ("c.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    for name in ["a.rs", "b.rs", "c.rs"] {
        open(&workspace, root.join(name), cx);
    }
    let pane = panes(&workspace, cx)[0].clone();
    // The first tab to the end.
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| w.move_tab(&pane, 0, &pane, 3, window, cx))
    });
    assert_eq!(tabs(&workspace, cx), [["b.rs", "c.rs", "a.rs"]]);

    // Into a new split, taking the tab out of the first pane.
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| w.split_active(Axis::Horizontal, window, cx))
    });
    let [first, second] = panes(&workspace, cx).try_into().ok().unwrap();
    let before = tabs(&workspace, cx);
    assert_eq!(before[1], ["a.rs"]);
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| w.move_tab(&first, 0, &second, 0, window, cx))
    });
    assert_eq!(
        tabs(&workspace, cx),
        [vec!["c.rs", "a.rs"], vec!["b.rs", "a.rs"]]
    );

    // A file the other pane already shows goes to its tab, and the moved one closes.
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| w.move_tab(&first, 1, &second, 0, window, cx))
    });
    assert_eq!(tabs(&workspace, cx), [vec!["c.rs"], vec!["b.rs", "a.rs"]]);

    // Moving the last tab out of a pane closes the pane.
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| w.move_tab(&first, 0, &second, 2, window, cx))
    });
    assert_eq!(tabs(&workspace, cx), [["b.rs", "a.rs", "c.rs"]]);
}

#[gpui::test]
fn a_tab_dragged_onto_another_moves_before_it(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", ""), ("b.rs", ""), ("c.rs", "")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    for name in ["a.rs", "b.rs", "c.rs"] {
        open(&workspace, root.join(name), cx);
    }
    let from = cx.debug_bounds("tab-2").unwrap().center();
    let to = cx.debug_bounds("tab-0").unwrap().center();
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        from + point(px(-10.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(tabs(&workspace, cx), [["c.rs", "a.rs", "b.rs"]]);
}

#[gpui::test]
fn the_title_bar_shows_the_project_and_branch_and_offers_recent_projects(cx: &mut TestAppContext) {
    let earlier = tempfile::tempdir().unwrap();
    let earlier_root = std::fs::canonicalize(earlier.path()).unwrap();
    let current = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(current.path())
        .unwrap()
        .join("fix-partner");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/master\n").unwrap();
    // Opening one project, then another, leaves the first among the recent ones.
    workspace_for(Some(&earlier_root), cx);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| workspace.read(cx).branch().map(String::from)),
        Some("master".to_string())
    );
    let bar = cx.debug_bounds("title-bar").expect("a title bar");
    assert_eq!(bar.top(), px(0.));
    assert_eq!(bar.size.height, px(TITLE_BAR_HEIGHT));
    assert!(cx.debug_bounds("title-branch").is_some());

    let project = cx.debug_bounds("title-project").expect("the project");
    cx.simulate_click(project.center(), Modifiers::none());
    let labels = cx.update(|_, cx| {
        workspace
            .read(cx)
            .open_menu()
            .map(|menu| menu.read(cx).labels())
            .unwrap_or_default()
    });
    let earlier_name = earlier_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(labels.contains(&earlier_name), "{labels:?}");
    assert!(!labels.contains(&"fix-partner".to_string()), "{labels:?}");
    assert_eq!(labels.last().map(String::as_str), Some("Open…"));
    // The menu opens below the bar.
    let first = cx.debug_bounds("menu-item-0").expect("an item");
    assert!(first.top() >= bar.bottom());
    cx.simulate_keystrokes("escape");
    assert!(cx.update(|_, cx| workspace.read(cx).open_menu().is_none()));
}

/// Runs git in a folder for a test's repository, with an identity so commits work anywhere.
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

#[gpui::test]
fn the_branch_menu_checks_out_creates_merges_and_deletes_with_git(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-q"]);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "one"]);
    git(&root, &["branch", "feature"]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    let branch = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| workspace.read(cx).branch().map(String::from))
    };
    assert_eq!(branch(cx).as_deref(), Some("main"));
    let menu = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            workspace
                .read(cx)
                .open_menu()
                .map(|m| m.read(cx).labels())
                .unwrap_or_default()
        })
    };
    // Clicks a menu item by its label, waiting for the menu to show.
    let choose = |label: &str, cx: &mut VisualTestContext| {
        run_until(cx, |cx| menu(cx).iter().any(|l| l == label));
        let index = menu(cx).iter().position(|l| l == label).unwrap();
        let item = cx
            .debug_bounds(format!("menu-item-{index}").leak())
            .unwrap();
        cx.simulate_click(item.center(), Modifiers::none());
    };
    let open_branches = |cx: &mut VisualTestContext| {
        let button = cx.debug_bounds("title-branch").expect("the branch");
        cx.simulate_click(button.center(), Modifiers::none());
    };
    let message = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).message)
            .unwrap_or_default()
    };

    // Checking out another branch.
    open_branches(cx);
    run_until(cx, |cx| menu(cx).contains(&"feature".to_string()));
    assert_eq!(menu(cx)[0], "New Branch…");
    choose("feature", cx);
    choose("Checkout", cx);
    run_until(cx, |cx| branch(cx).as_deref() == Some("feature"));

    // A new branch, named in a prompt, with a commit of its own.
    open_branches(cx);
    choose("New Branch…", cx);
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            workspace
                .read(cx)
                .active_modal::<crate::NamePrompt>()
                .is_some()
        })
    });
    cx.simulate_input("topic");
    cx.simulate_keystrokes("enter");
    run_until(cx, |cx| branch(cx).as_deref() == Some("topic"));
    std::fs::write(root.join("b.txt"), "two\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "two"]);

    // Merged into main, then deleted, which git allows as it's merged.
    git(&root, &["switch", "-q", "main"]);
    run_until(cx, |cx| {
        cx.update(|_, cx| workspace.update(cx, |w, cx| w.watch_branch_now(cx)));
        branch(cx).as_deref() == Some("main")
    });
    open_branches(cx);
    choose("topic", cx);
    choose("Merge 'topic' into 'main'", cx);
    run_until(cx, |cx| message(cx).contains("Merged topic into main"));
    assert!(root.join("b.txt").exists());
    open_branches(cx);
    choose("topic", cx);
    choose("Delete", cx);
    run_until(cx, |cx| message(cx).contains("Deleted topic"));
    let (local, _) = crate::workspace::list_branches(&root).unwrap();
    assert!(!local.contains(&"topic".to_string()));

    // Git won't switch over a change it would lose, and says why.
    git(&root, &["switch", "-q", "feature"]);
    std::fs::write(root.join("a.txt"), "feature\n").unwrap();
    git(&root, &["commit", "-q", "-am", "feature"]);
    git(&root, &["switch", "-q", "main"]);
    std::fs::write(root.join("a.txt"), "uncommitted\n").unwrap();
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.watch_branch_now(cx)));
    open_branches(cx);
    choose("feature", cx);
    choose("Checkout", cx);
    run_until(cx, |cx| message(cx).contains("would be overwritten"));
    assert_eq!(branch(cx).as_deref(), Some("main"));
}

/// The smallest PNG there is: one transparent pixel.
const PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];
const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;

/// Opens a file that should show as an image, which leaves no editor to resolve to.
fn open_image(workspace: &Entity<Workspace>, path: PathBuf, cx: &mut VisualTestContext) {
    let task = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| workspace.open_path(path, window, cx))
    });
    let error = resolve(cx, task).expect_err("an image has no editor");
    assert!(error.downcast_ref::<ShownAsImage>().is_some(), "{error:#}");
    cx.run_until_parked();
}

fn image_project() -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = project(&[("a.rs", "fn a() {}"), ("icon.svg", SVG)]);
    fs::write(root.join("logo.png"), PIXEL_PNG).unwrap();
    (dir, root)
}

#[gpui::test]
fn an_image_opens_in_a_preview_and_opening_it_again_reuses_the_tab(cx: &mut TestAppContext) {
    let (_dir, root) = image_project();
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    open_image(&workspace, root.join("logo.png"), cx);
    open(&workspace, root.join("a.rs"), cx);
    open_image(&workspace, root.join("logo.png"), cx);
    assert_eq!(tabs(&workspace, cx), [["a.rs", "logo.png"]]);
    cx.update(|_, cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.active_editor(cx).is_none());
        assert_eq!(workspace.active_file(cx), Some(root.join("logo.png")));
        let pane = workspace.active_pane().read(cx);
        assert!(pane.active_item().and_then(PaneItem::image).is_some());
    });
}

#[gpui::test]
fn an_svg_shows_as_an_image_and_its_text_opens_from_there(cx: &mut TestAppContext) {
    let (_dir, root) = image_project();
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open_image(&workspace, root.join("icon.svg"), cx);
    cx.dispatch_action(image_view::OpenAsText);
    cx.run_until_parked();
    run_until(cx, |cx| {
        cx.update(|_, cx| workspace.read(cx).active_editor(cx).is_some())
    });
    assert_eq!(tabs(&workspace, cx), [["icon.svg", "icon.svg"]]);
    assert_eq!(active_text(&workspace, cx), SVG);
}

#[gpui::test]
fn an_image_tab_closes_without_asking_and_never_counts_as_unsaved(cx: &mut TestAppContext) {
    let (_dir, root) = image_project();
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open_image(&workspace, root.join("logo.png"), cx);
    let closing = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| workspace.prepare_to_close(window, cx))
    });
    assert!(resolve(cx, closing));
    cx.simulate_keystrokes(&keys("mod-w"));
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());
    assert_eq!(tabs(&workspace, cx), [Vec::<String>::new()]);
}

#[gpui::test]
fn image_tabs_come_back_with_the_layout(cx: &mut TestAppContext) {
    let (_dir, root) = image_project();
    let (workspace, cx) = workspace_for(Some(&root), cx);
    open(&workspace, root.join("a.rs"), cx);
    open_image(&workspace, root.join("logo.png"), cx);
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.save_now(cx)));

    let store = cx.new(|_| BufferStore::new());
    let reopened = cx.update(|window, cx| {
        cx.new(|cx| {
            let mut workspace = Workspace::new(store, Some(root.clone()), window, cx);
            assert!(workspace.restore_layout(window, cx));
            workspace
        })
    });
    run_until(cx, |cx| {
        !cx.update(|_, cx| reopened.read(cx).is_restoring())
    });
    assert_eq!(tabs(&reopened, cx), [["a.rs", "logo.png"]]);
    assert_eq!(
        cx.update(|_, cx| reopened.read(cx).active_file(cx)),
        Some(root.join("logo.png"))
    );
}

/// Where there's no system menu bar, the title bar shows the app's menus, and their items act
/// where focus was before the menu opened.
#[cfg(not(target_os = "macos"))]
#[gpui::test]
fn the_title_bar_shows_the_app_menus_and_they_act_where_focus_was(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "a")]);
    let (workspace, cx) = workspace_for(Some(&root), cx);
    cx.update(|_, cx| {
        cx.set_menus([
            gpui::Menu {
                name: "File".into(),
                items: vec![
                    gpui::MenuItem::action("New", NewFile),
                    gpui::MenuItem::separator(),
                    gpui::MenuItem::action("Split Right", SplitRight),
                ],
                disabled: false,
            },
            gpui::Menu {
                name: "View".into(),
                items: vec![gpui::MenuItem::action("Split Down", SplitDown)],
                disabled: false,
            },
        ])
    });
    open(&workspace, root.join("a.rs"), cx);
    let menu_labels = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            workspace
                .read(cx)
                .open_menu()
                .map(|menu| menu.read(cx).labels())
        })
    };

    let file = cx.debug_bounds("app-menu-0").expect("the File menu's name");
    cx.simulate_click(file.center(), Modifiers::none());
    assert_eq!(
        menu_labels(cx),
        Some(vec!["New".to_string(), "Split Right".to_string()])
    );

    // Moving to the next name opens its menu instead.
    let view = cx.debug_bounds("app-menu-1").expect("the View menu's name");
    cx.simulate_mouse_move(view.center(), None, Modifiers::none());
    assert_eq!(menu_labels(cx), Some(vec!["Split Down".to_string()]));

    // Pressing the open menu's name closes it.
    cx.simulate_click(view.center(), Modifiers::none());
    assert_eq!(menu_labels(cx), None);

    // An item goes to the editor that had focus before the menu, even after moving between menus.
    cx.simulate_click(file.center(), Modifiers::none());
    cx.simulate_mouse_move(view.center(), None, Modifiers::none());
    cx.simulate_mouse_move(file.center(), None, Modifiers::none());
    let item = cx.debug_bounds("menu-item-1").expect("Split Right");
    cx.simulate_click(item.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(tabs(&workspace, cx), [["a.rs"], ["a.rs"]]);
    assert_eq!(menu_labels(cx), None);
}
