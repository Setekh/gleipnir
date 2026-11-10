//! The list of files in a project, kept current from file watching. This is what file search
//! searches, so it skips whatever git ignores, as IntelliJ's does.

use crate::tree::is_ignored;
use ignore::WalkBuilder;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Lists every file under `root` that git doesn't ignore, as paths relative to it. Walks folders in
/// parallel, which matters on a big repository.
pub fn scan_files(root: &Path) -> Vec<Arc<str>> {
    scan_under(root, root)
}

/// The same, for one folder within the project. Ignore rules still come from the whole project.
pub fn scan_under(root: &Path, dir: &Path) -> Vec<Arc<str>> {
    let found = Mutex::new(Vec::new());
    // In a Unity project, what Unity makes isn't the project's: its folders and `.meta` files.
    let unity = crate::unity::is_unity_project(root);
    let unity_root = root.to_path_buf();
    let walker = WalkBuilder::new(dir)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        // Projects that aren't repositories can still have a .gitignore that means something.
        .require_git(false)
        .parents(true)
        .follow_links(false)
        .filter_entry(move |entry| {
            if entry.file_name() == ".git" {
                return false;
            }
            !(unity
                && (crate::unity::is_generated(&unity_root, entry.path())
                    || entry
                        .file_name()
                        .to_str()
                        .is_some_and(crate::unity::is_meta)))
        })
        .build_parallel();
    walker.run(|| {
        let found = &found;
        Box::new(move |entry| {
            if let Ok(entry) = entry
                && entry
                    .file_type()
                    .is_some_and(|t| t.is_file() || t.is_symlink())
                && let Ok(relative) = entry.path().strip_prefix(root)
                && let Some(text) = relative.to_str()
            {
                found
                    .lock()
                    .expect("the scan's lock")
                    .push(Arc::<str>::from(text));
            }
            ignore::WalkState::Continue
        })
    });
    let mut files = found.into_inner().expect("the scan's lock");
    files.sort();
    files
}

/// The project's files, as relative paths.
#[derive(Clone, Debug, Default)]
pub struct FileList {
    files: BTreeSet<Arc<str>>,
}

