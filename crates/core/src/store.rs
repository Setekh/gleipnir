use crate::buffer::{BufferModel, read_text_file};
use anyhow::{Result, anyhow};
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{AppContext as _, Context, Entity, Task, WeakEntity};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

type OpenResult = std::result::Result<Entity<BufferModel>, Arc<anyhow::Error>>;

/// Hands out one `BufferModel` per file, however many views ask for it. Buffers are held weakly,
/// so one is freed when the last view on it closes.
pub struct BufferStore {
    buffers: HashMap<PathBuf, WeakEntity<BufferModel>>,
    opening: HashMap<PathBuf, Shared<Task<OpenResult>>>,
    /// Watches the folder of every open file, so a change on disk shows at once.
    watcher: Option<gleipnir_index::DirWatcher>,
    _watch: Option<Task<()>>,
    _poll: Option<Task<()>>,
}

impl BufferStore {
    /// Every open buffer reads its committed text again, as after a commit or a checkout.
    pub fn refresh_committed_texts(&self, cx: &mut gpui::App) {
        for buffer in self.buffers.values().filter_map(|b| b.upgrade()) {
            buffer.update(cx, |buffer, cx| buffer.refresh_committed_text(cx));
        }
    }

    /// Every open buffer takes up indentation settings as they change.
    pub fn apply_code_style(&self, cx: &mut gpui::App) {
        for buffer in self.buffers.values().filter_map(|b| b.upgrade()) {
            buffer.update(cx, |buffer, cx| buffer.apply_code_style(cx));
        }
    }

    pub fn new() -> Self {
        Self {
            buffers: HashMap::new(),
            opening: HashMap::new(),
            watcher: None,
            _watch: None,
            _poll: None,
        }
    }

    /// Watches the folders of open files for changes from other programs, wherever they are.
    pub fn start_watching(&mut self, cx: &mut Context<Self>) {
        let Ok((watcher, mut changes)) = gleipnir_index::watch_dirs() else {
            return;
        };
        self.watcher = Some(watcher);
        for path in self.buffers.keys().cloned().collect::<Vec<_>>() {
            self.watch_folder_of(&path);
        }
        self._watch = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(paths) = changes.next().await {
                if this
                    .update(cx, |this, cx| this.check_paths(&paths, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn watch_folder_of(&mut self, path: &Path) {
        if let (Some(watcher), Some(folder)) = (&mut self.watcher, path.parent()) {
            watcher.add(folder).ok();
        }
    }

    /// Checks the open buffers among some changed paths, or in changed folders, against disk.
    pub fn check_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        for buffer in self.open_buffers() {
            let affected = buffer.read(cx).path().is_some_and(|path| {
                paths
                    .iter()
                    .any(|changed| changed == path || path.parent() == Some(changed.as_path()))
            });
            if affected {
                buffer.update(cx, |buffer, cx| buffer.check_disk(cx));
            }
        }
    }

    /// The buffer open for a file, if there is one, without opening it.
    pub fn get(&self, path: &Path) -> Option<Entity<BufferModel>> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.buffers.get(&path).and_then(WeakEntity::upgrade)
    }

    /// Opens a file, or returns the buffer already open for it. The path is canonicalized first,
    /// so a symlink and its target, or two spellings of a path, share one buffer.
    pub fn open(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<BufferModel>>> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(buffer) = self.buffers.get(&path).and_then(WeakEntity::upgrade) {
            return Task::ready(Ok(buffer));
        }

        let shared = match self.opening.get(&path) {
            Some(pending) => pending.clone(),
            None => {
                let shared = self.start_open(path.clone(), cx);
                self.opening.insert(path, shared.clone());
                shared
            }
        };
        cx.background_spawn(async move { shared.await.map_err(|e| anyhow!("{e:#}")) })
    }

    fn start_open(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Shared<Task<OpenResult>> {
        let read_path = path.clone();
        let read = cx.background_spawn(async move { read_text_file(&read_path) });
        let task: Task<OpenResult> = cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                this.opening.remove(&path);
                let (text, mtime) = result.map_err(Arc::new)?;
                let buffer = cx
                    .new(|cx| BufferModel::with_mtime(&text, Some(path.clone()), Some(mtime), cx));
                this.watch_folder_of(&path);
                this.buffers.insert(path, buffer.downgrade());
                Ok(buffer)
            })
            .map_err(Arc::new)?
        });
        task.shared()
    }

    /// The buffers currently open, dropping any whose views have all closed.
    pub fn open_buffers(&mut self) -> Vec<Entity<BufferModel>> {
        self.buffers.retain(|_, weak| weak.upgrade().is_some());
        self.buffers
            .values()
            .filter_map(WeakEntity::upgrade)
            .collect()
    }

    /// Updates open buffers after a file or folder moved, so their tabs keep working. Everything
    /// at or under `from` is affected.
    pub fn paths_moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        let moved: Vec<(PathBuf, PathBuf)> = self
            .buffers
            .keys()
            .filter_map(|path| {
                let relative = path.strip_prefix(from).ok()?;
                // Joining an empty path would add a trailing slash to the file itself.
                let new = if relative.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(relative)
                };
                Some((path.clone(), new))
            })
            .collect();
        for (old, new) in moved {
            if let Some(weak) = self.buffers.remove(&old) {
                if let Some(buffer) = weak.upgrade() {
                    let path = new.clone();
                    buffer.update(cx, |buffer, cx| buffer.file_moved(path, cx));
                }
                self.buffers.insert(new, weak);
            }
        }
    }

    /// Checks every open buffer against its file.
    pub fn check_disk(&mut self, cx: &mut Context<Self>) {
        for buffer in self.open_buffers() {
            buffer.update(cx, |buffer, cx| buffer.check_disk(cx));
        }
    }

    /// Starts checking for external changes on an interval: a safety net under watching, as watchers
    /// can drop events, such as when a whole folder is replaced.
    pub fn start_polling(&mut self, interval: Duration, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                if this.update(cx, |this, cx| this.check_disk(cx)).is_err() {
                    break;
                }
            }
        }));
    }
}

