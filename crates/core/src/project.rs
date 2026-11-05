//! What's in an open project, kept current: its files, their symbols and their git status. There's
//! one per window, and the project tree and search both read from it, so the folder is only
//! watched and scanned once.

use futures::StreamExt as _;
use gleipnir_index::{
    FileChanges, FileList, GitStatus, ProjectWatcher, SymbolEntry, git, is_ignored, scan_files,
};
use gleipnir_packages::registry;
use gleipnir_syntax::{Symbol, extract_symbols};
use gpui::{AppContext as _, Context, EventEmitter, Task};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Files larger than this aren't searched for symbols. Generated code and bundles get this big,
/// and their symbols are noise.
const MAX_SYMBOL_FILE_SIZE: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub enum ProjectEvent {
    /// Paths changed on disk, as reported by the watcher. A hint: re-read what you care about.
    FilesChanged(Arc<Vec<PathBuf>>),
    /// Files were added to or removed from the searchable list.
    FileListChanged,
    SymbolsChanged,
    GitStatusChanged,
}

/// How far the symbol index has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexStatus {
    pub files: usize,
    pub symbols: usize,
    /// While indexing: (done, total).
    pub indexing: Option<(usize, usize)>,
    pub scanning: bool,
}

pub struct Project {
    root: PathBuf,
    files: Arc<Mutex<FileList>>,
    files_snapshot: Option<Arc<Vec<Arc<str>>>>,
    symbols: HashMap<Arc<str>, Arc<[Symbol]>>,
    symbols_snapshot: Option<Arc<Vec<SymbolEntry>>>,
    symbol_count: usize,
    scanning: bool,
    /// Files waiting for symbols, collected while a run is in progress.
    symbol_queue: Vec<Arc<str>>,
    indexing: Option<(Arc<AtomicUsize>, usize)>,
    /// Changed paths waiting to be applied to the file list, in order.
    change_queue: Vec<PathBuf>,
    applying_changes: bool,
    git: GitStatus,
    git_running: bool,
    git_rerun: bool,
    _watcher: Option<ProjectWatcher>,
    _watch_task: Option<Task<()>>,
}

impl EventEmitter<ProjectEvent> for Project {}

