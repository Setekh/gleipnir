use super::*;
use gleipnir_core::BufferStore;
use gleipnir_packages::settings::{DiffViewer, DiffWhitespace};
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

use gleipnir_workspace::Panel as _;

fn commit_file(root: &Path, name: &str, text: &str, message: &str) {
    std::fs::write(root.join(name), text).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", message]);
}

fn subjects(root: &Path) -> Vec<String> {
    git_ops::history(root, 20)
        .unwrap()
        .into_iter()
        .map(|c| c.subject)
        .collect()
}

#[gpui::test]
fn the_log_draws_a_lane_for_a_branch_and_joins_it_at_the_merge(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    git(&root, &["switch", "-q", "-c", "side"]);
    commit_file(&root, "c.txt", "side\n", "on side");
    git(&root, &["switch", "-q", "main"]);
    commit_file(&root, "d.txt", "main\n", "on main");
    git(
        &root,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );
    let log = cx.update(|_, cx| workspace.read(cx).panel::<LogPanel>(cx).unwrap());
    cx.update(|_, cx| workspace.update(cx, |w, cx| w.show_panel(LOG_PANEL, cx)));
    log.update(cx, |log, cx| log.refresh(cx));
    run_until(cx, |cx| {
        cx.update(|_, cx| log.read(cx).commits().len() == 4)
    });
    let (subjects, lanes): (Vec<String>, Vec<usize>) = cx.update(|_, cx| {
        let log = log.read(cx);
        log.commits()
            .iter()
            .zip(log.graph())
            .map(|(c, r)| (c.subject.clone(), r.lane))
            .unzip()
    });
    // The merge in the first lane, each branch's own commit in a lane of its own, and the
    // lanes meeting again at the first commit.
    let lane = |subject: &str| lanes[subjects.iter().position(|s| s == subject).unwrap()];
    assert_eq!(subjects[0], "merge side");
    assert_eq!(lane("merge side"), 0);
    assert_eq!(lane("first"), 0);
    assert_ne!(lane("on side"), lane("on main"));
    let widths: Vec<usize> =
        cx.update(|_, cx| log.read(cx).graph().iter().map(|r| r.width).collect());
    assert_eq!(widths, [2, 2, 2, 2]);
    // The rows on screen are drawn, graph and all.
    cx.run_until_parked();
    assert!(cx.debug_bounds("log-commit-3").is_some());
}

/// Opens a file in the workspace and waits for its editor.
fn open(
    workspace: &Entity<Workspace>,
    path: &Path,
    cx: &mut VisualTestContext,
) -> Entity<gleipnir_editor::EditorView> {
    let path = path.to_path_buf();
    cx.update(|window, cx| workspace.update(cx, |w, cx| w.open_path(path, window, cx).detach()));
    run_until(cx, |cx| {
        cx.update(|_, cx| workspace.read(cx).active_editor(cx).is_some())
    });
    cx.update(|_, cx| workspace.read(cx).active_editor(cx).unwrap())
}

