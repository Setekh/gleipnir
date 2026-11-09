use super::*;
use gleipnir_core::BufferStore;
use gleipnir_test_support::{isolate_state, run_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use std::path::PathBuf;

fn git(root: &Path, args: &[&str]) {
    git_ops::git(root, args).unwrap();
}

/// A repository with two committed files, and a workspace on it with the Git windows.
fn setup(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    PathBuf,
    Entity<Workspace>,
    &mut VisualTestContext,
) {
    isolate_state();
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "user.email", "t@t"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(root.join("b.txt"), "bee\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "first"]);
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_ui::bind_context_menu_keys(cx);
        gleipnir_editor::bind_keys(cx);
        gleipnir_workspace::bind_keys(cx);
        bind_keys(cx);
    });
    let store = cx.new(|_| BufferStore::new());
    let workspace_root = root.clone();
    let (workspace, cx) = cx.add_window_view(move |window, cx| {
        let mut workspace = Workspace::new(store, Some(workspace_root), window, cx);
        add_to_workspace(&mut workspace, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1400.), px(900.)));
    (dir, root, workspace, cx)
}

fn commit_panel(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<CommitPanel> {
    cx.update(|_, cx| workspace.read(cx).panel::<CommitPanel>(cx))
        .expect("the Commit window")
}

fn changed(panel: &Entity<CommitPanel>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        panel
            .read(cx)
            .files()
            .iter()
            .map(|f| f.relative.clone())
            .collect()
    })
}

#[gpui::test]
fn the_commit_window_commits_only_the_ticked_files(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    std::fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(root.join("b.txt"), "changed\n").unwrap();
    std::fs::write(root.join("c.txt"), "new\n").unwrap();
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    });
    let panel = commit_panel(&workspace, cx);
    run_until(cx, |cx| changed(&panel, cx).len() == 3);
    assert_eq!(changed(&panel, cx), ["a.txt", "b.txt", "c.txt"]);
    // The cursor is in the message, so typing writes it.
    cx.simulate_input("Change a and add c");
    assert_eq!(
        cx.update(|_, cx| panel.read(cx).message_text(cx)),
        "Change a and add c"
    );
    // Leave b out.
    let tick = cx.debug_bounds("commit-tick-1").expect("b's box");
    cx.simulate_click(tick.center(), Modifiers::none());
    let button = cx.debug_bounds("commit-button").expect("Commit");
    cx.simulate_click(button.center(), Modifiers::none());
    run_until(cx, |cx| changed(&panel, cx) == ["b.txt"]);
    let log = git_ops::history(&root, 5).unwrap();
    assert_eq!(log[0].subject, "Change a and add c");
    assert_eq!(cx.update(|_, cx| panel.read(cx).message_text(cx)), "");
}

#[gpui::test]
fn rollback_lines_puts_a_change_back_as_committed(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    let path = root.join("a.txt");
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| w.open_path(path.clone(), window, cx).detach())
    });
    run_until(cx, |cx| {
        cx.update(|_, cx| workspace.read(cx).active_editor(cx).is_some())
    });
    let editor = cx.update(|_, cx| workspace.read(cx).active_editor(cx).unwrap());
    let buffer = cx.update(|_, cx| editor.read(cx).buffer().clone());
    run_until(cx, |cx| {
        cx.update(|_, cx| buffer.read(cx).committed_text().is_some())
    });
    // "two" becomes two other lines.
    cx.update(|_, cx| {
        buffer.update(cx, |b, cx| {
            b.apply_external_edits(
                vec![gleipnir_text::TextEdit::new(4..7, "2\n2b".to_string())],
                cx,
            )
        })
    });
    let marks =
        |cx: &mut VisualTestContext| cx.update(|_, cx| buffer.read(cx).line_changes().len());
    run_until(cx, |cx| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(200));
        marks(cx) == 1
    });
    cx.update(|_, cx| editor.update(cx, |e, cx| e.jump_to(2, 0, cx)));
    cx.dispatch_action(RollbackLines);
    let text = |cx: &mut VisualTestContext| cx.update(|_, cx| buffer.read(cx).buffer().text());
    assert_eq!(text(cx), "one\ntwo\nthree\n");
    // As one step, which undo takes back.
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    assert_eq!(text(cx), "one\n2\n2b\nthree\n");
}

