//! IntelliJ's merge window for a conflicted file: your version on the left, theirs on the right,
//! and the result between. What only one side changed is merged already; each conflict is settled
//! by taking a side, or both, and Apply saves the result and marks the file resolved.

use gleipnir_index::git_ops::{self, ChangedFile};
use gleipnir_index::{MergeChunk, MergeKind, merge3};
use gleipnir_ui::Theme;
use gleipnir_workspace::{ModalView, Workspace};
use gpui::{
    AnyElement, App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    KeyBinding, SharedString, WeakEntity, Window, actions, div, prelude::*, px,
};
use std::path::PathBuf;

actions!(merge_view, [CloseMerge]);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", CloseMerge, Some("MergeView"))]);
}

/// How a conflict was settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Ours,
    Theirs,
    OursThenTheirs,
    TheirsThenOurs,
}

pub struct MergeView {
    focus_handle: FocusHandle,
    root: PathBuf,
    file: ChangedFile,
    chunks: Vec<MergeChunk>,
    /// Each chunk's settlement, for the conflicts among them.
    choices: Vec<Option<Choice>>,
    workspace: WeakEntity<Workspace>,
}

impl EventEmitter<DismissEvent> for MergeView {}
impl ModalView for MergeView {}

impl Focusable for MergeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl MergeView {
    pub fn new(
        root: PathBuf,
        file: ChangedFile,
        (base, ours, theirs): (String, String, String),
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let chunks = merge3(&base, &ours, &theirs);
        Self {
            focus_handle: cx.focus_handle(),
            root,
            file,
            choices: vec![None; chunks.len()],
            chunks,
            workspace,
        }
    }

    /// How many conflicts there are, and how many are still to settle.
    pub fn conflicts(&self) -> (usize, usize) {
        let all: Vec<usize> = self.conflict_indices();
        let left = all.iter().filter(|&&i| self.choices[i].is_none()).count();
        (all.len(), left)
    }

    fn conflict_indices(&self) -> Vec<usize> {
        self.chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.kind == MergeKind::Conflict)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn choose(&mut self, chunk: usize, choice: Choice, cx: &mut Context<Self>) {
        if let Some(slot) = self.choices.get_mut(chunk) {
            *slot = Some(choice);
            cx.notify();
        }
    }

    /// Settles every conflict still open with one side.
    pub fn choose_all(&mut self, choice: Choice, cx: &mut Context<Self>) {
        for index in self.conflict_indices() {
            if self.choices[index].is_none() {
                self.choices[index] = Some(choice);
            }
        }
        cx.notify();
    }

    /// The result's lines for a chunk, if it's settled.
    fn result_of(&self, index: usize) -> Option<Vec<String>> {
        let chunk = &self.chunks[index];
        if let Some(merged) = chunk.merged() {
            return Some(merged.to_vec());
        }
        Some(match self.choices[index]? {
            Choice::Ours => chunk.ours.clone(),
            Choice::Theirs => chunk.theirs.clone(),
            Choice::OursThenTheirs => [chunk.ours.clone(), chunk.theirs.clone()].concat(),
            Choice::TheirsThenOurs => [chunk.theirs.clone(), chunk.ours.clone()].concat(),
        })
    }

    /// The merged text, once every conflict is settled.
    pub fn result(&self) -> Option<String> {
        (0..self.chunks.len())
            .map(|i| self.result_of(i).map(|lines| lines.concat()))
            .collect()
    }

