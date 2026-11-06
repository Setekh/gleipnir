//! Usage highlights, inlay hints and semantic colors from a provider that answers at once.

use super::*;
use crate::{
    CompletionItem, Completions, HoverInfo, InlayHint, SemanticsProvider, SignatureInfo, Triggers,
};
use anyhow::Result;
use gleipnir_syntax::HighlightKind;
use gpui::{App, Task};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// Knows about one name, `count`, wherever it's written, and hints a type after its declaration.
struct Fake;

fn uses_of(text: &str, name: &str) -> Vec<Range<usize>> {
    text.match_indices(name)
        .map(|(at, _)| at..at + name.len())
        .collect()
}

impl SemanticsProvider for Fake {
    fn triggers(&self, _: &Entity<BufferModel>, _: &App) -> Triggers {
        Triggers::default()
    }
    fn completions(
        &self,
        _: &Entity<BufferModel>,
        _: usize,
        _: Option<char>,
        _: &mut App,
    ) -> Task<Result<Completions>> {
        Task::ready(Ok(Completions {
            items: Vec::new(),
            incomplete: false,
        }))
    }
    fn resolve(
        &self,
        _: &Entity<BufferModel>,
        item: CompletionItem,
        _: &mut App,
    ) -> Task<Result<CompletionItem>> {
        Task::ready(Ok(item))
    }
    fn hover(
        &self,
        _: &Entity<BufferModel>,
        _: usize,
        _: &mut App,
    ) -> Task<Result<Option<HoverInfo>>> {
        Task::ready(Ok(None))
    }
    fn signature_help(
        &self,
        _: &Entity<BufferModel>,
        _: usize,
        _: &mut App,
    ) -> Task<Result<Option<SignatureInfo>>> {
        Task::ready(Ok(None))
    }
    fn document_highlights(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Result<Vec<Range<usize>>>> {
        let text = buffer.read(cx).buffer().text();
        let uses = uses_of(&text, "count");
        let on_it = uses.iter().any(|r| r.start <= offset && offset <= r.end);
        Task::ready(Ok(if on_it { uses } else { Vec::new() }))
    }
    fn inlay_hints(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &mut App,
    ) -> Task<Result<Vec<InlayHint>>> {
        let text = buffer.read(cx).buffer().text();
        let hints = text
            .find("let count")
            .map(|at| InlayHint {
                offset: at + "let count".len(),
                label: ": i32".into(),
            })
            .into_iter()
            .collect();
        Task::ready(Ok(hints))
    }
    fn semantic_tokens(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &mut App,
    ) -> Task<Result<crate::SemanticTokens>> {
        let text = buffer.read(cx).buffer().text();
        Task::ready(Ok(uses_of(&text, "count")
            .into_iter()
            .map(|r| (r, HighlightKind::Parameter))
            .collect()))
    }
}

const TEXT: &str = "fn main() {\n    let count = 1;\n    print(count + count);\n}\n";

fn with_fake(cx: &mut TestAppContext) -> (Entity<EditorView>, &mut VisualTestContext) {
    let (editor, cx) = setup(TEXT, Some("/p/main.rs"), cx);
    cx.update(|_, cx| {
        editor.update(cx, |e, cx| {
            e.set_semantics(Rc::new(Fake));
            e.refresh_annotations(cx);
        })
    });
    cx.run_until_parked();
    (editor, cx)
}

fn state(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> crate::EditorState {
    cx.run_until_parked();
    editor.read_with(cx, |e, cx| e.state(cx))
}

#[gpui::test]
fn hints_and_semantic_colors_arrive_and_follow_edits(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake(cx);
    let s = state(&editor, cx);
    assert_eq!(s.inlay_hints, ["2: : i32"]);
    assert_eq!(s.semantic_tokens, 3);

    // A line typed above moves the hint down at once, before the server answers again.
    cx.simulate_keystrokes("enter");
    assert_eq!(state(&editor, cx).inlay_hints, ["3: : i32"]);
    cx.executor().advance_clock(Duration::from_secs(1));
    assert_eq!(state(&editor, cx).inlay_hints, ["3: : i32"]);
}

#[gpui::test]
fn a_click_after_a_hint_lands_on_the_text_not_the_hint(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake(cx);
    // The `=` after `count`, which the hint pushed right on screen.
    let at = TEXT.find(" = 1").unwrap() + 1;
    let line_start = TEXT.find("    let").unwrap();
    let target = text_point(&editor, 1, at - line_start, cx);
    cx.simulate_click(target, Modifiers::none());
    assert_eq!(heads(&editor, cx), vec![at]);
}

#[gpui::test]
fn resting_on_a_name_highlights_its_other_uses(cx: &mut TestAppContext) {
    let (editor, cx) = with_fake(cx);
    let on_count = TEXT.find("count + count").unwrap() + 2;
    cx.update(|_, cx| {
        editor.update(cx, |e, cx| {
            e.set_selections(SelectionSet::cursor(on_count), cx)
        })
    });
    assert!(
        state(&editor, cx).usage_highlights.is_empty(),
        "not before it rests"
    );
    cx.executor().advance_clock(Duration::from_millis(300));
    assert_eq!(
        state(&editor, cx).usage_highlights,
        ["count", "count", "count"]
    );

    // Moving off the name clears them.
    cx.update(|_, cx| editor.update(cx, |e, cx| e.set_selections(SelectionSet::cursor(0), cx)));
    cx.executor().advance_clock(Duration::from_millis(300));
    assert!(state(&editor, cx).usage_highlights.is_empty());
}