#[gpui::test]
fn a_files_changes_show_side_by_side(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    std::fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| {
            show_file_diff(w, &root.join("a.txt"), window, cx)
        })
    });
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<DiffView>())
        .expect("the diff");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).debug_rows()),
        ["  one | one", "~ two | TWO", "  three | three", "+  | four"]
    );
    cx.simulate_keystrokes("escape");
    assert!(cx.update(|_, cx| workspace.read(cx).active_modal::<DiffView>().is_none()));
}

#[gpui::test]
fn the_log_lists_commits_and_what_each_changed(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    std::fs::write(root.join("a.txt"), "changed\n").unwrap();
    git(&root, &["commit", "-q", "-am", "second"]);
    let log = cx.update(|_, cx| workspace.read(cx).panel::<LogPanel>(cx).unwrap());
    log.update(cx, |log, cx| log.refresh(cx));
    run_until(cx, |cx| {
        cx.update(|_, cx| log.read(cx).commits().len() == 2)
    });
    log.update(cx, |log, cx| log.select(0, cx));
    run_until(cx, |cx| cx.update(|_, cx| log.read(cx).files().len() == 1));
    cx.update(|window, cx| log.update(cx, |log, cx| log.show_file(0, window, cx)));
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<DiffView>())
        .expect("the commit's diff");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).debug_rows()),
        // Three lines replaced by one: one change, the shorter side padded.
        ["~ one | changed", "~ two | ", "~ three | "]
    );
}

#[gpui::test]
fn a_conflict_is_settled_from_the_commit_window(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    git(&root, &["switch", "-q", "-c", "other"]);
    std::fs::write(root.join("b.txt"), "theirs\n").unwrap();
    git(&root, &["commit", "-q", "-am", "theirs"]);
    git(&root, &["switch", "-q", "main"]);
    std::fs::write(root.join("b.txt"), "ours\n").unwrap();
    git(&root, &["commit", "-q", "-am", "ours"]);
    assert!(git_ops::git(&root, &["merge", "other"]).is_err());
    let panel = commit_panel(&workspace, cx);
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.show_panel(COMMIT_PANEL, cx)));
    panel.update(cx, |p, cx| p.refresh(cx));
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .files()
                .first()
                .is_some_and(|f| f.state == gleipnir_index::GitState::Conflicted)
        })
    });
    cx.run_until_parked();
    let yours = cx.debug_bounds("take-yours-0").expect("Yours");
    cx.simulate_click(yours.center(), Modifiers::none());
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .files()
                .iter()
                .all(|f| f.state != gleipnir_index::GitState::Conflicted)
        })
    });
    assert_eq!(
        std::fs::read_to_string(root.join("b.txt")).unwrap(),
        "ours\n"
    );
}

#[gpui::test]
fn a_diff_is_highlighted_and_puts_changes_back_one_at_a_time(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    let path = root.join("lib.rs");
    std::fs::write(&path, "fn one() {}\nfn two() {}\nfn three() {}\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "lib"]);
    std::fs::write(
        &path,
        "fn ONE() {}\nfn two() {}\nfn three() {}\nfn four() {}\n",
    )
    .unwrap();
    cx.update(|window, cx| workspace.update(cx, |w, cx| show_file_diff(w, &path, window, cx)));
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<DiffView>())
        .expect("the diff");
    let (left, right) = cx.update(|_, cx| view.read(cx).highlight_count());
    assert!(
        left > 0 && right > 0,
        "highlighted as Rust: {left}, {right}"
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).change_rows()), [0, 3]);
    // Putting back the added function leaves the renamed one.
    cx.run_until_parked();
    let revert = cx
        .debug_bounds("diff-revert-3")
        .expect("the second change's ⟲");
    cx.simulate_click(revert.center(), Modifiers::none());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn ONE() {}\nfn two() {}\nfn three() {}\n"
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).change_rows()), [0]);
    view.update(cx, |v, cx| v.revert_change(0, cx));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn one() {}\nfn two() {}\nfn three() {}\n"
    );
}

