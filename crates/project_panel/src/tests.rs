use super::*;
use gleipnir_test_support::{isolate_state, project, resolve, run_until};
use gleipnir_workspace::Workspace;
use gpui::{Modifiers, TestAppContext, VisualTestContext, size};
use std::fs;
use std::process::Command;
use std::sync::Mutex;

fn init(cx: &mut TestAppContext) {
    isolate_state();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_ui::bind_context_menu_keys(cx);
        gleipnir_editor::bind_keys(cx);
        gleipnir_workspace::bind_keys(cx);
        bind_keys(cx);
    });
}

struct Setup<'a> {
    workspace: Entity<Workspace>,
    panel: Entity<ProjectPanel>,
    cx: &'a mut VisualTestContext,
    /// What went to the Trash, instead of the real one.
    trashed: Arc<Mutex<Vec<PathBuf>>>,
}

fn setup<'a>(root: &Path, cx: &'a mut TestAppContext) -> Setup<'a> {
    init(cx);
    let store = cx.new(|_| BufferStore::new());
    let root = root.to_path_buf();
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(store, Some(root), window, cx);
        add_to_workspace(&mut workspace, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1200.), px(800.)));
    cx.update(|window, cx| workspace.update(cx, |w, cx| w.toggle_panel(PANEL_ID, window, cx)));
    let panel = cx.update(|_, cx| {
        let snapshot_panel = workspace.read(cx);
        find_panel(snapshot_panel, cx)
    });
    let trashed = Arc::new(Mutex::new(Vec::new()));
    let log = trashed.clone();
    cx.update(|_, cx| {
        panel.update(cx, |panel, _| {
            panel.set_trash(move |path| {
                fs::remove_file(path).or_else(|_| fs::remove_dir_all(path))?;
                log.lock().unwrap().push(path.to_path_buf());
                Ok(())
            })
        })
    });
    let mut setup = Setup {
        workspace,
        panel,
        cx,
        trashed,
    };
    setup.settle();
    setup
}

/// Finds the panel inside a workspace by focusing it, which avoids exposing the docks for tests.
fn find_panel(workspace: &Workspace, cx: &App) -> Entity<ProjectPanel> {
    workspace
        .panel::<ProjectPanel>(cx)
        .expect("the project panel is added")
}

impl Setup<'_> {
    fn settle(&mut self) {
        let panel = self.panel.clone();
        run_until(self.cx, |cx| {
            cx.update(|_, cx| !panel.read(cx).is_loading())
        });
    }

    fn rows(&mut self) -> Vec<String> {
        self.settle();
        self.cx.update(|_, cx| self.panel.read(cx).visible_names())
    }

    fn wait_for_rows(&mut self, check: impl Fn(&[String]) -> bool) {
        let panel = self.panel.clone();
        run_until(self.cx, |cx| {
            let rows = cx.update(|_, cx| panel.read(cx).visible_names());
            check(&rows)
        });
    }

    fn selected(&mut self) -> Option<PathBuf> {
        self.cx
            .update(|_, cx| self.panel.read(cx).selected().map(Path::to_path_buf))
    }

    fn focus_panel(&mut self) {
        let panel = self.panel.clone();
        self.cx.update(|window, cx| {
            let handle = panel.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        });
    }

    fn keys(&mut self, keys: &str) {
        let keys = keys.replace(
            "mod-",
            if cfg!(target_os = "macos") {
                "cmd-"
            } else {
                "ctrl-"
            },
        );
        self.cx.simulate_keystrokes(&keys);
        self.settle();
    }

    fn tabs(&mut self) -> Vec<String> {
        let workspace = self.workspace.clone();
        self.cx.update(|window, cx| {
            workspace
                .read(cx)
                .snapshot(window, cx)
                .panes
                .into_iter()
                .flat_map(|p| p.tabs)
                .collect()
        })
    }

    fn debug(&mut self) -> serde_json::Value {
        self.cx.update(|_, cx| self.panel.read(cx).debug_state(cx))
    }
}

#[gpui::test]
fn the_tree_starts_with_the_root_open_and_folders_closed(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("src/main.rs", ""), ("README.md", "")]);
    let mut s = setup(&root, cx);
    let name = root.file_name().unwrap().to_string_lossy().to_string();
    assert_eq!(
        s.rows(),
        [name, "  src".to_string(), "  README.md".to_string()]
    );
}

