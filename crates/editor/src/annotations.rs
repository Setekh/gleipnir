//! What a language server adds to the text, asked for in the background and kept in step with
//! edits: the other uses of the name under the cursor, inlay hints, and semantic highlighting,
//! which refines the syntax tree's colors where the server knows more, such as which names are
//! types or parameters.

use crate::view::EditorView;
use gleipnir_syntax::{HighlightKind, HighlightSpan};
use gleipnir_text::{AppliedEdit, Bias, transform_offset};
use gpui::{Context, Task};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// How long the cursor rests before its name's uses are looked up.
const HIGHLIGHT_DELAY: Duration = Duration::from_millis(250);
/// How long typing pauses before hints and semantic colors are asked for again.
const REFRESH_DELAY: Duration = Duration::from_millis(400);
/// Past this many lines, semantic colors aren't asked for: the answer would be large, and the
/// syntax tree's colors are close enough.
const SEMANTIC_LINE_LIMIT: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHint {
    /// The byte offset it's shown before.
    pub offset: usize,
    /// The text, with any padding the server asked for.
    pub label: String,
}

#[derive(Default)]
pub(crate) struct Annotations {
    /// Uses of the name under the cursor.
    pub highlights: Vec<Range<usize>>,
    pub inlays: Rc<Vec<InlayHint>>,
    /// Sorted, non-overlapping.
    pub tokens: Rc<Vec<(Range<usize>, HighlightKind)>>,
    /// Bumped whenever `tokens` change, so highlight caches know to rebuild.
    pub tokens_generation: u64,
    highlight_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
}

fn shift(range: &Range<usize>, edits: &[AppliedEdit]) -> Option<Range<usize>> {
    let mut range = range.clone();
    for edit in edits {
        // Anything the edit touched is stale; the next answer replaces it.
        if range.end > edit.start_byte && range.start < edit.old_end_byte {
            return None;
        }
        range = transform_offset(range.start, edit, Bias::Right)
            ..transform_offset(range.end, edit, Bias::Left);
    }
    Some(range)
}

/// Syntax spans with semantic ones laid over them: each token's range is cut out of the syntax
/// spans and colored as the token says. The result is sorted.
pub(crate) fn overlay(
    syntax: &[HighlightSpan],
    semantic: &[(Range<usize>, HighlightKind)],
    visible: Range<usize>,
) -> Vec<HighlightSpan> {
    let tokens: Vec<&(Range<usize>, HighlightKind)> = semantic
        .iter()
        .filter(|(r, _)| r.end > visible.start && r.start < visible.end)
        .collect();
    if tokens.is_empty() {
        return syntax.to_vec();
    }
    let mut out = Vec::with_capacity(syntax.len() + tokens.len() * 2);
    for span in syntax {
        let mut pieces = vec![span.range.clone()];
        let overlapping = tokens
            .iter()
            .filter(|(r, _)| r.start < span.range.end && r.end > span.range.start);
        for (token, _) in overlapping {
            pieces = pieces
                .into_iter()
                .flat_map(|piece| {
                    [
                        piece.start..token.start.min(piece.end),
                        token.end.max(piece.start)..piece.end,
                    ]
                })
                .filter(|r| r.start < r.end)
                .collect();
        }
        out.extend(pieces.into_iter().map(|range| HighlightSpan {
            range,
            kind: span.kind,
        }));
    }
    out.extend(tokens.iter().map(|(range, kind)| HighlightSpan {
        range: range.clone(),
        kind: *kind,
    }));
    out.sort_by_key(|s| s.range.start);
    out
}

impl EditorView {
    /// Inlay hints as `line: text`, one-based, for tests and the driver.
    pub fn inlay_labels(&self, cx: &gpui::App) -> Vec<String> {
        let buffer = self.buffer.read(cx).buffer();
        self.annotations
            .inlays
            .iter()
            .map(|h| format!("{}: {}", buffer.offset_to_point(h.offset).line + 1, h.label))
            .collect()
    }

    /// The text of each highlighted use of the name under the cursor, for tests and the driver.
    pub fn usage_highlights(&self, cx: &gpui::App) -> Vec<Range<usize>> {
        let _ = cx;
        self.annotations.highlights.clone()
    }

    /// How many semantic tokens are applied, for tests and the driver.
    pub fn semantic_token_count(&self) -> usize {
        self.annotations.tokens.len()
    }

    /// Moves what's known through an edit, then asks again once typing pauses.
    pub(crate) fn annotations_edited(&mut self, edits: &[AppliedEdit], cx: &mut Context<Self>) {
        let annotations = &mut self.annotations;
        annotations.highlights.clear();
        annotations.highlight_task = None;
        let inlays: Vec<InlayHint> = annotations
            .inlays
            .iter()
            .filter_map(|h| {
                let moved = shift(&(h.offset..h.offset), edits)?;
                Some(InlayHint {
                    offset: moved.start,
                    label: h.label.clone(),
                })
            })
            .collect();
        annotations.inlays = Rc::new(inlays);
        let tokens: Vec<(Range<usize>, HighlightKind)> = annotations
            .tokens
            .iter()
            .filter_map(|(r, k)| Some((shift(r, edits)?, *k)))
            .collect();
        annotations.tokens = Rc::new(tokens);
        annotations.tokens_generation += 1;
        self.schedule_annotation_refresh(REFRESH_DELAY, cx);
    }

