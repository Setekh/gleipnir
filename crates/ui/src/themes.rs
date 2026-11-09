//! Themes: the built-in ones, and the person's, each a TOML file in the themes folder. Files there
//! are read again whenever they change, so a theme can be made or changed by hand or by an AI
//! agent and is seen at once. The folder has a README saying how, written by Gleipnir.

use crate::theme::{COLOR_KEYS, DARK_THEME, LIGHT_THEME, SYNTAX_KEYS, Theme, ThemeFile};
use gpui::Global;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Where the person's themes go.
pub fn themes_dir() -> PathBuf {
    gleipnir_packages::config_dir().join("themes")
}

pub struct NamedTheme {
    pub name: String,
    pub theme: Theme,
    /// The file it came from; none for a built-in theme.
    pub path: Option<PathBuf>,
}

/// Every theme there is, and what's wrong with any file that isn't one.
#[derive(Default)]
pub struct Themes {
    pub themes: Vec<NamedTheme>,
    /// (file, problem)
    pub problems: Vec<(PathBuf, String)>,
}

impl Global for Themes {}

impl Themes {
    /// The built-in themes, then each file in the folder; a file with a built-in's name replaces it.
    pub fn load(dir: &Path) -> Self {
        let mut this = Self {
            themes: vec![
                NamedTheme {
                    name: "Gleipnir Dark".into(),
                    theme: Theme::dark(),
                    path: None,
                },
                NamedTheme {
                    name: "Gleipnir Light".into(),
                    theme: Theme::light(),
                    path: None,
                },
            ],
            problems: Vec::new(),
        };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| is_theme_file(p))
            .collect();
        files.sort();
        for path in files {
            let loaded = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| ThemeFile::parse(&text))
                .and_then(|file| Ok((file.name.clone(), Theme::from_file(&file)?)));
            match loaded {
                Ok((name, theme)) => {
                    this.themes.retain(|t| t.name != name);
                    this.themes.push(NamedTheme {
                        name,
                        theme,
                        path: Some(path),
                    });
                }
                Err(problem) => this.problems.push((path, problem)),
            }
        }
        this
    }

    /// A theme by its name. The settings' older names for the built-ins still work.
    pub fn find(&self, name: &str) -> Option<&Theme> {
        let name = match name {
            "Dark" => "Gleipnir Dark",
            "Light" => "Gleipnir Light",
            other => other,
        };
        self.themes
            .iter()
            .find(|t| t.name == name)
            .map(|t| &t.theme)
    }

    pub fn names(&self) -> Vec<String> {
        self.themes.iter().map(|t| t.name.clone()).collect()
    }
}

/// A theme of the person's: a `.toml` file, but not one of the examples Gleipnir puts there.
fn is_theme_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    name.ends_with(".toml") && !name.ends_with(".example.toml")
}

/// What changing the folder changes: each theme file's name, size and time. Cheap enough to take
/// every second.
pub fn fingerprint(dir: &Path) -> Vec<(PathBuf, u64, Option<SystemTime>)> {
    let mut found: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| is_theme_file(&e.path()))
        .map(|e| {
            let meta = e.metadata().ok();
            (
                e.path(),
                meta.as_ref().map_or(0, |m| m.len()),
                meta.and_then(|m| m.modified().ok()),
            )
        })
        .collect();
    found.sort();
    found
}

/// The README the themes folder gets, saying how to make a theme, with every key there is.
pub fn readme() -> String {
    let mut text = String::from(
        "# Gleipnir themes\n\n\
         Gleipnir writes this file; changes to it are overwritten.\n\n\
         Each `.toml` file in this folder is a theme. Gleipnir reads them again as soon as they \
         change, so a theme can be made or changed here, by hand or by an AI agent, and the \
         window follows within a second.\n\n\
         ## Making a theme\n\n\
         Copy `gleipnir-dark.example.toml` (or `gleipnir-light.example.toml`) to a new `.toml` \
         file here, give it a new `name`, and change its colors. A theme only needs the colors it \
         changes: anything left out comes from the built-in theme its `appearance` names.\n\n\
         ```toml\n\
         name = \"Midnight\"\n\
         appearance = \"dark\"\n\n\
         [colors]\n\
         background = \"#0b0e14\"\n\
         accent = \"#7aa2f7\"\n\n\
         [syntax]\n\
         keyword = \"#bb9af7\"\n\
         string = \"#9ece6a\"\n\
         ```\n\n\
         Colors are `#rrggbb`, or `#rrggbbaa` where the last two digits are opacity, which suits \
         the colors laid over others, like `hover` and `editor_selection`.\n\n\
         ## Using a theme\n\n\
         Choose it in Settings (Cmd+, on macOS, Ctrl+Alt+S elsewhere) under Appearance, or set it \
         in `../settings.json`, which Gleipnir also reads again when it changes:\n\n\
         ```json\n{ \"appearance\": { \"theme\": \"Midnight\" } }\n```\n\n\
         A file with a mistake in it is left out, and Settings says what's wrong under \
         Appearance. A theme with the same name as a built-in one replaces it.\n\n\
         ## Keys\n\n",
    );
    text.push_str("`[colors]`, for the window and the editor:\n\n");
    for key in COLOR_KEYS {
        text.push_str(&format!("- `{key}`\n"));
    }
    text.push_str("\n`[syntax]`, for code, by what the language says each piece is:\n\n");
    for key in SYNTAX_KEYS {
        text.push_str(&format!("- `{key}`\n"));
    }
    text.push_str("\nThe example files say what each key colors.\n");
    text
}

