//! IntelliJ's Interactively Rebase from Here: the commits from the one chosen up to the latest,
//! oldest first, each to pick, reword, squash, fix up or drop, in an order that can be changed,
//! with their messages to edit. Start Rebasing hands the plan to git.

use crate::rebase::{self, RebaseAction, RebaseEntry, RebaseRange};
use gleipnir_core::BufferModel;
use gleipnir_editor::EditorView;
use gleipnir_ui::Theme;
use gleipnir_workspace::{ModalView, Workspace};
use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, KeyBinding, SharedString, WeakEntity, Window, actions, div, prelude::*,
    px,
};
use std::path::{Path, PathBuf};

actions!(
    rebase,
    [
        CloseRebase,
        StartRebasing,
        SelectPrevious,
        SelectNext,
        MoveUp,
        MoveDown,
        PickCommit,
        RewordCommit,
        SquashCommit,
        FixupCommit,
        DropCommit,
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    let list = Some("RebaseList");
    cx.bind_keys([
        KeyBinding::new("escape", CloseRebase, Some("InteractiveRebase")),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("alt-up", MoveUp, list),
        KeyBinding::new("alt-down", MoveDown, list),
        KeyBinding::new("p", PickCommit, list),
        KeyBinding::new("r", RewordCommit, list),
        KeyBinding::new("s", SquashCommit, list),
        KeyBinding::new("f", FixupCommit, list),
        KeyBinding::new("d", DropCommit, list),
        KeyBinding::new("delete", DropCommit, list),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new(
        "cmd-enter",
        StartRebasing,
        Some("InteractiveRebase"),
    )]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new(
        "ctrl-enter",
        StartRebasing,
        Some("InteractiveRebase"),
    )]);
}

/// Opens the window for rebasing from a commit, or says why it can't be.
pub(crate) fn open_rebase(
    workspace: &mut Workspace,
    hash: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        return;
    };
    match rebase::rebase_entries(&root, hash) {
        Ok((entries, range)) => {
            let handle = cx.weak_entity();
            workspace.toggle_modal(window, cx, move |window, cx| {
                RebaseView::new(range, entries, handle, window, cx)
            });
        }
        Err(error) => workspace.show_error(format!("{error:#}"), cx),
    }
}

pub struct RebaseView {
    focus_handle: FocusHandle,
    range: RebaseRange,
    entries: Vec<RebaseEntry>,
    selected: usize,
    message: Entity<EditorView>,
    /// The entry whose message is in the message box.
    editing: Option<usize>,
    workspace: WeakEntity<Workspace>,
}

impl EventEmitter<DismissEvent> for RebaseView {}
impl ModalView for RebaseView {}

