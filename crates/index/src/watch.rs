//! Tells the application when files change on disk.

use anyhow::Result;
use futures::channel::mpsc;
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Waits for this much quiet before reporting, so a save or a checkout arrives as one batch.
const QUIET_PERIOD: Duration = Duration::from_millis(60);
/// Reports at least this often, so that something like a long build can't delay updates forever.
const MAX_DELAY: Duration = Duration::from_millis(400);

/// Stops watching when dropped.
pub struct ProjectWatcher {
    _watcher: RecommendedWatcher,
}

/// Watches a folder recursively. Each item from the receiver is a batch of changed paths. They're
/// hints rather than a precise log, as platforms may merge events or report a parent folder instead,
/// so callers should re-read what they care about.
pub fn watch(root: &Path) -> Result<(ProjectWatcher, mpsc::UnboundedReceiver<Vec<PathBuf>>)> {
    let (raw_tx, raw_rx) = std_mpsc::channel::<PathBuf>();
    let (batch_tx, batch_rx) = mpsc::unbounded();

    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                // A failed send means we're shutting down.
                raw_tx.send(path).ok();
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;

    thread::Builder::new()
        .name("project-watcher".to_string())
        .spawn(move || batch_events(raw_rx, batch_tx))?;

    Ok((ProjectWatcher { _watcher: watcher }, batch_rx))
}

/// Watches individual folders, not their subfolders: for open files wherever they are, which a
/// project's recursive watch may not cover.
pub struct DirWatcher {
    watcher: RecommendedWatcher,
    watched: BTreeSet<PathBuf>,
}

impl DirWatcher {
    /// Starts watching a folder, if it isn't already.
    pub fn add(&mut self, dir: &Path) -> Result<()> {
        if self.watched.contains(dir) {
            return Ok(());
        }
        self.watcher.watch(dir, RecursiveMode::NonRecursive)?;
        self.watched.insert(dir.to_path_buf());
        Ok(())
    }

    pub fn remove(&mut self, dir: &Path) {
        if self.watched.remove(dir) {
            self.watcher.unwatch(dir).ok();
        }
    }

    pub fn watched(&self) -> impl Iterator<Item = &Path> {
        self.watched.iter().map(PathBuf::as_path)
    }
}

/// A watcher for single folders, added as needed. Batches arrive as with `watch`.
pub fn watch_dirs() -> Result<(DirWatcher, mpsc::UnboundedReceiver<Vec<PathBuf>>)> {
    let (raw_tx, raw_rx) = std_mpsc::channel::<PathBuf>();
    let (batch_tx, batch_rx) = mpsc::unbounded();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                raw_tx.send(path).ok();
            }
        }
    })?;
    thread::Builder::new()
        .name("file-watcher".to_string())
        .spawn(move || batch_events(raw_rx, batch_tx))?;
    Ok((
        DirWatcher {
            watcher,
            watched: BTreeSet::new(),
        },
        batch_rx,
    ))
}

/// Ends when the watcher is dropped, which closes the incoming channel.
fn batch_events(raw: std_mpsc::Receiver<PathBuf>, batches: mpsc::UnboundedSender<Vec<PathBuf>>) {
    while let Ok(first) = raw.recv() {
        let started = Instant::now();
        let mut paths = BTreeSet::from([first]);
        loop {
            let remaining = MAX_DELAY.saturating_sub(started.elapsed());
            match raw.recv_timeout(QUIET_PERIOD.min(remaining)) {
                Ok(path) => {
                    paths.insert(path);
                    if started.elapsed() >= MAX_DELAY {
                        break;
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => break,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    batches.unbounded_send(paths.into_iter().collect()).ok();
                    return;
                }
            }
        }
        if batches.unbounded_send(paths.into_iter().collect()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;
    use std::fs;

    /// Waits for a batch containing `target`, ignoring any others, such as the temp dir's creation
    /// still echoing through FSEvents.
    fn wait_for(rx: &mut mpsc::UnboundedReceiver<Vec<PathBuf>>, target: &Path) -> bool {
        futures::executor::block_on(async {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let next = futures::future::select(
                    rx.next(),
                    Box::pin(async {
                        let (tx, rx) = futures::channel::oneshot::channel::<()>();
                        thread::spawn(move || {
                            thread::sleep(Duration::from_millis(200));
                            tx.send(()).ok();
                        });
                        rx.await.ok();
                    }),
                )
                .await;
                if let futures::future::Either::Left((Some(batch), _)) = next
                    && batch.iter().any(|p| p == target || target.starts_with(p))
                {
                    return true;
                }
            }
            false
        })
    }

    #[test]
    fn single_folders_report_their_files_but_not_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        let (mut watcher, mut rx) = watch_dirs().unwrap();
        watcher.add(&root).unwrap();
        watcher.add(&root).unwrap();
        assert_eq!(watcher.watched().count(), 1);
        thread::sleep(Duration::from_millis(100));
        let file = root.join("open.txt");
        fs::write(&file, "changed").unwrap();
        assert!(wait_for(&mut rx, &file));
    }

    #[test]
    fn reports_created_changed_and_deleted_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        let (_watcher, mut rx) = watch(&root).unwrap();
        // FSEvents needs a moment before it's actually watching.
        thread::sleep(Duration::from_millis(200));

        let file = root.join("sub/new.txt");
        fs::write(&file, "hello").unwrap();
        assert!(wait_for(&mut rx, &file), "creation");

        fs::write(&file, "changed").unwrap();
        assert!(wait_for(&mut rx, &file), "modification");

        fs::remove_file(&file).unwrap();
        assert!(wait_for(&mut rx, &file), "deletion");
    }

    #[test]
    fn a_burst_of_changes_arrives_batched() {
        let (raw_tx, raw_rx) = std_mpsc::channel();
        let (batch_tx, mut batch_rx) = mpsc::unbounded();
        let worker = thread::spawn(move || batch_events(raw_rx, batch_tx));
        for name in ["a", "b", "a", "c"] {
            raw_tx.send(PathBuf::from(name)).unwrap();
        }
        let batch = futures::executor::block_on(batch_rx.next()).unwrap();
        assert_eq!(
            batch,
            [PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]
        );
        drop(raw_tx);
        worker.join().unwrap();
    }

    #[test]
    fn nonstop_changes_still_report_within_the_maximum_delay() {
        let (raw_tx, raw_rx) = std_mpsc::channel();
        let (batch_tx, mut batch_rx) = mpsc::unbounded();
        let worker = thread::spawn(move || batch_events(raw_rx, batch_tx));
        let sender = thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(1500) {
                raw_tx.send(PathBuf::from("busy")).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });
        let started = Instant::now();
        futures::executor::block_on(batch_rx.next()).unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "{:?}",
            started.elapsed()
        );
        sender.join().unwrap();
        worker.join().unwrap();
    }
}