/// Gives the themes folder its README and examples, keeping them current with this version.
pub fn write_guide(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, text) in [
        ("README.md", readme()),
        ("gleipnir-dark.example.toml", DARK_THEME.to_string()),
        ("gleipnir-light.example.toml", LIGHT_THEME.to_string()),
    ] {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(path, text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_themes_set_every_key() {
        for text in [DARK_THEME, LIGHT_THEME] {
            let file = ThemeFile::parse(text).unwrap();
            let colors: Vec<&str> = file.colors.keys().map(String::as_str).collect();
            let mut expected = COLOR_KEYS.to_vec();
            expected.sort();
            assert_eq!(colors, expected, "{}", file.name);
            let syntax: Vec<&str> = file.syntax.keys().map(String::as_str).collect();
            let mut expected = SYNTAX_KEYS.to_vec();
            expected.sort();
            assert_eq!(syntax, expected, "{}", file.name);
        }
        assert!(Theme::light().is_light());
        assert!(!Theme::dark().is_light());
    }

    #[test]
    fn a_theme_file_changes_only_what_it_names_and_replaces_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("midnight.toml"),
            "name = \"Midnight\"\nappearance = \"light\"\n[colors]\naccent = \"#7aa2f7\"\nhover = \"#ffffff10\"\n[syntax]\ntype = \"#112233\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("dark.toml"),
            "name = \"Gleipnir Dark\"\n[colors]\nbackground = \"#000000\"\n",
        )
        .unwrap();
        let themes = Themes::load(dir.path());
        assert_eq!(
            themes.names(),
            ["Gleipnir Light", "Gleipnir Dark", "Midnight"]
        );
        assert!(themes.problems.is_empty());
        let midnight = themes.find("Midnight").unwrap();
        assert_eq!(
            midnight.accent,
            crate::theme::parse_color("#7aa2f7").unwrap()
        );
        assert_eq!(
            midnight.syntax.type_,
            crate::theme::parse_color("#112233").unwrap()
        );
        // The rest is the light theme's.
        assert_eq!(midnight.background, Theme::light().background);
        assert_eq!(themes.find("Dark").unwrap().background.l, 0.);
    }

    #[test]
    fn a_broken_theme_is_left_out_with_what_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        for (file, text) in [
            (
                "a.toml",
                "name = \"A\"\n[colors]\nbackgrund = \"#000000\"\n",
            ),
            ("b.toml", "name = \"B\"\n[syntax]\nkeyword = \"blue\"\n"),
            ("c.toml", "name = \"C\"\ncolours = 1\n"),
            ("notes.txt", "not a theme"),
        ] {
            std::fs::write(dir.path().join(file), text).unwrap();
        }
        let themes = Themes::load(dir.path());
        assert_eq!(themes.names(), ["Gleipnir Dark", "Gleipnir Light"]);
        let problems: Vec<&str> = themes.problems.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(problems.len(), 3);
        assert!(problems[0].contains("colors.backgrund"), "{problems:?}");
        assert!(problems[1].contains("syntax.keyword") && problems[1].contains("\"blue\""));
        assert!(problems[2].contains("colours"), "{problems:?}");
    }

    #[test]
    fn the_guide_lists_every_key_and_is_rewritten_only_when_it_differs() {
        let dir = tempfile::tempdir().unwrap();
        write_guide(dir.path()).unwrap();
        let readme = std::fs::read_to_string(dir.path().join("README.md")).unwrap();
        for key in COLOR_KEYS.iter().chain(SYNTAX_KEYS) {
            assert!(readme.contains(&format!("`{key}`")), "{key}");
        }
        let example = dir.path().join("gleipnir-dark.example.toml");
        assert_eq!(std::fs::read_to_string(&example).unwrap(), DARK_THEME);
        // The examples aren't themes themselves, so they don't show twice.
        assert_eq!(
            Themes::load(dir.path()).names(),
            ["Gleipnir Dark", "Gleipnir Light"]
        );
        let before = fingerprint(dir.path());
        std::fs::write(dir.path().join("new.toml"), "name = \"New\"\n").unwrap();
        assert_ne!(fingerprint(dir.path()), before);
    }
}
