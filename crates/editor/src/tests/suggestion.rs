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
