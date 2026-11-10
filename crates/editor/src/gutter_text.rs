//! Text in the gutter beside each line, such as who last changed it and when, as IntelliJ's
//! Annotate shows it. The editor knows nothing of where it comes from: it keeps each label beside
//! its line as the text is edited, blanks lines that change, shows a label's details when the
//! mouse rests on it, and reports clicks as events.

use crate::view::{EditorEvent, EditorView};
use gleipnir_text::AppliedEdit;
use gpui::{
    AnyElement, Context, Pixels, Point, SharedString, anchored, deferred, div, prelude::*, px,
};

/// One line's label.
#[derive(Clone, Debug, PartialEq)]
pub struct GutterLabel {
    pub text: SharedString,
    /// Which entry of `GutterText::details` says more about it, shared by the lines of one commit.
    pub group: usize,
    /// How strongly its background is tinted, from 0 to 1: newer changes stand out more.
    pub shade: f32,
}

/// Labels for the lines of a file, and what each group's tooltip says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GutterText {
    /// One per line; none where there's nothing to say, such as a line not yet committed.
    pub lines: Vec<Option<GutterLabel>>,
    pub details: Vec<SharedString>,
}

impl GutterText {
    /// The widest label, in characters, which sets the column's width.
    pub(crate) fn width_in_chars(&self) -> usize {
        self.lines
            .iter()
            .flatten()
            .map(|l| l.text.chars().count())
            .max()
            .unwrap_or(0)
    }

    /// Moves the labels with an edit: lines added have none, and lines the edit changed lose
    /// theirs, as they're no longer what the label describes.
    pub(crate) fn edited(&mut self, edit: &AppliedEdit) {
        shift_lines(&mut self.lines, edit);
    }
}

/// Keeps one entry per line in step with an edit.
pub(crate) fn shift_lines<T>(lines: &mut Vec<Option<T>>, edit: &AppliedEdit) {
    let (start, old_end, new_end) = (edit.start.line, edit.old_end.line, edit.new_end.line);
    // Whole lines replaced, with the line after untouched: that line keeps its entry.
    let whole_lines = edit.old_end.col == 0 && edit.new_end.col == 0;
    let (removed_end, added) = if whole_lines {
        (old_end, new_end - start)
    } else {
        (old_end + 1, new_end - start + 1)
    };
    let from = start.min(lines.len());
    let to = removed_end.min(lines.len()).max(from);
    lines.splice(from..to, std::iter::repeat_with(|| None).take(added));
}

impl EditorView {
    /// Shows text beside each line in the gutter, or with `None`, takes it away.
    pub fn set_gutter_text(&mut self, text: Option<GutterText>, cx: &mut Context<Self>) {
        self.gutter_text_chars = text.as_ref().map_or(0, GutterText::width_in_chars);
        self.gutter_text = text;
        self.gutter_hover = None;
        cx.notify();
    }

    pub fn has_gutter_text(&self) -> bool {
        self.gutter_text.is_some()
    }

    /// Each line's label as `line: text`, one-based, for tests and the driver.
    pub fn gutter_labels(&self) -> Vec<String> {
        self.gutter_text.as_ref().map_or_else(Vec::new, |text| {
            text.lines
                .iter()
                .enumerate()
                .filter_map(|(i, l)| Some(format!("{}: {}", i + 1, l.as_ref()?.text)))
                .collect()
        })
    }

    /// The group a line's label is in, if it has one.
    pub fn gutter_group(&self, line: usize) -> Option<usize> {
        let text = self.gutter_text.as_ref()?;
        Some(text.lines.get(line)?.as_ref()?.group)
    }

    /// The details shown for a line's label, if it has one.
    pub fn gutter_details(&self, line: usize) -> Option<SharedString> {
        let text = self.gutter_text.as_ref()?;
        let label = text.lines.get(line)?.as_ref()?;
        text.details.get(label.group).cloned()
    }

    pub(crate) fn gutter_text_edited(&mut self, edits: &[AppliedEdit]) {
        if let Some(text) = &mut self.gutter_text {
            for edit in edits {
                text.edited(edit);
            }
        }
        self.gutter_hover = None;
    }

    /// The mouse over a label, or off them all.
    pub(crate) fn gutter_text_hovered(
        &mut self,
        line: Option<usize>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let line = line.filter(|l| self.gutter_details(*l).is_some());
        if self.gutter_hover.map(|(l, _)| l) != line {
            self.gutter_hover = line.map(|l| (l, position));
            cx.notify();
        }
    }

    pub(crate) fn gutter_text_clicked(&mut self, line: usize, cx: &mut Context<Self>) {
        if self.gutter_details(line).is_some() {
            cx.emit(EditorEvent::GutterTextClicked { line });
        }
    }

    pub(crate) fn render_gutter_tooltip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (line, position) = self.gutter_hover?;
        let details = self.gutter_details(line)?;
        let theme = cx.global::<gleipnir_ui::Theme>().clone();
        Some(
            deferred(
                anchored()
                    .position(position + gpui::point(px(12.), px(12.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("gutter-text-details")
                            .debug_selector(|| "gutter-text-details".into())
                            .max_w(px(520.))
                            .p(px(10.))
                            .bg(theme.surface_raised)
                            .border_1()
                            .border_color(theme.border)
                            .rounded(px(6.))
                            .shadow_lg()
                            .text_size(px(12.))
                            .text_color(theme.text)
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .children(details.lines().map(|l| {
                                div()
                                    .min_h(px(14.))
                                    .child(SharedString::from(l.to_string()))
                            })),
                    ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gleipnir_text::{Buffer, SelectionSet, TextEdit, UndoGrouping};

    fn edit(text: &str, range: std::ops::Range<usize>, new: &str) -> AppliedEdit {
        let mut buffer = Buffer::new(text);
        let cursor = SelectionSet::cursor(0);
        buffer
            .apply_edits(
                vec![TextEdit::new(range, new)],
                &cursor,
                &cursor,
                UndoGrouping::Distinct,
                std::time::Instant::now(),
            )
            .remove(0)
    }

    fn shifted(text: &str, range: std::ops::Range<usize>, new: &str) -> Vec<Option<char>> {
        let mut lines: Vec<Option<char>> = ('a'..).take(text.lines().count()).map(Some).collect();
        shift_lines(&mut lines, &edit(text, range, new));
        lines
    }

    const TEXT: &str = "one\ntwo\nthree\nfour\n";

    #[test]
    fn lines_added_above_push_labels_down() {
        assert_eq!(
            shifted(TEXT, 4..4, "new\nnewer\n"),
            [Some('a'), None, None, Some('b'), Some('c'), Some('d')]
        );
    }

    #[test]
    fn whole_lines_removed_take_their_labels_with_them() {
        assert_eq!(shifted(TEXT, 4..14, ""), [Some('a'), Some('d')]);
    }

    #[test]
    fn a_line_typed_in_loses_its_label() {
        assert_eq!(
            shifted(TEXT, 5..5, "x"),
            [Some('a'), None, Some('c'), Some('d')]
        );
        // Enter in the middle of a line: both halves have changed.
        assert_eq!(
            shifted(TEXT, 5..5, "\n"),
            [Some('a'), None, None, Some('c'), Some('d')]
        );
    }

    #[test]
    fn joining_lines_blanks_the_joined_line_only() {
        // Deleting the line break after "two".
        assert_eq!(shifted(TEXT, 7..8, ""), [Some('a'), None, Some('d')]);
    }
}