impl Project {
    /// Opens a project, scanning and watching it in the background.
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            files: Arc::new(Mutex::new(FileList::default())),
            files_snapshot: None,
            symbols: HashMap::new(),
            symbols_snapshot: None,
            symbol_count: 0,
            scanning: true,
            symbol_queue: Vec::new(),
            indexing: None,
            change_queue: Vec::new(),
            applying_changes: false,
            git: GitStatus::default(),
            git_running: false,
            git_rerun: false,
            _watcher: None,
            _watch_task: None,
        };
        this.start_watching(cx);
        this.start_scan(cx);
        this.refresh_git(cx);
        this
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git(&self) -> &GitStatus {
        &self.git
    }

    pub fn status(&self) -> IndexStatus {
        IndexStatus {
            files: self.files.lock().expect("the file list's lock").len(),
            symbols: self.symbol_count,
            indexing: self
                .indexing
                .as_ref()
                .map(|(done, total)| (done.load(Ordering::Relaxed).min(*total), *total)),
            scanning: self.scanning,
        }
    }

    /// Whether anything is still being scanned, indexed or applied. Lets tests wait for quiet.
    pub fn is_busy(&self) -> bool {
        self.scanning || self.indexing.is_some() || self.applying_changes || self.git_running
    }

    /// The searchable files, relative to the root. Cheap to call repeatedly: it's rebuilt only
    /// after the list changes.
    pub fn files(&mut self) -> Arc<Vec<Arc<str>>> {
        self.files_snapshot
            .get_or_insert_with(|| self.files.lock().expect("the file list's lock").snapshot())
            .clone()
    }

    pub fn symbols(&mut self) -> Arc<Vec<SymbolEntry>> {
        self.symbols_snapshot
            .get_or_insert_with(|| {
                let mut entries: Vec<SymbolEntry> = self
                    .symbols
                    .iter()
                    .flat_map(|(path, symbols)| {
                        symbols.iter().map(|symbol| SymbolEntry {
                            path: path.clone(),
                            symbol: symbol.clone(),
                        })
                    })
                    .collect();
                // A stable order, so results don't jump around between searches.
                entries.sort_by(|a, b| {
                    (&a.path, a.symbol.line, a.symbol.column).cmp(&(
                        &b.path,
                        b.symbol.line,
                        b.symbol.column,
                    ))
                });
                Arc::new(entries)
            })
            .clone()
    }

    fn start_watching(&mut self, cx: &mut Context<Self>) {
        let Ok((watcher, mut changes)) = gleipnir_index::watch(&self.root) else {
            return;
        };
        self._watcher = Some(watcher);
        self._watch_task = Some(cx.spawn(async move |this, cx| {
            while let Some(paths) = changes.next().await {
                if this
                    .update(cx, |this, cx| this.files_changed(paths, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn start_scan(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let scan = cx.background_spawn(async move { scan_files(&root) });
        cx.spawn(async move |this, cx| {
            let files = scan.await;
            this.update(cx, |this, cx| {
                let indexable: Vec<Arc<str>> =
                    files.iter().filter(|f| is_indexable(f)).cloned().collect();
                *this.files.lock().expect("the file list's lock") = FileList::new(files);
                this.files_snapshot = None;
                this.scanning = false;
                cx.emit(ProjectEvent::FileListChanged);
                this.queue_symbols(indexable, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn files_changed(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.git_relevant(&paths) {
            self.refresh_git(cx);
        }
        let paths = Arc::new(paths);
        cx.emit(ProjectEvent::FilesChanged(paths.clone()));
        self.change_queue.extend(paths.iter().cloned());
        self.apply_queued_changes(cx);
    }

    /// Whether a batch of changes could change git's status. Of git's own files, only a few mean
    /// that it has. Elsewhere, anything inside an ignored top-level folder, like `target` or
    /// `node_modules`, doesn't, and builds churn through those constantly.
    fn git_relevant(&self, paths: &[PathBuf]) -> bool {
        let mut checked: HashMap<PathBuf, bool> = HashMap::new();
        paths.iter().any(|path| {
            let Ok(relative) = path.strip_prefix(&self.root) else {
                return false;
            };
            let Some(first) = relative.components().next() else {
                return true;
            };
            if first.as_os_str() == ".git" {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                return matches!(name, "index" | "HEAD" | "MERGE_HEAD" | "ORIG_HEAD");
            }
            let top = self.root.join(first);
            let ignored = *checked
                .entry(top.clone())
                .or_insert_with(|| top != *path && is_ignored(&self.root, &top, true));
            !ignored
        })
    }

    /// Applies watcher changes to the file list off the main thread, one batch at a time, then
    /// refreshes the symbols of whatever changed.
    fn apply_queued_changes(&mut self, cx: &mut Context<Self>) {
        if self.applying_changes || self.scanning || self.change_queue.is_empty() {
            return;
        }
        self.applying_changes = true;
        let paths = std::mem::take(&mut self.change_queue);
        let files = self.files.clone();
        let root = self.root.clone();
        let apply = cx.background_spawn(async move {
            files
                .lock()
                .expect("the file list's lock")
                .apply_changes(&root, &paths)
        });
        cx.spawn(async move |this, cx| {
            let changes: FileChanges = apply.await;
            this.update(cx, |this, cx| {
                this.applying_changes = false;
                if !changes.is_empty() {
                    this.files_snapshot = None;
                    cx.emit(ProjectEvent::FileListChanged);
                }
                if !changes.removed.is_empty() {
                    for file in &changes.removed {
                        this.symbols.remove(file);
                    }
                    this.symbols_changed(cx);
                }
                let indexable: Vec<Arc<str>> = changes
                    .updated
                    .into_iter()
                    .filter(|f| is_indexable(f))
                    .collect();
                this.queue_symbols(indexable, cx);
                this.apply_queued_changes(cx);
            })
            .ok();
        })
        .detach();
    }

    fn queue_symbols(&mut self, files: Vec<Arc<str>>, cx: &mut Context<Self>) {
        self.symbol_queue.extend(files);
        self.index_queued_symbols(cx);
    }

    /// Reads and parses queued files on every core. Progress can be read while it runs.
    fn index_queued_symbols(&mut self, cx: &mut Context<Self>) {
        if self.indexing.is_some() || self.symbol_queue.is_empty() {
            return;
        }
        let mut files = std::mem::take(&mut self.symbol_queue);
        files.sort();
        files.dedup();
        let done = Arc::new(AtomicUsize::new(0));
        self.indexing = Some((done.clone(), files.len()));
        let root = self.root.clone();
        let run = cx.background_spawn(async move { index_symbols(&root, files, &done) });
        cx.spawn(async move |this, cx| {
            let results = run.await;
            this.update(cx, |this, cx| {
                for (path, symbols) in results {
                    match symbols {
                        Some(symbols) => this.symbols.insert(path, symbols.into()),
                        None => this.symbols.remove(&path),
                    };
                }
                this.indexing = None;
                this.symbols_changed(cx);
                this.index_queued_symbols(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn symbols_changed(&mut self, cx: &mut Context<Self>) {
        self.symbols_snapshot = None;
        self.symbol_count = self.symbols.values().map(|s| s.len()).sum();
        cx.emit(ProjectEvent::SymbolsChanged);
        cx.notify();
    }

    pub fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git_running {
            self.git_rerun = true;
            return;
        }
        self.git_running = true;
        let root = self.root.clone();
        let read = cx.background_spawn(async move { git::read_status(&root) });
        cx.spawn(async move |this, cx| {
            let status = read.await;
            this.update(cx, |this, cx| {
                this.git_running = false;
                if let Ok(status) = status
                    && status != this.git
                {
                    this.git = status;
                    cx.emit(ProjectEvent::GitStatusChanged);
                }
                if std::mem::take(&mut this.git_rerun) {
                    this.refresh_git(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn is_indexable(relative: &str) -> bool {
    registry().language_for_path(Path::new(relative)).is_some()
}

/// Extracts symbols from files in parallel. `None` means the file is gone, or too big to bother.
fn index_symbols(
    root: &Path,
    files: Vec<Arc<str>>,
    done: &AtomicUsize,
) -> Vec<(Arc<str>, Option<Vec<Symbol>>)> {
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    let chunk = files.len().div_ceil(threads).max(16);
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|slice| {
                scope.spawn(move || {
                    slice
                        .iter()
                        .map(|relative| {
                            let symbols = symbols_for(root, relative);
                            done.fetch_add(1, Ordering::Relaxed);
                            (relative.clone(), symbols)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("an indexing thread"))
            .collect()
    })
}

fn symbols_for(root: &Path, relative: &str) -> Option<Vec<Symbol>> {
    let path = root.join(relative);
    let language = registry().language_for_path(&path)?.grammar;
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_SYMBOL_FILE_SIZE {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    Some(extract_symbols(language, &text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext};
    use std::fs;
    use std::time::{Duration, Instant};

    fn project_dir(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for (path, text) in files {
            let full = root.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        (dir, root)
    }

    fn wait(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn idle(project: &Entity<Project>, cx: &mut TestAppContext) {
        wait(cx, |cx| project.read_with(cx, |p, _| !p.is_busy()));
    }

    fn files(project: &Entity<Project>, cx: &mut TestAppContext) -> Vec<String> {
        project.update(cx, |p, _| p.files().iter().map(|f| f.to_string()).collect())
    }

    fn symbol_names(project: &Entity<Project>, cx: &mut TestAppContext) -> Vec<String> {
        project.update(cx, |p, _| {
            p.symbols().iter().map(|e| e.symbol.name.clone()).collect()
        })
    }

    #[gpui::test]
    fn the_project_is_scanned_and_its_symbols_indexed(cx: &mut TestAppContext) {
        let (_dir, root) = project_dir(&[
            ("src/lib.rs", "pub struct Engine;\nfn start() {}"),
            ("app.go", "package main\nfunc Run() {}"),
            ("notes.md", "# not code"),
        ]);
        let project = cx.new(|cx| Project::new(root, cx));
        idle(&project, cx);
        assert_eq!(files(&project, cx), ["app.go", "notes.md", "src/lib.rs"]);
        assert_eq!(symbol_names(&project, cx), ["Run", "Engine", "start"]);
        let status = project.read_with(cx, |p, _| p.status());
        assert_eq!(
            (status.files, status.symbols, status.indexing),
            (3, 3, None)
        );
    }

    #[gpui::test]
    fn new_changed_and_deleted_files_are_followed(cx: &mut TestAppContext) {
        let (_dir, root) = project_dir(&[("a.rs", "fn old_name() {}"), ("b.rs", "fn doomed() {}")]);
        let project = cx.new(|cx| Project::new(root.clone(), cx));
        idle(&project, cx);
        std::thread::sleep(Duration::from_millis(200));

        fs::write(root.join("a.rs"), "fn new_name() {}").unwrap();
        fs::write(root.join("c.rs"), "struct Fresh;").unwrap();
        fs::remove_file(root.join("b.rs")).unwrap();
        wait(cx, |cx| {
            let names = symbol_names(&project, cx);
            names.contains(&"new_name".to_string())
                && names.contains(&"Fresh".to_string())
                && !names.contains(&"doomed".to_string())
        });
        assert_eq!(files(&project, cx), ["a.rs", "c.rs"]);
        assert!(!symbol_names(&project, cx).contains(&"old_name".to_string()));
    }

    #[gpui::test]
    fn huge_files_are_listed_but_not_indexed(cx: &mut TestAppContext) {
        let big = "fn f() {}\n".repeat(120_000);
        let (_dir, root) = project_dir(&[("big.rs", &big), ("small.rs", "fn small() {}")]);
        let project = cx.new(|cx| Project::new(root, cx));
        idle(&project, cx);
        assert_eq!(files(&project, cx).len(), 2);
        assert_eq!(symbol_names(&project, cx), ["small"]);
    }

    #[gpui::test]
    fn git_status_is_read_and_refreshed(cx: &mut TestAppContext) {
        let (_dir, root) = project_dir(&[("a.rs", "1")]);
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&["-c", "user.email=t@e.com", "-c", "user.name=t", "add", "."]);
        git(&[
            "-c",
            "user.email=t@e.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "x",
        ]);
        let project = cx.new(|cx| Project::new(root.clone(), cx));
        idle(&project, cx);
        assert!(project.read_with(cx, |p, _| p.git().is_empty()));
        std::thread::sleep(Duration::from_millis(200));
        fs::write(root.join("a.rs"), "2").unwrap();
        let path = root.join("a.rs");
        wait(cx, |cx| {
            project.read_with(cx, |p, _| p.git().state(&path).is_some())
        });
    }

    #[gpui::test]
    fn changes_in_ignored_folders_do_not_refresh_git(cx: &mut TestAppContext) {
        let (_dir, root) = project_dir(&[(".gitignore", "target/\n"), ("a.rs", "")]);
        fs::create_dir(root.join("target")).unwrap();
        let project = cx.new(|cx| Project::new(root.clone(), cx));
        idle(&project, cx);
        project.read_with(cx, |p, _| {
            assert!(!p.git_relevant(&[root.join("target/debug/x.o"), root.join("target/y")]));
            assert!(p.git_relevant(&[root.join("target/x.o"), root.join("a.rs")]));
            assert!(p.git_relevant(&[root.join(".git/index")]));
            assert!(!p.git_relevant(&[root.join(".git/objects/ab/cdef")]));
        });
    }
}