impl FileList {
    pub fn new(files: Vec<Arc<str>>) -> Self {
        Self {
            files: files.into_iter().collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn contains(&self, relative: &str) -> bool {
        self.files.contains(relative)
    }

    /// A snapshot for searching on another thread.
    pub fn snapshot(&self) -> Arc<Vec<Arc<str>>> {
        Arc::new(self.files.iter().cloned().collect())
    }

    /// Brings the list up to date with paths a watcher reported. Returns the files that were added
    /// or changed, and those removed, so dependent indexes can follow.
    pub fn apply_changes(&mut self, root: &Path, changed: &[PathBuf]) -> FileChanges {
        let mut result = FileChanges::default();
        for path in changed {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            if relative.starts_with(".git") || relative.as_os_str().is_empty() {
                continue;
            }
            let Some(relative_text) = relative.to_str() else {
                continue;
            };

            if path.is_dir() {
                // A folder appeared or changed: rescan it, and drop anything in it that's gone. The
                // walker never applies ignore rules to the folder it starts from, so check that
                // here, or a new build folder would flood the list.
                let prefix = format!("{relative_text}/");
                let present: BTreeSet<Arc<str>> = if is_ignored(root, path, true) {
                    BTreeSet::new()
                } else {
                    scan_under(root, path).into_iter().collect()
                };
                let stale: Vec<Arc<str>> = self
                    .files
                    .iter()
                    .filter(|f| f.starts_with(&prefix) && !present.contains(*f))
                    .cloned()
                    .collect();
                for file in stale {
                    self.files.remove(&file);
                    result.removed.push(file);
                }
                for file in present {
                    if self.files.insert(file.clone()) {
                        result.updated.push(file);
                    }
                }
            } else if path.is_file() {
                if is_ignored(root, path, false) {
                    if self.files.remove(relative_text) {
                        result.removed.push(Arc::from(relative_text));
                    }
                    continue;
                }
                let file: Arc<str> = Arc::from(relative_text);
                self.files.insert(file.clone());
                result.updated.push(file);
            } else {
                // Gone: the path itself, or everything under it if it was a folder.
                let prefix = format!("{relative_text}/");
                let gone: Vec<Arc<str>> = self
                    .files
                    .iter()
                    .filter(|f| ***f == *relative_text || f.starts_with(&prefix))
                    .cloned()
                    .collect();
                for file in gone {
                    self.files.remove(&file);
                    result.removed.push(file);
                }
            }
        }
        result.updated.sort();
        result.updated.dedup();
        result
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChanges {
    pub updated: Vec<Arc<str>>,
    pub removed: Vec<Arc<str>>,
}

impl FileChanges {
    pub fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.removed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        (dir, root)
    }

    fn names(files: &[Arc<str>]) -> Vec<&str> {
        files.iter().map(|f| &**f).collect()
    }

    #[test]
    fn scanning_finds_files_and_skips_ignored_ones_and_dot_git() {
        let (_dir, root) = project(&[
            "src/a.rs",
            "target/out.o",
            ".git/HEAD",
            ".gitignore",
            ".env",
            "b.rs",
        ]);
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        assert_eq!(
            names(&scan_files(&root)),
            [".env", ".gitignore", "b.rs", "src/a.rs"]
        );
    }

    #[test]
    fn nested_gitignores_apply() {
        let (_dir, root) = project(&["sub/keep.rs", "sub/gen.rs"]);
        fs::write(root.join("sub/.gitignore"), "gen.rs\n").unwrap();
        assert!(!names(&scan_files(&root)).contains(&"sub/gen.rs"));
    }

    #[test]
    fn changes_add_and_remove_files() {
        let (_dir, root) = project(&["a.rs"]);
        let mut list = FileList::new(scan_files(&root));
        fs::write(root.join("b.rs"), "").unwrap();
        fs::remove_file(root.join("a.rs")).unwrap();
        let changes = list.apply_changes(&root, &[root.join("a.rs"), root.join("b.rs")]);
        assert_eq!(names(&changes.updated), ["b.rs"]);
        assert_eq!(names(&changes.removed), ["a.rs"]);
        assert!(list.contains("b.rs") && !list.contains("a.rs"));
    }

    #[test]
    fn a_new_folder_is_scanned_whole() {
        let (_dir, root) = project(&["a.rs"]);
        let mut list = FileList::new(scan_files(&root));
        fs::create_dir_all(root.join("new/deep")).unwrap();
        fs::write(root.join("new/deep/x.rs"), "").unwrap();
        fs::write(root.join("new/y.rs"), "").unwrap();
        let changes = list.apply_changes(&root, &[root.join("new")]);
        assert_eq!(names(&changes.updated), ["new/deep/x.rs", "new/y.rs"]);
    }

    #[test]
    fn a_deleted_folder_takes_its_files_with_it() {
        let (_dir, root) = project(&["gone/a.rs", "gone/b/c.rs", "gone2.rs"]);
        let mut list = FileList::new(scan_files(&root));
        fs::remove_dir_all(root.join("gone")).unwrap();
        let changes = list.apply_changes(&root, &[root.join("gone")]);
        assert_eq!(changes.removed.len(), 2);
        // A file whose name merely starts the same way isn't inside it.
        assert!(list.contains("gone2.rs"));
    }

    #[test]
    fn a_file_created_in_an_ignored_folder_is_not_added() {
        let (_dir, root) = project(&["a.rs"]);
        fs::write(root.join(".gitignore"), "build/\n").unwrap();
        let mut list = FileList::new(scan_files(&root));
        fs::create_dir(root.join("build")).unwrap();
        fs::write(root.join("build/out.rs"), "").unwrap();
        list.apply_changes(&root, &[root.join("build/out.rs"), root.join("build")]);
        assert!(!list.contains("build/out.rs"));
    }

    #[test]
    fn changes_inside_dot_git_are_ignored() {
        let (_dir, root) = project(&["a.rs"]);
        let mut list = FileList::new(scan_files(&root));
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/index"), "").unwrap();
        assert!(
            list.apply_changes(&root, &[root.join(".git/index")])
                .is_empty()
        );
    }
}
