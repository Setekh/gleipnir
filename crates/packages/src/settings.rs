//! What a person can change about Gleipnir, kept in `settings.json` in the config folder. Every
//! field has a default, so a file from an older version, or one with only a few settings in it,
//! reads fine, and only what differs from the defaults needs to be there.

use crate::dirs::config_dir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub editor: EditorSettings,
    /// Indentation by language name, like `Rust`, overriding the language's own.
    pub code_style: BTreeMap<String, CodeStyle>,
    pub tools: ToolSettings,
    pub tests: TestSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// A theme's name: a built-in one, or one from the themes folder.
    pub theme: String,
    pub ui_font_size: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "Gleipnir Dark".into(),
            ui_font_size: 13.,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// A font family, or none for the first good monospace font installed.
    pub font_family: Option<String>,
    pub font_size: f32,
    pub line_height: f32,
    /// Whether completion pops up while typing, rather than only on Ctrl+Space.
    pub completion_while_typing: bool,
    /// Whether Parameter Info opens by itself on typing a call's parenthesis.
    pub parameter_info_while_typing: bool,
    /// Parameter names and inferred types, written between the code.
    pub inlay_hints: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_family: None,
            font_size: 13.,
            line_height: 20.,
            completion_while_typing: true,
            parameter_info_while_typing: true,
            inlay_hints: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeStyle {
    pub tab_size: usize,
    pub hard_tabs: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolSettings {
    /// Whether a missing tool that can be installed is installed as soon as it's needed.
    pub install_automatically: bool,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            install_automatically: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestSettings {
    /// Whether the results tree shows tests that passed, or only the others.
    pub show_passed: bool,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self { show_passed: true }
    }
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    /// The saved settings, or the defaults, with why the file couldn't be read if it couldn't.
    pub fn load() -> (Self, Option<String>) {
        let path = settings_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (Self::default(), None);
        };
        match serde_json::from_str(&text) {
            Ok(settings) => (settings, None),
            Err(error) => (
                Self::default(),
                Some(format!("{} couldn't be read: {error}", path.display())),
            ),
        }
    }

    /// Writes the settings, via a file aside so a crash can't leave half of them.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = settings_path();
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let partial = path.with_extension("json.tmp");
        std::fs::write(&partial, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&partial, &path)?;
        Ok(())
    }

    /// A language's indentation: its own, unless the settings say otherwise.
    pub fn code_style_for(&self, language: &str, own: CodeStyle) -> CodeStyle {
        self.code_style.get(language).copied().unwrap_or(own)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_with_some_settings_keeps_the_defaults_for_the_rest() {
        let settings: Settings = serde_json::from_str(
            r#"{"editor": {"font_size": 15}, "appearance": {"theme": "Light"}}"#,
        )
        .unwrap();
        assert_eq!(settings.editor.font_size, 15.);
        assert_eq!(settings.editor.line_height, 20.);
        assert!(settings.editor.completion_while_typing);
        assert_eq!(settings.appearance.theme, "Light");
        assert!(settings.tools.install_automatically);
    }

    #[test]
    fn code_style_overrides_only_the_languages_it_names() {
        let mut settings = Settings::default();
        let own = CodeStyle {
            tab_size: 4,
            hard_tabs: false,
        };
        settings.code_style.insert(
            "Go".into(),
            CodeStyle {
                tab_size: 8,
                hard_tabs: true,
            },
        );
        assert_eq!(settings.code_style_for("Go", own).tab_size, 8);
        assert_eq!(settings.code_style_for("Rust", own), own);
    }
}