    /// Asks again for hints and semantic colors, as when a server finishes loading.
    pub fn refresh_annotations(&mut self, cx: &mut Context<Self>) {
        self.schedule_annotation_refresh(Duration::ZERO, cx);
        self.annotations.highlight_task = None;
        self.request_usage_highlights(cx);
    }

    fn schedule_annotation_refresh(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(provider) = self.semantics.provider.clone() else {
            return;
        };
        let buffer = self.buffer.clone();
        self.annotations.refresh_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Ok((hints, tokens, version)) = this.update(cx, |_, cx| {
                let model = buffer.read(cx);
                let version = model.buffer().version();
                let small = model.buffer().line_count() <= SEMANTIC_LINE_LIMIT;
                let hints = provider.inlay_hints(&buffer, cx);
                let tokens = small.then(|| provider.semantic_tokens(&buffer, cx));
                (hints, tokens, version)
            }) else {
                return;
            };
            let hints = hints.await.unwrap_or_default();
            let tokens = match tokens {
                Some(task) => task.await.ok(),
                None => Some(Vec::new()),
            };
            this.update(cx, |this, cx| {
                // An answer about older text would put things in the wrong places.
                if this.buffer.read(cx).buffer().version() != version {
                    return;
                }
                let mut hints = hints;
                hints.sort_by_key(|h| h.offset);
                this.annotations.inlays = Rc::new(hints);
                if let Some(mut tokens) = tokens {
                    tokens.sort_by_key(|(r, _)| r.start);
                    tokens.dedup_by(|b, a| b.0.start < a.0.end);
                    this.annotations.tokens = Rc::new(tokens);
                    this.annotations.tokens_generation += 1;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// After the cursor rests, highlights the other uses of the name it's on.
    pub(crate) fn request_usage_highlights(&mut self, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        // Still on one of the highlighted uses: nothing new to ask.
        if self
            .annotations
            .highlights
            .iter()
            .any(|r| r.start <= head && head <= r.end)
        {
            return;
        }
        self.annotations.highlights.clear();
        let Some(provider) = self.semantics.provider.clone() else {
            return;
        };
        let buffer = self.buffer.clone();
        self.annotations.highlight_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIGHLIGHT_DELAY).await;
            let Ok((task, version)) = this.update(cx, |this, cx| {
                let version = buffer.read(cx).buffer().version();
                let head = this.selections.primary().head;
                (provider.document_highlights(&buffer, head, cx), version)
            }) else {
                return;
            };
            let ranges = task.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                if this.buffer.read(cx).buffer().version() != version {
                    return;
                }
                // A name used once isn't worth marking.
                this.annotations.highlights = if ranges.len() > 1 { ranges } else { Vec::new() };
                cx.notify();
            })
            .ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(range: Range<usize>, kind: HighlightKind) -> HighlightSpan {
        HighlightSpan { range, kind }
    }

    #[test]
    fn semantic_tokens_win_where_they_overlap_syntax() {
        let syntax = vec![
            span(0..3, HighlightKind::Keyword),
            span(4..10, HighlightKind::Variable),
            span(12..15, HighlightKind::Function),
        ];
        let semantic = vec![
            (4..7, HighlightKind::Parameter),
            (10..12, HighlightKind::Type),
        ];
        let out = overlay(&syntax, &semantic, 0..20);
        let shape: Vec<(Range<usize>, HighlightKind)> =
            out.into_iter().map(|s| (s.range, s.kind)).collect();
        assert_eq!(
            shape,
            [
                (0..3, HighlightKind::Keyword),
                (4..7, HighlightKind::Parameter),
                (7..10, HighlightKind::Variable),
                (10..12, HighlightKind::Type),
                (12..15, HighlightKind::Function),
            ]
        );
    }

    #[test]
    fn ranges_an_edit_touches_are_dropped_and_later_ones_shift() {
        let mut buffer = gleipnir_text::Buffer::new("let total = count;");
        let cursor = gleipnir_text::SelectionSet::cursor(0);
        let edits = buffer.apply_edits(
            vec![gleipnir_text::TextEdit::new(0..0, "// x\n")],
            &cursor,
            &cursor,
            gleipnir_text::UndoGrouping::Distinct,
            std::time::Instant::now(),
        );
        assert_eq!(shift(&(4..9), &edits), Some(9..14));
        let edits = buffer.apply_edits(
            vec![gleipnir_text::TextEdit::new(10..12, "")],
            &cursor,
            &cursor,
            gleipnir_text::UndoGrouping::Distinct,
            std::time::Instant::now(),
        );
        assert_eq!(shift(&(9..14), &edits), None);
    }
}
