//! The settings in effect, as a global everything reads from and the Settings screen changes.
//! Changing them saves them and notifies whoever observes the global, which is how the theme,
//! fonts and indentation follow at once.

use gleipnir_packages::settings::Settings;
use gpui::{App, Global};
use std::sync::OnceLock;

pub struct AppSettings {
    pub settings: Settings,
    /// Why the settings file couldn't be read, when it couldn't, so it can be shown.
    pub problem: Option<String>,
}

impl Global for AppSettings {}

impl AppSettings {
    /// Why the settings file couldn't be read at startup, if it couldn't.
    pub fn problem(cx: &App) -> Option<String> {
        cx.try_global::<Self>().and_then(|s| s.problem.clone())
    }
}

/// Reads the settings file into the global.
pub fn init_settings(cx: &mut App) {
    let (settings, problem) = Settings::load();
    cx.set_global(AppSettings { settings, problem });
}

/// The settings in effect: the defaults where they've never been read, as in most tests.
pub fn settings(cx: &App) -> &Settings {
    static DEFAULTS: OnceLock<Settings> = OnceLock::new();
    cx.try_global::<AppSettings>()
        .map(|s| &s.settings)
        .unwrap_or_else(|| DEFAULTS.get_or_init(Settings::default))
}

/// Changes the settings and saves them. Observers of the global hear of it either way; a failure
/// to save is returned to be shown.
pub fn update_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
    let mut settings = settings(cx).clone();
    change(&mut settings);
    let saved = settings.save();
    cx.set_global(AppSettings {
        settings,
        problem: None,
    });
    saved
}
