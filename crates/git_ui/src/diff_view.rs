//! A file's two versions side by side, as IntelliJ's diff shows them: the older on the left, the
//! newer on the right, highlighted as code, changed lines tinted, and their numbers beside them.
//! A diff of the working copy can put each change back as it was, one at a time. A change someone
//! else proposes, like an AI agent, is reviewed in the same view: the file as it is on the left,
//! the proposal on the right, each change accepted into the file or rejected.

use gleipnir_core::BufferModel;
use gleipnir_index::{DiffRow, DiffRowKind, merge3, side_by_side};
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

/// One change decided on while reviewing a proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewDecision {
    pub accepted: bool,
    /// The change's first line in the file, from 1, when it was decided.
    pub line: usize,
    /// The file's lines it takes out, and the lines it puts in.
    pub removed: String,
    pub added: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewEvent {
    /// Every change has been decided, or the review was closed with `undecided` changes left,
    /// which stay out of the file.
    Finished {
        decisions: Vec<ReviewDecision>,
        undecided: usize,
    },
}

/// A proposal being reviewed: the open file the left side is, which accepted changes go into.
struct Review {
    buffer: Entity<BufferModel>,
    decisions: Vec<ReviewDecision>,
    /// Changes that stopped fitting the file when it changed underneath, left out undecided.
    left_out: usize,
    finished: bool,
    /// Said in the title bar, like that the file changed underneath.
    notice: Option<SharedString>,
}

/// One change: the lines it spans on each side, and their text.
struct Hunk {
    left_bytes: Range<usize>,
    right_bytes: Range<usize>,
    left_line: usize,
    left_text: String,
    right_text: String,
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
    review: Option<Review>,
}