#[gpui::test]
fn blame_labels_each_line_follows_edits_and_shows_the_commit_in_the_log(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    // a.txt is "one\ntwo\nthree\n" from "first"; "two" changes in a second commit.
    commit_file(
        &root,
        "a.txt",
        "one\nTWO\nthree\n",
        "second\n\nWith a body.",
    );
    let editor = open(&workspace, &root.join("a.txt"), cx);
    cx.dispatch_action(ToggleBlame);
    let labels = |cx: &mut VisualTestContext| cx.update(|_, cx| editor.read(cx).gutter_labels());
    run_until(cx, |cx| labels(cx).len() == 3);
    let shown = labels(cx);
    assert!(
        shown[0].starts_with("1: ") && shown[0].ends_with(" t"),
        "{shown:?}"
    );
    let details = cx
        .update(|_, cx| editor.read(cx).gutter_details(1))
        .unwrap();
    assert!(details.contains("t <t@t>"), "{details}");
    assert!(details.ends_with("second\n\nWith a body."), "{details}");
    let group =
        |line, cx: &mut VisualTestContext| cx.update(|_, cx| editor.read(cx).gutter_group(line));
    assert_ne!(group(0, cx), group(1, cx));
    assert_eq!(group(0, cx), group(2, cx));

    // A line typed above moves the labels down at once, and has none itself.
    let buffer = cx.update(|_, cx| editor.read(cx).buffer().clone());
    cx.update(|_, cx| {
        buffer.update(cx, |b, cx| {
            b.apply_external_edits(vec![gleipnir_text::TextEdit::new(0..0, "new\n")], cx)
        })
    });
    let shown = labels(cx);
    assert_eq!(shown.len(), 3);
    assert!(shown[0].starts_with("2: "), "{shown:?}");
    // "TWO" typed over and put back: its label goes as it changes, and comes back once it's
    // blamed again after typing stops, against the unsaved text.
    let edit = |range: std::ops::Range<usize>, text: &str, cx: &mut VisualTestContext| {
        let text = text.to_string();
        cx.update(|_, cx| {
            buffer.update(cx, |b, cx| {
                b.apply_external_edits(vec![gleipnir_text::TextEdit::new(range, text)], cx)
            })
        })
    };
    edit(11..11, "x", cx);
    edit(11..12, "", cx);
    assert!(
        cx.update(|_, cx| editor.read(cx).gutter_details(2))
            .is_none()
    );
    run_until(cx, |cx| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(500));
        cx.update(|_, cx| editor.read(cx).gutter_details(2))
            .is_some()
    });
    assert_ne!(group(2, cx), group(1, cx));
    assert_eq!(group(1, cx), group(3, cx));
    assert!(
        cx.update(|_, cx| editor.read(cx).gutter_details(0))
            .is_none()
    );

    // Clicking a label chooses its commit in the log.
    let log = cx.update(|_, cx| workspace.read(cx).panel::<LogPanel>(cx).unwrap());
    log.update(cx, |log, cx| log.refresh(cx));
    run_until(cx, |cx| {
        cx.update(|_, cx| log.read(cx).commits().len() == 2)
    });
    cx.update(|_, cx| {
        editor.update(cx, |_, cx| {
            cx.emit(gleipnir_editor::EditorEvent::GutterTextClicked { line: 2 })
        })
    });
    run_until(cx, |cx| {
        cx.update(|_, cx| log.read(cx).selected() == Some(0))
    });

    // And the same action takes the labels away.
    cx.dispatch_action(ToggleBlame);
    assert!(labels(cx).is_empty());
}

#[gpui::test]
fn a_file_not_yet_committed_says_so_when_annotated(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    let editor = open(&workspace, &root.join("new.txt"), cx);
    cx.dispatch_action(ToggleBlame);
    run_until(cx, |cx| {
        cx.update(|window, cx| workspace.read(cx).snapshot(window, cx).message)
            .is_some_and(|m| m.starts_with("Couldn't annotate"))
    });
    assert!(!cx.update(|_, cx| editor.read(cx).has_gutter_text()));
}

/// Three commits on a.txt's first line, then one adding b2.txt, after "first".
fn history_to_rebase(root: &Path) {
    commit_file(root, "a.txt", "one v2\ntwo\nthree\n", "a two");
    commit_file(root, "b2.txt", "bee two\n", "add b2");
    commit_file(root, "a.txt", "one v3\ntwo\nthree\n", "a three");
    commit_file(root, "c.txt", "see\n", "add c");
}

fn entries_from(root: &Path, subject: &str) -> (Vec<RebaseEntry>, rebase::RebaseRange) {
    let hash = git_ops::history(root, 20)
        .unwrap()
        .into_iter()
        .find(|c| c.subject == subject)
        .unwrap()
        .hash;
    rebase::rebase_entries(root, &hash).unwrap()
}

