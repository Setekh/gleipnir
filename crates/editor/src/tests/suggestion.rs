//! Grey suggestions, against a suggester that offers what the test says.

use super::*;
use crate::{InlineSuggester, InlineSuggestion};
use gleipnir_core::BufferModel;
use gpui::{App, Task};
use std::rc::Rc;

/// Offers `text` at the cursor, with a score.
struct Offers(&'static str);

impl InlineSuggester for Offers {
    fn suggest(
        &self,
        _: &Entity<BufferModel>,
        offset: usize,
        _: &mut App,
    ) -> Task<Option<InlineSuggestion>> {
        Task::ready(Some(InlineSuggestion {
            offset,
            text: self.0.to_string(),
            score: Some(0.9),
            source: "test".into(),
        }))
    }
}

fn shown(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.run_until_parked();
    editor.read_with(cx, |e, _| e.suggestion().map(|s| s.text.clone()))
}

fn with_suggester<'a>(
    text: &str,
    offers: &'static str,
    cx: &'a mut TestAppContext,
) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let (editor, cx) = setup(text, Some("/tmp/a.ts"), cx);
    editor.update(cx, |e, _| e.set_suggester(Rc::new(Offers(offers))));
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-end"
    } else {
        "ctrl-end"
    });
    (editor, cx)
}

#[gpui::test]
fn a_suggestion_shows_after_typing_and_tab_takes_it_as_one_step(cx: &mut TestAppContext) {
    let (editor, cx) = with_suggester("address: x(A", ", // done", cx);
    cx.simulate_input(")");
    assert_eq!(shown(&editor, cx).as_deref(), Some(", // done"));
    cx.simulate_keystrokes("tab");
    assert_eq!(text_of(&editor, cx), "address: x(A), // done");
    assert_eq!(shown(&editor, cx), None);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    assert_eq!(text_of(&editor, cx), "address: x(A)");
}

#[gpui::test]
fn typing_what_it_says_keeps_the_rest_and_escape_or_moving_drops_it(cx: &mut TestAppContext) {
    let (editor, cx) = with_suggester("value", "s.len()", cx);
    cx.simulate_input("x");
    assert_eq!(shown(&editor, cx).as_deref(), Some("s.len()"));
    // Typing its first character keeps the rest.
    cx.simulate_input("s");
    assert_eq!(shown(&editor, cx).as_deref(), Some(".len()"));
    cx.simulate_keystrokes("escape");
    assert_eq!(shown(&editor, cx), None);
    cx.simulate_input("y");
    assert!(shown(&editor, cx).is_some());
    cx.simulate_keystrokes("left");
    assert_eq!(shown(&editor, cx), None, "moving away drops it");
    // Without a suggestion, Tab does what it always did.
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-end"
    } else {
        "ctrl-end"
    });
    assert_eq!(shown(&editor, cx), None);
    cx.simulate_keystrokes("tab");
    let text = text_of(&editor, cx);
    assert!(
        text.starts_with("valuexsy") && text.len() > "valuexsy".len() && !text.contains("len"),
        "{text:?}"
    );
}

const NEXT_WORD: &str = if cfg!(target_os = "macos") {
    "alt-right"
} else {
    "ctrl-right"
};
const NEXT_LINE: &str = if cfg!(target_os = "macos") {
    "cmd-right"
} else {
    "end"
};

#[gpui::test]
fn a_word_at_a_time_each_its_own_undo_step(cx: &mut TestAppContext) {
    let (editor, cx) = with_suggester("address: x(A", ", // done", cx);
    cx.simulate_input(")");
    cx.simulate_keystrokes(NEXT_WORD);
    assert_eq!(text_of(&editor, cx), "address: x(A),");
    assert_eq!(shown(&editor, cx).as_deref(), Some(" // done"));
    cx.simulate_keystrokes(NEXT_WORD);
    assert_eq!(text_of(&editor, cx), "address: x(A), //");
    assert_eq!(shown(&editor, cx).as_deref(), Some(" done"));
    cx.simulate_keystrokes(Keys::UNDO);
    assert_eq!(text_of(&editor, cx), "address: x(A),");
    // Without a suggestion, the key moves as it always did.
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "alt-left"
    } else {
        "ctrl-left"
    });
    cx.simulate_keystrokes(NEXT_WORD);
    assert_eq!(text_of(&editor, cx), "address: x(A),");
}

#[gpui::test]
fn a_suggestion_of_several_lines_makes_room_and_is_taken_a_line_at_a_time(cx: &mut TestAppContext) {
    let (editor, cx) = with_suggester("let a = 1\nafter", "\n  b\n  c", cx);
    cx.simulate_keystrokes("up");
    cx.simulate_keystrokes(NEXT_LINE);
    cx.simulate_input(";");
    assert_eq!(shown(&editor, cx).as_deref(), Some("\n  b\n  c"));
    // How many rows a line takes, with those made for the suggestion under it.
    let gap = |line: usize, cx: &mut VisualTestContext| {
        cx.run_until_parked();
        editor.read_with(cx, |e, _| {
            let g = e.geometry.as_ref().unwrap();
            (g.line_top(line + 1) - g.line_top(line)) / g.line_height
        })
    };
    assert_eq!(gap(0, cx), 3., "two rows for the lines after the first");
    cx.simulate_keystrokes(NEXT_LINE);
    assert_eq!(text_of(&editor, cx), "let a = 1;\n  b\nafter");
    assert_eq!(shown(&editor, cx).as_deref(), Some("\n  c"));
    assert_eq!((gap(0, cx), gap(1, cx)), (1., 2.));
    cx.simulate_keystrokes("escape");
    assert_eq!(gap(1, cx), 1., "the rows go with it");
}

#[test]
fn rows_made_after_a_line_count_as_that_line() {
    let rows = crate::element::Rows {
        added: Some((2, 3)),
    };
    let lines: Vec<usize> = (0..8).map(|row| rows.line_of_row(row)).collect();
    assert_eq!(lines, [0, 1, 2, 2, 2, 2, 3, 4]);
    assert_eq!([1, 2, 3, 4].map(|line| rows.row_of(line)), [1, 2, 6, 7]);
}