#[gpui::test]
fn arrow_keys_walk_the_tree_and_open_folders(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("src/main.rs", ""), ("z.txt", "")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down");
    assert_eq!(s.selected(), Some(root.clone()));
    s.keys("down");
    assert_eq!(s.selected(), Some(root.join("src")));
    s.keys("right");
    assert!(s.rows().contains(&"    main.rs".to_string()));
    s.keys("right");
    assert_eq!(s.selected(), Some(root.join("src/main.rs")));
    s.keys("left");
    assert_eq!(s.selected(), Some(root.join("src")));
    s.keys("left");
    assert!(!s.rows().contains(&"    main.rs".to_string()));
}

#[gpui::test]
fn enter_on_a_file_opens_it_and_focuses_the_editor(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "fn a() {}")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down down enter");
    let workspace = s.workspace.clone();
    run_until(s.cx, |cx| {
        cx.update(|_, cx| workspace.read(cx).active_editor(cx).is_some())
    });
    assert_eq!(s.tabs(), ["a.rs"]);
    s.cx.simulate_input("x");
    let text =
        s.cx.update(|_, cx| workspace.read(cx).editor_state(cx).unwrap().text);
    assert_eq!(text, "xfn a() {}");
}

#[gpui::test]
fn double_clicking_a_file_opens_it(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let mut s = setup(&root, cx);
    let row = s.cx.debug_bounds("row-a.rs").expect("the row is drawn");
    s.cx.simulate_click(row.center(), Modifiers::none());
    assert!(s.tabs().is_empty(), "a single click only selects");
    s.cx.simulate_event(gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: row.center(),
        modifiers: Modifiers::none(),
        click_count: 2,
        first_mouse: false,
    });
    s.cx.simulate_event(gpui::MouseUpEvent {
        button: MouseButton::Left,
        position: row.center(),
        modifiers: Modifiers::none(),
        click_count: 2,
    });
    let workspace = s.workspace.clone();
    run_until(s.cx, |cx| {
        cx.update(|_, cx| workspace.read(cx).active_editor(cx).is_some())
    });
}

#[gpui::test]
fn opening_a_file_elsewhere_reveals_it_in_the_tree(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a/b/deep.rs", "")]);
    let mut s = setup(&root, cx);
    let workspace = s.workspace.clone();
    let path = root.join("a/b/deep.rs");
    let task = s
        .cx
        .update(|window, cx| workspace.update(cx, |w, cx| w.open_path(path.clone(), window, cx)));
    resolve(s.cx, task).unwrap();
    s.wait_for_rows(|rows| rows.iter().any(|r| r.trim() == "deep.rs"));
    let panel = s.panel.clone();
    run_until(s.cx, |cx| {
        cx.update(|_, cx| panel.read(cx).selected() == Some(path.as_path()))
    });
}

#[gpui::test]
fn a_new_file_is_created_inline_and_opened(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("src/", "")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down down");
    s.cx.dispatch_action(NewFile);
    s.settle();
    assert_eq!(s.debug()["editing"], true);
    s.cx.simulate_input("lib.rs");
    s.keys("enter");
    assert!(root.join("src/lib.rs").is_file());
    s.wait_for_rows(|rows| rows.iter().any(|r| r.trim() == "lib.rs"));
    let workspace = s.workspace.clone();
    run_until(s.cx, |cx| {
        cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).panes[0].tabs == ["lib.rs"])
    });
}

#[gpui::test]
fn a_new_folder_can_be_created_with_nested_names(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("x.txt", "")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down");
    s.cx.dispatch_action(NewFolder);
    s.cx.simulate_input("a/b");
    s.keys("enter");
    assert!(root.join("a/b").is_dir());
    assert!(s.tabs().is_empty(), "folders don't open as tabs");
}

#[gpui::test]
fn a_bad_name_keeps_the_input_open_to_fix(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("taken.rs", "keep")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down");
    s.cx.dispatch_action(NewFile);
    s.cx.simulate_input("taken.rs");
    s.keys("enter");
    assert_eq!(s.debug()["editing"], true);
    assert_eq!(fs::read_to_string(root.join("taken.rs")).unwrap(), "keep");
    let message =
        s.cx.update(|window, cx| s.workspace.read(cx).snapshot(window, cx).message);
    assert!(message.unwrap_or_default().contains("already exists"));
    s.keys("escape");
    assert_eq!(s.debug()["editing"], false);
}

