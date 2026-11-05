//! Every command in the application, by name. GPUI registers actions itself; this adds readable
//! titles and current shortcuts, and it's what Search Everywhere's action tab will list.

use gpui::{App, Window};

/// Namespaces that are user-facing commands, rather than a widget's internal key handling.
const COMMAND_NAMESPACES: &[&str] = &["editor", "workspace", "project_panel"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionInfo {
    /// The name to bind in a keymap, like `editor::DuplicateLine`.
    pub name: String,
    /// What to show a person, like `Duplicate Line`.
    pub title: String,
    pub shortcut: Option<String>,
}

/// Turns `editor::DuplicateLine` into `Duplicate Line`.
pub fn humanize_action_name(name: &str) -> String {
    let short = name.rsplit("::").next().unwrap_or(name);
    let mut title = String::with_capacity(short.len() + 4);
    for (index, c) in short.char_indices() {
        if c.is_uppercase() && index > 0 {
            title.push(' ');
        }
        title.push(c);
    }
    title
}

/// All commands, sorted by title, with whichever shortcut currently wins for each.
pub fn list_actions(window: &Window, cx: &App) -> Vec<ActionInfo> {
    let mut actions: Vec<ActionInfo> = cx
        .all_action_names()
        .iter()
        .filter(|name| {
            name.split_once("::")
                .is_some_and(|(space, _)| COMMAND_NAMESPACES.contains(&space))
        })
        .filter_map(|name| {
            let action = cx.build_action(name, None).ok()?;
            let shortcut = window
                .highest_precedence_binding_for_action(action.as_ref())
                .map(|binding| {
                    binding
                        .keystrokes()
                        .iter()
                        .map(|k| k.unparse(cx.platform_info().os))
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            Some(ActionInfo {
                name: name.to_string(),
                title: humanize_action_name(name),
                shortcut,
            })
        })
        .collect();
    actions.sort_by(|a, b| a.title.cmp(&b.title));
    actions
}