#[test]
fn a_rebase_rewords_squashes_fixes_up_drops_and_reorders_for_real() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "user.email", "t@t"]);
    commit_file(&root, "a.txt", "one\ntwo\nthree\n", "first");
    history_to_rebase(&root);
    let (mut entries, range) = entries_from(&root, "a two");
    let shown: Vec<&str> = entries.iter().map(|e| e.subject.as_str()).collect();
    assert_eq!(shown, ["a two", "add b2", "a three", "add c"]);
    // Reword "a two", squash "a three" into it with "add b2" moved after, and drop "add c".
    entries.swap(1, 2);
    entries[0].action = RebaseAction::Reword;
    entries[0].new_message = Some("a, rewritten\n\nWhy.".into());
    entries[1].action = RebaseAction::Squash;
    entries[3].action = RebaseAction::Drop;
    let message = rebase::start(&root, &range, &entries).unwrap();
    assert_eq!(message, "Rebased 4 commits");
    assert_eq!(subjects(&root), ["add b2", "a, rewritten", "first"]);
    assert_eq!(
        git_ops::last_message(&root).unwrap(),
        "add b2",
        "the commits after keep their messages"
    );
    let squashed = git_ops::git(&root, &["log", "-1", "--format=%B", "HEAD~1"]).unwrap();
    assert_eq!(squashed.trim_end(), "a, rewritten\n\nWhy.\n\na three");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one v3\ntwo\nthree\n"
    );
    assert!(!root.join("c.txt").exists());
    assert!(git_ops::operation_in_progress(&root).is_none());
    assert!(!root.join(".git/gleipnir-rebase").exists(), "tidied up");

    // A fixup keeps the first message, and from the very first commit there's no parent.
    let (mut entries, range) = entries_from(&root, "first");
    entries[1].action = RebaseAction::Fixup;
    rebase::start(&root, &range, &entries).unwrap();
    assert_eq!(subjects(&root), ["add b2", "first"]);

    // The branch moving on meanwhile stops it before it starts.
    let (entries, range) = entries_from(&root, "add b2");
    commit_file(&root, "d.txt", "dee\n", "more");
    assert!(rebase::start(&root, &range, &entries).is_err());
    assert_eq!(subjects(&root)[0], "more");
}

#[test]
fn only_commits_on_the_branch_without_merges_after_them_can_be_rebased_from() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "user.email", "t@t"]);
    commit_file(&root, "a.txt", "a\n", "first");
    git(&root, &["switch", "-q", "-c", "side"]);
    commit_file(&root, "s.txt", "s\n", "on side");
    git(&root, &["switch", "-q", "main"]);
    let side = git_ops::git(&root, &["rev-parse", "side"]).unwrap();
    let error = rebase::rebase_entries(&root, side.trim()).unwrap_err();
    assert!(format!("{error}").contains("current branch"), "{error}");
    git(&root, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    let first = git_ops::git(&root, &["rev-parse", "HEAD~1"]).unwrap();
    let error = rebase::rebase_entries(&root, first.trim()).unwrap_err();
    assert!(format!("{error}").contains("merges"), "{error}");
}

#[gpui::test]
fn the_rebase_window_reorders_and_a_conflict_stops_it_for_the_commit_window(
    cx: &mut TestAppContext,
) {
    let (_dir, root, workspace, cx) = setup(cx);
    history_to_rebase(&root);
    let hash = git_ops::history(&root, 20).unwrap()[3].hash.clone();
    cx.update(|window, cx| {
        workspace.update(cx, |w, cx| rebase_view::open_rebase(w, &hash, window, cx))
    });
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<RebaseView>())
        .expect("the rebase window");
    let rows = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).rows());
    assert_eq!(rows(cx).len(), 4);
    assert!(rows(cx)[0].starts_with("pick ") && rows(cx)[0].ends_with(" a two"));
    // Keys on the list: down to "a three", which moves up above "add b2", then above "a
    // two", which it can't apply on without a conflict.
    cx.simulate_keystrokes("down down alt-up alt-up");
    let shown: Vec<String> = rows(cx)
        .iter()
        .map(|r| {
            r.split_once(' ')
                .unwrap()
                .1
                .split_once(' ')
                .unwrap()
                .1
                .to_string()
        })
        .collect();
    assert_eq!(shown, ["a three", "a two", "add b2", "add c"]);
    // Reword it, writing a new message in the box.
    cx.simulate_keystrokes("r");
    assert!(cx.update(|_, cx| view.read(cx).is_editing_message()));
    assert_eq!(cx.update(|_, cx| view.read(cx).message_text(cx)), "a three");
    view.update(cx, |v, cx| v.set_message("a three, reworded", cx));
    cx.run_until_parked();
    let start = cx.debug_bounds("rebase-start").expect("Start Rebasing");
    cx.simulate_click(start.center(), Modifiers::none());
    assert!(cx.update(|_, cx| workspace.read(cx).active_modal::<RebaseView>().is_none()));

    // It stops at the conflict, which the Commit window lists, with Continue and Abort.
    let panel = commit_panel(&workspace, cx);
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .files()
                .first()
                .is_some_and(|f| f.state == gleipnir_index::GitState::Conflicted)
        })
    });
    assert_eq!(
        git_ops::operation_in_progress(&root),
        Some(git_ops::Operation::Rebase)
    );
    assert_eq!(
        cx.update(|_, cx| panel.read(cx).debug_state(cx))["operation"],
        "Rebasing"
    );
    // Settled with the commit being applied, then continued; "a two" then conflicts the other
    // way round, and is settled the same way.
    settle_with_theirs_and_continue(&panel, cx);
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .files()
                .first()
                .is_some_and(|f| f.state == gleipnir_index::GitState::Conflicted)
        })
    });
    settle_with_theirs_and_continue(&panel, cx);
    run_until(cx, |_| git_ops::operation_in_progress(&root).is_none());
    // The reword was made once its commit was, conflict and all.
    assert_eq!(
        subjects(&root),
        ["add c", "add b2", "a two", "a three, reworded", "first"]
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one v2\ntwo\nthree\n"
    );
    run_until(cx, |cx| {
        cx.update(|_, cx| panel.read(cx).debug_state(cx))["operation"].is_null()
    });
}