    /// Saves the result, marks the file resolved, and closes.
    pub fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.result() else {
            return;
        };
        let result = git_ops::resolve_with(&self.root, &self.file, &text)
            .map(|_| format!("Merged {}", self.file.relative));
        self.workspace
            .update(cx, |workspace, cx| {
                match result {
                    Ok(message) => workspace.show_message(message, cx),
                    Err(error) => workspace.show_error(format!("{error:#}"), cx),
                }
                crate::commit_panel::refresh_git_state(workspace, cx);
                if let Some(panel) = workspace.panel::<crate::CommitPanel>(cx) {
                    panel.update(cx, |panel, cx| panel.refresh(cx));
                }
            })
            .ok();
        cx.emit(DismissEvent);
    }

    fn render_chunk(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let chunk = &self.chunks[index];
        let tint = |color: Hsla| color.opacity(0.14);
        let (left_tint, right_tint, middle_tint) = match chunk.kind {
            MergeKind::Same => (None, None, None),
            MergeKind::Ours => (
                Some(tint(theme.git_modified)),
                None,
                Some(tint(theme.git_modified)),
            ),
            MergeKind::Theirs => (
                None,
                Some(tint(theme.git_modified)),
                Some(tint(theme.git_modified)),
            ),
            MergeKind::Both => (
                Some(tint(theme.git_modified)),
                Some(tint(theme.git_modified)),
                Some(tint(theme.git_modified)),
            ),
            MergeKind::Conflict => {
                let settled = self.choices[index].is_some();
                (
                    Some(tint(theme.git_conflicted)),
                    Some(tint(theme.git_conflicted)),
                    Some(tint(if settled {
                        theme.git_added
                    } else {
                        theme.git_conflicted
                    })),
                )
            }
        };
        let lines = |lines: &[String], color: Option<Hsla>| {
            div()
                .flex_1()
                .min_w_0()
                .px(px(8.))
                .when_some(color, |d, c| d.bg(c))
                .children(lines.iter().map(|line| {
                    div()
                        .h(px(20.))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(line.trim_end_matches('\n').replace('\t', "    "))
                }))
        };
        let conflict = chunk.kind == MergeKind::Conflict;
        let result = self.result_of(index);
        let middle = match &result {
            Some(lines_) => lines(lines_, middle_tint),
            // Unsettled: the base, faint, until a side is chosen.
            None => lines(&chunk.base, middle_tint).text_color(theme.text_faint),
        };
        let arrow = |id: &'static str, label: &'static str, choice: Choice| {
            div()
                .id(SharedString::from(format!("{id}-{index}")))
                .debug_selector(move || format!("{id}-{index}"))
                .w(px(26.))
                .flex_none()
                .flex()
                .justify_center()
                .when(conflict, |d| {
                    d.cursor_pointer()
                        .text_color(theme.accent)
                        .font_weight(FontWeight::BOLD)
                        .hover(|s| s.bg(theme.hover))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| this.choose(index, choice, cx)))
                })
        };
        div()
            .flex()
            .flex_row()
            .border_b_1()
            .border_color(if chunk.kind == MergeKind::Same {
                gpui::transparent_black()
            } else {
                theme.border
            })
            .child(lines(&chunk.ours, left_tint))
            .child(arrow("take-ours", "»", Choice::Ours))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(middle)
                    .when(conflict, |d| {
                        let both =
                            |id: &'static str, label: &'static str, choice: Choice| {
                                div()
                                    .id(SharedString::from(format!("{id}-{index}")))
                                    .debug_selector(move || format!("{id}-{index}"))
                                    .px(px(6.))
                                    .text_size(px(11.))
                                    .text_color(theme.text_muted)
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(theme.accent))
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.choose(index, choice, cx)
                                    }))
                            };
                        d.child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(4.))
                                .child(both(
                                    "take-both",
                                    "Yours, then theirs",
                                    Choice::OursThenTheirs,
                                ))
                                .child(both(
                                    "take-both-reversed",
                                    "Theirs, then yours",
                                    Choice::TheirsThenOurs,
                                )),
                        )
                    }),
            )
            .child(arrow("take-theirs", "«", Choice::Theirs))
            .child(lines(&chunk.theirs, right_tint))
            .into_any_element()
    }
}

impl Render for MergeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let font = gleipnir_editor::editor_font(window, cx);
        let (total, left) = self.conflicts();
        let chunks: Vec<AnyElement> = (0..self.chunks.len())
            .map(|i| self.render_chunk(i, &theme, cx))
            .collect();
        let size = window.viewport_size();
        let button = |id: &'static str, label: String, primary: bool, enabled: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(10.))
                .py(px(3.))
                .rounded(px(5.))
                .border_1()
                .border_color(if primary { theme.accent } else { theme.border })
                .when(primary, |d| d.bg(theme.accent).text_color(theme.background))
                .when(!enabled, |d| d.opacity(0.5))
                .when(enabled, |d| d.cursor_pointer())
                .child(label)
        };
        div()
            .key_context("MergeView")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(size.width - px(60.))
            .h(size.height - px(120.))
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_color(theme.text)
            .on_action(cx.listener(|_, _: &CloseMerge, _, cx| cx.emit(DismissEvent)))
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
                    .gap(px(8.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Merge {}", self.file.relative)),
                    )
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child(match (total, left) {
                                (0, _) => "No conflicts".to_string(),
                                (_, 0) => "All conflicts settled".to_string(),
                                (_, 1) => "1 conflict left".to_string(),
                                (_, n) => format!("{n} conflicts left"),
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        button("merge-all-ours", "Accept Yours".into(), false, left > 0).on_click(
                            cx.listener(|this, _, _, cx| this.choose_all(Choice::Ours, cx)),
                        ),
                    )
                    .child(
                        button("merge-all-theirs", "Accept Theirs".into(), false, left > 0)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.choose_all(Choice::Theirs, cx)),
                            ),
                    )
                    .child(
                        button("merge-apply", "Apply".into(), true, left == 0).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if left == 0 {
                                    this.apply(cx)
                                }
                            }),
                        ),
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
                    .child(div().flex_1().px(px(14.)).py(px(4.)).child("Yours"))
                    .child(div().w(px(26.)))
                    .child(div().flex_1().px(px(14.)).py(px(4.)).child("Result"))
                    .child(div().w(px(26.)))
                    .child(div().flex_1().px(px(14.)).py(px(4.)).child("Theirs")),
            )
            .child(
                div()
                    .id("merge-chunks")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .font(font)
                    .text_size(px(theme.editor_font_size))
                    .children(chunks),
            )
    }
}