impl Default for BufferStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BufferEvent, DiskStatus};
    use gleipnir_text::ops::EditContext;
    use gleipnir_text::{SelectionSet, UndoGrouping};
    use gpui::{EntityId, TestAppContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn view_id(n: u64) -> EntityId {
        EntityId::from(n)
    }

    /// Brings every background file operation to completion. They block on real IO, which the
    /// test scheduler can't see, so it has to be allowed to wait for them.
    fn settle(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        for _ in 0..4 {
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(5));
        }
        cx.run_until_parked();
    }

    async fn open(
        store: &Entity<BufferStore>,
        path: &Path,
        cx: &mut TestAppContext,
    ) -> Result<Entity<BufferModel>> {
        cx.executor().allow_parking();
        store.update(cx, |store, cx| store.open(path, cx)).await
    }

    fn insert(buffer: &Entity<BufferModel>, text: &str, cx: &mut TestAppContext) {
        buffer.update(cx, |buffer, cx| {
            let mut selections = SelectionSet::cursor(buffer.buffer().len());
            buffer.edit(&mut selections, view_id(1), cx, |edit: &mut EditContext| {
                ((), edit.insert(text, UndoGrouping::Distinct))
            });
        });
    }

    fn text_of(buffer: &Entity<BufferModel>, cx: &TestAppContext) -> String {
        buffer.read_with(cx, |buffer, _| buffer.buffer().text())
    }

    fn is_dirty(buffer: &Entity<BufferModel>, cx: &TestAppContext) -> bool {
        buffer.read_with(cx, |buffer, _| buffer.buffer().is_dirty())
    }

    /// Filesystem timestamps have limited resolution, so give a rewrite a distinct mtime.
    fn rewrite(path: &Path, text: &str) {
        std::thread::sleep(Duration::from_millis(20));
        fs::write(path, text).unwrap();
    }

    #[gpui::test]
    async fn two_opens_of_one_path_share_a_buffer(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "fn a() {}\n").unwrap();
        let store = cx.new(|_| BufferStore::new());

        let first = open(&store, &path, cx).await.unwrap();
        let second = open(&store, &path, cx).await.unwrap();
        assert_eq!(first.entity_id(), second.entity_id());
        assert_eq!(text_of(&first, cx), "fn a() {}\n");
    }

    #[gpui::test]
    async fn opens_racing_each_other_share_a_buffer(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "x").unwrap();
        let store = cx.new(|_| BufferStore::new());
        cx.executor().allow_parking();

        let (first, second) = store.update(cx, |store, cx| {
            (store.open(&path, cx), store.open(&path, cx))
        });
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert_eq!(first.entity_id(), second.entity_id());
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn a_symlink_and_its_target_share_a_buffer(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("real.rs");
        let link = dir.path().join("link.rs");
        fs::write(&path, "x").unwrap();
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let store = cx.new(|_| BufferStore::new());

        let real = open(&store, &path, cx).await.unwrap();
        let linked = open(&store, &link, cx).await.unwrap();
        assert_eq!(real.entity_id(), linked.entity_id());
    }

    #[gpui::test]
    async fn a_buffer_is_freed_when_nothing_holds_it(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "x").unwrap();
        let store = cx.new(|_| BufferStore::new());

        let buffer = open(&store, &path, cx).await.unwrap();
        assert_eq!(store.update(cx, |s, _| s.open_buffers().len()), 1);
        drop(buffer);
        settle(cx);
        assert_eq!(store.update(cx, |s, _| s.open_buffers().len()), 0);
    }

    #[gpui::test]
    async fn opening_a_missing_file_is_an_error(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let store = cx.new(|_| BufferStore::new());
        assert!(open(&store, &dir.path().join("nope"), cx).await.is_err());
    }

    #[gpui::test]
    async fn opening_a_directory_is_an_error(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let store = cx.new(|_| BufferStore::new());
        let error = open(&store, dir.path(), cx).await.unwrap_err();
        assert!(error.to_string().contains("directory"), "{error}");
    }

    #[gpui::test]
    async fn non_utf8_files_refuse_to_open_rather_than_get_corrupted(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary");
        fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        let store = cx.new(|_| BufferStore::new());
        let error = open(&store, &path, cx).await.unwrap_err();
        assert!(error.to_string().contains("UTF-8"), "{error}");
    }

    #[gpui::test]
    async fn saving_writes_the_file_and_clears_dirty(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "one").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        insert(&buffer, " two", cx);
        assert!(is_dirty(&buffer, cx));
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "one two");
        assert!(!is_dirty(&buffer, cx));
    }

    #[gpui::test]
    async fn line_endings_and_bom_survive_a_save(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "\u{feff}one\r\ntwo\r\n").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        assert_eq!(text_of(&buffer, cx), "one\ntwo\n");
        insert(&buffer, "three\n", cx);
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\u{feff}one\r\ntwo\r\nthree\r\n"
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn saving_keeps_permissions(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        insert(&buffer, "echo\n", cx);
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn saving_writes_through_a_symlink_rather_than_replacing_it(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        fs::write(&real, "a").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let store = cx.new(|_| BufferStore::new());

        let buffer = open(&store, &link, cx).await.unwrap();
        insert(&buffer, "b", cx);
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "ab");
    }

    #[gpui::test]
    async fn saving_leaves_no_temporary_files(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "a").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();
        insert(&buffer, "b", cx);
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn a_failed_save_keeps_the_buffer_dirty_and_reports_it(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "a").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();
        insert(&buffer, "b", cx);

        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let result = buffer.update(cx, |b, cx| b.save(cx)).await;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        // Root ignores directory permissions, so this can't fail in a root container.
        if result.is_err() {
            assert!(is_dirty(&buffer, cx));
            assert_eq!(fs::read_to_string(&path).unwrap(), "a");
        }
    }

    #[gpui::test]
    async fn a_watched_file_reloads_when_changed_elsewhere(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(dir.path()).unwrap().join("a.txt");
        fs::write(&path, "one\ntwo\n").unwrap();
        let store = cx.new(|cx| {
            let mut store = BufferStore::new();
            store.start_watching(cx);
            store
        });
        let buffer = open(&store, &path, cx).await.unwrap();
        let edits = Rc::new(RefCell::new(Vec::new()));
        let seen = edits.clone();
        cx.update(|cx| {
            cx.subscribe(&buffer, move |_, event: &BufferEvent, _| {
                if let BufferEvent::Edited { edits, .. } = event {
                    seen.borrow_mut().extend(edits.iter().map(|e| e.start.line));
                }
            })
            .detach()
        });
        // Long enough for the watcher to settle, and for the modification time to differ.
        std::thread::sleep(Duration::from_millis(300));
        fs::write(&path, "one\nTWO\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while text_of(&buffer, cx) != "one\nTWO\n" {
            assert!(std::time::Instant::now() < deadline, "never reloaded");
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        // Only the changed line was replaced.
        assert_eq!(*edits.borrow(), [1]);
    }

    #[gpui::test]
    async fn typing_during_a_save_stays_dirty(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "a").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        insert(&buffer, "b", cx);
        let save = buffer.update(cx, |b, cx| b.save(cx));
        insert(&buffer, "c", cx);
        save.await.unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "ab");
        assert!(is_dirty(&buffer, cx));
        assert_eq!(text_of(&buffer, cx), "abc");
    }

    #[gpui::test]
    async fn a_clean_buffer_reloads_when_the_file_changes(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "old").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        cx.update(|cx| {
            cx.subscribe(&buffer, move |_, event: &BufferEvent, _| {
                if let BufferEvent::Edited { origin, .. } = event {
                    log.borrow_mut().push(*origin);
                }
            })
            .detach();
        });

        rewrite(&path, "new");
        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);

        assert_eq!(text_of(&buffer, cx), "new");
        assert!(!is_dirty(&buffer, cx));
        // Views need to hear about it, marked as not coming from any of them.
        assert_eq!(*events.borrow(), vec![None]);
    }

    #[gpui::test]
    async fn a_dirty_buffer_keeps_its_edits_when_the_file_changes(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "old").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();
        insert(&buffer, " mine", cx);

        rewrite(&path, "theirs");
        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);

        assert_eq!(text_of(&buffer, cx), "old mine");
        assert_eq!(
            buffer.read_with(cx, |b, _| b.disk_status()),
            DiskStatus::Conflict
        );
    }

    #[gpui::test]
    async fn a_conflict_can_be_resolved_either_way(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "old").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        // Keeping ours: saving overwrites the file and clears the flag.
        insert(&buffer, " mine", cx);
        rewrite(&path, "theirs");
        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "old mine");
        assert_eq!(
            buffer.read_with(cx, |b, _| b.disk_status()),
            DiskStatus::InSync
        );

        // Taking theirs: reloading discards ours.
        insert(&buffer, " more", cx);
        rewrite(&path, "theirs again");
        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);
        assert_eq!(
            buffer.read_with(cx, |b, _| b.disk_status()),
            DiskStatus::Conflict
        );
        buffer
            .update(cx, |b, cx| b.reload_from_disk(cx))
            .await
            .unwrap();
        assert_eq!(text_of(&buffer, cx), "theirs again");
        assert!(!is_dirty(&buffer, cx));
        assert_eq!(
            buffer.read_with(cx, |b, _| b.disk_status()),
            DiskStatus::InSync
        );
    }

    #[gpui::test]
    async fn our_own_save_is_not_mistaken_for_an_external_change(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "a").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();
        insert(&buffer, "b", cx);
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();
        insert(&buffer, "c", cx);

        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);

        assert_eq!(
            buffer.read_with(cx, |b, _| b.disk_status()),
            DiskStatus::InSync
        );
        assert_eq!(text_of(&buffer, cx), "abc");
    }

    #[gpui::test]
    async fn a_touched_but_identical_file_is_not_a_conflict(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "same").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        rewrite(&path, "same");
        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);

        assert_eq!(
            buffer.read_with(cx, |b, _| b.disk_status()),
            DiskStatus::InSync
        );
    }

    #[gpui::test]
    async fn a_deleted_file_is_flagged_and_the_text_kept(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "keep me").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        fs::remove_file(&path).unwrap();
        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);

        assert_eq!(
            buffer.read_with(cx, |b, _| b.disk_status()),
            DiskStatus::Deleted
        );
        assert_eq!(text_of(&buffer, cx), "keep me");
    }

    #[gpui::test]
    async fn saving_a_deleted_file_recreates_it(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, "keep me").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();
        fs::remove_file(&path).unwrap();

        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
    }

    #[gpui::test]
    async fn buffers_follow_a_renamed_file_and_keep_their_edits(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = fs::canonicalize(dir.path()).unwrap();
        let path = dir_path.join("a.txt");
        fs::write(&path, "text").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();
        insert(&buffer, " unsaved", cx);

        let new_path = dir_path.join("b.rs");
        fs::rename(&path, &new_path).unwrap();
        store.update(cx, |s, cx| s.paths_moved(&path, &new_path, cx));

        buffer.read_with(cx, |b, _| {
            assert_eq!(b.path(), Some(new_path.as_path()));
            assert_eq!(b.language(), Some(gleipnir_syntax::Language::Rust));
            assert_eq!(b.buffer().text(), "text unsaved");
        });
        // Opening the new path finds the same buffer.
        let reopened = open(&store, &new_path, cx).await.unwrap();
        assert_eq!(reopened.entity_id(), buffer.entity_id());
        buffer.update(cx, |b, cx| b.save(cx)).await.unwrap();
        assert_eq!(fs::read_to_string(&new_path).unwrap(), "text unsaved");
    }

    #[gpui::test]
    async fn buffers_follow_a_moved_folder(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(dir_path.join("old/sub")).unwrap();
        let inner = dir_path.join("old/sub/a.txt");
        fs::write(&inner, "x").unwrap();
        let other = dir_path.join("older.txt");
        fs::write(&other, "y").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &inner, cx).await.unwrap();
        let unrelated = open(&store, &other, cx).await.unwrap();

        let (from, to) = (dir_path.join("old"), dir_path.join("new"));
        fs::rename(&from, &to).unwrap();
        store.update(cx, |s, cx| s.paths_moved(&from, &to, cx));

        let expected = to.join("sub/a.txt");
        assert_eq!(
            buffer.read_with(cx, |b, _| b.path().map(Path::to_path_buf)),
            Some(expected)
        );
        // `older.txt` shares a text prefix with `old`, but isn't inside it.
        assert_eq!(
            unrelated.read_with(cx, |b, _| b.path().map(Path::to_path_buf)),
            Some(other)
        );
    }

    #[gpui::test]
    async fn a_buffer_parses_in_the_background_and_after_edits(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "fn a() {}\n").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();

        settle(cx);
        assert!(!buffer.read_with(cx, |b, _| b.is_parse_pending()));
        let kind = buffer.read_with(cx, |b, _| {
            b.syntax()
                .unwrap()
                .tree()
                .unwrap()
                .root_node()
                .named_child(0)
                .unwrap()
                .kind()
                .to_string()
        });
        assert_eq!(kind, "function_item");

        insert(&buffer, "struct S;\n", cx);
        assert!(buffer.read_with(cx, |b, _| b.is_parse_pending()));
        settle(cx);
        let count = buffer.read_with(cx, |b, _| {
            assert!(!b.is_parse_pending());
            b.syntax()
                .unwrap()
                .tree()
                .unwrap()
                .root_node()
                .named_child_count()
        });
        assert_eq!(count, 2);
    }

    #[gpui::test]
    async fn a_reload_reparses_the_new_text(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        fs::write(&path, "fn a() {}\n").unwrap();
        let store = cx.new(|_| BufferStore::new());
        let buffer = open(&store, &path, cx).await.unwrap();
        settle(cx);

        rewrite(&path, "struct S;\n");
        store.update(cx, |s, cx| s.check_disk(cx));
        settle(cx);

        let kind = buffer.read_with(cx, |b, _| {
            b.syntax()
                .unwrap()
                .tree()
                .unwrap()
                .root_node()
                .named_child(0)
                .unwrap()
                .kind()
                .to_string()
        });
        assert_eq!(kind, "struct_item");
    }

    #[gpui::test]
    async fn save_as_picks_up_the_new_language(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let buffer = cx.new(|cx| BufferModel::new("package main\n", None, cx));
        assert_eq!(buffer.read_with(cx, |b, _| b.language()), None);
        cx.executor().allow_parking();

        let path = dir.path().join("main.go");
        buffer
            .update(cx, |b, cx| b.save_as(path.clone(), cx))
            .await
            .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "package main\n");
        let (language, hard_tabs) =
            buffer.read_with(cx, |b, _| (b.language(), b.edit_config().hard_tabs));
        assert_eq!(language, Some(gleipnir_syntax::Language::Go));
        assert!(hard_tabs);
    }
}