#[gpui::test]
fn renaming_an_open_file_keeps_its_tab_and_unsaved_edits(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("old.rs", "text")]);
    let mut s = setup(&root, cx);
    let workspace = s.workspace.clone();
    let path = root.join("old.rs");
    let task =
        s.cx.update(|window, cx| workspace.update(cx, |w, cx| w.open_path(path, window, cx)));
    resolve(s.cx, task).unwrap();
    s.cx.simulate_input("unsaved ");

    s.focus_panel();
    s.cx.dispatch_action(Rename);
    s.cx.simulate_input("new.rs");
    s.keys("enter");
    assert!(root.join("new.rs").is_file() && !root.join("old.rs").exists());
    assert_eq!(s.tabs(), ["new.rs"]);
    let state =
        s.cx.update(|_, cx| workspace.read(cx).editor_state(cx).unwrap());
    assert_eq!(state.text, "unsaved text");
    assert!(state.dirty);
}

#[gpui::test]
fn the_rename_input_starts_with_the_current_name_selected(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("name.rs", "")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down down");
    s.cx.dispatch_action(Rename);
    // Typing replaces the whole name.
    s.cx.simulate_input("other.rs");
    s.keys("enter");
    assert!(root.join("other.rs").exists());
}

#[gpui::test]
fn deleting_asks_then_uses_the_trash(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", ""), ("b.rs", "")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down down");
    s.keys("delete");
    assert!(s.cx.has_pending_prompt());
    s.cx.simulate_prompt_answer("Cancel");
    s.settle();
    assert!(root.join("a.rs").exists());

    s.keys("delete");
    s.cx.simulate_prompt_answer("Move to Trash");
    let trashed = s.trashed.clone();
    run_until(s.cx, |_| !trashed.lock().unwrap().is_empty());
    assert_eq!(*s.trashed.lock().unwrap(), [root.join("a.rs")]);
    s.wait_for_rows(|rows| !rows.iter().any(|r| r.trim() == "a.rs"));
}

#[gpui::test]
fn the_root_cannot_be_deleted_or_renamed(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    s.keys("down");
    s.keys("delete");
    assert!(!s.cx.has_pending_prompt());
    s.cx.dispatch_action(Rename);
    assert_eq!(s.debug()["editing"], false);
}

#[gpui::test]
fn moving_an_entry_into_a_folder_carries_open_buffers_along(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "x"), ("dest/", "")]);
    let mut s = setup(&root, cx);
    let workspace = s.workspace.clone();
    let path = root.join("a.rs");
    let task =
        s.cx.update(|window, cx| workspace.update(cx, |w, cx| w.open_path(path, window, cx)));
    resolve(s.cx, task).unwrap();

    let (from, onto) = (root.join("a.rs"), root.join("dest"));
    s.cx.update(|_, cx| s.panel.update(cx, |p, cx| p.move_entry(&from, &onto, cx)));
    assert!(root.join("dest/a.rs").is_file());
    s.wait_for_rows(|rows| rows.iter().any(|r| r == "    a.rs"));
    let path =
        s.cx.update(|_, cx| workspace.read(cx).editor_state(cx).unwrap().path);
    assert_eq!(path, Some(root.join("dest/a.rs").display().to_string()));
}

#[gpui::test]
fn a_folder_cannot_be_dropped_into_itself(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a/inner/", "")]);
    let s = setup(&root, cx);
    let (from, onto) = (root.join("a"), root.join("a/inner"));
    s.cx.update(|_, cx| s.panel.update(cx, |p, cx| p.move_entry(&from, &onto, cx)));
    assert!(root.join("a/inner").is_dir());
}

#[gpui::test]
fn files_created_outside_the_app_appear(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let mut s = setup(&root, cx);
    // Give the watcher a moment to start before changing anything.
    std::thread::sleep(std::time::Duration::from_millis(200));
    fs::write(root.join("external.rs"), "").unwrap();
    s.wait_for_rows(|rows| rows.iter().any(|r| r.trim() == "external.rs"));
    fs::remove_file(root.join("a.rs")).unwrap();
    s.wait_for_rows(|rows| !rows.iter().any(|r| r.trim() == "a.rs"));
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
}

