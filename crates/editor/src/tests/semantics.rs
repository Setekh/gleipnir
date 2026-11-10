//! Completion, hover, signature help and links, against a provider that answers instantly.

use super::*;
use crate::{
    CompletionItem, CompletionKind, Completions, EditorEvent, HoverInfo, SemanticsProvider,
    SignatureInfo, Triggers,
};
use anyhow::Result;
use gleipnir_core::{Diagnostic, Severity};
use gleipnir_text::TextEdit;
use gpui::{App, Task};
use std::cell::RefCell;
use std::rc::Rc;

const HOVER: &str = if cfg!(target_os = "macos") {
    "f1"
} else {
    "ctrl-q"
};

/// (label, text to insert, what to select in it, an import to add when resolved)
type FakeItem = (
    &'static str,
    &'static str,
    Option<std::ops::Range<usize>>,
    Option<&'static str>,
);

#[derive(Default)]
struct Fake {
    /// (label, text to insert, what to select in it, an import to add when resolved)
    items: Vec<FakeItem>,
    asked: Rc<RefCell<Vec<Option<char>>>>,
    /// When set, completions wait until the test answers them, like a slow server.
    held: Option<Rc<RefCell<Vec<futures::channel::oneshot::Sender<()>>>>>,
}

impl SemanticsProvider for Fake {
    fn triggers(&self, _: &Entity<BufferModel>, _: &App) -> Triggers {
        Triggers {
            completion: vec!['.'],
            signature: vec!['('],
        }
    }

    fn completions(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        trigger: Option<char>,
        cx: &mut App,
    ) -> Task<Result<Completions>> {
        self.asked.borrow_mut().push(trigger);
        let buffer = buffer.read(cx).buffer();
        let mut start = offset;
        while let Some(c) = buffer.char_before(start).filter(|c| c.is_alphanumeric()) {
            start -= c.len_utf8();
        }
        let items = self
            .items
            .iter()
            .map(|(label, text, selection, import)| CompletionItem {
                label: label.to_string(),
                detail: Some("detail".into()),
                kind: CompletionKind::Function,
                filter_text: label.to_string(),
                sort_text: label.to_string(),
                replace_start: start,
                new_text: text.to_string(),
                selection: selection.clone(),
                additional_edits: Vec::new(),
                documentation: None,
                data: Rc::new(*import),
            })
            .collect();
        let completions = Completions {
            items,
            incomplete: false,
        };
        match &self.held {
            Some(held) => {
                let (answer, answered) = futures::channel::oneshot::channel();
                held.borrow_mut().push(answer);
                cx.spawn(async move |_| {
                    answered.await.ok();
                    Ok(completions)
                })
            }
            None => Task::ready(Ok(completions)),
        }
    }

    fn resolve(
        &self,
        _: &Entity<BufferModel>,
        mut item: CompletionItem,
        _: &mut App,
    ) -> Task<Result<CompletionItem>> {
        if let Some(Some(import)) = item.data.downcast_ref::<Option<&'static str>>() {
            item.additional_edits
                .push(TextEdit::new(0..0, import.to_string()));
        }
        Task::ready(Ok(item))
    }

    fn hover(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Result<Option<HoverInfo>>> {
        let word = buffer
            .read(cx)
            .buffer()
            .word_at(offset)
            .map(|r| buffer.read(cx).buffer().slice(r));
        Task::ready(Ok(word.map(|w| HoverInfo {
            markdown: format!("```\nfn {w}()\n```\nDocs for **{w}**."),
        })))
    }

    fn signature_help(
        &self,
        _: &Entity<BufferModel>,
        _: usize,
        _: &mut App,
    ) -> Task<Result<Option<SignatureInfo>>> {
        Task::ready(Ok(Some(SignatureInfo {
            label: "area(width: f64, height: f64)".into(),
            active_parameter: Some(5..15),
            documentation: None,
        })))
    }
}

