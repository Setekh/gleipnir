//! The project tree as a lazy model. Directories are only read once they're expanded, so opening
//! a huge repository costs the same as opening a small one.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Never shown. Everything else, including other dotfiles, is the user's to see.
const HIDDEN_NAMES: &[&str] = &[".git", ".DS_Store"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Matched by a gitignore rule, so shown dimmed.
    pub ignored: bool,
    pub symlink: bool,
}

/// One visible line of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub entry: DirEntry,
    pub depth: usize,
    pub expanded: bool,
    pub loading: bool,
}

/// The gitignore rules that apply in a directory, nearest last.
struct IgnoreRules {
    root: PathBuf,
    matchers: Vec<Gitignore>,
}

impl IgnoreRules {
    fn for_dir(root: &Path, dir: &Path) -> Self {
        let mut matchers = Vec::new();
        let exclude = root.join(".git/info/exclude");
        if exclude.is_file() {
            let mut builder = GitignoreBuilder::new(root);
            builder.add(&exclude);
            if let Ok(matcher) = builder.build() {
                matchers.push(matcher);
            }
        }

        let mut current = root.to_path_buf();
        Self::add_dir(&mut matchers, &current);
        if let Ok(relative) = dir.strip_prefix(root) {
            for component in relative.components() {
                current.push(component);
                Self::add_dir(&mut matchers, &current);
            }
        }
        Self {
            root: root.to_path_buf(),
            matchers,
        }
    }

    fn add_dir(matchers: &mut Vec<Gitignore>, dir: &Path) {
        let file = dir.join(".gitignore");
        if file.is_file() {
            let (matcher, _) = Gitignore::new(&file);
            matchers.push(matcher);
        }
    }

    /// Git's precedence: the deepest file with an opinion decides, and global rules come last.
    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in self.matchers.iter().rev() {
            let matched = matcher.matched_path_or_any_parents(path, is_dir);
            if matched.is_ignore() {
                return true;
            }
            if matched.is_whitelist() {
                return false;
            }
        }
        // The global file has no root of its own, so it's given a relative path.
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        global_gitignore()
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    }
}

fn global_gitignore() -> &'static Gitignore {
    static GLOBAL: OnceLock<Gitignore> = OnceLock::new();
    GLOBAL.get_or_init(|| Gitignore::global().0)
}

/// Lists a directory: folders first, then files, each in natural case-insensitive order. This does
/// blocking IO, so run it off the UI thread.
pub fn read_directory(root: &Path, dir: &Path) -> io::Result<Vec<DirEntry>> {
    let rules = IgnoreRules::for_dir(root, dir);
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if HIDDEN_NAMES.contains(&name.as_str()) {
            continue;
        }
        let path = item.path();
        let file_type = item.file_type()?;
        let symlink = file_type.is_symlink();
        // Follow symlinks to tell whether they lead to a folder. A dangling one is just a file.
        let is_dir = if symlink {
            path.is_dir()
        } else {
            file_type.is_dir()
        };
        entries.push(DirEntry {
            ignored: rules.is_ignored(&path, is_dir),
            kind: if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            name,
            path,
            symlink,
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Dir)
            .cmp(&(b.kind != EntryKind::Dir))
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    Ok(entries)
}

/// Compares names so `file2` sorts before `file10`, ignoring case. Case breaks ties, so the order
/// stays stable on case-sensitive filesystems.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) if l.is_ascii_digit() && r.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        chars.next();
                    }
                    digits
                };
                let (l, r) = (take(&mut left), take(&mut right));
                let (ls, rs) = (l.trim_start_matches('0'), r.trim_start_matches('0'));
                let order = ls.len().cmp(&rs.len()).then_with(|| ls.cmp(rs));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(l), Some(r)) => {
                let order = l.to_lowercase().cmp(r.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                left.next();
                right.next();
            }
        }
    }
}

