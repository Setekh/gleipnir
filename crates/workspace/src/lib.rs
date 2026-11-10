//! The window around the editors: split panes, tabs, docked panels and the status bar.

mod actions_list;
pub mod dock;
pub mod image_view;
mod keymap;
mod name_prompt;
mod pane;
mod pane_tree;
pub mod persistence;
mod workspace;

pub use actions_list::{ActionInfo, humanize_action_name, list_actions};
pub use dock::{DockPosition, Panel, PanelHandle};
pub use image_view::{ImageView, is_image};
pub use keymap::{
    KeymapEntry, KeymapReload, add_user_binding, load_user_keymap, parse_keymap,
    remove_user_binding, shortcuts_by_action, user_bindings, user_keymap_path,
};
pub use name_prompt::NamePrompt;
pub use pane::{Pane, PaneItem};
pub use pane_tree::{Axis, MIN_RATIO, PaneTree};
pub use persistence::{config_dir, last_project, project_root_for, state_dir};
pub use workspace::*;

#[cfg(test)]
mod tests;
