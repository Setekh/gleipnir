//! Panels and the docks that hold them.

use gleipnir_ui::Icon;
use gpui::{AnyView, App, Context, Entity, FocusHandle, Focusable, Pixels, Render, Window, px};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DockPosition {
    Left,
    Bottom,
    Right,
}

/// Something that lives in a dock, like the project tree.
pub trait Panel: Focusable + Render + 'static {
    /// Stable across versions, as saved layouts refer to panels by it.
    fn id(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn icon(&self) -> Icon;
    fn position(&self) -> DockPosition;
    /// Whatever the panel wants to remember between sessions.
    fn save_state(&self, _: &App) -> serde_json::Value {
        serde_json::Value::Null
    }
    fn restore_state(&mut self, _: serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
    /// Whether the panel opens again with the window if it was showing when it closed. One whose
    /// contents don't come back would only open empty.
    fn reopens(&self) -> bool {
        true
    }
    /// A description for tests and the end-to-end driver, which can be more than what's saved.
    fn debug_state(&self, cx: &App) -> serde_json::Value {
        self.save_state(cx)
    }
}

/// A panel with its type erased, so a dock can hold any mix of them.
pub trait PanelHandle {
    fn id(&self, cx: &App) -> &'static str;
    fn title(&self, cx: &App) -> &'static str;
    fn icon(&self, cx: &App) -> Icon;
    fn view(&self) -> AnyView;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn save_state(&self, cx: &App) -> serde_json::Value;
    fn debug_state(&self, cx: &App) -> serde_json::Value;
    fn restore_state(&self, state: serde_json::Value, window: &mut Window, cx: &mut App);
    fn reopens(&self, cx: &App) -> bool;
}

impl<T: Panel> PanelHandle for Entity<T> {
    fn id(&self, cx: &App) -> &'static str {
        self.read(cx).id()
    }
    fn title(&self, cx: &App) -> &'static str {
        self.read(cx).title()
    }
    fn icon(&self, cx: &App) -> Icon {
        self.read(cx).icon()
    }
    fn view(&self) -> AnyView {
        self.clone().into()
    }
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.read(cx).focus_handle(cx)
    }
    fn save_state(&self, cx: &App) -> serde_json::Value {
        self.read(cx).save_state(cx)
    }
    fn debug_state(&self, cx: &App) -> serde_json::Value {
        self.read(cx).debug_state(cx)
    }
    fn restore_state(&self, state: serde_json::Value, window: &mut Window, cx: &mut App) {
        self.update(cx, |panel, cx| panel.restore_state(state, window, cx));
    }
    fn reopens(&self, cx: &App) -> bool {
        self.read(cx).reopens()
    }
}

/// Keeps a panel from being dragged too small to use, or so large the editor disappears.
pub const MIN_DOCK_SIZE: f32 = 120.;
pub const MIN_CENTER_SIZE: f32 = 200.;

pub(crate) struct Dock {
    pub position: DockPosition,
    pub panels: Vec<Box<dyn PanelHandle>>,
    pub active: usize,
    pub visible: bool,
    pub size: Pixels,
}

impl Dock {
    pub fn new(position: DockPosition) -> Self {
        let size = match position {
            DockPosition::Left | DockPosition::Right => px(280.),
            DockPosition::Bottom => px(240.),
        };
        Self {
            position,
            panels: Vec::new(),
            active: 0,
            visible: false,
            size,
        }
    }

    pub fn active_panel(&self) -> Option<&dyn PanelHandle> {
        self.panels.get(self.active).map(|p| p.as_ref())
    }
}
