//! User key bindings, from a TOML-like file of lines:
//!
//! ```text
//! # comments and blank lines are fine
//! cmd-k = editor::DuplicateLine
//! ctrl-j = editor::MoveDown in Editor
//! ```
//!
//! The optional `in` names a context, where the binding applies. Later bindings take precedence, so
//! these override the defaults. Mistakes in the file are reported, and don't stop the others working.

use gpui::{App, KeyBinding, KeyBindingContextPredicate};
use std::path::Path;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapEntry {
    pub line: usize,
    pub keystrokes: String,
    pub action: String,
    pub context: Option<String>,
}

/// Parses the file's text into entries, and messages about any lines that don't parse.
pub fn parse_keymap(text: &str) -> (Vec<KeymapEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((keys, rest)) = line.split_once('=') else {
            errors.push(format!("line {line_number}: expected `keys = action`"));
            continue;
        };
        let (action, context) = match rest.split_once(" in ") {
            Some((action, context)) => (action.trim(), Some(context.trim().to_string())),
            None => (rest.trim(), None),
        };
        let keys = keys.trim();
        if keys.is_empty() || action.is_empty() {
            errors.push(format!("line {line_number}: expected `keys = action`"));
            continue;
        }
        entries.push(KeymapEntry {
            line: line_number,
            keystrokes: keys.to_string(),
            action: action.to_string(),
            context,
        });
    }
    (entries, errors)
}

/// Applies a user keymap file on top of the defaults, returning messages about anything wrong with
/// it. A missing file isn't a problem, as most people won't have one.
pub fn load_user_keymap(path: &Path, cx: &mut App) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let (entries, mut errors) = parse_keymap(&text);
    let mut bindings = Vec::new();
    for entry in entries {
        match build_binding(&entry, cx) {
            Ok(binding) => bindings.push(binding),
            Err(message) => errors.push(format!("line {}: {message}", entry.line)),
        }
    }
    cx.bind_keys(bindings);
    errors
}

/// Where a person's own key bindings are kept.
pub fn user_keymap_path() -> std::path::PathBuf {
    gleipnir_packages::config_dir().join("keymap.conf")
}

/// Rebinds every key from scratch: the defaults, then the person's own. Set by the app, as only
/// it knows every feature's defaults; used after a binding is removed, which gpui can't undo alone.
#[derive(Clone)]
pub struct KeymapReload(pub Rc<Rebind>);

/// Binds every key again, returning problems with the person's keymap file.
pub type Rebind = dyn Fn(&mut App) -> Vec<String>;

impl gpui::Global for KeymapReload {}

/// Every shortcut bound to each action, by the action's name, as (keystrokes, context) with the
/// most recently bound, which wins, first.
pub fn shortcuts_by_action(
    cx: &App,
) -> std::collections::HashMap<String, Vec<(String, Option<String>)>> {
    let mut found: std::collections::HashMap<String, Vec<(String, Option<String>)>> =
        std::collections::HashMap::new();
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    for binding in keymap.bindings().rev() {
        let name = binding.action().name();
        if name == "zed::NoAction" || name.ends_with("::NoAction") {
            continue;
        }
        let keys = binding
            .keystrokes()
            .iter()
            .map(|k| k.unparse(cx.platform_info().os))
            .collect::<Vec<_>>()
            .join(" ");
        let context = binding.predicate().map(|p| p.to_string());
        let shortcuts = found.entry(name.to_string()).or_default();
        if !shortcuts.iter().any(|(k, c)| *k == keys && *c == context) {
            shortcuts.push((keys, context));
        }
    }
    found
}

/// The person's own bindings, from their keymap file.
pub fn user_bindings(path: &Path) -> Vec<KeymapEntry> {
    std::fs::read_to_string(path)
        .map(|text| parse_keymap(&text).0)
        .unwrap_or_default()
}

/// Binds keys to an action, at once and from now on, by adding it to the person's keymap.
pub fn add_user_binding(
    path: &Path,
    keystrokes: &str,
    action: &str,
    cx: &mut App,
) -> Result<(), String> {
    let entry = KeymapEntry {
        line: 0,
        keystrokes: keystrokes.to_string(),
        action: action.to_string(),
        context: None,
    };
    let binding = build_binding(&entry, cx)?;
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{keystrokes} = {action}\n"));
    write_keymap(path, &text)?;
    cx.bind_keys([binding]);
    Ok(())
}

/// Takes one of the person's own bindings out of their keymap, and rebinds every key without it.
pub fn remove_user_binding(
    path: &Path,
    keystrokes: &str,
    action: &str,
    cx: &mut App,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (entries, _) = parse_keymap(&text);
    let drop: Vec<usize> = entries
        .iter()
        .filter(|e| e.keystrokes == keystrokes && e.action == action)
        .map(|e| e.line)
        .collect();
    let kept: String = text
        .lines()
        .enumerate()
        .filter(|(index, _)| !drop.contains(&(index + 1)))
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    write_keymap(path, &kept)?;
    if let Some(reload) = cx.try_global::<KeymapReload>().cloned() {
        (reload.0)(cx);
    }
    Ok(())
}

fn write_keymap(path: &Path, text: &str) -> Result<(), String> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn build_binding(entry: &KeymapEntry, cx: &App) -> Result<KeyBinding, String> {
    let action = cx
        .build_action(&entry.action, None)
        .map_err(|_| format!("there's no action called {}", entry.action))?;
    let context = match &entry.context {
        Some(context) => Some(Rc::from(
            KeyBindingContextPredicate::parse(context)
                .map_err(|e| format!("bad context {context:?}: {e}"))?,
        )),
        None => None,
    };
    KeyBinding::load(&entry.keystrokes, action, context, false, None, cx)
        .map_err(|e| format!("bad keystroke {:?}: {e}", entry.keystrokes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bindings_with_and_without_a_context() {
        let (entries, errors) = parse_keymap(
            "# my keys\n\ncmd-k = editor::DuplicateLine\nctrl-j = editor::MoveDown in Editor\n",
        );
        assert!(errors.is_empty());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].keystrokes, "cmd-k");
        assert_eq!(entries[0].action, "editor::DuplicateLine");
        assert_eq!(entries[0].context, None);
        assert_eq!(entries[0].line, 3);
        assert_eq!(entries[1].context.as_deref(), Some("Editor"));
    }

    #[test]
    fn chords_keep_their_spaces() {
        let (entries, _) = parse_keymap("cmd-k cmd-s = editor::Save");
        assert_eq!(entries[0].keystrokes, "cmd-k cmd-s");
    }

    #[test]
    fn bad_lines_are_reported_with_their_numbers_and_the_rest_still_load() {
        let (entries, errors) = parse_keymap("nonsense\ncmd-k = a::B\n= a::B\ncmd-j =\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(errors.len(), 3);
        assert!(errors[0].contains("line 1"));
        assert!(errors[1].contains("line 3"));
        assert!(errors[2].contains("line 4"));
    }
}
