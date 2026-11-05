//! The window around the editors: split panes, tabs, docked panels and the status bar.

mod actions_list;
pub mod dock;
mod keymap;
mod pane;
mod pane_tree;
pub mod persistence;
mod workspace;

pub use actions_list::{ActionInfo, humanize_action_name, list_actions};
pub use dock::{DockPosition, Panel, PanelHandle};
pub use keymap::{KeymapEntry, load_user_keymap, parse_keymap};
pub use pane::Pane;
pub use pane_tree::{Axis, MIN_RATIO, PaneTree};
pub use persistence::{config_dir, last_project, project_root_for, state_dir};
pub use workspace::*;

#[cfg(test)]
mod tests;