/// Takes the side of the commit being applied for the one conflicted file, then continues.
fn settle_with_theirs_and_continue(panel: &Entity<CommitPanel>, cx: &mut VisualTestContext) {
    cx.run_until_parked();
    let theirs = cx.debug_bounds("take-theirs-0").expect("Theirs");
    cx.simulate_click(theirs.center(), Modifiers::none());
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            panel
                .read(cx)
                .files()
                .iter()
                .all(|f| f.state != gleipnir_index::GitState::Conflicted)
        })
    });
    cx.run_until_parked();
    let resume = cx.debug_bounds("operation-continue").expect("Continue");
    cx.simulate_click(resume.center(), Modifiers::none());
}

#[gpui::test]
fn abort_puts_a_stopped_rebase_back(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    history_to_rebase(&root);
    let before = subjects(&root);
    let (mut entries, range) = entries_from(&root, "a two");
    entries.swap(0, 2);
    let message = rebase::start(&root, &range, &entries).unwrap();
    assert!(message.contains("stopped"), "{message}");
    let panel = commit_panel(&workspace, cx);
    panel.update(cx, |p, cx| p.refresh(cx));
    run_until(cx, |cx| {
        cx.update(|_, cx| panel.read(cx).debug_state(cx))["operation"] == "Rebasing"
    });
    cx.dispatch_action(AbortOperation);
    run_until(cx, |_| git_ops::operation_in_progress(&root).is_none());
    assert_eq!(subjects(&root), before);
    assert!(!root.join(".git/gleipnir-rebase").exists());
    run_until(cx, |cx| {
        cx.update(|_, cx| panel.read(cx).debug_state(cx))["operation"].is_null()
    });
}

/// A Rust file committed, then changed: a space after line 3, and a word in line 15 of 30.
fn changed_file(root: &Path) -> PathBuf {
    let lines = |changed: bool| {
        (0..30)
            .map(|i| match i {
                3 if changed => "let x3 = 3;  ".to_string(),
                15 if changed => "let sum = price * count;".to_string(),
                15 => "let total = price * count;".to_string(),
                i => format!("let x{i} = {i};"),
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let path = root.join("lib.rs");
    std::fs::write(&path, lines(false)).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "lib"]);
    std::fs::write(&path, lines(true)).unwrap();
    path
}

#[gpui::test]
fn the_diff_toolbar_changes_how_changes_show(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    let path = changed_file(&root);
    cx.update(|window, cx| workspace.update(cx, |w, cx| show_file_diff(w, &path, window, cx)));
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<DiffView>())
        .expect("the diff");
    let shown = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).debug_display());

    // At first: both changes, the changed words marked, and long unchanged runs folded.
    assert_eq!(cx.update(|_, cx| view.read(cx).change_rows()), [3, 15]);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).debug_words()),
        (
            vec!["total".to_string()],
            vec!["  ".to_string(), "sum".to_string()]
        )
    );
    let display = shown(cx);
    assert!(display.contains(&"⋯ 3".to_string()), "{display:?}");
    assert!(display.contains(&"⋯ 10".to_string()), "{display:?}");

    // Trimmed whitespace leaves only the real change.
    view.update(cx, |v, cx| {
        v.set_options(|o| o.whitespace = DiffWhitespace::Trim, cx)
    });
    assert_eq!(cx.update(|_, cx| view.read(cx).change_rows()), [15]);

    // The unified viewer, from the toolbar's menu: the old line above the new.
    cx.run_until_parked();
    let viewer = cx.debug_bounds("diff-viewer").expect("the viewer menu");
    cx.simulate_click(viewer.center(), Modifiers::none());
    cx.run_until_parked();
    let unified = cx.debug_bounds("menu-item-1").expect("Unified viewer");
    cx.simulate_click(unified.center(), Modifiers::none());
    cx.run_until_parked();
    let display = shown(cx);
    let at = display
        .iter()
        .position(|l| l == "- let total = price * count;")
        .expect("the old line");
    assert_eq!(display[at + 1], "+ let sum = price * count;");
    assert_eq!(
        cx.update(|_, cx| gleipnir_core::settings(cx).diff.viewer),
        DiffViewer::Unified,
        "kept for next time"
    );

    // A fold opens when clicked.
    let fold = cx
        .debug_bounds("diff-fold-20")
        .expect("the fold after the change");
    cx.simulate_click(fold.center(), Modifiers::none());
    let display = shown(cx);
    assert!(!display.contains(&"⋯ 10".to_string()), "{display:?}");
    assert!(display.contains(&"  let x29 = 29;".to_string()));

    cx.update(|_, cx| gleipnir_core::update_settings(cx, |s| s.diff = Default::default()))
        .unwrap();
}