/// The tree's state. Knows nothing of threads: callers do the reads from `dirs_to_load` however
/// they like, then hand the results to `set_children`.
pub struct FileTree {
    root: DirEntry,
    children: HashMap<PathBuf, Vec<DirEntry>>,
    expanded: HashSet<PathBuf>,
    loading: HashSet<PathBuf>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        Self {
            root: DirEntry {
                name,
                path: root,
                kind: EntryKind::Dir,
                ignored: false,
                symlink: false,
            },
            children: HashMap::new(),
            expanded,
            loading: HashSet::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root.path
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    pub fn expanded_dirs(&self) -> impl Iterator<Item = &PathBuf> {
        self.expanded.iter()
    }

    pub fn is_loaded(&self, dir: &Path) -> bool {
        self.children.contains_key(dir)
    }

    /// Directories whose contents are known, which are the ones worth refreshing on a change.
    pub fn loaded_dirs(&self) -> impl Iterator<Item = &PathBuf> {
        self.children.keys()
    }

    pub fn expand(&mut self, dir: &Path) {
        self.expanded.insert(dir.to_path_buf());
    }

    pub fn collapse(&mut self, dir: &Path) {
        self.expanded.remove(dir);
    }

    /// Expanded folders nothing has read yet. Marks them as loading, so each is returned once.
    pub fn dirs_to_load(&mut self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let mut stack = vec![self.root.path.clone()];
        while let Some(dir) = stack.pop() {
            if !self.expanded.contains(&dir) {
                continue;
            }
            match self.children.get(&dir) {
                Some(entries) => stack.extend(
                    entries
                        .iter()
                        .filter(|e| e.kind == EntryKind::Dir)
                        .map(|e| e.path.clone()),
                ),
                None => {
                    if self.loading.insert(dir.clone()) {
                        dirs.push(dir);
                    }
                }
            }
        }
        dirs
    }

    /// Records a directory's contents. Anything that disappeared from it is forgotten, including
    /// the expansion state of folders inside it.
    pub fn set_children(&mut self, dir: &Path, entries: Vec<DirEntry>) {
        self.loading.remove(dir);
        if let Some(old) = self.children.get(dir) {
            let gone: Vec<PathBuf> = old
                .iter()
                .filter(|o| o.kind == EntryKind::Dir)
                .filter(|o| {
                    !entries
                        .iter()
                        .any(|n| n.path == o.path && n.kind == EntryKind::Dir)
                })
                .map(|o| o.path.clone())
                .collect();
            for path in gone {
                self.forget(&path);
            }
        }
        self.children.insert(dir.to_path_buf(), entries);
    }

    /// Records that a directory couldn't be read, usually because it's gone.
    pub fn load_failed(&mut self, dir: &Path) {
        self.loading.remove(dir);
        if dir == self.root.path {
            self.children.insert(dir.to_path_buf(), Vec::new());
        } else {
            self.forget(dir);
        }
    }

    fn forget(&mut self, dir: &Path) {
        self.children.retain(|path, _| !path.starts_with(dir));
        self.expanded.retain(|path| !path.starts_with(dir));
        self.loading.retain(|path| !path.starts_with(dir));
    }

    /// Expands every folder above a path, so that it becomes visible once they've loaded. Returns
    /// false for paths outside the project.
    pub fn expand_to(&mut self, path: &Path) -> bool {
        if !path.starts_with(&self.root.path) {
            return false;
        }
        let root = self.root.path.clone();
        for ancestor in path.ancestors().skip(1) {
            if !ancestor.starts_with(&root) {
                break;
            }
            self.expanded.insert(ancestor.to_path_buf());
        }
        true
    }

    /// The visible rows, top to bottom. The root is always first.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, &mut rows);
        rows
    }

    fn push_rows(&self, entry: &DirEntry, depth: usize, rows: &mut Vec<Row>) {
        let expanded = entry.kind == EntryKind::Dir && self.expanded.contains(&entry.path);
        rows.push(Row {
            entry: entry.clone(),
            depth,
            expanded,
            loading: expanded && self.loading.contains(&entry.path),
        });
        if expanded && let Some(children) = self.children.get(&entry.path) {
            for child in children {
                self.push_rows(child, depth + 1, rows);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn project(files: &[&str]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            if file.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, "").unwrap();
            }
        }
        dir
    }