impl Focusable for RebaseView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RebaseView {
    pub fn new(
        range: RebaseRange,
        entries: Vec<RebaseEntry>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| BufferModel::new("", None, cx));
        let message = cx.new(|cx| {
            let mut editor = EditorView::new(buffer, window, cx);
            editor.set_plain(true, cx);
            editor
        });
        Self {
            focus_handle: cx.focus_handle(),
            range,
            entries,
            selected: 0,
            message,
            editing: None,
            workspace,
        }
    }

    pub fn entries(&self) -> &[RebaseEntry] {
        &self.entries
    }

    /// Each entry as `action short subject`, for tests and the driver.
    pub fn rows(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| format!("{} {} {}", e.action.name(), e.short, e.subject))
            .collect()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.entries.len() {
            return;
        }
        self.keep_message(cx);
        self.selected = index;
        self.load_message(cx);
        cx.notify();
    }

    pub fn set_action(&mut self, action: RebaseAction, cx: &mut Context<Self>) {
        self.keep_message(cx);
        if let Some(entry) = self.entries.get_mut(self.selected) {
            entry.action = action;
        }
        self.load_message(cx);
        cx.notify();
    }

    /// Moves the chosen commit up, earlier, or down, later.
    pub fn move_selected(&mut self, up: bool, cx: &mut Context<Self>) {
        let from = self.selected;
        let to = if up {
            from.checked_sub(1)
        } else {
            Some(from + 1).filter(|&t| t < self.entries.len())
        };
        let Some(to) = to else { return };
        self.keep_message(cx);
        self.entries.swap(from, to);
        self.selected = to;
        self.load_message(cx);
        cx.notify();
    }

    /// The message box's text, for a reword or squash.
    pub fn set_message(&mut self, text: &str, cx: &mut Context<Self>) {
        let buffer = self.message.read(cx).buffer().clone();
        buffer.update(cx, |buffer, cx| {
            let len = buffer.buffer().len();
            buffer.apply_external_edits(vec![gleipnir_text::TextEdit::new(0..len, text)], cx);
        });
    }

    pub fn message_text(&self, cx: &App) -> String {
        self.message.read(cx).buffer().read(cx).buffer().text()
    }

    /// Whether the message box is showing, for the chosen commit.
    pub fn is_editing_message(&self) -> bool {
        self.editing.is_some()
    }

    /// Keeps what's in the message box with the commit it's for.
    fn keep_message(&mut self, cx: &App) {
        let Some(index) = self.editing else {
            return;
        };
        let text = self.message_text(cx);
        if let Some(entry) = self.entries.get_mut(index) {
            entry.new_message = (text.trim_end() != entry.message.trim_end()).then_some(text);
        }
    }

    /// Shows the chosen commit's message in the box, if its action takes one.
    fn load_message(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        if entry.action.takes_message() {
            let text = entry.message().to_string();
            self.editing = Some(self.selected);
            self.set_message(&text, cx);
        } else {
            self.editing = None;
        }
    }

    /// What's wrong with the plan, if anything.
    fn problem(&self) -> Option<String> {
        rebase::plan(&self.entries, |_| PathBuf::new())
            .err()
            .map(|e| format!("{e:#}"))
    }

    /// Hands the plan to git, in the background, and closes.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.keep_message(cx);
        if self.problem().is_some() {
            cx.notify();
            return;
        }
        let (range, entries) = (self.range.clone(), self.entries.clone());
        self.workspace
            .update(cx, |workspace, cx| {
                crate::run_git_task(
                    workspace,
                    "Rebasing…",
                    move |root| rebase::start(root, &range, &entries),
                    cx,
                )
            })
            .ok();
        cx.emit(DismissEvent);
    }

    fn render_row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let entry = &self.entries[index];
        let selected = self.selected == index;
        let color = match entry.action {
            RebaseAction::Pick => theme.text_muted,
            RebaseAction::Reword => theme.accent,
            RebaseAction::Squash | RebaseAction::Fixup => theme.warning,
            RebaseAction::Drop => theme.git_deleted,
        };
        let folded = matches!(entry.action, RebaseAction::Squash | RebaseAction::Fixup);
        let subject = match &entry.new_message {
            Some(message) if entry.action.takes_message() => {
                message.lines().next().unwrap_or("").to_string()
            }
            _ => entry.subject.clone(),
        };
        div()
            .id(("rebase-row", index))
            .debug_selector(move || format!("rebase-row-{index}"))
            .h(px(24.))
            .px(px(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .when(selected, |d| d.bg(theme.selection))
            .when(!selected, |d| d.hover(|s| s.bg(theme.hover)))
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(entry.action.name()),
            )
            .child(
                div()
                    .w(px(64.))
                    .flex_none()
                    .text_color(theme.text_faint)
                    .child(entry.short.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    // Folded into the commit above, so shown under it.
                    .when(folded, |d| d.pl(px(16.)))
                    .when(entry.action == RebaseAction::Drop, |d| {
                        d.text_color(theme.text_faint).line_through()
                    })
                    .child(subject),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus_handle, cx);
                this.select(index, cx)
            }))
            .into_any_element()
    }
}

