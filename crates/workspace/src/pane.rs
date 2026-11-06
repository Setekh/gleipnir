//! One editor pane: a row of tabs and the active editor below them.

use crate::workspace::{CloseTabAt, Workspace};
use gleipnir_core::DiskStatus;
use gleipnir_editor::EditorView;
use gleipnir_ui::{FileIcon, Icon, Theme, file_icon};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, SharedString,
    Subscription, WeakEntity, Window, div, prelude::*, px, svg,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub enum PaneEvent {
    Focused,
    /// The active tab or the set of tabs changed.
    ItemsChanged,
}

impl EventEmitter<PaneEvent> for Pane {}

/// A tab being dragged: where it came from, and what it's called for the preview.
#[derive(Clone)]
pub struct DraggedTab {
    pub pane: Entity<Pane>,
    pub index: usize,
    pub title: SharedString,
}

/// What follows the pointer while a tab is dragged.
struct TabPreview(SharedString);

impl Render for TabPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        div()
            .px(px(10.))
            .py(px(4.))
            .rounded(px(4.))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.accent)
            .text_color(theme.text)
            .text_size(px(theme.ui_font_size))
            .child(self.0.clone())
    }
}

pub struct Pane {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    items: Vec<Entity<EditorView>>,
    active: usize,
    /// While restoring a layout, the saved position of each item.
    restored_order: Vec<usize>,
    item_subscriptions: HashMap<gpui::EntityId, Vec<Subscription>>,
    _focus_subscription: Subscription,
}