#[gpui::test]
fn git_changes_and_ignored_files_are_marked(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[
        ("tracked.rs", "1"),
        ("clean.rs", "same"),
        (".gitignore", "target/\n"),
    ]);
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@example.com"]);
    git(&root, &["config", "user.name", "T"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    fs::write(root.join("tracked.rs"), "2").unwrap();
    fs::write(root.join("new.rs"), "").unwrap();
    fs::create_dir(root.join("target")).unwrap();

    let mut s = setup(&root, cx);
    let panel = s.panel.clone();
    run_until(s.cx, |cx| {
        let state = cx.update(|_, cx| panel.read(cx).debug_state(cx));
        state["git"]["tracked.rs"] == "Modified" && state["git"]["new.rs"] == "Untracked"
    });
    let state = s.debug();
    assert!(state["git"].get("clean.rs").is_none());
    let ignored = s.cx.update(|_, cx| {
        panel
            .read(cx)
            .rows
            .iter()
            .find(|r| r.entry.name == "target")
            .map(|r| r.entry.ignored)
    });
    assert_eq!(ignored, Some(true));
}

#[gpui::test]
fn the_tree_state_is_saved_and_restored(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a/b/c.rs", ""), ("z.rs", "")]);
    let mut s = setup(&root, cx);
    s.focus_panel();
    // One at a time: moving into a folder needs its contents loaded first.
    for key in ["down", "down", "right", "right", "right", "right"] {
        s.keys(key);
    }
    let saved = s.cx.update(|_, cx| s.panel.read(cx).save_state(cx));
    assert_eq!(saved["expanded"], serde_json::json!(["", "a", "a/b"]));

    let workspace = s.workspace.clone();
    let panel = s.cx.update(|window, cx| {
        let store = workspace.read(cx).store().clone();
        let project = workspace.read(cx).project().cloned();
        let weak = workspace.downgrade();
        cx.new(|cx| ProjectPanel::new(weak, workspace.clone(), store, project, window, cx))
    });
    s.cx.update(|window, cx| {
        panel.update(cx, |p, cx| p.restore_state(saved.clone(), window, cx));
    });
    run_until(s.cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .visible_names()
                .iter()
                .any(|r| r.trim() == "c.rs")
        })
    });
    let selected =
        s.cx.update(|_, cx| panel.read(cx).selected().map(Path::to_path_buf));
    assert_eq!(selected, Some(root.join("a/b/c.rs")));
}

#[gpui::test]
fn right_clicking_opens_a_menu_whose_items_run_actions(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("src/", "")]);
    let mut s = setup(&root, cx);
    let row = s.cx.debug_bounds("row-src").expect("the row is drawn");
    s.cx.simulate_mouse_down(row.center(), MouseButton::Right, Modifiers::none());
    s.cx.simulate_mouse_up(row.center(), MouseButton::Right, Modifiers::none());
    s.settle();
    assert!(s.cx.update(|_, cx| s.panel.read(cx).menu.is_some()));
    // The first item is New File, which starts an inline edit in the clicked folder. The menu
    // closing hands focus back to the tree, which mustn't happen after the edit takes it, or the
    // edit would be cancelled straight away.
    s.keys("down enter");
    assert_eq!(s.debug()["editing"], true);
    let focused_input = s.cx.update(|window, cx| {
        let panel = s.panel.read(cx);
        panel
            .edit
            .as_ref()
            .is_some_and(|e| e.input.read(cx).focus_handle(cx).is_focused(window))
    });
    assert!(focused_input);
    s.cx.simulate_input("made.rs");
    s.keys("enter");
    assert!(root.join("src/made.rs").is_file());
}

#[gpui::test]
fn escape_returns_to_the_editor(cx: &mut TestAppContext) {
    let (_dir, root) = project(&[("a.rs", "")]);
    let mut s = setup(&root, cx);
    let workspace = s.workspace.clone();
    let path = root.join("a.rs");
    let task =
        s.cx.update(|window, cx| workspace.update(cx, |w, cx| w.open_path(path, window, cx)));
    resolve(s.cx, task).unwrap();
    s.focus_panel();
    s.keys("escape");
    s.cx.simulate_input("typed");
    let text =
        s.cx.update(|_, cx| workspace.read(cx).editor_state(cx).unwrap().text);
    assert_eq!(text, "typed");
}

#[gpui::test]
fn a_large_folder_only_reads_what_is_shown(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    for folder in 0..50 {
        let path = root.join(format!("dir{folder}"));
        fs::create_dir(&path).unwrap();
        for file in 0..200 {
            fs::write(path.join(format!("f{file}.rs")), "").unwrap();
        }
    }
    let mut s = setup(&root, cx);
    // Ten thousand files, but only the root is read: its 50 folders show, closed. Counting reads
    // rather than timing them keeps this from failing on a busy machine.
    assert_eq!(s.rows().len(), 51);
    let panel = s.panel.clone();
    let loaded = s.cx.update(|_, cx| panel.read(cx).loaded_folder_count());
    assert_eq!(loaded, 1);
}