    fn names(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn ignored(root: &Path, dir: &str, name: &str) -> bool {
        let entries = read_directory(root, &root.join(dir)).unwrap();
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .ignored
    }

    /// Loads everything the tree asks for, repeating as new folders are revealed.
    fn load_all(tree: &mut FileTree) {
        loop {
            let dirs = tree.dirs_to_load();
            if dirs.is_empty() {
                return;
            }
            let root = tree.root().to_path_buf();
            for dir in dirs {
                let entries = read_directory(&root, &dir).unwrap();
                tree.set_children(&dir, entries);
            }
        }
    }

    fn row_names(tree: &FileTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.entry.name))
            .collect()
    }

    #[test]
    fn folders_come_first_then_natural_order() {
        let dir = project(&["b.txt", "A.txt", "z/", "c/", "file10", "file2"]);
        let entries = read_directory(dir.path(), dir.path()).unwrap();
        assert_eq!(
            names(&entries),
            ["c", "z", "A.txt", "b.txt", "file2", "file10"]
        );
    }

    #[test]
    fn natural_comparison_handles_numbers_and_case() {
        use std::cmp::Ordering::*;
        assert_eq!(natural_cmp("a2", "a10"), Less);
        assert_eq!(natural_cmp("a10", "a2"), Greater);
        assert_eq!(natural_cmp("A", "b"), Less);
        assert_eq!(natural_cmp("a", "a1"), Less);
        assert_eq!(natural_cmp("a01", "a1"), "a01".cmp("a1"));
        assert!(natural_cmp("é", "É").is_ne());
        // Numbers too long for any integer type still compare by value.
        assert_eq!(
            natural_cmp("99999999999999999999999", "100000000000000000000000"),
            Less
        );
    }

    #[test]
    fn dot_git_and_ds_store_are_hidden_but_other_dotfiles_are_not() {
        let dir = project(&[".git/config", ".DS_Store", ".gitignore", ".env"]);
        let entries = read_directory(dir.path(), dir.path()).unwrap();
        assert_eq!(names(&entries), [".env", ".gitignore"]);
    }

    #[test]
    fn gitignored_entries_are_marked_not_hidden() {
        let dir = project(&["target/", "src/", "a.log", "a.rs"]);
        fs::write(dir.path().join(".gitignore"), "target\n*.log\n").unwrap();
        assert!(ignored(dir.path(), "", "target"));
        assert!(ignored(dir.path(), "", "a.log"));
        assert!(!ignored(dir.path(), "", "src"));
        assert!(!ignored(dir.path(), "", "a.rs"));
    }

    #[test]
    fn everything_inside_an_ignored_folder_is_ignored() {
        let dir = project(&["target/debug/app"]);
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        assert!(ignored(dir.path(), "target/debug", "app"));
    }

    #[test]
    fn nested_gitignores_can_override_their_parents() {
        let dir = project(&["sub/keep.log", "sub/other.log", "top.log"]);
        fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        fs::write(dir.path().join("sub/.gitignore"), "!keep.log\n").unwrap();
        assert!(ignored(dir.path(), "", "top.log"));
        assert!(ignored(dir.path(), "sub", "other.log"));
        assert!(!ignored(dir.path(), "sub", "keep.log"));
    }

    #[test]
    fn git_info_exclude_counts() {
        let dir = project(&[".git/info/exclude", "secret.txt", "plain.txt"]);
        fs::write(dir.path().join(".git/info/exclude"), "secret.txt\n").unwrap();
        assert!(ignored(dir.path(), "", "secret.txt"));
        assert!(!ignored(dir.path(), "", "plain.txt"));
    }

    #[test]
    fn a_folder_only_rule_does_not_match_a_file_of_that_name() {
        let dir = project(&["build", "sub/build/"]);
        fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
        assert!(!ignored(dir.path(), "", "build"));
        assert!(ignored(dir.path(), "sub", "build"));
    }

    #[test]
    fn symlinks_report_what_they_point_at() {
        let dir = project(&["real/", "file.txt"]);
        symlink(dir.path().join("real"), dir.path().join("to-dir")).unwrap();
        symlink(dir.path().join("file.txt"), dir.path().join("to-file")).unwrap();
        symlink(dir.path().join("gone"), dir.path().join("dangling")).unwrap();
        let entries = read_directory(dir.path(), dir.path()).unwrap();
        let find = |n: &str| entries.iter().find(|e| e.name == n).unwrap();
        assert_eq!(find("to-dir").kind, EntryKind::Dir);
        assert!(find("to-dir").symlink);
        assert_eq!(find("to-file").kind, EntryKind::File);
        assert_eq!(find("dangling").kind, EntryKind::File);
        assert!(!find("real").symlink);
    }

    #[test]
    fn only_the_root_loads_until_a_folder_is_expanded() {
        let dir = project(&["src/main.rs", "src/deep/x.rs"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        let first = tree.dirs_to_load();
        assert_eq!(first, vec![dir.path().to_path_buf()]);
        assert!(
            tree.dirs_to_load().is_empty(),
            "each load is only requested once"
        );

        let root = dir.path().to_path_buf();
        tree.set_children(&root, read_directory(&root, &root).unwrap());
        assert!(!tree.is_loaded(&dir.path().join("src")));
        assert_eq!(tree.rows().len(), 2);

        tree.expand(&dir.path().join("src"));
        assert_eq!(tree.dirs_to_load(), vec![dir.path().join("src")]);
    }

    #[test]
    fn rows_flatten_expanded_folders_with_depth() {
        let dir = project(&["a/b/c.txt", "a/d.txt", "e.txt"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        tree.expand(&dir.path().join("a"));
        tree.expand(&dir.path().join("a/b"));
        load_all(&mut tree);
        let rows = row_names(&tree);
        assert_eq!(
            rows[1..],
            ["  a", "    b", "      c.txt", "    d.txt", "  e.txt"]
        );
    }

    #[test]
    fn collapsing_hides_children_but_remembers_inner_state() {
        let dir = project(&["a/b/c.txt"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        tree.expand(&dir.path().join("a"));
        tree.expand(&dir.path().join("a/b"));
        load_all(&mut tree);
        assert_eq!(tree.rows().len(), 4);
        tree.collapse(&dir.path().join("a"));
        assert_eq!(tree.rows().len(), 2);
        tree.expand(&dir.path().join("a"));
        assert_eq!(tree.rows().len(), 4);
    }

    #[test]
    fn rows_show_a_folder_as_loading_until_it_arrives() {
        let dir = project(&["a/b.txt"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        load_all(&mut tree);
        tree.expand(&dir.path().join("a"));
        tree.dirs_to_load();
        assert!(tree.rows()[1].loading);
        tree.set_children(
            &dir.path().join("a"),
            read_directory(dir.path(), &dir.path().join("a")).unwrap(),
        );
        assert!(!tree.rows()[1].loading);
    }

    #[test]
    fn expand_to_reveals_a_deep_path() {
        let dir = project(&["a/b/c/d.txt"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        load_all(&mut tree);
        assert!(tree.expand_to(&dir.path().join("a/b/c/d.txt")));
        load_all(&mut tree);
        assert!(tree.rows().iter().any(|r| r.entry.name == "d.txt"));
    }

    #[test]
    fn expand_to_refuses_paths_outside_the_project() {
        let dir = project(&["a.txt"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        assert!(!tree.expand_to(Path::new("/etc/hosts")));
        assert!(tree.expanded_dirs().count() == 1);
    }

    #[test]
    fn a_folder_that_vanishes_is_forgotten_with_its_contents() {
        let dir = project(&["a/b/c.txt", "z.txt"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        tree.expand(&dir.path().join("a"));
        tree.expand(&dir.path().join("a/b"));
        load_all(&mut tree);

        fs::remove_dir_all(dir.path().join("a")).unwrap();
        let root = dir.path().to_path_buf();
        tree.set_children(&root, read_directory(&root, &root).unwrap());

        assert_eq!(tree.rows().len(), 2);
        assert!(!tree.is_loaded(&dir.path().join("a/b")));
        assert!(!tree.is_expanded(&dir.path().join("a")));
        assert!(tree.dirs_to_load().is_empty());
    }

    #[test]
    fn a_failed_load_forgets_the_folder_but_not_the_root() {
        let dir = project(&["a/"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        tree.expand(&dir.path().join("a"));
        load_all(&mut tree);
        tree.load_failed(&dir.path().join("a"));
        assert!(!tree.is_expanded(&dir.path().join("a")));

        let root = dir.path().to_path_buf();
        tree.load_failed(&root);
        assert!(tree.is_expanded(&root));
        assert!(tree.dirs_to_load().is_empty(), "it shouldn't retry forever");
    }

    #[test]
    fn a_new_file_appears_when_its_folder_is_refreshed() {
        let dir = project(&["a.txt"]);
        let mut tree = FileTree::new(dir.path().to_path_buf());
        load_all(&mut tree);
        fs::write(dir.path().join("b.txt"), "").unwrap();
        let root = dir.path().to_path_buf();
        tree.set_children(&root, read_directory(&root, &root).unwrap());
        assert_eq!(tree.rows().len(), 3);
    }
}
