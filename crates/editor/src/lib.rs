//! The code editor: a view over a shared `BufferModel`, with its own selections and scroll.

mod actions;
mod annotations;
mod caret_gesture;
mod decorations;
mod element;
mod semantics;
mod view;

pub use actions::{
    NextProblem, PreviousProblem, Save, ShowCompletions, ShowHover, ShowSignatureHelp, bind_keys,
    keymap,
};
pub use annotations::InlayHint;
pub use decorations::{BreakpointKind, BreakpointMark, DecorationProvider};
pub use element::{editor_font, editor_font_family};
pub use semantics::{
    CompletionItem, CompletionKind, Completions, HoverInfo, SemanticTokens, SemanticsProvider,
    SignatureInfo, Triggers,
};
pub use view::{EditorEvent, EditorState, EditorView};

#[cfg(test)]
mod tests;
