//! GPUI entities around the text and syntax models: open documents and how they reach the disk.

mod buffer;
mod store;

pub use buffer::{BufferEvent, BufferModel, DiskStatus};
pub use store::BufferStore;
