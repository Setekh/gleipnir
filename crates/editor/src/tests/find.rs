//! Find and Replace, through the bar's keys and buttons.

// A selection is checked as a list of one range.
#![allow(clippy::single_range_in_vec_init)]

use super::*;

const FIND: &str = if cfg!(target_os = "macos") {
    "cmd-f"
} else {
    "ctrl-f"
};
const REPLACE: &str = if cfg!(target_os = "macos") {
    "cmd-r"
} else {
    "ctrl-r"
};

fn found(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> (Option<usize>, usize) {
    editor.read_with(cx, |e, _| e.find_matches())
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector}"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn finding_as_you_type_steps_through_every_match(cx: &mut TestAppContext) {
    let (editor, cx) = setup("let a = 1;\nlet b = a + a;\nLet c;\n", None, cx);
    cx.simulate_keystrokes(FIND);
    assert!(editor.read_with(cx, |e, _| e.is_finding()));
    cx.simulate_input("a");
    assert_eq!(found(&editor, cx), (Some(0), 3));
    assert_eq!(ranges(&editor, cx), [4..5], "the first match is selected");

    cx.simulate_keystrokes("enter");
    assert_eq!(found(&editor, cx), (Some(1), 3));
    cx.simulate_keystrokes("shift-enter shift-enter");
    assert_eq!(
        found(&editor, cx),
        (Some(2), 3),
        "around the start to the last"
    );
    assert_eq!(ranges(&editor, cx), [23..24]);

    // Typing goes into the bar, not the file.
    assert_eq!(text_of(&editor, cx), "let a = 1;\nlet b = a + a;\nLet c;\n");

    // Escape leaves the match selected, and the last search carries on from the editor.
    cx.simulate_keystrokes("escape");
    assert!(!editor.read_with(cx, |e, _| e.is_finding()));
    assert_eq!(ranges(&editor, cx), [23..24]);
    cx.simulate_keystrokes("f3");
    assert_eq!(ranges(&editor, cx), [4..5], "around the end to the first");
}

#[gpui::test]
fn case_words_and_patterns_narrow_what_matches(cx: &mut TestAppContext) {
    let (editor, cx) = setup("Let let letter\nlet_it lot\n", None, cx);
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("let");
    assert_eq!(found(&editor, cx).1, 4);
    cx.simulate_keystrokes("alt-c");
    assert_eq!(found(&editor, cx).1, 3, "Match Case leaves out Let");
    click("find-words", cx);
    assert_eq!(
        found(&editor, cx).1,
        1,
        "Words leaves out letter and let_it"
    );
    cx.simulate_keystrokes("alt-w alt-x");
    cx.simulate_keystrokes("backspace backspace backspace");
    cx.simulate_input("l.t\\b");
    assert_eq!(
        found(&editor, cx).1,
        2,
        "let and lot, with Match Case still on"
    );

    // A pattern that isn't one says so.
    cx.simulate_input("(");
    assert_eq!(found(&editor, cx).1, 0);
    cx.run_until_parked();
}

#[gpui::test]
fn replacing_one_match_then_all_of_them(cx: &mut TestAppContext) {
    let (editor, cx) = setup("foo bar foo baz foo\n", None, cx);
    cx.simulate_keystrokes(REPLACE);
    cx.simulate_input("foo");
    assert_eq!(found(&editor, cx), (Some(0), 3));
    click("find-replacement", cx);
    cx.simulate_input("qux");
    cx.simulate_keystrokes("enter");
    assert_eq!(text_of(&editor, cx), "qux bar foo baz foo\n");
    assert_eq!(found(&editor, cx), (Some(0), 2), "on to the next");
    assert_eq!(ranges(&editor, cx), [8..11]);

    click("find-replace-all", cx);
    assert_eq!(text_of(&editor, cx), "qux bar qux baz qux\n");
    assert_eq!(found(&editor, cx).1, 0);

    // Replace All is one step back.
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes(Keys::UNDO);
    assert_eq!(text_of(&editor, cx), "qux bar foo baz foo\n");
}

#[gpui::test]
fn a_pattern_replaces_with_what_it_caught(cx: &mut TestAppContext) {
    let (editor, cx) = setup("ann@home bob@work\n", None, cx);
    cx.simulate_keystrokes(REPLACE);
    cx.simulate_keystrokes("alt-x");
    cx.simulate_input("(\\w+)@(\\w+)");
    assert_eq!(found(&editor, cx).1, 2);
    click("find-replacement", cx);
    cx.simulate_input("$2:$1");
    cx.simulate_keystrokes("alt-a");
    assert_eq!(text_of(&editor, cx), "home:ann work:bob\n");
}

#[gpui::test]
fn the_selection_is_what_to_find_and_every_match_can_get_a_cursor(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one two one three one\n", None, cx);
    editor.update(cx, |e, cx| {
        e.set_selections(SelectionSet::single(Selection::new(0, 3)), cx)
    });
    cx.simulate_keystrokes(FIND);
    assert_eq!(found(&editor, cx), (Some(0), 3));
    click("find-select-all", cx);
    assert!(!editor.read_with(cx, |e, _| e.is_finding()));
    assert_eq!(ranges(&editor, cx), [0..3, 8..11, 18..21]);
    cx.simulate_input("1");
    assert_eq!(text_of(&editor, cx), "1 two 1 three 1\n");
}

#[gpui::test]
fn a_selection_over_several_lines_is_where_to_find(cx: &mut TestAppContext) {
    let (editor, cx) = setup("a\nb a\nc a\nd a\n", None, cx);
    // Lines two and three selected.
    editor.update(cx, |e, cx| {
        e.set_selections(SelectionSet::single(Selection::new(2, 10)), cx)
    });
    cx.simulate_keystrokes(REPLACE);
    cx.simulate_input("a");
    assert_eq!(found(&editor, cx).1, 2, "only in the selection");
    click("find-replacement", cx);
    cx.simulate_input("x");
    cx.simulate_keystrokes("alt-a");
    assert_eq!(text_of(&editor, cx), "a\nb x\nc x\nd a\n");
    // Off, it finds everywhere.
    click("find-in-selection", cx);
    assert_eq!(found(&editor, cx).1, 2, "the first and last a");
}

#[gpui::test]
fn preserve_case_keeps_each_matchs_case(cx: &mut TestAppContext) {
    let (editor, cx) = setup("count Count COUNT\n", None, cx);
    cx.simulate_keystrokes(REPLACE);
    cx.simulate_input("count");
    click("find-replacement", cx);
    cx.simulate_input("total");
    click("find-preserve-case", cx);
    click("find-replace-all", cx);
    assert_eq!(text_of(&editor, cx), "total Total TOTAL\n");
}

#[gpui::test]
fn up_and_down_bring_back_earlier_searches(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one two three\n", None, cx);
    let query = |cx: &mut VisualTestContext| {
        editor.read_with(cx, |e, cx| e.find.as_ref().unwrap().query.read(cx).text())
    };
    cx.simulate_keystrokes(FIND);
    cx.simulate_input("one");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes(Keys::SELECT_ALL);
    cx.simulate_input("two");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes(Keys::SELECT_ALL);
    cx.simulate_input("thr");
    cx.simulate_keystrokes("up");
    assert_eq!(query(cx), "two");
    cx.simulate_keystrokes("up");
    assert_eq!(query(cx), "one");
    cx.simulate_keystrokes("down");
    assert_eq!(query(cx), "two");
}
