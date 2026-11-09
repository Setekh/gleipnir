//! How a file differs from its last commit, line by line, kept current while it's edited: the
//! committed text is read once, and again after a save or a commit, and the lines are compared
//! off the UI thread a moment after typing stops.

use super::BufferModel;
use gleipnir_index::{LineChange, git};
use gpui::{Context, Task};
use std::sync::Arc;
use std::time::Duration;

/// How long after the last edit the lines are compared again, so typing doesn't do it per key.
const SETTLE: Duration = Duration::from_millis(150);

#[derive(Default)]
pub(super) struct GitMarks {
    /// The file as committed; none until read, or if it isn't in a commit.
    base: Option<Arc<String>>,
    changes: Arc<Vec<LineChange>>,
    base_task: Option<Task<()>>,
    diff_task: Option<Task<()>>,
}

impl BufferModel {
    /// The runs of lines that differ from the last commit, as of a moment ago.
    pub fn line_changes(&self) -> &Arc<Vec<LineChange>> {
        &self.git.changes
    }

    /// The file as of the last commit, if it's in one.
    pub fn committed_text(&self) -> Option<&Arc<String>> {
        self.git.base.as_ref()
    }

    /// Reads the committed text again, as after a save or a commit, then compares.
    pub fn refresh_committed_text(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let read = cx
            .background_executor()
            .spawn(async move { git::committed_text(&path).ok().flatten() });
        self.git.base_task = Some(cx.spawn(async move |this, cx| {
            let base = read.await;
            this.update(cx, |this, cx| {
                let changed = this.git.base.as_deref() != base.as_ref();
                this.git.base = base.map(Arc::new);
                if changed {
                    this.compare_lines(Duration::ZERO, cx);
                }
            })
            .ok();
        }));
    }

    /// Compares the text with the committed text after `delay`, unless more edits come first.
    pub(super) fn compare_lines(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(base) = self.git.base.clone() else {
            if !self.git.changes.is_empty() {
                self.git.changes = Arc::new(Vec::new());
                cx.notify();
            }
            return;
        };
        let text = self.buffer.snapshot();
        self.git.diff_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let changes = cx
                .background_executor()
                .spawn(async move { git::line_changes(&base, &text.slice(0..text.len())) })
                .await;
            this.update(cx, |this, cx| {
                if *this.git.changes != changes {
                    this.git.changes = Arc::new(changes);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub(super) fn edited_for_git(&mut self, cx: &mut Context<Self>) {
        self.compare_lines(SETTLE, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gleipnir_index::LineChangeKind;
    use gleipnir_text::TextEdit;
    use gpui::{AppContext as _, TestAppContext};
    use std::process::Command;

    #[gpui::test]
    fn edits_are_marked_against_the_commit_and_cleared_by_one(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                    .args(args)
                    .current_dir(dir.path())
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        let buffer = cx.new(|cx| BufferModel::new("one\ntwo\nthree\n", Some(path.clone()), cx));
        let marks = |cx: &mut TestAppContext| {
            buffer.read_with(cx, |b, _| {
                b.line_changes()
                    .iter()
                    .map(|c| (c.kind, c.lines.clone()))
                    .collect::<Vec<_>>()
            })
        };
        let settle = |cx: &mut TestAppContext| {
            for _ in 0..50 {
                cx.executor().advance_clock(Duration::from_millis(200));
                cx.run_until_parked();
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        settle(cx);
        assert!(marks(cx).is_empty());
        buffer.update(cx, |b, cx| {
            b.apply_external_edits(vec![TextEdit::new(4..7, "TWO\nmore".to_string())], cx)
        });
        settle(cx);
        assert_eq!(marks(cx), [(LineChangeKind::Modified, 1..3)]);
        // Committed: nothing differs any more.
        std::fs::write(&path, "one\nTWO\nmore\nthree\n").unwrap();
        git(&["commit", "-q", "-am", "two"]);
        buffer.update(cx, |b, cx| b.refresh_committed_text(cx));
        settle(cx);
        assert!(marks(cx).is_empty());
    }
}
