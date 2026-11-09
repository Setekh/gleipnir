//! Grey suggestions after the cursor, as Copilot shows them: asked for after typing, shown with
//! how likely they are, taken with Tab, dropped with Escape or by moving away. Typing what one
//! says keeps it, shorter by what was typed.

use crate::EditorView;
use gleipnir_core::BufferModel;
use gpui::{App, Context, Entity, SharedString, Task};
use std::rc::Rc;

/// Text that could come next at a place in the buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineSuggestion {
    /// Where it goes: the cursor, when it was asked for.
    pub offset: usize,
    pub text: String,
    /// How likely it is to be what comes next, from 0 to 1, if the source can say.
    pub score: Option<f32>,
    /// What suggested it, like `this file` or a model's name.
    pub source: SharedString,
}

/// Something that suggests what comes next at the cursor.
pub trait InlineSuggester {
    fn suggest(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Option<InlineSuggestion>>;
}

#[derive(Default)]
pub(crate) struct SuggestionState {
    pub suggester: Option<Rc<dyn InlineSuggester>>,
    pub current: Option<InlineSuggestion>,
    task: Option<Task<()>>,
}

impl EditorView {
    pub fn set_suggester(&mut self, suggester: Rc<dyn InlineSuggester>) {
        self.suggestion.suggester = Some(suggester);
    }

    /// The suggestion showing, if there is one.
    pub fn suggestion(&self) -> Option<&InlineSuggestion> {
        self.suggestion.current.as_ref()
    }

    /// Whether a suggestion should be asked for here: one cursor, at the end of its line, with no
    /// completion list open, which Tab would otherwise mean.
    fn wants_suggestion(&self, cx: &App) -> bool {
        if self.plain || self.selections.len() > 1 || self.semantics.completion.is_some() {
            return false;
        }
        let primary = self.selections.primary();
        if !primary.is_empty() {
            return false;
        }
        let buffer = self.buffer.read(cx).buffer();
        let point = buffer.offset_to_point(primary.head);
        let line = buffer.line(point.line);
        line[point.col.min(line.len())..].trim().is_empty() && !line.trim().is_empty()
    }

    /// Asks for a suggestion at the cursor, replacing any on the way.
    pub(crate) fn request_suggestion(&mut self, cx: &mut Context<Self>) {
        self.suggestion.current = None;
        self.suggestion.task = None;
        let Some(suggester) = self.suggestion.suggester.clone() else {
            return;
        };
        if !self.wants_suggestion(cx) {
            cx.notify();
            return;
        }
        let offset = self.selections.primary().head;
        let version = self.buffer.read(cx).buffer().version();
        let task = suggester.suggest(&self.buffer, offset, cx);
        self.suggestion.task = Some(cx.spawn(async move |this, cx| {
            let suggestion = task.await;
            this.update(cx, |this, cx| {
                // Only if nothing's changed since it was asked for.
                let unchanged = this.buffer.read(cx).buffer().version() == version
                    && this.selections.primary().head == offset;
                if unchanged && this.wants_suggestion(cx) {
                    this.suggestion.current = suggestion.filter(|s| !s.text.is_empty());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// After typing a character: if the suggestion said it, keep the rest; else ask again.
    pub(crate) fn typed_for_suggestion(&mut self, typed: char, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        if let Some(current) = &mut self.suggestion.current
            && current.text.starts_with(typed)
            && current.offset + typed.len_utf8() == head
        {
            current.offset = head;
            current.text.drain(..typed.len_utf8());
            if current.text.is_empty() {
                self.suggestion.current = None;
            }
            cx.notify();
            return;
        }
        self.request_suggestion(cx);
    }

    pub(crate) fn clear_suggestion(&mut self, cx: &mut Context<Self>) -> bool {
        self.suggestion.task = None;
        let had = self.suggestion.current.take().is_some();
        if had {
            cx.notify();
        }
        had
    }

    /// Takes the suggestion, as one step that undo takes back.
    pub(crate) fn accept_suggestion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(suggestion) = self.suggestion.current.take() else {
            return false;
        };
        if self.selections.primary().head != suggestion.offset {
            return false;
        }
        self.edit(cx, |edit| {
            edit.insert(&suggestion.text, gleipnir_text::UndoGrouping::Distinct)
        });
        self.buffer
            .update(cx, |buffer, _| buffer.break_undo_group());
        cx.notify();
        true
    }
}
