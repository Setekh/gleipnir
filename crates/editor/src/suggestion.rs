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

    /// The line a multi-line suggestion starts on, and how many lines it adds below it, which
    /// the layout makes room for.
    pub(crate) fn suggestion_rows(&self, cx: &App) -> Option<(usize, usize)> {
        let suggestion = self.suggestion.current.as_ref()?;
        let rows = suggestion.text.matches('\n').count();
        (rows > 0).then(|| {
            let line = self
                .buffer
                .read(cx)
                .buffer()
                .offset_to_point(suggestion.offset)
                .line;
            (line, rows)
        })
    }

    /// Whether a suggestion should be asked for here: one cursor, at the end of its line, blank
    /// or not, with no completion list open, which Tab would otherwise mean.
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
        line[point.col.min(line.len())..].trim().is_empty()
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
        self.accept_suggestion_part(SuggestionPart::All, cx)
    }

    /// Takes the next word or line of the suggestion, or all of it, as one undo step. What's
    /// left still shows, after the cursor.
    pub(crate) fn accept_suggestion_part(
        &mut self,
        part: SuggestionPart,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut suggestion) = self.suggestion.current.take() else {
            return false;
        };
        if self.selections.primary().head != suggestion.offset {
            return false;
        }
        let len = part.len_in(&suggestion.text);
        let taken: String = suggestion.text.drain(..len).collect();
        self.edit(cx, |edit| {
            edit.insert(&taken, gleipnir_text::UndoGrouping::Distinct)
        });
        self.buffer
            .update(cx, |buffer, _| buffer.break_undo_group());
        if !suggestion.text.is_empty() {
            suggestion.offset = self.selections.primary().head;
            self.suggestion.current = Some(suggestion);
        }
        cx.notify();
        true
    }
}

/// How much of a suggestion to take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SuggestionPart {
    All,
    /// Any spaces, then a word, or a run of punctuation.
    Word,
    /// The rest of this line, or, at its end, the next line.
    Line,
}

impl SuggestionPart {
    /// How many bytes of `text` this part is.
    pub(crate) fn len_in(self, text: &str) -> usize {
        match self {
            SuggestionPart::All => text.len(),
            SuggestionPart::Line => match text.find('\n') {
                Some(0) => text[1..].find('\n').map_or(text.len(), |i| i + 1),
                Some(end) => end,
                None => text.len(),
            },
            SuggestionPart::Word => {
                let word = |c: char| c.is_alphanumeric() || c == '_';
                let spaces = text.len() - text.trim_start().len();
                let rest = &text[spaces..];
                let Some(first) = rest.chars().next() else {
                    return text.len();
                };
                let end = rest
                    .find(|c: char| c.is_whitespace() || word(c) != word(first))
                    .unwrap_or(rest.len());
                spaces + end
            }
        }
    }
}
