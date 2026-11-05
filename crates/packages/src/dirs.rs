//! Where Gleipnir keeps its own files. Never inside a project: opening a folder writes nothing to it.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Machine-managed state: layouts and installed tools. `GLEIPNIR_STATE_DIR` overrides it.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("GLEIPNIR_STATE_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Gleipnir")
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map_or_else(|| home().join(".local/state"), PathBuf::from)
            .join("gleipnir")
    }
}

/// Files people edit by hand, like the keymap. `GLEIPNIR_CONFIG_DIR` overrides it.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("GLEIPNIR_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map_or_else(|| home().join(".config"), PathBuf::from)
        .join("gleipnir")
}

/// Where tools Gleipnir installs go, one folder per tool and version.
pub fn managed_tools_dir() -> PathBuf {
    state_dir().join("tools")
}

/// Expands a leading `~/`.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}
