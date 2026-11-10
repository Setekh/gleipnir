//! What the debugger does with a test run's results: finds where each test is declared, keeps
//! each test's last result for the gutter, and runs a chosen few of a run's tests again.

use super::{Debugger, DebuggerEvent, LaunchId};
use crate::test_results::{NodeKind, ROOT, TestState};
use gleipnir_packages::{JS_NAME_SEPARATOR, TestPick, adapter_for};
use gpui::Context;
use std::path::{Path, PathBuf};

impl Debugger {
    /// A test's last result, by the file it's in and its function's name.
    pub fn test_result(&self, path: &Path, name: &str) -> Option<TestState> {
        self.test_marks.get(path)?.get(name).copied()
    }

    /// Where a node of a launch's results is declared: its own place, or for a suite or group,
    /// its first test's.
    pub fn test_location(&self, id: LaunchId, node: usize) -> Option<(PathBuf, u32)> {
        let tests = self.launch(id)?.tests.as_ref()?;
        let mut stack = vec![node];
        while let Some(index) = stack.pop() {
            let node = tests.node(index)?;
            if let Some(location) = &node.location {
                return Some(location.clone());
            }
            stack.extend(node.children.iter().rev());
        }
        None
    }

    /// Finds where a finished run's tests are declared, off the UI thread, then marks the gutter.
    pub(super) fn locate_tests(&mut self, id: LaunchId, cx: &mut Context<Self>) {
        let Some(launch) = self.launch(id) else {
            return;
        };
        let Some(tests) = &launch.tests else { return };
        let Some(adapter) = adapter_for(&launch.target.package_id) else {
            return;
        };
        let target = launch.target.clone();
        let work = tests.unlocated();
        if work.is_empty() {
            self.mark_tests(id);
            return;
        }
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let mut found = Vec::new();
                    for (suite, tests) in work {
                        let names: Vec<String> = tests.iter().map(|(_, n)| n.clone()).collect();
                        let places = adapter.locate_tests(&target, &suite, &names);
                        found.extend(
                            tests
                                .into_iter()
                                .zip(places)
                                .filter_map(|((node, _), place)| Some((node, place?))),
                        );
                    }
                    found
                })
                .await;
            this.update(cx, |this, cx| {
                this.with_launch(id, |l| {
                    if let Some(tests) = &mut l.tests {
                        for (node, place) in found {
                            tests.set_location(node, place);
                        }
                    }
                });
                this.mark_tests(id);
                cx.emit(DebuggerEvent::MarksChanged);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Records each located test's result, by file and function name.
    fn mark_tests(&mut self, id: LaunchId) {
        let Some(tests) = self.launch(id).and_then(|l| l.tests.as_ref()) else {
            return;
        };
        let package = self
            .launch(id)
            .map(|l| l.target.package_id.clone())
            .unwrap_or_default();
        let mut marks = Vec::new();
        for (index, node) in tests.nodes().iter().enumerate() {
            if node.kind != NodeKind::Test {
                continue;
            }
            // A Go subtest or a doc test has no function of its own.
            let own = match package.as_str() {
                "go" => !node.name.contains('/'),
                "rust" => !node.name.contains(" - "),
                _ => true,
            };
            let Some((path, _)) = &node.location else {
                continue;
            };
            if !own {
                continue;
            }
            // How the gutter names it: a function, or a JavaScript test's titles.
            let key = if package == "typescript" {
                node.name.replace(JS_NAME_SEPARATOR, " › ")
            } else {
                node.name
                    .rsplit("::")
                    .next()
                    .unwrap_or(&node.name)
                    .to_string()
            };
            marks.push((path.clone(), key, tests.state(index)));
        }
        for (path, function, state) in marks {
            self.test_marks
                .entry(path)
                .or_default()
                .insert(function, state);
        }
    }

    /// What running some nodes of a launch's results again means, for its language.
    fn picks(&self, id: LaunchId, nodes: &[usize]) -> Option<Vec<TestPick>> {
        let tests = self.launch(id)?.tests.as_ref()?;
        let mut picks = Vec::new();
        for &index in nodes {
            let node = tests.node(index)?;
            let suite = tests.suite(index).cloned().unwrap_or_default();
            picks.push(match node.kind {
                NodeKind::Root => return Some(Vec::new()),
                NodeKind::Suite => TestPick {
                    suite,
                    name: None,
                    exact: false,
                },
                NodeKind::Group => TestPick {
                    suite,
                    name: Some(node.name.clone()),
                    exact: false,
                },
                NodeKind::Test => TestPick {
                    suite,
                    name: Some(node.name.clone()),
                    exact: true,
                },
            });
        }
        Some(picks)
    }

    /// Runs, or debugs, some of a launch's tests again: a test, a group, a suite, or with the
    /// root, everything it ran.
    pub fn run_tests(
        &mut self,
        id: LaunchId,
        nodes: &[usize],
        debug: bool,
        cx: &mut Context<Self>,
    ) -> Option<LaunchId> {
        let picks = self.picks(id, nodes)?;
        let target = self.launch(id)?.target.clone();
        let target = if picks.is_empty() {
            target
        } else {
            adapter_for(&target.package_id)?.select_tests(&target, &picks)?
        };
        Some(self.start(target, debug, cx))
    }

    /// Runs the tests that failed in a launch again, and only those.
    pub fn rerun_failed(&mut self, id: LaunchId, cx: &mut Context<Self>) -> Option<LaunchId> {
        let tests = self.launch(id)?.tests.as_ref()?;
        let failed = tests.failed_tests();
        if failed.is_empty() {
            return None;
        }
        let debug = self.launch(id)?.debug;
        self.run_tests(id, &failed, debug, cx)
    }

    /// Whether a launch has failed tests to run again.
    pub fn has_failed_tests(&self, id: LaunchId) -> bool {
        self.launch(id)
            .and_then(|l| l.tests.as_ref())
            .is_some_and(|t| {
                t.is_finished()
                    && t.state(ROOT) == TestState::Failed
                    && !t.failed_tests().is_empty()
            })
    }
}
