//! A file's two versions side by side, as IntelliJ's diff shows them: the older on the left, the
//! newer on the right, highlighted as code, changed lines tinted, and their numbers beside them.
//! A diff of the working copy can put each change back as it was, one at a time.

use gleipnir_core::BufferModel;
use gleipnir_index::{DiffRow, DiffRowKind, side_by_side};
use gleipnir_syntax::{HighlightKind, Language, highlight_text};
use gleipnir_ui::Theme;
use gleipnir_workspace::ModalView;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, Hsla, KeyBinding, SharedString, StyledText, UniformListScrollHandle, Window,
    actions, div, prelude::*, px, uniform_list,
};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

actions!(diff_view, [CloseDiff, NextChange, PreviousChange]);

pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some("DiffView");
    cx.bind_keys([
        KeyBinding::new("escape", CloseDiff, context),
        // IntelliJ's next and previous difference.
        KeyBinding::new("f7", NextChange, context),
        KeyBinding::new("shift-f7", PreviousChange, context),
    ]);
}

/// Where the newer side lives, when changes can be put back: an open file's buffer, so it's
/// undoable, or the file on disk.
#[derive(Clone)]
pub enum Revertible {
    Buffer(Entity<BufferModel>),
    File(PathBuf),
}

/// One side of the diff: its lines, and each line's highlights, relative to the line.
struct Side {
    text: String,
    lines: Vec<String>,
    highlights: Vec<Vec<(Range<usize>, HighlightKind)>>,
}

impl Side {
    fn new(text: &str, language: Option<Language>) -> Self {
        let text = text.replace("\r\n", "\n");
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let mut highlights = vec![Vec::new(); lines.len()];
        if let Some(language) = language {
            let mut starts = Vec::with_capacity(lines.len());
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                starts.push(offset);
                offset += line.len();
            }
            for span in highlight_text(language, &text) {
                // The line holding the span's start; spans across lines are cut at its end.
                let line = starts
                    .partition_point(|&s| s <= span.range.start)
                    .saturating_sub(1);
                let (Some(&start), Some(content)) = (starts.get(line), lines.get(line)) else {
                    continue;
                };
                let from = span.range.start - start;
                let to = (span.range.end - start).min(content.len());
                if from < to {
                    highlights[line].push((from..to, span.kind));
                }
            }
        }
        Self {
            text,
            lines,
            highlights,
        }
    }
}

pub struct DiffView {
    focus_handle: FocusHandle,
    title: SharedString,
    left_title: SharedString,
    right_title: SharedString,
    language: Option<Language>,
    left: Rc<Side>,
    right: Rc<Side>,
    rows: Rc<Vec<DiffRow>>,
    scroll: UniformListScrollHandle,
    /// The change last moved to with F7, as its first row.
    current: Option<usize>,
    revertible: Option<Revertible>,
}

