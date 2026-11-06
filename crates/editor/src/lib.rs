//! The code editor: a view over a shared `BufferModel`, with its own selections and scroll.

mod actions;
mod decorations;
mod element;
mod semantics;
mod view;

pub use actions::{
    NextProblem, PreviousProblem, Save, ShowCompletions, ShowHover, ShowSignatureHelp, bind_keys,
    keymap,
};
pub use decorations::{BreakpointKind, BreakpointMark, DecorationProvider};
pub use semantics::{
    CompletionItem, CompletionKind, Completions, HoverInfo, SemanticsProvider, SignatureInfo,
    Triggers,
};
pub use view::{EditorEvent, EditorState, EditorView};

#[cfg(test)]
mod tests;