#[gpui::test]
fn the_log_shows_every_branch_and_cherry_picks_from_a_commits_menu(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    git(&root, &["switch", "-q", "-c", "side"]);
    std::fs::write(root.join("c.txt"), "side\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "on side"]);
    git(&root, &["switch", "-q", "main"]);
    let log = cx.update(|_, cx| workspace.read(cx).panel::<LogPanel>(cx).unwrap());
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.show_panel(LOG_PANEL, cx)));
    log.update(cx, |log, cx| log.refresh(cx));
    run_until(cx, |cx| {
        cx.update(|_, cx| log.read(cx).commits().len() == 2)
    });
    let side = cx.update(|_, cx| log.read(cx).commits()[0].clone());
    assert_eq!(side.subject, "on side");
    assert_eq!(side.refs, ["side"]);
    cx.run_until_parked();
    let row = cx.debug_bounds("log-commit-0").expect("the side commit");
    cx.simulate_mouse_down(row.center(), gpui::MouseButton::Right, Modifiers::none());
    let labels = cx.update(|_, cx| {
        workspace
            .read(cx)
            .open_menu()
            .map(|m| m.read(cx).labels())
            .unwrap_or_default()
    });
    let pick = labels
        .iter()
        .position(|l| l == "Cherry-Pick")
        .unwrap_or_else(|| panic!("{labels:?}"));
    let item = cx.debug_bounds(format!("menu-item-{pick}").leak()).unwrap();
    cx.simulate_click(item.center(), Modifiers::none());
    run_until(cx, |_| root.join("c.txt").exists());
    assert_eq!(git_ops::history(&root, 5).unwrap()[0].subject, "on side");
}

#[gpui::test]
fn the_merge_window_merges_one_sided_changes_and_settles_conflicts(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    // a.txt is "one\ntwo\nthree\n". Ours changes one and three; theirs two and three.
    git(&root, &["switch", "-q", "-c", "other"]);
    std::fs::write(root.join("a.txt"), "one\nTWO\nthree-theirs\n").unwrap();
    git(&root, &["commit", "-q", "-am", "theirs"]);
    git(&root, &["switch", "-q", "main"]);
    std::fs::write(root.join("a.txt"), "ONE\ntwo\nthree-ours\n").unwrap();
    git(&root, &["commit", "-q", "-am", "ours"]);
    assert!(git_ops::git(&root, &["merge", "other"]).is_err());
    let panel = commit_panel(&workspace, cx);
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.show_panel(COMMIT_PANEL, cx)));
    panel.update(cx, |p, cx| p.refresh(cx));
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .files()
                .first()
                .is_some_and(|f| f.state == gleipnir_index::GitState::Conflicted)
        })
    });
    cx.run_until_parked();
    let merge = cx.debug_bounds("merge-0").expect("Merge…");
    cx.simulate_click(merge.center(), Modifiers::none());
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<MergeView>())
        .expect("the merge window");
    assert_eq!(cx.update(|_, cx| view.read(cx).conflicts()), (1, 1));
    assert_eq!(cx.update(|_, cx| view.read(cx).result()), None);
    // Theirs changed lines two and three, ours three: with nothing unchanged between, that's
    // one conflict, as git has it. Ours's change to line one merged by itself.
    cx.update(|_, cx| view.update(cx, |v, cx| v.choose_all(Choice::Theirs, cx)));
    assert_eq!(cx.update(|_, cx| view.read(cx).conflicts()), (1, 0));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).result()).as_deref(),
        Some("ONE\nTWO\nthree-theirs\n")
    );
    cx.run_until_parked();
    let apply = cx.debug_bounds("merge-apply").expect("Apply");
    cx.simulate_click(apply.center(), Modifiers::none());
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "ONE\nTWO\nthree-theirs\n"
    );
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .files()
                .iter()
                .all(|f| f.state != gleipnir_index::GitState::Conflicted)
        })
    });
}
