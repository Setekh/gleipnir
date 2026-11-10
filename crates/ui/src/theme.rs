use gleipnir_syntax::HighlightKind;
use gpui::{App, Global, Hsla, Rgba, rgb, rgba};

/// All the colors and metrics in the interface. The colors come from a theme file: a built-in one,
/// or one of the person's in the themes folder (see `themes`).
#[derive(Clone, Debug, Default)]
pub struct Theme {
    /// As its file names it.
    pub name: String,
    pub background: Hsla,
    pub surface: Hsla,
    pub surface_raised: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub accent: Hsla,
    pub error: Hsla,
    pub warning: Hsla,
    /// Rows and tabs under the pointer.
    pub hover: Hsla,
    /// The selected row when its list has focus, and when it doesn't.
    pub selection: Hsla,
    pub selection_inactive: Hsla,
    pub git_added: Hsla,
    pub git_modified: Hsla,
    pub git_deleted: Hsla,
    pub git_conflicted: Hsla,

    pub editor_background: Hsla,
    pub editor_gutter_text: Hsla,
    pub editor_gutter_text_active: Hsla,
    pub editor_active_line: Hsla,
    pub editor_selection: Hsla,
    pub editor_cursor: Hsla,
    pub breakpoint: Hsla,
    /// Behind the other uses of the name under the cursor.
    pub usage_highlight: Hsla,
    /// The line execution is stopped on.
    pub execution_line: Hsla,
    /// The values the debugger shows beside the code.
    pub inline_value: Hsla,

    pub syntax: SyntaxColors,
    /// The terminal's 16 colors, in ANSI's order: black, red, green, yellow, blue, magenta, cyan,
    /// white, then their bright versions.
    pub terminal: [Hsla; 16],

    pub ui_font: &'static str,
    pub ui_font_size: f32,
    pub editor_font_size: f32,
    pub editor_line_height: f32,
}

/// One color per highlight category.
#[derive(Clone, Debug, Default)]
pub struct SyntaxColors {
    pub attribute: Hsla,
    pub comment: Hsla,
    pub constant: Hsla,
    pub function: Hsla,
    pub keyword: Hsla,
    pub label: Hsla,
    pub macro_: Hsla,
    pub namespace: Hsla,
    pub number: Hsla,
    pub operator: Hsla,
    pub parameter: Hsla,
    pub property: Hsla,
    pub punctuation: Hsla,
    pub string: Hsla,
    pub string_escape: Hsla,
    pub tag: Hsla,
    pub type_: Hsla,
    pub variable: Hsla,
    pub variable_builtin: Hsla,
}

impl SyntaxColors {
    pub fn color(&self, kind: HighlightKind) -> Hsla {
        match kind {
            HighlightKind::Attribute => self.attribute,
            HighlightKind::Comment => self.comment,
            HighlightKind::Constant => self.constant,
            HighlightKind::Function => self.function,
            HighlightKind::Keyword => self.keyword,
            HighlightKind::Label => self.label,
            HighlightKind::Macro => self.macro_,
            HighlightKind::Namespace => self.namespace,
            HighlightKind::Number => self.number,
            HighlightKind::Operator => self.operator,
            HighlightKind::Parameter => self.parameter,
            HighlightKind::Property => self.property,
            HighlightKind::Punctuation => self.punctuation,
            HighlightKind::String => self.string,
            HighlightKind::StringEscape => self.string_escape,
            HighlightKind::Tag => self.tag,
            HighlightKind::Type => self.type_,
            HighlightKind::Variable => self.variable,
            HighlightKind::VariableBuiltin => self.variable_builtin,
        }
    }
}

fn hsla(color: Rgba) -> Hsla {
    color.into()
}

/// The built-in themes' files, which also show the format.
pub const DARK_THEME: &str = include_str!("../themes/dark.toml");
pub const LIGHT_THEME: &str = include_str!("../themes/light.toml");

/// What a theme file says: its name, the built-in theme it starts from, and the colors it changes.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    pub name: String,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub colors: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub syntax: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub terminal: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

/// Every key a theme's `[colors]` can set, with the field it sets.
macro_rules! color_keys {
    ($($key:ident),* $(,)?) => {
        pub const COLOR_KEYS: &[&str] = &[$(stringify!($key)),*];
        fn set_color(theme: &mut Theme, key: &str, color: Hsla) -> bool {
            match key {
                $(stringify!($key) => theme.$key = color,)*
                _ => return false,
            }
            true
        }
    };
}

color_keys!(
    background,
    surface,
    surface_raised,
    border,
    text,
    text_muted,
    text_faint,
    accent,
    error,
    warning,
    hover,
    selection,
    selection_inactive,
    git_added,
    git_modified,
    git_deleted,
    git_conflicted,
    editor_background,
    editor_gutter_text,
    editor_gutter_text_active,
    editor_active_line,
    editor_selection,
    editor_cursor,
    breakpoint,
    usage_highlight,
    execution_line,
    inline_value,
);

