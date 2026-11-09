//! Helpers shared by the UI crates' tests.

use gpui::{TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Points saved state at a temporary folder for the whole test run, so tests never read or write
/// the real application's layouts. Call before anything that could touch state.
pub fn isolate_state() {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("a temp dir for state"));
    // SAFETY: every test calls this before starting anything that reads the environment, and the
    // value never changes after the first call.
    unsafe {
        std::env::set_var("GLEIPNIR_STATE_DIR", dir.path());
        // Settings a test changes are saved; never over the person's own.
        std::env::set_var("GLEIPNIR_CONFIG_DIR", dir.path().join("config"));
        // Asking the login shell for its environment takes a second and depends on the machine.
        std::env::set_var("GLEIPNIR_NO_SHELL_ENV", "1");
    }
}

/// A project folder on disk. Paths ending in `/` are folders, others are files with the given text.
pub fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temp dir");
    // Canonical, because file watching reports canonical paths, such as /private/var on macOS.
    let root = std::fs::canonicalize(dir.path()).expect("canonical path");
    for (path, text) in files {
        let full = root.join(path);
        if path.ends_with('/') {
            std::fs::create_dir_all(&full).unwrap();
        } else {
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, text).unwrap();
        }
    }
    (dir, root)
}

pub fn path_in(root: &Path, relative: &str) -> PathBuf {
    root.join(relative)
}

/// Runs the app until a condition holds. Real file IO, git and file watching happen on threads the
/// test scheduler can't see, so this lets them finish instead of guessing with fixed sleeps.
pub fn run_until(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    cx.executor().allow_parking();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a condition"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The same, for tests without a window.
pub fn run_until_app(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    cx.executor().allow_parking();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a condition"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits for a task to finish, while letting the app keep running so it can make progress.
pub fn resolve<T: 'static>(cx: &mut VisualTestContext, task: gpui::Task<T>) -> T {
    use std::cell::RefCell;
    use std::rc::Rc;
    let slot = Rc::new(RefCell::new(None));
    let result = slot.clone();
    cx.update(|_, cx| {
        cx.spawn(async move |_| {
            *result.borrow_mut() = Some(task.await);
        })
        .detach();
    });
    run_until(cx, |_| slot.borrow().is_some());
    slot.borrow_mut().take().expect("the task finished")
}