fn with_fake<'a>(
    text: &str,
    fake: Fake,
    cx: &'a mut TestAppContext,
) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let (editor, cx) = setup(text, Some("/tmp/a.rs"), cx);
    editor.update(cx, |e, _| e.set_semantics(Rc::new(fake)));
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-end"
    } else {
        "ctrl-end"
    });
    (editor, cx)
}

fn labels(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.run_until_parked();
    editor.read_with(cx, |e, _| e.completion_labels())
}

fn items() -> Fake {
    Fake {
        items: vec![
            ("area", "area", None, None),
            ("arena", "arena", None, None),
            ("print", "print", None, None),
            (
                "argmax",
                "argmax()",
                Some(7..7),
                Some("use stats::argmax;\n"),
            ),
            ("clamp", "clamp(lo, hi)", Some(6..8), None),
        ],
        ..Default::default()
    }
}

#[gpui::test]
fn typing_a_word_shows_matching_completions_and_enter_inserts_one(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("let x = ", items(), cx);
    cx.simulate_input("ar");
    assert_eq!(labels(&editor, cx), ["area", "arena", "argmax"]);
    cx.simulate_input("n");
    assert_eq!(labels(&editor, cx), ["arena"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(text_of(&editor, cx), "let x = arena");
    assert!(labels(&editor, cx).is_empty());
}

#[gpui::test]
fn the_arrow_keys_choose_and_escape_only_closes_the_menu(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("x", items(), cx);
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-alt-up"
    } else {
        "ctrl-alt-up"
    });
    cx.simulate_input(" ar");
    assert_eq!(labels(&editor, cx).len(), 3);
    cx.simulate_keystrokes("down down up escape");
    assert!(labels(&editor, cx).is_empty());
    assert_eq!(text_of(&editor, cx), "x ar", "nothing inserted");
    cx.simulate_keystrokes("ctrl-space");
    cx.simulate_keystrokes("down tab");
    cx.run_until_parked();
    assert_eq!(text_of(&editor, cx), "x arena");
}

#[gpui::test]
fn backspacing_widens_the_filter_and_leaving_the_word_closes_it(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("", items(), cx);
    cx.simulate_input("are");
    assert_eq!(labels(&editor, cx), ["area", "arena"]);
    cx.simulate_keystrokes("backspace");
    assert_eq!(labels(&editor, cx), ["area", "arena", "argmax"]);
    cx.simulate_input(" ");
    assert!(labels(&editor, cx).is_empty());
    cx.simulate_input("pr");
    assert_eq!(labels(&editor, cx), ["print"]);
    cx.simulate_keystrokes("left");
    assert!(
        labels(&editor, cx).is_empty(),
        "moving the cursor closes it"
    );
}

#[gpui::test]
fn typing_on_while_a_list_is_on_the_way_waits_for_it_rather_than_asking_again(
    cx: &mut TestAppContext,
) {
    let held = Rc::new(RefCell::new(Vec::new()));
    let fake = Fake {
        held: Some(held.clone()),
        ..items()
    };
    let asked = fake.asked.clone();
    let (editor, cx) = with_fake("let x = ", fake, cx);
    cx.simulate_input("are");
    assert_eq!(asked.borrow().len(), 1, "one request for the whole word");
    assert!(labels(&editor, cx).is_empty());
    held.borrow_mut().remove(0).send(()).ok();
    assert_eq!(
        labels(&editor, cx),
        ["area", "arena"],
        "filtered by all typed"
    );

    // A list that arrives after the caret has left its word is dropped.
    cx.simulate_input(" pr");
    assert_eq!(asked.borrow().len(), 2);
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    held.borrow_mut().remove(0).send(()).ok();
    assert!(labels(&editor, cx).is_empty());
    assert_eq!(text_of(&editor, cx), "let x = ar");
}

