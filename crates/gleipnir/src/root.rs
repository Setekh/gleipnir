//! The window contents. For now one editor and a status line; the workspace replaces this in M2.

use crate::{NewFile, OpenFile, SaveAs};
use gleipnir_core::{BufferModel, BufferStore, DiskStatus};
use gleipnir_editor::{EditorEvent, EditorView};
use gleipnir_ui::Theme;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement as _,
    PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window, div, prelude::*,
    px,
};
use std::path::PathBuf;

pub struct Root {
    store: Entity<BufferStore>,
    editor: Option<Entity<EditorView>>,
    /// The last failure, such as a file that wouldn't open or save, until something succeeds.
    message: Option<SharedString>,
    focus_handle: FocusHandle,
    _editor_subscriptions: Vec<Subscription>,
}

impl Root {
    pub fn new(
        store: Entity<BufferStore>,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            store,
            editor: None,
            message: None,
            focus_handle: cx.focus_handle(),
            _editor_subscriptions: Vec::new(),
        };
        match path {
            Some(path) => this.open(path, window, cx),
            None => this.new_file(window, cx),
        }
        this
    }

    pub fn editor(&self) -> Option<&Entity<EditorView>> {
        self.editor.as_ref()
    }

    pub fn message(&self) -> Option<&SharedString> {
        self.message.as_ref()
    }

    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.store.update(cx, |store, cx| store.open(&path, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = open.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(buffer) => this.show(buffer, window, cx),
                Err(error) => {
                    this.message = Some(format!("{error:#}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = cx.new(|cx| BufferModel::new("", None, cx));
        self.show(buffer, window, cx);
    }

    fn show(&mut self, buffer: Entity<BufferModel>, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| EditorView::new(buffer.clone(), window, cx));
        editor.read(cx).focus_handle(cx).focus(window, cx);
        self._editor_subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
                EditorEvent::SaveAsRequested => this.save_as(window, cx),
            }),
            cx.observe(&buffer, |_, _, cx| cx.notify()),
        ];
        self.editor = Some(editor);
        self.message = None;
        cx.notify();
    }

    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.editor.as_ref().map(|e| e.read(cx).buffer().clone()) else {
            return;
        };
        let directory = buffer
            .read(cx)
            .path()
            .and_then(|p| p.parent().map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let prompt = cx.prompt_for_new_path(&directory, None);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let save = this.update(cx, |_, cx| buffer.update(cx, |b, cx| b.save_as(path, cx)));
            let Ok(save) = save else { return };
            if let Err(error) = save.await {
                this.update(cx, |this, cx| {
                    this.message = Some(format!("{error:#}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn on_open(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update_in(cx, |this, window, cx| this.open(path, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn status_text(&self, cx: &Context<Self>) -> (String, String) {
        let Some(editor) = &self.editor else {
            return (String::new(), String::new());
        };
        let view = editor.read(cx);
        let model = view.buffer().read(cx);
        let buffer = model.buffer();
        let name = model
            .path()
            .map_or_else(|| "untitled".to_string(), |p| p.display().to_string());
        let dirty = if buffer.is_dirty() { " •" } else { "" };
        let status = match model.disk_status() {
            DiskStatus::InSync => "",
            DiskStatus::Conflict => " (changed on disk)",
            DiskStatus::Deleted => " (deleted on disk)",
        };

        let primary = view.selections().primary();
        let point = buffer.offset_to_point(primary.head);
        let cursors = match view.selections().len() {
            1 => String::new(),
            n => format!("{n} cursors   "),
        };
        let language = model.language().map_or("Plain Text", |l| l.name());
        let ending = if buffer.is_crlf() { "CRLF" } else { "LF" };
        let config = model.edit_config();
        let indent = if config.hard_tabs {
            "Tab".to_string()
        } else {
            format!("{} spaces", config.tab_size)
        };
        (
            format!("{name}{dirty}{status}"),
            format!(
                "{cursors}{}:{}   {language}   {ending}   UTF-8   {indent}",
                point.line + 1,
                point.col + 1
            ),
        )
    }
}

impl Focusable for Root {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        // With nothing focused, like after a failed open, key bindings still need a path to here.
        if window.focused(cx).is_none() {
            self.focus_handle.focus(window, cx);
        }
        let (left, right) = self.status_text(cx);
        let left: SharedString = match &self.message {
            Some(message) => message.clone(),
            None => left.into(),
        };
        let left_color = if self.message.is_some() {
            theme.error
        } else {
            theme.text_muted
        };

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(theme.ui_font)
            .text_size(px(theme.ui_font_size))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| this.new_file(window, cx)))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| this.save_as(window, cx)))
            .child(div().flex_1().min_h_0().children(self.editor.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .h(px(24.))
                    .px(px(12.))
                    .flex_none()
                    .bg(theme.surface)
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(12.))
                    .child(div().text_color(left_color).child(left))
                    .child(div().text_color(theme.text_muted).child(right)),
            )
    }
}
