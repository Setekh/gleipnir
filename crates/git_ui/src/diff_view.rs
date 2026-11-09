//! A file's two versions side by side, as IntelliJ's diff shows them: the older on the left, the
//! newer on the right, changed lines tinted, and their numbers beside them.

use gleipnir_index::{DiffRow, DiffRowKind, side_by_side};
use gleipnir_ui::Theme;
use gleipnir_workspace::ModalView;
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Hsla, KeyBinding,
    SharedString, UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

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

pub struct DiffView {
    focus_handle: FocusHandle,
    title: SharedString,
    left_title: SharedString,
    right_title: SharedString,
    rows: Vec<DiffRow>,
    scroll: UniformListScrollHandle,
    /// The change last moved to with F7, as its first row.
    current: Option<usize>,
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
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            title: title.into(),
            left_title: left_title.into(),
            right_title: right_title.into(),
            rows: side_by_side(left, right),
            scroll: UniformListScrollHandle::new(),
            current: None,
        };
        // Opens at the first change, as there's nothing to see before it.
        this.move_to_change(true);
        this
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

    /// The first row of each run of changed rows.
    fn change_starts(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(i, row)| {
                row.kind != DiffRowKind::Same
                    && (*i == 0 || self.rows[i - 1].kind == DiffRowKind::Same)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn move_to_change(&mut self, forward: bool) {
        let starts = self.change_starts();
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

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let font = gleipnir_editor::editor_font(window, cx);
        let count = self.rows.len();
        let digits = self
            .rows
            .iter()
            .filter_map(|r| r.right.as_ref().or(r.left.as_ref()).map(|l| l.0 + 1))
            .max()
            .unwrap_or(1)
            .to_string()
            .len();
        let number_width = px(10. + 8. * digits as f32);
        let rows = cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            range
                .map(|index| {
                    let row = &this.rows[index];
                    let side = |line: &Option<(usize, String)>| {
                        let (number, text) = match line {
                            Some((n, text)) => ((n + 1).to_string(), text.replace('\t', "    ")),
                            None => (String::new(), String::new()),
                        };
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
                                    .child(text),
                            )
                    };
                    div()
                        .id(("diff-row", index))
                        .w_full()
                        .h(px(20.))
                        .flex()
                        .flex_row()
                        .child(side(&row.left))
                        .child(div().w(px(1.)).h_full().bg(theme.border))
                        .child(side(&row.right))
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        });
        let size = window.viewport_size();
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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(div().text_color(theme.text_muted).child({
                        let count = self.change_starts().len();
                        let changes = match count {
                            0 => "No changes".to_string(),
                            1 => "1 change".to_string(),
                            n => format!("{n} changes"),
                        };
                        format!("{changes} · F7 next, Shift+F7 previous")
                    })),
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
