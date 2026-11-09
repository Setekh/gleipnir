//! A small popup asking for a name, like a new branch's: Enter takes it, Escape or a click
//! elsewhere doesn't.

use crate::{ModalView, Workspace};
use gleipnir_ui::{TextInput, Theme};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, SharedString, WeakEntity, Window, actions, div, prelude::*, px,
};

actions!(name_prompt, [ConfirmName, CancelName]);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", ConfirmName, Some("NamePrompt")),
        KeyBinding::new("escape", CancelName, Some("NamePrompt")),
    ]);
}

type OnName = Box<dyn FnOnce(String, &mut Workspace, &mut Context<Workspace>)>;

pub struct NamePrompt {
    title: SharedString,
    input: Entity<TextInput>,
    workspace: WeakEntity<Workspace>,
    on_name: Option<OnName>,
}

impl EventEmitter<DismissEvent> for NamePrompt {}
impl ModalView for NamePrompt {}

impl Focusable for NamePrompt {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Workspace {
    /// Asks for a name, then hands it, trimmed and not empty, to `on_name`.
    pub fn ask_for_name(
        &mut self,
        title: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        on_name: impl FnOnce(String, &mut Workspace, &mut Context<Workspace>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, placeholder) = (title.into(), placeholder.into());
        let workspace = cx.weak_entity();
        self.toggle_modal(window, cx, move |_, cx| NamePrompt {
            title,
            input: cx.new(|cx| TextInput::new(placeholder, cx)),
            workspace,
            on_name: Some(Box::new(on_name)),
        });
    }
}

impl NamePrompt {
    pub fn set_name(&mut self, name: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(name, cx));
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let name = self.input.read(cx).text().trim().to_string();
        cx.emit(DismissEvent);
        let Some(on_name) = self.on_name.take() else {
            return;
        };
        if name.is_empty() {
            return;
        }
        self.workspace
            .update(cx, |workspace, cx| on_name(name, workspace, cx))
            .ok();
    }
}

impl Render for NamePrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        div()
            .key_context("NamePrompt")
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
            .on_action(cx.listener(|this, _: &ConfirmName, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &CancelName, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(self.input.clone()),
            )
    }
}