impl Pane {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscription = cx.on_focus_in(&focus_handle, window, |_, _, cx| {
            cx.emit(PaneEvent::Focused)
        });
        Self {
            focus_handle,
            workspace,
            items: Vec::new(),
            active: 0,
            restored_order: Vec::new(),
            item_subscriptions: HashMap::new(),
            _focus_subscription: focus_subscription,
        }
    }

    pub fn items(&self) -> &[Entity<EditorView>] {
        &self.items
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_item(&self) -> Option<&Entity<EditorView>> {
        self.items.get(self.active)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds a tab after the active one, and activates it.
    pub(crate) fn add_item(
        &mut self,
        item: Entity<EditorView>,
        subscriptions: Vec<Subscription>,
        cx: &mut Context<Self>,
    ) {
        let buffer = item.read(cx).buffer().clone();
        let mut subscriptions = subscriptions;
        // Tab titles show the name and whether it's dirty, which change with the buffer. Cursor
        // movement doesn't matter here, as that's only in the status bar.
        subscriptions.push(cx.observe(&buffer, |_, _, cx| cx.notify()));
        self.item_subscriptions
            .insert(item.entity_id(), subscriptions);

        let index = if self.items.is_empty() {
            0
        } else {
            self.active + 1
        };
        self.items.insert(index, item);
        self.active = index;
        cx.emit(PaneEvent::ItemsChanged);
        cx.notify();
    }

    /// Adds a tab while restoring a layout. Files finish loading in any order, so this slots each
    /// into the position it was saved at, relative to those that have arrived.
    pub(crate) fn insert_restored(
        &mut self,
        saved_index: usize,
        item: Entity<EditorView>,
        subscriptions: Vec<Subscription>,
        saved_active: usize,
        cx: &mut Context<Self>,
    ) {
        let buffer = item.read(cx).buffer().clone();
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.observe(&buffer, |_, _, cx| cx.notify()));
        self.item_subscriptions
            .insert(item.entity_id(), subscriptions);

        let position = self
            .restored_order
            .partition_point(|index| *index < saved_index);
        self.restored_order.insert(position, saved_index);
        self.items.insert(position, item);
        // The saved-active tab, once it's here. Until then, whichever is nearest.
        self.active = self
            .restored_order
            .iter()
            .position(|index| *index == saved_active)
            .unwrap_or_else(|| self.active.min(self.items.len() - 1));
        cx.emit(PaneEvent::ItemsChanged);
        cx.notify();
    }

    /// Removes a tab, returning whether it was there. The next tab along becomes active, as in most
    /// editors, so closing several in a row walks steadily through them.
    pub(crate) fn remove_item(
        &mut self,
        item: &Entity<EditorView>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.items.iter().position(|i| i == item) else {
            return false;
        };
        self.items.remove(index);
        self.item_subscriptions.remove(&item.entity_id());
        if index < self.active || self.active >= self.items.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.emit(PaneEvent::ItemsChanged);
        cx.notify();
        true
    }

    /// Takes a tab out with its subscriptions, to put it somewhere else.
    pub(crate) fn take_item(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Option<(Entity<EditorView>, Vec<Subscription>)> {
        if index >= self.items.len() {
            return None;
        }
        let item = self.items.remove(index);
        let subscriptions = self
            .item_subscriptions
            .remove(&item.entity_id())
            .unwrap_or_default();
        if index < self.active || self.active >= self.items.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.emit(PaneEvent::ItemsChanged);
        cx.notify();
        Some((item, subscriptions))
    }

    /// Puts a tab in at a position, keeping the subscriptions it came with, and activates it.
    pub(crate) fn insert_item_at(
        &mut self,
        index: usize,
        item: Entity<EditorView>,
        subscriptions: Vec<Subscription>,
        cx: &mut Context<Self>,
    ) {
        let index = index.min(self.items.len());
        self.item_subscriptions
            .insert(item.entity_id(), subscriptions);
        self.items.insert(index, item);
        self.active = index;
        cx.emit(PaneEvent::ItemsChanged);
        cx.notify();
    }

    /// Drops a dragged tab at a position here, after this frame's event handling, since moving
    /// it updates panes that may be in the middle of handling the drop.
    fn drop_tab(
        &self,
        dragged: &DraggedTab,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (workspace, to, dragged) = (self.workspace.clone(), cx.entity(), dragged.clone());
        window.defer(cx, move |window, cx| {
            if let Some(workspace) = workspace.upgrade() {
                workspace.update(cx, |workspace, cx| {
                    workspace.move_tab(&dragged.pane, dragged.index, &to, index, window, cx)
                });
            }
        });
    }

    pub(crate) fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.items.len() && index != self.active {
            self.active = index;
            cx.emit(PaneEvent::ItemsChanged);
            cx.notify();
        }
    }

    pub(crate) fn activate_relative(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.items.len() > 1 {
            let count = self.items.len();
            let next = if forward {
                self.active + 1
            } else {
                self.active + count - 1
            };
            self.activate(next % count, cx);
        }
    }

    /// Where keyboard focus belongs: the active tab, or the pane itself if it has none.
    pub(crate) fn focus_target(&self, cx: &App) -> FocusHandle {
        match self.active_item() {
            Some(item) => item.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }

    /// The tab's text: the file name, with the folder added if another open tab has the same name.
    pub(crate) fn tab_title(&self, path: Option<&Path>, cx: &App) -> SharedString {
        let Some(path) = path else {
            return "untitled".into();
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let ambiguous = self
            .items
            .iter()
            .filter_map(|item| item.read(cx).buffer().read(cx).path().map(PathBuf::from))
            .any(|other| other != path && other.file_name() == path.file_name());
        match path
            .parent()
            .and_then(Path::file_name)
            .filter(|_| ambiguous)
        {
            Some(parent) => format!("{name} — {}", parent.to_string_lossy()).into(),
            None => name.into(),
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let workspace = self.workspace.clone();
        let pane = cx.entity();
        let accent = theme.accent;
        let tabs = self.items.iter().enumerate().map(|(index, item)| {
            let view = item.read(cx);
            let model = view.buffer().read(cx);
            let path = model.path().map(PathBuf::from);
            let dirty = model.buffer().is_dirty();
            let missing = model.disk_status() == DiskStatus::Deleted;
            let conflicted = model.disk_status() == DiskStatus::Conflict;
            let title = self.tab_title(path.as_deref(), cx);
            let active = index == self.active;
            let close_workspace = workspace.clone();
            let middle_workspace = workspace.clone();
            let (close_pane, middle_pane) = (pane.clone(), pane.clone());

            let icon = match path.as_deref().map(file_icon) {
                Some(FileIcon::Badge(text, color)) => div()
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(color)
                    .child(text)
                    .into_any_element(),
                _ => svg()
                    .path(Icon::File.path())
                    .size(px(13.))
                    .text_color(theme.text_muted)
                    .into_any_element(),
            };
            let text_color = if missing {
                theme.git_deleted
            } else if conflicted {
                theme.warning
            } else if active {
                theme.text
            } else {
                theme.text_muted
            };

            div()
                .id(("tab", index))
                .debug_selector(|| format!("tab-{index}"))
                .group("tab")
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h_full()
                .pl(px(10.))
                .pr(px(6.))
                .border_r_1()
                .border_color(theme.border)
                .relative()
                .when(active, |tab| {
                    // The thread: a line of the accent color marking where you are.
                    tab.bg(theme.background).child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(2.))
                            .bg(theme.accent),
                    )
                })
                .when(!active, |tab| tab.hover(|t| t.bg(theme.hover)))
                .text_color(text_color)
                .on_drag(
                    DraggedTab {
                        pane: pane.clone(),
                        index,
                        title: title.clone(),
                    },
                    |dragged, _, _, cx| cx.new(|_| TabPreview(dragged.title.clone())),
                )
                .drag_over::<DraggedTab>(move |style, _, _, _| {
                    style.border_l_2().border_color(accent)
                })
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                    this.drop_tab(dragged, index, window, cx)
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate(index, cx);
                    let target = this.focus_target(cx);
                    window.focus(&target, cx);
                }))
                .on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                    if let Some(workspace) = middle_workspace.upgrade() {
                        let pane = middle_pane.clone();
                        workspace.update(cx, |workspace, cx| {
                            workspace.close_tab_at(CloseTabAt { pane, index }, window, cx);
                        });
                    }
                })
                .child(icon)
                .child(title)
                .child(
                    div()
                        .id(("tab-close", index))
                        .debug_selector(|| format!("tab-close-{index}"))
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .hover(|c| c.bg(theme.hover))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            if let Some(workspace) = close_workspace.upgrade() {
                                let pane = close_pane.clone();
                                workspace.update(cx, |workspace, cx| {
                                    workspace.close_tab_at(CloseTabAt { pane, index }, window, cx);
                                });
                            }
                        })
                        .child(if dirty {
                            // The close button doubles as the unsaved mark, to save space.
                            div()
                                .size(px(7.))
                                .rounded(px(4.))
                                .bg(theme.text_muted)
                                .into_any_element()
                        } else {
                            svg()
                                .path(Icon::Close.path())
                                .size(px(12.))
                                .text_color(theme.text_faint)
                                .into_any_element()
                        }),
                )
        });
        let tabs: Vec<_> = tabs.collect();
        let count = self.items.len();
        // The empty space after the tabs takes a dropped tab at the end.
        let after = div()
            .id("tab-bar-end")
            .flex_1()
            .h_full()
            .min_w(px(40.))
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(theme.hover))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                this.drop_tab(dragged, count, window, cx)
            }));

        div()
            .id("tab-bar")
            .flex()
            .flex_row()
            .flex_none()
            .h(px(32.))
            .w_full()
            .overflow_x_scroll()
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .child(after)
    }
}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty_hint = self.items.is_empty().then(|| {
            self.workspace
                .upgrade()
                .map(|workspace| workspace.read(cx).render_empty_hint(window, cx))
        });

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .when(!self.items.is_empty(), |pane| {
                pane.child(self.render_tab_bar(cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    // A tab dropped on another pane's editor moves into that pane.
                    .on_drop(cx.listener(|this, dragged: &DraggedTab, window, cx| {
                        let end = this.items.len();
                        this.drop_tab(dragged, end, window, cx)
                    }))
                    .children(self.active_item().cloned())
                    .children(empty_hint.flatten()),
            )
    }
}