/// Every key a theme's `[syntax]` can set, as written in the file, with the field it sets.
macro_rules! syntax_keys {
    ($($key:literal => $field:ident),* $(,)?) => {
        pub const SYNTAX_KEYS: &[&str] = &[$($key),*];
        fn set_syntax(colors: &mut SyntaxColors, key: &str, color: Hsla) -> bool {
            match key {
                $($key => colors.$field = color,)*
                _ => return false,
            }
            true
        }
    };
}

syntax_keys!(
    "attribute" => attribute, "comment" => comment, "constant" => constant,
    "function" => function, "keyword" => keyword, "label" => label, "macro" => macro_,
    "namespace" => namespace, "number" => number, "operator" => operator,
    "parameter" => parameter, "property" => property, "punctuation" => punctuation,
    "string" => string, "string_escape" => string_escape, "tag" => tag, "type" => type_,
    "variable" => variable, "variable_builtin" => variable_builtin,
);

/// Every key a theme's `[terminal]` can set, in ANSI's order.
pub const TERMINAL_KEYS: &[&str] = &[
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

/// A color written `#rrggbb` or `#rrggbbaa`.
pub fn parse_color(text: &str) -> Option<Hsla> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(hsla(rgb(value))),
        8 => Some(hsla(rgba(value))),
        _ => None,
    }
}

impl ThemeFile {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.message().to_string())
    }
}

impl Theme {
    /// A theme from its file, over the built-in theme it names as its base. Says what's wrong
    /// with any key or color that can't be used, naming it.
    pub fn from_file(file: &ThemeFile) -> Result<Self, String> {
        let mut theme = match file.appearance {
            Appearance::Dark => Self::dark(),
            Appearance::Light => Self::light(),
        };
        theme.apply(file)?;
        Ok(theme)
    }

    fn apply(&mut self, file: &ThemeFile) -> Result<(), String> {
        self.name = file.name.clone();
        for (key, value) in &file.colors {
            let color = parse_color(value)
                .ok_or_else(|| format!("colors.{key}: {value:?} isn't a color like \"#1a2b3c\""))?;
            if !set_color(self, key, color) {
                return Err(format!("colors.{key} isn't a color a theme can set"));
            }
        }
        for (key, value) in &file.syntax {
            let color = parse_color(value)
                .ok_or_else(|| format!("syntax.{key}: {value:?} isn't a color like \"#1a2b3c\""))?;
            if !set_syntax(&mut self.syntax, key, color) {
                return Err(format!(
                    "syntax.{key} isn't a kind of code a theme can color"
                ));
            }
        }
        for (key, value) in &file.terminal {
            let color = parse_color(value).ok_or_else(|| {
                format!("terminal.{key}: {value:?} isn't a color like \"#1a2b3c\"")
            })?;
            let index = TERMINAL_KEYS
                .iter()
                .position(|k| k == key)
                .ok_or_else(|| format!("terminal.{key} isn't one of the terminal's colors"))?;
            self.terminal[index] = color;
        }
        Ok(())
    }

    /// A built-in theme, from its file, which sets every color.
    fn builtin(text: &str) -> Self {
        let file = ThemeFile::parse(text).expect("a built-in theme parses");
        let mut theme = Self {
            ui_font: ".SystemUIFont",
            ui_font_size: 13.0,
            editor_font_size: 13.0,
            editor_line_height: 20.0,
            ..Self::default()
        };
        theme.apply(&file).expect("a built-in theme is valid");
        theme
    }

    /// The default dark theme: cool ink for the chrome, with a gold accent for the thread.
    pub fn dark() -> Self {
        static DARK: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
        DARK.get_or_init(|| Self::builtin(DARK_THEME)).clone()
    }

    /// The light theme: paper for the chrome, with the same gold thread, darkened to read on it.
    pub fn light() -> Self {
        static LIGHT: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
        LIGHT.get_or_init(|| Self::builtin(LIGHT_THEME)).clone()
    }

    /// The theme the settings choose, from those there are, with the settings' font sizes. A
    /// theme that's gone falls back to the built-in dark one.
    pub fn from_settings(
        settings: &gleipnir_packages::settings::Settings,
        themes: &crate::themes::Themes,
    ) -> Self {
        let mut theme = themes
            .find(&settings.appearance.theme)
            .cloned()
            .unwrap_or_else(Self::dark);
        theme.ui_font_size = settings.appearance.ui_font_size.clamp(9., 24.);
        theme.editor_font_size = settings.editor.font_size.clamp(8., 40.);
        theme.editor_line_height = settings
            .editor
            .line_height
            .clamp(theme.editor_font_size, theme.editor_font_size * 3.);
        theme
    }

    pub fn is_light(&self) -> bool {
        self.background.l > 0.5
    }
}

impl Global for Theme {}

pub fn init_theme(cx: &mut App) {
    cx.set_global(Theme::dark());
}
