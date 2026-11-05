//! What's in the project, independent of the UI: its files, their git status, and how they change.

pub mod fs_ops;
pub mod fuzzy;
pub mod git;
pub mod scan;
pub mod search;
pub mod tree;
pub mod watch;

pub use git::{GitState, GitStatus};
pub use scan::{FileChanges, FileList, scan_files};
pub use search::{
    Cancel, FileHit, Query, SymbolEntry, SymbolFilter, SymbolHit, search_files, search_symbols,
};
pub use tree::{DirEntry, EntryKind, FileTree, Row, is_ignored, read_directory};
pub use watch::{ProjectWatcher, watch};
