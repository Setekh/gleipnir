//! Shift+F6: renames the symbol under the cursor everywhere it's used.

use crate::apply_workspace_edit;
use gleipnir_core::BufferModel;
use gleipnir_ui::Theme;
use gleipnir_ui::text_input::TextInput;
use gleipnir_workspace::{ModalView, Workspace};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, WeakEntity, Window, actions, div, prelude::*, px,
};

actions!(rename, [Confirm, Cancel]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, Some("RenameModal")),
        KeyBinding::new("escape", Cancel, Some("RenameModal")),
    ]);
}

pub struct RenameModal {
    input: Entity<TextInput>,
    old_name: String,
    buffer: Entity<BufferModel>,
    offset: usize,
    workspace: WeakEntity<Workspace>,
}

impl EventEmitter<DismissEvent> for RenameModal {}
impl ModalView for RenameModal {}

impl Focusable for RenameModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

pub fn start(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(editor) = workspace.active_editor(cx) else {
        return;
    };
    let view = editor.read(cx);
    let buffer = view.buffer().clone();
    let offset = view.selections().primary().head;
    let Some(old_name) = ({
        let text = buffer.read(cx).buffer();
        text.word_at(offset).map(|r| text.slice(r))
    }) else {
        workspace.show_message("Put the cursor on a name to rename it", cx);
        return;
    };
    let weak = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |_, cx| {
        let input = cx.new(|cx| {
            let mut input = TextInput::new("New name", cx);
            input.set_text(&old_name, cx);
            input
        });
        RenameModal {
            input,
            old_name,
            buffer,
            offset,
            workspace: weak,
        }
    });
}

impl RenameModal {
    pub fn set_name(&mut self, name: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(name, cx));
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let new_name = self.input.read(cx).text().trim().to_string();
        cx.emit(DismissEvent);
        if new_name.is_empty() || new_name == self.old_name {
            return;
        }
        let (buffer, offset, old_name) = (self.buffer.clone(), self.offset, self.old_name.clone());
        self.workspace
            .update(cx, |workspace, cx| {
                let request = workspace
                    .language_servers()
                    .read(cx)
                    .rename(&buffer, offset, &new_name, cx);
                cx.spawn(async move |this, cx| {
                    let result: anyhow::Result<usize> = async {
                        let edit = request.await?;
                        this.update(cx, |w, cx| apply_workspace_edit(w, edit, cx))?
                            .await
                    }
                    .await;
                    this.update(cx, |w, cx| match result {
                        Ok(0) => w.show_message(format!("Nothing to rename for {old_name}"), cx),
                        Ok(files) => w.show_message(
                            format!(
                                "Renamed {old_name} to {new_name} in {files} file{}",
                                if files == 1 { "" } else { "s" }
                            ),
                            cx,
                        ),
                        Err(error) => {
                            w.show_error(format!("Can't rename {old_name}: {error:#}"), cx)
                        }
                    })
                    .ok();
                })
                .detach();
            })
            .ok();
    }
}

impl Render for RenameModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        div()
            .key_context("RenameModal")
            .occlude()
            .w(px(420.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(10.))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .text_size(px(theme.ui_font_size))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .text_color(theme.text_muted)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Rename {} and its usages to:", self.old_name)),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .border_1()
                    .border_color(theme.accent)
                    .rounded(px(4.))
                    .child(self.input.clone()),
            )
    }
}
