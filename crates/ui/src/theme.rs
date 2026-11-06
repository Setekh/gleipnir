use gleipnir_syntax::HighlightKind;
use gpui::{App, Global, Hsla, Rgba, rgb, rgba};

/// All the colors and metrics in the interface. Only a dark palette for now, but nothing reads a
/// color from anywhere else, so adding themes later only touches this struct's construction.
#[derive(Clone, Debug)]
pub struct Theme {
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

    pub ui_font: &'static str,
    pub ui_font_size: f32,
    pub editor_font_size: f32,
    pub editor_line_height: f32,
}

/// One color per highlight category.
#[derive(Clone, Debug)]
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

impl Theme {
    /// The default dark theme: cool ink for the chrome, with a gold accent for the thread.
    pub fn dark() -> Self {
        Self {
            background: hsla(rgb(0x14161a)),
            surface: hsla(rgb(0x1a1d22)),
            surface_raised: hsla(rgb(0x22262c)),
            border: hsla(rgb(0x2b3037)),
            text: hsla(rgb(0xd7dae0)),
            text_muted: hsla(rgb(0x98a0ad)),
            text_faint: hsla(rgb(0x5d6573)),
            accent: hsla(rgb(0xe0b65c)),
            error: hsla(rgb(0xef6f6c)),
            warning: hsla(rgb(0xe0b65c)),
            hover: hsla(rgba(0xffffff0d)),
            selection: hsla(rgba(0x4f7cac66)),
            selection_inactive: hsla(rgba(0xffffff14)),
            git_added: hsla(rgb(0x8fc27a)),
            git_modified: hsla(rgb(0x82b6ee)),
            git_deleted: hsla(rgb(0xef6f6c)),
            git_conflicted: hsla(rgb(0xe3a65d)),

            editor_background: hsla(rgb(0x14161a)),
            editor_gutter_text: hsla(rgb(0x4a515d)),
            editor_gutter_text_active: hsla(rgb(0xa9b0bc)),
            editor_active_line: hsla(rgba(0xffffff09)),
            editor_selection: hsla(rgba(0x4f7cac55)),
            editor_cursor: hsla(rgb(0xe0b65c)),
            breakpoint: hsla(rgb(0xe5545e)),
            usage_highlight: hsla(rgba(0x82b6ee2e)),
            execution_line: hsla(rgba(0x4f7cac40)),
            inline_value: hsla(rgb(0x7f8ea3)),

            syntax: SyntaxColors {
                attribute: hsla(rgb(0xb9a36b)),
                comment: hsla(rgb(0x6a7280)),
                constant: hsla(rgb(0xf0917f)),
                function: hsla(rgb(0x82b6ee)),
                keyword: hsla(rgb(0xe3a65d)),
                label: hsla(rgb(0xb9a36b)),
                macro_: hsla(rgb(0xd492d6)),
                namespace: hsla(rgb(0x92bccc)),
                number: hsla(rgb(0xf0917f)),
                operator: hsla(rgb(0xaab2be)),
                parameter: hsla(rgb(0xdccaa8)),
                property: hsla(rgb(0xbcc7d6)),
                punctuation: hsla(rgb(0x868e9c)),
                string: hsla(rgb(0xa8cb7f)),
                string_escape: hsla(rgb(0xe8c36a)),
                tag: hsla(rgb(0x82b6ee)),
                type_: hsla(rgb(0x72d3c2)),
                variable: hsla(rgb(0xd7dae0)),
                variable_builtin: hsla(rgb(0xe3a65d)),
            },

            ui_font: ".SystemUIFont",
            ui_font_size: 13.0,
            editor_font_size: 13.0,
            editor_line_height: 20.0,
        }
    }
}

impl Global for Theme {}

pub fn init_theme(cx: &mut App) {
    cx.set_global(Theme::dark());
}