#[gpui::test]
fn a_diff_scrolls_sideways_and_jumps_to_the_code(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    let path = root.join("long.txt");
    let long = "word ".repeat(200);
    std::fs::write(&path, format!("a\nb\n{long}\nc\nd\n")).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "long"]);
    std::fs::write(&path, format!("a\nb\n{long}\nC\nd\n")).unwrap();
    cx.update(|window, cx| workspace.update(cx, |w, cx| show_file_diff(w, &path, window, cx)));
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<DiffView>())
        .expect("the diff");
    cx.run_until_parked();

    // A sideways swipe over the code.
    let middle = cx.debug_bounds("diff-revert-3").expect("the change's row");
    cx.simulate_event(gpui::ScrollEvent {
        position: middle.center() - gpui::point(px(200.), px(0.)),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(-120.), px(0.))),
        ..Default::default()
    });
    assert_eq!(cx.update(|_, cx| view.read(cx).scroll_x()), px(120.));

    // F4 opens the file at the change.
    cx.simulate_keystrokes("f4");
    run_until(cx, |cx| {
        cx.update(|_, cx| {
            workspace.read(cx).active_modal::<DiffView>().is_none()
                && workspace.read(cx).active_editor(cx).is_some_and(|editor| {
                    let editor = editor.read(cx);
                    let text = editor.buffer().read(cx).buffer();
                    text.offset_to_point(editor.selections().primary().head)
                        .line
                        == 3
                })
        })
    });
}

#[gpui::test]
fn with_sync_scrolling_off_each_side_scrolls_on_its_own(cx: &mut TestAppContext) {
    let (_dir, root, workspace, cx) = setup(cx);
    let path = root.join("long.txt");
    let lines = |changed: &str| {
        (0..200)
            .map(|i| match i {
                3 => format!("{changed} {}", "word ".repeat(80)),
                i => format!("line {i}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    std::fs::write(&path, lines("old")).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "long"]);
    std::fs::write(&path, lines("new")).unwrap();
    cx.update(|window, cx| workspace.update(cx, |w, cx| show_file_diff(w, &path, window, cx)));
    let view = cx
        .update(|_, cx| workspace.read(cx).active_modal::<DiffView>())
        .expect("the diff");
    view.update(cx, |v, cx| {
        v.set_options(|o| o.collapse_unchanged = false, cx)
    });
    cx.run_until_parked();
    let sync = cx.debug_bounds("diff-sync").expect("Sync scrolling");
    cx.simulate_click(sync.center(), Modifiers::none());
    cx.run_until_parked();

    // Down and sideways over the left side only.
    let left = cx.debug_bounds("diff-rows-left").expect("the left side");
    for delta in [
        gpui::point(px(0.), px(-300.)),
        gpui::point(px(-90.), px(0.)),
    ] {
        cx.simulate_event(gpui::ScrollEvent {
            position: left.center(),
            delta: gpui::ScrollDelta::Pixels(delta),
            ..Default::default()
        });
        cx.run_until_parked();
    }
    let [(left_down, left_x), (right_down, right_x)] =
        cx.update(|_, cx| view.read(cx).scroll_offsets());
    assert!(left_down > px(0.), "the left side scrolled down");
    assert_eq!(left_x, px(90.));
    assert_eq!(
        (right_down, right_x),
        (px(0.), px(0.)),
        "the right side stayed"
    );

    cx.update(|_, cx| gleipnir_core::update_settings(cx, |s| s.diff = Default::default()))
        .unwrap();
}
