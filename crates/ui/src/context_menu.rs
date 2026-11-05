//! A popup list of commands, for right-click menus.

use crate::theme::Theme;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, SharedString,
    Window, actions, div, prelude::*, px,
};
use std::rc::Rc;

actions!(context_menu, [Up, Down, Confirm, Cancel]);

pub fn bind_keys(cx: &mut App) {
    let context = Some("ContextMenu");
    cx.bind_keys([
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Cancel, context),
    ]);
}

/// Emitted when the menu should go away, whether or not something was chosen.
pub struct MenuDismissed;

impl EventEmitter<MenuDismissed> for ContextMenu {}

type MenuHandler = Rc<dyn Fn(&mut Window, &mut App)>;

struct MenuItem {
    label: SharedString,
    hint: Option<SharedString>,
    handler: MenuHandler,
}

enum MenuEntry {
    Item(MenuItem),
    Separator,
}

pub struct ContextMenu {
    focus_handle: FocusHandle,
    entries: Vec<MenuEntry>,
    highlighted: Option<usize>,
}

impl ContextMenu {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            entries: Vec::new(),
            highlighted: None,
        }
    }

    pub fn item(
        mut self,
        label: impl Into<SharedString>,
        hint: Option<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.entries.push(MenuEntry::Item(MenuItem {
            label: label.into(),
            hint,
            handler: Rc::new(handler),
        }));
        self
    }

    pub fn separator(mut self) -> Self {
        self.entries.push(MenuEntry::Separator);
        self
    }

    /// Moves the highlight, wrapping, and skipping separators.
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let items: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, MenuEntry::Item(_)))
            .map(|(i, _)| i)
            .collect();
        if items.is_empty() {
            return;
        }
        let position = self
            .highlighted
            .and_then(|h| items.iter().position(|i| *i == h));
        let next = match (position, forward) {
            (None, true) => 0,
            (None, false) => items.len() - 1,
            (Some(p), true) => (p + 1) % items.len(),
            (Some(p), false) => (p + items.len() - 1) % items.len(),
        };
        self.highlighted = Some(items[next]);
        cx.notify();
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(MenuEntry::Item(item)) = self.entries.get(index) else {
            return;
        };
        let handler = item.handler.clone();
        // The handler runs after the dismissal has been handled. Owners usually refocus themselves
        // on dismissal, and a handler that focuses something else, like a rename field, must win.
        cx.emit(MenuDismissed);
        window.defer(cx, move |window, cx| handler(window, cx));
    }
}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ContextMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let highlighted = self.highlighted;
        let rows = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                MenuEntry::Separator => div()
                    .h(px(1.))
                    .my(px(4.))
                    .bg(theme.border)
                    .into_any_element(),
                MenuEntry::Item(item) => div()
                    .id(index)
                    .flex()
                    .flex_row()
                    .justify_between()
                    .gap(px(24.))
                    .h(px(24.))
                    .px(px(10.))
                    .items_center()
                    .rounded(px(4.))
                    .when(highlighted == Some(index), |row| row.bg(theme.selection))
                    .hover(|row| row.bg(theme.selection))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate(index, window, cx);
                    }))
                    .child(item.label.clone())
                    .children(item.hint.clone().map(|hint| {
                        div()
                            .text_color(theme.text_muted)
                            .text_size(px(11.))
                            .child(hint)
                    }))
                    .into_any_element(),
            });
        let rows: Vec<_> = rows.collect();

        div()
            .key_context("ContextMenu")
            .track_focus(&self.focus_handle)
            .occlude()
            .min_w(px(190.))
            .p(px(4.))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.))
            .shadow_lg()
            .text_color(theme.text)
            .text_size(px(theme.ui_font_size))
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                if let Some(index) = this.highlighted {
                    this.activate(index, window, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(MenuDismissed)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(MenuDismissed)))
            // A press inside shouldn't reach whatever's underneath.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use std::cell::RefCell;

    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Entity<ContextMenu>,
        Rc<RefCell<Vec<&'static str>>>,
        &mut VisualTestContext,
    ) {
        cx.update(|cx| {
            crate::init_theme(cx);
            bind_keys(cx);
        });
        let log = Rc::new(RefCell::new(Vec::new()));
        let (a, b) = (log.clone(), log.clone());
        let (menu, cx) = cx.add_window_view(move |_, cx| {
            ContextMenu::new(cx)
                .item("First", None, move |_, _| a.borrow_mut().push("first"))
                .separator()
                .item("Second", Some("cmd-2".into()), move |_, _| {
                    b.borrow_mut().push("second")
                })
        });
        let closed = log.clone();
        cx.update(|window, cx| {
            let handle = menu.read(cx).focus_handle.clone();
            handle.focus(window, cx);
            cx.subscribe(&menu, move |_, _: &MenuDismissed, _| {
                closed.borrow_mut().push("dismissed");
            })
            .detach();
        });
        (menu, log, cx)
    }

    #[gpui::test]
    fn arrows_skip_separators_and_enter_runs_the_item(cx: &mut TestAppContext) {
        let (_menu, log, cx) = setup(cx);
        cx.simulate_keystrokes("down down enter");
        assert_eq!(*log.borrow(), ["dismissed", "second"]);
    }

    #[gpui::test]
    fn the_highlight_wraps_around(cx: &mut TestAppContext) {
        let (menu, log, cx) = setup(cx);
        cx.simulate_keystrokes("up");
        menu.read_with(cx, |menu, _| assert_eq!(menu.highlighted, Some(2)));
        cx.simulate_keystrokes("down");
        menu.read_with(cx, |menu, _| assert_eq!(menu.highlighted, Some(0)));
        assert!(log.borrow().is_empty());
    }

    #[gpui::test]
    fn escape_dismisses_without_running_anything(cx: &mut TestAppContext) {
        let (_menu, log, cx) = setup(cx);
        cx.simulate_keystrokes("escape");
        assert_eq!(*log.borrow(), ["dismissed"]);
    }

    #[gpui::test]
    fn enter_with_nothing_highlighted_does_nothing(cx: &mut TestAppContext) {
        let (_menu, log, cx) = setup(cx);
        cx.simulate_keystrokes("enter");
        assert!(log.borrow().is_empty());
    }
}