impl EventEmitter<DismissEvent> for DiffView {}
impl ModalView for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DiffView {
    pub fn new(
        title: impl Into<SharedString>,
        (left_title, left): (impl Into<SharedString>, &str),
        (right_title, right): (impl Into<SharedString>, &str),
        cx: &mut Context<Self>,
    ) -> Self {
        let title = title.into();
        // The title starts with the file's path, which says its language.
        let path = title
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let language = gleipnir_packages::registry()
            .language_for_path(std::path::Path::new(&path))
            .map(|l| l.grammar);
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            title,
            left_title: left_title.into(),
            right_title: right_title.into(),
            language,
            left: Rc::new(Side::new(left, language)),
            right: Rc::new(Side::new(right, language)),
            rows: Rc::new(side_by_side(left, right)),
            scroll: UniformListScrollHandle::new(),
            current: None,
            revertible: None,
        };
        // Opens at the first change, as there's nothing to see before it.
        this.move_to_change(true);
        this
    }

    /// Lets each change be put back as it was on the left, in the file the right side is.
    pub fn revertible(mut self, target: Revertible) -> Self {
        self.revertible = Some(target);
        self
    }

    /// The rows, as `left | right` with a mark for how they differ, for tests.
    pub fn debug_rows(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let mark = match row.kind {
                    DiffRowKind::Same => ' ',
                    DiffRowKind::Added => '+',
                    DiffRowKind::Deleted => '-',
                    DiffRowKind::Changed => '~',
                };
                fn side(line: &Option<(usize, String)>) -> &str {
                    line.as_ref().map(|(_, t)| t.as_str()).unwrap_or("")
                }
                format!("{mark} {} | {}", side(&row.left), side(&row.right))
            })
            .collect()
    }

    /// How many highlighted pieces each side has, for tests.
    pub fn highlight_count(&self) -> (usize, usize) {
        let count = |side: &Side| side.highlights.iter().map(Vec::len).sum();
        (count(&self.left), count(&self.right))
    }

    /// Each run of changed rows, as the rows it spans.
    fn changes(&self) -> Vec<Range<usize>> {
        let mut runs = Vec::new();
        let mut start = None;
        for (i, row) in self.rows.iter().enumerate() {
            match (row.kind == DiffRowKind::Same, start) {
                (false, None) => start = Some(i),
                (true, Some(s)) => {
                    runs.push(s..i);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            runs.push(s..self.rows.len());
        }
        runs
    }

    /// The first row of each change, for tests.
    pub fn change_rows(&self) -> Vec<usize> {
        self.changes().into_iter().map(|r| r.start).collect()
    }

    fn move_to_change(&mut self, forward: bool) {
        let starts = self.change_rows();
        let next = match (self.current, forward) {
            (None, _) => starts.first().copied(),
            (Some(at), true) => starts.iter().copied().find(|&s| s > at),
            (Some(at), false) => starts.iter().rev().copied().find(|&s| s < at),
        };
        if let Some(row) = next {
            self.current = Some(row);
            self.scroll
                .scroll_to_item(row.saturating_sub(3), gpui::ScrollStrategy::Top);
        }
    }

    /// The lines a run of rows covers on one side; for a side with none, where they'd go.
    fn lines_of(&self, rows: Range<usize>, right: bool) -> Range<usize> {
        let line = |row: &DiffRow| {
            if right {
                row.right.as_ref().map(|l| l.0)
            } else {
                row.left.as_ref().map(|l| l.0)
            }
        };
        let found: Vec<usize> = self.rows[rows.clone()].iter().filter_map(line).collect();
        match (found.first(), found.last()) {
            (Some(&first), Some(&last)) => first..last + 1,
            _ => {
                let at = self.rows[rows.end..]
                    .iter()
                    .find_map(line)
                    .unwrap_or(if right {
                        self.right.lines.len()
                    } else {
                        self.left.lines.len()
                    });
                at..at
            }
        }
    }

    /// Puts the change starting at a row back as it is on the left.
    pub fn revert_change(&mut self, first_row: usize, cx: &mut Context<Self>) {
        let Some(target) = self.revertible.clone() else {
            return;
        };
        let Some(run) = self.changes().into_iter().find(|r| r.start == first_row) else {
            return;
        };
        let old = self.lines_of(run.clone(), false);
        let new = self.lines_of(run, true);
        let left: Vec<&str> = self.left.text.split_inclusive('\n').collect();
        let replacement: String = left[old].concat();
        let right: Vec<&str> = self.right.text.split_inclusive('\n').collect();
        let start: usize = right[..new.start].iter().map(|l| l.len()).sum();
        let end: usize = start + right[new].iter().map(|l| l.len()).sum::<usize>();
        let mut text = self.right.text.clone();
        text.replace_range(start..end, &replacement);
        match target {
            Revertible::Buffer(buffer) => buffer.update(cx, |buffer, cx| {
                buffer.apply_external_edits(
                    vec![gleipnir_text::TextEdit::new(start..end, replacement)],
                    cx,
                )
            }),
            Revertible::File(path) => {
                std::fs::write(path, &text).ok();
            }
        }
        self.right = Rc::new(Side::new(&text, self.language));
        self.rows = Rc::new(side_by_side(&self.left.text, &text));
        self.current = None;
        cx.notify();
    }
}

fn tint(kind: DiffRowKind, theme: &Theme) -> Option<Hsla> {
    let color = match kind {
        DiffRowKind::Same => return None,
        DiffRowKind::Added => theme.git_added,
        DiffRowKind::Deleted => theme.git_deleted,
        DiffRowKind::Changed => theme.git_modified,
    };
    Some(color.opacity(0.16))
}

/// A line as code, colored by its highlights.
fn code_line(side: &Side, line: usize, theme: &Theme) -> StyledText {
    let text = side.lines.get(line).cloned().unwrap_or_default();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = side
        .highlights
        .get(line)
        .into_iter()
        .flatten()
        .filter(|(range, _)| {
            range.end <= text.len()
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end)
        })
        .map(|(range, kind)| {
            (
                range.clone(),
                HighlightStyle {
                    color: Some(theme.syntax.color(*kind)),
                    ..Default::default()
                },
            )
        })
        .collect();
    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let font = gleipnir_editor::editor_font(window, cx);
        let count = self.rows.len();
        let digits = self
            .left
            .lines
            .len()
            .max(self.right.lines.len())
            .max(1)
            .to_string()
            .len();
        let number_width = px(10. + 8. * digits as f32);
        let change_starts = self.change_rows();
        let can_revert = self.revertible.is_some();
        let starts = change_starts.clone();
        let rows = cx.processor(move |this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            let (left, right, rows) = (this.left.clone(), this.right.clone(), this.rows.clone());
            range
                .map(|index| {
                    let row = &rows[index];
                    let side = |line: &Option<(usize, String)>, side: &Side| {
                        let number = line
                            .as_ref()
                            .map(|(n, _)| (n + 1).to_string())
                            .unwrap_or_default();
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .when_some(
                                line.as_ref().and_then(|_| tint(row.kind, &theme)),
                                |d, color| d.bg(color),
                            )
                            .child(
                                div()
                                    .w(number_width)
                                    .flex_none()
                                    .pr(px(8.))
                                    .text_right()
                                    .text_color(theme.editor_gutter_text)
                                    .child(number),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .children(
                                        line.as_ref().map(|(n, _)| code_line(side, *n, &theme)),
                                    ),
                            )
                    };
                    let starts_change = can_revert && starts.contains(&index);
                    div()
                        .id(("diff-row", index))
                        .w_full()
                        .h(px(20.))
                        .flex()
                        .flex_row()
                        .child(side(&row.left, &left))
                        .child(
                            div()
                                .id(("diff-revert", index))
                                .debug_selector(move || format!("diff-revert-{index}"))
                                .w(px(18.))
                                .h_full()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .border_l_1()
                                .border_r_1()
                                .border_color(theme.border)
                                .when(starts_change, |d| {
                                    d.cursor_pointer()
                                        .text_color(theme.accent)
                                        .font_weight(FontWeight::BOLD)
                                        .hover(|s| s.bg(theme.hover))
                                        .child("⟲")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.revert_change(index, cx)
                                        }))
                                }),
                        )
                        .child(side(&row.right, &right))
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        });
        let size = window.viewport_size();
        let changes = match change_starts.len() {
            0 => "No changes".to_string(),
            1 => "1 change".to_string(),
            n => format!("{n} changes"),
        };
        let hint = if can_revert {
            "F7 next, Shift+F7 previous, ⟲ puts a change back"
        } else {
            "F7 next, Shift+F7 previous"
        };
        div()
            .key_context("DiffView")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(size.width - px(80.))
            .h(size.height - px(140.))
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_color(theme.text)
            .on_action(cx.listener(|_, _: &CloseDiff, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| {
                this.move_to_change(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| {
                this.move_to_change(false);
                cx.notify();
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .bg(theme.surface)
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(13.))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child(format!("{changes} · {hint}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .bg(theme.surface)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .px(px(14.))
                            .py(px(4.))
                            .child(self.left_title.clone()),
                    )
                    .child(div().w(px(18.)).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .px(px(14.))
                            .py(px(4.))
                            .child(self.right_title.clone()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font(font)
                    .text_size(px(theme.editor_font_size))
                    .child(
                        uniform_list("diff-rows", count, rows)
                            .track_scroll(&self.scroll)
                            .size_full(),
                    ),
            )
    }
}
