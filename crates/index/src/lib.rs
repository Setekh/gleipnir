//! What's in the project, independent of the UI: its files, their git status, and how they change.

pub mod fs_ops;
pub mod git;
pub mod tree;
pub mod watch;

pub use git::{GitState, GitStatus};
pub use tree::{DirEntry, EntryKind, FileTree, Row, read_directory};
pub use watch::{ProjectWatcher, watch};