#[gpui::test]
fn a_trigger_character_asks_even_without_a_word(cx: &mut TestAppContext) {
    let fake = items();
    let asked = fake.asked.clone();
    let (editor, cx) = with_fake("shape", fake, cx);
    cx.simulate_input(".");
    assert_eq!(labels(&editor, cx).len(), 5);
    assert_eq!(*asked.borrow(), [Some('.')]);
}

#[gpui::test]
fn an_auto_import_and_a_snippet_cursor_land_in_one_undo_step(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("fn main() {\n    let m = \n}", items(), cx);
    cx.simulate_keystrokes("up end");
    cx.simulate_input("argm");
    assert_eq!(labels(&editor, cx), ["argmax"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        text_of(&editor, cx),
        "use stats::argmax;\nfn main() {\n    let m = argmax()\n}"
    );
    let inside_parens = text_of(&editor, cx).rfind("()").unwrap() + 1;
    assert_eq!(heads(&editor, cx), [inside_parens]);
    assert!(
        editor.read_with(cx, |e, _| e.signature_label()).is_some(),
        "the signature shows inside the call"
    );
    cx.simulate_keystrokes(Keys::UNDO);
    assert_eq!(text_of(&editor, cx), "fn main() {\n    let m = argm\n}");
}

#[gpui::test]
fn a_snippet_placeholder_is_selected_so_typing_replaces_it(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("", items(), cx);
    cx.simulate_input("cla");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(ranges(&editor, cx), vec![6..8]);
    cx.simulate_input("0");
    assert_eq!(text_of(&editor, cx), "clamp(0, hi)");
}

#[gpui::test]
fn signature_help_opens_on_a_paren_and_closes_on_the_closing_one(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("area", Fake::default(), cx);
    cx.simulate_input("(");
    cx.run_until_parked();
    assert_eq!(
        editor.read_with(cx, |e, _| e.signature_label()).as_deref(),
        Some("area(width: f64, height: f64)")
    );
    cx.simulate_input("1, 2");
    cx.run_until_parked();
    assert!(editor.read_with(cx, |e, _| e.signature_label()).is_some());
    cx.simulate_input(")");
    cx.run_until_parked();
    assert!(editor.read_with(cx, |e, _| e.signature_label()).is_none());
}

#[gpui::test]
fn signature_help_closes_when_the_cursor_leaves_the_call(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("area", Fake::default(), cx);
    let open = |editor: &Entity<EditorView>, cx: &mut VisualTestContext| {
        cx.run_until_parked();
        editor.read_with(cx, |e, _| e.signature_label().is_some())
    };
    cx.simulate_input("(1)");
    cx.simulate_keystrokes("left");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-p"
    } else {
        "ctrl-p"
    });
    assert!(open(&editor, cx));
    cx.simulate_keystrokes("right");
    assert!(!open(&editor, cx), "past the closing parenthesis");
    cx.simulate_keystrokes("left");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-p"
    } else {
        "ctrl-p"
    });
    assert!(open(&editor, cx));
    cx.simulate_keystrokes("enter");
    assert!(!open(&editor, cx), "onto another line");
}

#[gpui::test]
fn hover_shows_documentation_with_any_problem_first(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("let total = area;", Fake::default(), cx);
    editor.update(cx, |e, cx| {
        e.buffer().update(cx, |b, cx| {
            b.set_diagnostics(
                vec![Diagnostic {
                    range: 12..16,
                    severity: Severity::Error,
                    message: "unknown name".into(),
                    source: None,
                }],
                cx,
            )
        })
    });
    cx.simulate_keystrokes("left left");
    cx.simulate_keystrokes(HOVER);
    cx.run_until_parked();
    assert_eq!(
        editor.read_with(cx, |e, _| e.hover_text()).as_deref(),
        Some("unknown name\nfn area()\nDocs for area.")
    );
    cx.simulate_keystrokes("escape");
    assert!(editor.read_with(cx, |e, _| e.hover_text()).is_none());
}

