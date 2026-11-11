//! What's in the project, independent of the UI: its files, their git status, and how they change.

pub mod find;
pub mod fs_ops;
pub mod fuzzy;
pub mod git;
pub mod git_ops;
pub mod scan;
pub mod search;
pub mod tree;
pub mod unity;
pub mod watch;

pub use git::{
    DiffRow, DiffRowKind, GitState, GitStatus, LineChange, LineChangeKind, MergeChunk, MergeKind,
    Whitespace, merge3, side_by_side, side_by_side_with, word_changes,
};
pub use scan::{FileChanges, FileList, scan_files};
pub use search::{
    Cancel, FileHit, Query, SymbolEntry, SymbolFilter, SymbolHit, search_files, search_symbols,
};
pub use tree::{DirEntry, EntryKind, FileTree, Row, is_ignored, read_directory};
pub use watch::{DirWatcher, ProjectWatcher, watch, watch_dirs};