impl EventEmitter<DismissEvent> for DiffView {}
impl EventEmitter<ReviewEvent> for DiffView {}
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
            review: None,
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

    /// The change starting at a row, with where it is in each side's text.
    fn hunk(&self, first_row: usize) -> Option<Hunk> {
        let run = self.changes().into_iter().find(|r| r.start == first_row)?;
        let bytes = |side: &Side, lines: Range<usize>| {
            let all: Vec<&str> = side.text.split_inclusive('\n').collect();
            let lines = lines.start.min(all.len())..lines.end.min(all.len());
            let start: usize = all[..lines.start].iter().map(|l| l.len()).sum();
            let end = start + all[lines].iter().map(|l| l.len()).sum::<usize>();
            start..end
        };
        let left_lines = self.lines_of(run.clone(), false);
        let left_bytes = bytes(&self.left, left_lines.clone());
        let right_bytes = bytes(&self.right, self.lines_of(run, true));
        Some(Hunk {
            left_text: self.left.text[left_bytes.clone()].to_string(),
            right_text: self.right.text[right_bytes.clone()].to_string(),
            left_bytes,
            right_bytes,
            left_line: left_lines.start + 1,
        })
    }

    /// Replaces one side's text, and works out the diff again.
    fn set_side(&mut self, right: bool, text: &str) {
        let side = Rc::new(Side::new(text, self.language));
        if right {
            self.right = side;
        } else {
            self.left = side;
        }
        self.rows = Rc::new(side_by_side(&self.left.text, &self.right.text));
        self.current = None;
    }

    /// Puts the change starting at a row back as it is on the left.
    pub fn revert_change(&mut self, first_row: usize, cx: &mut Context<Self>) {
        let Some(target) = self.revertible.clone() else {
            return;
        };
        let Some(hunk) = self.hunk(first_row) else {
            return;
        };
        let mut text = self.right.text.clone();
        text.replace_range(hunk.right_bytes.clone(), &hunk.left_text);
        match target {
            Revertible::Buffer(buffer) => buffer.update(cx, |buffer, cx| {
                buffer.apply_external_edits(
                    vec![gleipnir_text::TextEdit::new(
                        hunk.right_bytes,
                        hunk.left_text,
                    )],
                    cx,
                )
            }),
            Revertible::File(path) => {
                std::fs::write(path, &text).ok();
            }
        }
        self.set_side(true, &text);
        cx.notify();
    }

    /// Reviews a proposed change to an open file, which is the left side, with the proposal on
    /// the right. Accepting a change edits the file, as a step undo takes back, unsaved;
    /// rejecting it leaves the file alone. Once every change is decided, or the view is closed,
    /// it says what was decided with `ReviewEvent::Finished`.
    pub fn reviewing(mut self, buffer: Entity<BufferModel>) -> Self {
        self.review = Some(Review {
            buffer,
            decisions: Vec::new(),
            left_out: 0,
            finished: false,
            notice: None,
        });
        self
    }

    pub fn is_reviewing(&self) -> bool {
        self.review.is_some()
    }

    /// What's been decided so far, in the order it was.
    pub fn decisions(&self) -> &[ReviewDecision] {
        self.review.as_ref().map_or(&[], |r| &r.decisions)
    }

    /// Checks the file is still what the left side shows. If it changed, as from another edit
    /// landing while the person looks, the proposal's changes are carried over to the file as it
    /// is now, as git merges, and nothing's decided this time. Changes that clash with the
    /// file's are left out.
    fn file_unchanged(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(review) = &self.review else {
            return false;
        };
        let now = review.buffer.read(cx).buffer().text();
        if now == self.left.text {
            return true;
        }
        let mut clashed = 0;
        let proposed: String = merge3(&self.left.text, &now, &self.right.text)
            .iter()
            .map(|chunk| match chunk.merged() {
                Some(lines) => lines.concat(),
                None => {
                    clashed += 1;
                    chunk.ours.concat()
                }
            })
            .collect();
        self.set_side(false, &now);
        self.set_side(true, &proposed);
        if let Some(review) = &mut self.review {
            review.left_out += clashed;
            review.notice = Some(if clashed == 0 {
                "The file changed, so the proposal was carried over to it".into()
            } else {
                format!(
                    "The file changed, and {clashed} proposed change(s) no longer fit it, so \
                     they were left out"
                )
                .into()
            });
        }
        if self.change_rows().is_empty() {
            self.finish_review(cx);
        }
        cx.notify();
        false
    }

    /// Takes the change starting at a row into the file.
    pub fn accept_change(&mut self, first_row: usize, cx: &mut Context<Self>) {
        self.decide(Some(first_row), true, cx);
    }

    /// Leaves the change starting at a row out of the file.
    pub fn reject_change(&mut self, first_row: usize, cx: &mut Context<Self>) {
        self.decide(Some(first_row), false, cx);
    }

    /// Takes every change left into the file, as one step undo takes back.
    pub fn accept_all(&mut self, cx: &mut Context<Self>) {
        self.decide(None, true, cx);
    }

    /// Leaves every change left out of the file.
    pub fn reject_all(&mut self, cx: &mut Context<Self>) {
        self.decide(None, false, cx);
    }

    /// Decides one change, by its first row, or with `None`, all that are left.
    fn decide(&mut self, first_row: Option<usize>, accept: bool, cx: &mut Context<Self>) {
        if self.review.as_ref().is_none_or(|r| r.finished) || !self.file_unchanged(cx) {
            return;
        }
        let rows = match first_row {
            Some(row) => vec![row],
            None => self.change_rows(),
        };
        let hunks: Vec<Hunk> = rows.into_iter().filter_map(|row| self.hunk(row)).collect();
        if hunks.is_empty() {
            return;
        }
        let mut left = self.left.text.clone();
        let mut right = self.right.text.clone();
        // From the end back, so earlier ranges stay where they are.
        for hunk in hunks.iter().rev() {
            if accept {
                left.replace_range(hunk.left_bytes.clone(), &hunk.right_text);
            } else {
                right.replace_range(hunk.right_bytes.clone(), &hunk.left_text);
            }
        }
        let review = self.review.as_mut().expect("reviewing");
        if accept {
            let edits = hunks
                .iter()
                .map(|h| gleipnir_text::TextEdit::new(h.left_bytes.clone(), h.right_text.clone()))
                .collect();
            review
                .buffer
                .update(cx, |buffer, cx| buffer.apply_external_edits(edits, cx));
        }
        review.notice = None;
        review
            .decisions
            .extend(hunks.into_iter().map(|h| ReviewDecision {
                accepted: accept,
                line: h.left_line,
                removed: h.left_text,
                added: h.right_text,
            }));
        if accept {
            self.set_side(false, &left);
        } else {
            self.set_side(true, &right);
        }
        if self.change_rows().is_empty() {
            self.finish_review(cx);
        } else {
            self.move_to_change(true);
        }
        cx.notify();
    }

    /// Ends the review, leaving out whatever wasn't decided, and closes the view.
    fn finish_review(&mut self, cx: &mut Context<Self>) {
        let mut undecided = self.change_rows().len();
        let Some(review) = &mut self.review else {
            return;
        };
        undecided += review.left_out;
        if !review.finished {
            review.finished = true;
            cx.emit(ReviewEvent::Finished {
                decisions: review.decisions.clone(),
                undecided,
            });
        }
        cx.emit(DismissEvent);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.review.is_some() {
            self.finish_review(cx);
        } else {
            cx.emit(DismissEvent);
        }
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
        let reviewing = self.review.is_some();
        let middle = if reviewing { px(46.) } else { px(18.) };
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
                    let review_button = |label: &'static str, accept: bool| {
                        let kind = if accept { "accept" } else { "reject" };
                        div()
                            .id((kind, index))
                            .debug_selector(move || format!("review-{kind}-{index}"))
                            .px(px(4.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .font_weight(FontWeight::BOLD)
                            .text_color(if accept {
                                theme.git_added
                            } else {
                                theme.git_deleted
                            })
                            .hover(|s| s.bg(theme.hover))
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if accept {
                                    this.accept_change(index, cx)
                                } else {
                                    this.reject_change(index, cx)
                                }
                            }))
                    };
                    let review_here = reviewing && starts.contains(&index);
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
                                .w(middle)
                                .h_full()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap(px(2.))
                                .border_l_1()
                                .border_r_1()
                                .border_color(theme.border)
                                .when(review_here, |d| {
                                    d.child(review_button("✓", true))
                                        .child(review_button("✕", false))
                                })
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
        let hint = if reviewing {
            "F7 next, Shift+F7 previous, ✓ accepts a change, ✕ rejects it"
        } else if can_revert {
            "F7 next, Shift+F7 previous, ⟲ puts a change back"
        } else {
            "F7 next, Shift+F7 previous"
        };
        let notice = self.review.as_ref().and_then(|r| r.notice.clone());
        let header_button = |id: &'static str, label: &'static str, accept: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .text_color(theme.text)
                .hover(|s| s.bg(theme.hover))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if accept {
                        this.accept_all(cx)
                    } else {
                        this.reject_all(cx)
                    }
                }))
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
            .on_action(cx.listener(|this, _: &CloseDiff, _, cx| this.close(cx)))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| {
                this.move_to_change(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| {
                this.move_to_change(false);
                cx.notify();
            }))
            // A review isn't thrown away by a stray click outside.
            .when(!reviewing, |d| {
                d.on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            })
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
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .children(notice.map(|n| div().text_color(theme.warning).child(n)))
                            .child(
                                div()
                                    .text_color(theme.text_muted)
                                    .child(format!("{changes} · {hint}")),
                            )
                            .when(reviewing, |d| {
                                d.child(header_button("review-accept-all", "Accept All", true))
                                    .child(header_button("review-reject-all", "Reject All", false))
                            }),
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
                    .child(div().w(middle).flex_none())
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