#[gpui::test]
fn resting_the_mouse_on_a_name_shows_its_documentation(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("let total = area;", Fake::default(), cx);
    let target = text_point(&editor, 0, 13, cx);
    cx.simulate_mouse_move(target, None, Modifiers::none());
    cx.run_until_parked();
    assert!(
        editor.read_with(cx, |e, _| e.hover_text()).is_none(),
        "not straight away"
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    assert_eq!(
        editor.read_with(cx, |e, _| e.hover_text()).as_deref(),
        Some("fn area()\nDocs for area.")
    );
    let away = text_point(&editor, 0, 1, cx);
    cx.simulate_mouse_move(away, None, Modifiers::none());
    assert!(editor.read_with(cx, |e, _| e.hover_text()).is_none());
}

#[gpui::test]
fn a_secondary_click_asks_to_go_to_the_definition(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("let total = area;", Fake::default(), cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&editor, move |_, event: &EditorEvent, _| {
            if let EditorEvent::GoToDefinition { offset } = event {
                seen.borrow_mut().push(*offset);
            }
        })
        .detach()
    });
    let secondary = if cfg!(target_os = "macos") {
        Modifiers::command()
    } else {
        Modifiers::control()
    };
    let target = text_point(&editor, 0, 13, cx);
    cx.simulate_mouse_move(target, None, secondary);
    assert_eq!(
        editor.read_with(cx, |e, _| e.semantics.link.clone()),
        Some(12..16),
        "underlined as a link"
    );
    cx.simulate_click(target, secondary);
    assert_eq!(*events.borrow(), [13]);
    assert_eq!(heads(&editor, cx), [13]);
}

#[gpui::test]
fn diagnostics_follow_edits_and_draw(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("let total = area;", Fake::default(), cx);
    editor.update(cx, |e, cx| {
        e.buffer().update(cx, |b, cx| {
            b.set_diagnostics(
                vec![
                    Diagnostic {
                        range: 12..16,
                        severity: Severity::Error,
                        message: "e".into(),
                        source: None,
                    },
                    Diagnostic {
                        range: 4..9,
                        severity: Severity::Warning,
                        message: "w".into(),
                        source: None,
                    },
                ],
                cx,
            )
        })
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-home"
    } else {
        "ctrl-home"
    });
    cx.simulate_input("// ");
    cx.run_until_parked();
    let ranges: Vec<_> = editor.read_with(cx, |e, cx| {
        e.buffer()
            .read(cx)
            .diagnostics()
            .iter()
            .map(|d| d.range.clone())
            .collect()
    });
    assert_eq!(ranges, [7..12, 15..19]);
}

#[gpui::test]
fn f2_visits_each_problem_in_turn_and_wraps(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake("let a = b;\nlet c = d;\n", Fake::default(), cx);
    editor.update(cx, |e, cx| {
        e.buffer().update(cx, |b, cx| {
            b.set_diagnostics(
                vec![
                    Diagnostic {
                        range: 8..9,
                        severity: Severity::Error,
                        message: "no b".into(),
                        source: None,
                    },
                    Diagnostic {
                        range: 19..20,
                        severity: Severity::Warning,
                        message: "no d".into(),
                        source: None,
                    },
                    Diagnostic {
                        range: 4..5,
                        severity: Severity::Hint,
                        message: "hint".into(),
                        source: None,
                    },
                ],
                cx,
            )
        })
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-home"
    } else {
        "ctrl-home"
    });
    cx.simulate_keystrokes("f2");
    cx.run_until_parked();
    assert_eq!(heads(&editor, cx), [8]);
    assert!(
        editor
            .read_with(cx, |e, _| e.hover_text())
            .unwrap()
            .starts_with("no b\n"),
        "its message shows"
    );
    cx.simulate_keystrokes("f2");
    assert_eq!(heads(&editor, cx), [19]);
    cx.simulate_keystrokes("f2");
    assert_eq!(heads(&editor, cx), [8], "wraps, skipping hints");
    cx.simulate_keystrokes("shift-f2");
    assert_eq!(heads(&editor, cx), [19]);
}