impl Render for RebaseView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let rows: Vec<AnyElement> = (0..self.entries.len())
            .map(|i| self.render_row(i, &theme, cx))
            .collect();
        let problem = self.problem();
        let size = window.viewport_size();
        let button = |id: &'static str, label: &'static str, primary: bool, enabled: bool| {
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
        let tool = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(7.))
                .py(px(2.))
                .rounded(px(4.))
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.bg(theme.hover))
                .child(label)
        };
        let action_tool = |id: &'static str, label: &'static str, action: RebaseAction| {
            tool(id, label).on_click(cx.listener(move |this, _, _, cx| this.set_action(action, cx)))
        };
        let editing = self
            .editing
            .and_then(|i| self.entries.get(i))
            .map(|e| match e.action {
                RebaseAction::Squash => format!("What {} adds to the message", e.short),
                _ => format!("New message for {}", e.short),
            });
        let count = self.entries.len();
        let can_start = problem.is_none();
        div()
            .key_context("InteractiveRebase")
            .occlude()
            .w((size.width - px(80.)).min(px(820.)))
            .h((size.height - px(120.)).min(px(600.)))
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_color(theme.text)
            .text_size(px(12.5))
            .on_action(cx.listener(|_, _: &CloseRebase, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &StartRebasing, _, cx| this.start(cx)))
            .child(
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(13.))
                            .child(format!(
                                "Rebase {count} commit{}",
                                if count == 1 { "" } else { "s" }
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(theme.text_faint)
                            .child("P pick · R reword · S squash · F fixup · D drop · ⌥↑ ⌥↓ move"),
                    ),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.))
                    .child(action_tool("rebase-pick", "Pick", RebaseAction::Pick))
                    .child(action_tool("rebase-reword", "Reword", RebaseAction::Reword))
                    .child(action_tool("rebase-squash", "Squash", RebaseAction::Squash))
                    .child(action_tool("rebase-fixup", "Fixup", RebaseAction::Fixup))
                    .child(action_tool("rebase-drop", "Drop", RebaseAction::Drop))
                    .child(div().w(px(12.)))
                    .child(
                        tool("rebase-up", "Move Up")
                            .on_click(cx.listener(|this, _, _, cx| this.move_selected(true, cx))),
                    )
                    .child(
                        tool("rebase-down", "Move Down")
                            .on_click(cx.listener(|this, _, _, cx| this.move_selected(false, cx))),
                    ),
            )
            .child(
                div()
                    .id("rebase-list")
                    .key_context("RebaseList")
                    .track_focus(&self.focus_handle)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py(px(4.))
                    .bg(theme.editor_background)
                    .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                        let index = this.selected.saturating_sub(1);
                        this.select(index, cx)
                    }))
                    .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                        let index = this.selected + 1;
                        this.select(index, cx)
                    }))
                    .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.move_selected(true, cx)))
                    .on_action(
                        cx.listener(|this, _: &MoveDown, _, cx| this.move_selected(false, cx)),
                    )
                    .on_action(cx.listener(|this, _: &PickCommit, _, cx| {
                        this.set_action(RebaseAction::Pick, cx)
                    }))
                    .on_action(cx.listener(|this, _: &RewordCommit, _, cx| {
                        this.set_action(RebaseAction::Reword, cx)
                    }))
                    .on_action(cx.listener(|this, _: &SquashCommit, _, cx| {
                        this.set_action(RebaseAction::Squash, cx)
                    }))
                    .on_action(cx.listener(|this, _: &FixupCommit, _, cx| {
                        this.set_action(RebaseAction::Fixup, cx)
                    }))
                    .on_action(cx.listener(|this, _: &DropCommit, _, cx| {
                        this.set_action(RebaseAction::Drop, cx)
                    }))
                    .children(rows),
            )
            .child(
                div()
                    .h(px(150.))
                    .flex_none()
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().text_size(px(12.)).text_color(theme.text_muted).child(
                        SharedString::from(editing.clone().unwrap_or_else(|| {
                            "Reword or squash a commit to edit its message".to_string()
                        })),
                    ))
                    .when(editing.is_some(), |d| {
                        d.child(
                            div()
                                .flex_1()
                                .rounded(px(5.))
                                .border_1()
                                .border_color(theme.border)
                                .overflow_hidden()
                                .child(self.message.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.error)
                            .children(problem),
                    )
                    .child(
                        button("rebase-cancel", "Cancel", false, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        button("rebase-start", "Start Rebasing", true, can_start).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if can_start {
                                    this.start(cx)
                                }
                            }),
                        ),
                    ),
            )
    }
}
