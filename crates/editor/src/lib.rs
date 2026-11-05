//! The code editor: a view over a shared `BufferModel`, with its own selections and scroll.

mod actions;
mod element;
mod view;

pub use actions::{Save, bind_keys, keymap};
pub use view::{EditorEvent, EditorState, EditorView};

#[cfg(test)]
mod tests;
