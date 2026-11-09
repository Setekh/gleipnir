//! A test run's results, read from what it prints as it goes: a tree of suites, groups and tests,
//! each with its state, its time and its own output.

use crate::debugger::{ConsoleKind, ConsoleLine};
use gleipnir_packages::{Suite, TestFormat};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TestState {
    Running,
    Passed,
    Failed,
    Skipped,
    /// The run ended before the test did.
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Root,
    /// A test binary, or a package.
    Suite,
    /// A module, holding tests.
    Group,
    /// A test, which in Go can hold subtests.
    Test,
}

/// How many tests under a node are in each state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TestCounts {
    pub total: usize,
    pub running: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub stopped: usize,
}

impl TestCounts {
    fn bump(&mut self, state: TestState, delta: isize) {
        let field = match state {
            TestState::Running => &mut self.running,
            TestState::Passed => &mut self.passed,
            TestState::Failed => &mut self.failed,
            TestState::Skipped => &mut self.skipped,
            TestState::Stopped => &mut self.stopped,
        };
        *field = field.saturating_add_signed(delta);
    }
}

#[derive(Clone, Debug)]
pub struct TestNode {
    pub kind: NodeKind,
    /// What the tree shows: the last part of the name.
    pub label: String,
    /// The name the tool knows it by within its suite, like `parser::tests::parses`, or for a
    /// group, its prefix, like `parser::tests::`.
    pub name: String,
    /// Shown after the label, quieter, like the file a test binary was built from.
    pub detail: Option<String>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// A test's own state, or for a suite, one its tests don't account for, like failing to build.
    pub state: Option<TestState>,
    pub counts: TestCounts,
    pub duration: Option<Duration>,
    pub output: Vec<ConsoleLine>,
    /// Where it's declared, once found.
    pub location: Option<(PathBuf, u32)>,
    /// The suite it's in, for the root and suites themselves.
    pub suite: usize,
}

pub struct TestRun {
    nodes: Vec<TestNode>,
    /// Nodes by suite and name.
    by_name: HashMap<(usize, String), usize>,
    /// Each suite's identity, as the language adapter needs it.
    suites: HashMap<usize, Suite>,
    parser: Parser,
    /// What's arrived of a line not yet ended, per stream.
    partial: [String; 2],
    finished: bool,
    /// Bumped by every change, for views that cache.
    version: u64,
}

pub const ROOT: usize = 0;

impl TestRun {
    pub fn new(format: TestFormat, label: &str) -> Self {
        let root = TestNode {
            kind: NodeKind::Root,
            label: label.to_string(),
            name: String::new(),
            detail: None,
            parent: None,
            children: Vec::new(),
            state: None,
            counts: TestCounts::default(),
            duration: None,
            output: Vec::new(),
            location: None,
            suite: ROOT,
        };
        let parser = match format {
            TestFormat::Libtest => Parser::Libtest(Libtest::default()),
            TestFormat::GoJson => Parser::GoJson,
            TestFormat::GoVerbose => Parser::GoVerbose(Vec::new()),
            TestFormat::Odin => Parser::Odin(Odin::default()),
            TestFormat::Jest => Parser::Jest(Jest::default()),
        };
        Self {
            nodes: vec![root],
            by_name: HashMap::new(),
            suites: HashMap::new(),
            parser,
            partial: [String::new(), String::new()],
            finished: false,
            version: 0,
        }
    }

    pub fn nodes(&self) -> &[TestNode] {
        &self.nodes
    }

    pub fn node(&self, index: usize) -> Option<&TestNode> {
        self.nodes.get(index)
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn suite(&self, node: usize) -> Option<&Suite> {
        self.suites.get(&self.nodes.get(node)?.suite)
    }

    /// The suite a node is in, and every suite with the tests in each that need finding.
    pub fn unlocated(&self) -> Vec<(Suite, Vec<(usize, String)>)> {
        let mut by_suite: HashMap<usize, Vec<(usize, String)>> = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            if node.kind == NodeKind::Test && node.location.is_none() {
                by_suite
                    .entry(node.suite)
                    .or_default()
                    .push((index, node.name.clone()));
            }
        }
        let mut found: Vec<_> = by_suite
            .into_iter()
            .filter_map(|(suite, tests)| Some((self.suites.get(&suite)?.clone(), tests)))
            .collect();
        found.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        found
    }

    pub fn set_location(&mut self, node: usize, location: (PathBuf, u32)) {
        if let Some(node) = self.nodes.get_mut(node) {
            node.location = Some(location);
            self.version += 1;
        }
    }

    /// What a node shows: a test's own state, or what its tests add up to.
    pub fn state(&self, index: usize) -> TestState {
        let node = &self.nodes[index];
        if node.kind == NodeKind::Test
            && let Some(state) = node.state
        {
            return state;
        }
        let c = node.counts;
        let own = node.state;
        if own == Some(TestState::Failed) || c.failed > 0 {
            TestState::Failed
        } else if c.running > 0 || (!self.finished && c.total == 0) {
            TestState::Running
        } else if c.stopped > 0 || own == Some(TestState::Stopped) {
            TestState::Stopped
        } else if c.passed > 0 {
            TestState::Passed
        } else if c.total > 0 && c.skipped == c.total {
            TestState::Skipped
        } else {
            own.unwrap_or(TestState::Passed)
        }
    }

    /// Tests that failed, at their deepest: subtests rather than the tests holding them.
    pub fn failed_tests(&self) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| {
                let node = &self.nodes[i];
                node.kind == NodeKind::Test
                    && node.state == Some(TestState::Failed)
                    && !node
                        .children
                        .iter()
                        .any(|&c| self.nodes[c].state == Some(TestState::Failed))
            })
            .collect()
    }

    /// Every output line of a node and the nodes under it, in tree order.
    pub fn output_of(&self, index: usize) -> Vec<ConsoleLine> {
        let mut lines = Vec::new();
        let mut stack = vec![index];
        while let Some(i) = stack.pop() {
            let node = &self.nodes[i];
            lines.extend(node.output.iter().cloned());
            stack.extend(node.children.iter().rev());
        }
        lines
    }

    /// Takes output as it arrives, and returns what the console should show for it, which for
    /// `go test -json` is the text inside the events rather than the events.
    pub fn feed(&mut self, kind: ConsoleKind, text: &str) -> Vec<(ConsoleKind, String)> {
        let stream = match kind {
            ConsoleKind::Stdout => 0,
            ConsoleKind::Stderr => 1,
            _ => return vec![(kind, text.to_string())],
        };
        let mut shown = Vec::new();
        // Go's events, and jest's reporter on stdout, are read rather than shown.
        let decodes = match self.parser {
            Parser::GoJson => true,
            Parser::Jest(_) => stream == 0,
            _ => false,
        };
        if !decodes {
            shown.push((kind, text.to_string()));
        }
        self.partial[stream].push_str(text);
        while let Some(end) = self.partial[stream].find('\n') {
            let mut line: String = self.partial[stream].drain(..=end).collect();
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            self.line(kind, &line, &mut shown);
        }
        // Go's events are whole lines; anything else is shown as it comes.
        if decodes && self.partial[stream].len() > 64 * 1024 {
            let rest = std::mem::take(&mut self.partial[stream]);
            shown.push((kind, rest));
        }
        shown
    }

    /// Ends the run: tests still going were stopped, and a run that failed with no test failing
    /// failed as a whole, like a build that broke.
    pub fn finish(&mut self, code: Option<i64>, stopped: bool) {
        let mut shown = Vec::new();
        for stream in 0..2 {
            let rest = std::mem::take(&mut self.partial[stream]);
            if !rest.is_empty() {
                let kind = [ConsoleKind::Stdout, ConsoleKind::Stderr][stream];
                self.line(kind, &rest, &mut shown);
            }
        }
        if let Parser::Odin(odin) = &mut self.parser {
            // Failures that named no test there was.
            for (_, kind, line) in std::mem::take(&mut odin.unclaimed) {
                self.nodes[ROOT].output.push(ConsoleLine {
                    kind,
                    text: line,
                    complete: true,
                });
            }
        }
        for index in 0..self.nodes.len() {
            if self.nodes[index].state == Some(TestState::Running) {
                self.set_state(index, TestState::Stopped);
            }
        }
        let counts = self.nodes[ROOT].counts;
        if stopped {
            self.nodes[ROOT].state = Some(TestState::Stopped);
        } else if code.is_some_and(|c| c != 0) && counts.failed == 0 {
            self.nodes[ROOT].state = Some(TestState::Failed);
        }
        self.finished = true;
        self.version += 1;
    }

    /// Tells a jest-style run where it runs, which its file names are relative to, and whether a
    /// title pattern narrows it, so the tests it leaves out, reported like skipped ones, aren't
    /// shown.
    pub fn set_jest_context(&mut self, cwd: PathBuf, filtered: bool) {
        if let Parser::Jest(jest) = &mut self.parser {
            jest.cwd = cwd;
            jest.filtered = filtered;
        }
    }

    fn jest_context(&self) -> (PathBuf, bool) {
        match &self.parser {
            Parser::Jest(jest) => (jest.cwd.clone(), jest.filtered),
            _ => (PathBuf::new(), false),
        }
    }

    /// The suite for a test file, made if it's new.
    fn jest_suite(&mut self, path: &Path) -> usize {
        let (cwd, _) = self.jest_context();
        let relative = path
            .strip_prefix(&cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        if let Some(&index) = self.by_name.get(&(ROOT, relative.clone())) {
            return index;
        }
        let suite = self.add_suite(
            &relative,
            Suite {
                name: relative.clone(),
                source: Some(path.to_string_lossy().into_owned()),
            },
            None,
        );
        self.by_name.insert((ROOT, relative), suite);
        suite
    }

    /// One test's result, as jest reports it, under its file's suite.
    fn jest_result(&mut self, suite: usize, path: &Path, result: &Value) {
        let (_, filtered) = self.jest_context();
        let state = match result["status"].as_str() {
            Some("passed") => TestState::Passed,
            Some("failed") => TestState::Failed,
            _ if filtered => return,
            _ => TestState::Skipped,
        };
        let separator = gleipnir_packages::JS_NAME_SEPARATOR.to_string();
        let mut titles: Vec<String> = result["ancestorTitles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(String::from))
            .collect();
        titles.push(result["title"].as_str().unwrap_or_default().to_string());
        let name = titles.join(&separator);
        let index = self.test(suite, &name, Some(&separator), true);
        // Seen already, as a report at the end repeats what was reported along the way.
        if self.nodes[index].state.is_some() {
            return;
        }
        self.set_state(index, state);
        if let Some(ms) = result["duration"].as_f64() {
            self.nodes[index].duration = Some(Duration::from_secs_f64(ms / 1000.));
        }
        if let Some(line) = result["location"]["line"].as_u64() {
            self.nodes[index].location = Some((path.to_path_buf(), line.saturating_sub(1) as u32));
        }
        for message in result["failureMessages"].as_array().into_iter().flatten() {
            for line in message.as_str().unwrap_or_default().lines() {
                if !is_runner_frame(line) {
                    self.output(index, ConsoleKind::Stderr, line);
                }
            }
        }
    }

    /// A file that couldn't run, like one that doesn't compile, fails as a whole.
    fn jest_file_failed(&mut self, suite: usize, message: &str) {
        if self.nodes[suite].state == Some(TestState::Failed) {
            return;
        }
        self.set_state(suite, TestState::Failed);
        for line in message.lines() {
            if !is_runner_frame(line) {
                self.output(suite, ConsoleKind::Stderr, line);
            }
        }
    }

    /// Fills the tree from a jest or vitest JSON report: a suite per file, a group per
    /// `describe`, and each test with its time, its failure and where it is. What was already
    /// reported along the way stays as it was.
    pub fn load_jest_report(&mut self, json: &str) -> anyhow::Result<()> {
        let report: Value = serde_json::from_str(json)?;
        for file in report["testResults"].as_array().into_iter().flatten() {
            let Some(path) = file["name"].as_str().map(PathBuf::from) else {
                continue;
            };
            let suite = self.jest_suite(&path);
            let assertions = file["assertionResults"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for assertion in &assertions {
                self.jest_result(suite, &path, assertion);
            }
            if file["status"].as_str() == Some("failed")
                && assertions.iter().all(|a| a["status"] != "failed")
            {
                self.jest_file_failed(suite, file["message"].as_str().unwrap_or_default());
            }
            if let (Some(start), Some(end)) = (file["startTime"].as_f64(), file["endTime"].as_f64())
                && end >= start
            {
                self.nodes[suite].duration = Some(Duration::from_secs_f64((end - start) / 1000.));
            }
        }
        self.version += 1;
        Ok(())
    }

    /// A line jest printed: the reporter's events build the tree as tests finish; anything else
    /// is shown as it is.
    fn jest_line(&mut self, kind: ConsoleKind, line: &str, shown: &mut Vec<(ConsoleKind, String)>) {
        let Some(event) = line
            .strip_prefix(gleipnir_packages::JS_REPORTER_MARK)
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
        else {
            // Stderr is shown as it comes; stdout is held to its line ends, to find the events.
            if kind == ConsoleKind::Stdout {
                shown.push((kind, format!("{line}\n")));
            }
            return;
        };
        let Some(path) = event["path"].as_str().map(PathBuf::from) else {
            return;
        };
        let suite = self.jest_suite(&path);
        match event["event"].as_str() {
            Some("file") => {
                self.nodes[suite].state = Some(TestState::Running);
                self.version += 1;
            }
            Some("test") => self.jest_result(suite, &path, &event),
            Some("fileDone") => {
                if self.nodes[suite].state == Some(TestState::Running) {
                    self.nodes[suite].state = None;
                }
                if let Some(ms) = event["duration"].as_f64() {
                    self.nodes[suite].duration = Some(Duration::from_secs_f64(ms / 1000.));
                }
                if let Some(message) = event["error"].as_str() {
                    self.jest_file_failed(suite, message);
                }
                self.version += 1;
            }
            _ => {}
        }
    }

    // Building the tree.

    fn add_suite(&mut self, label: &str, suite: Suite, detail: Option<String>) -> usize {
        let index = self.nodes.len();
        self.nodes.push(TestNode {
            kind: NodeKind::Suite,
            label: label.to_string(),
            name: suite.name.clone(),
            detail,
            parent: Some(ROOT),
            children: Vec::new(),
            state: None,
            counts: TestCounts::default(),
            duration: None,
            output: Vec::new(),
            location: None,
            suite: index,
        });
        self.nodes[ROOT].children.push(index);
        self.suites.insert(index, suite);
        self.version += 1;
        index
    }

    /// The suite with a name, made if it's new.
    fn suite_named(&mut self, name: &str) -> usize {
        if let Some(&index) = self.by_name.get(&(ROOT, name.to_string())) {
            return index;
        }
        let suite = Suite {
            name: name.to_string(),
            source: None,
        };
        let index = self.add_suite(name, suite, None);
        self.by_name.insert((ROOT, name.to_string()), index);
        index
    }

    /// The node for a test, made with the groups above it if it's new. `separator` splits the
    /// name into its levels; `groups` says whether the levels above are groups or tests.
    fn test(&mut self, suite: usize, name: &str, separator: Option<&str>, groups: bool) -> usize {
        if let Some(&index) = self.by_name.get(&(suite, name.to_string())) {
            return index;
        }
        let (parent, label) = match separator.and_then(|s| name.rsplit_once(s).map(|p| (p, s))) {
            Some(((above, last), separator)) => {
                let parent = if groups {
                    self.group(suite, above, separator)
                } else {
                    self.test(suite, above, Some(separator), false)
                };
                (parent, last.to_string())
            }
            None => (suite, name.to_string()),
        };
        let index = self.nodes.len();
        self.nodes.push(TestNode {
            kind: NodeKind::Test,
            label,
            name: name.to_string(),
            detail: None,
            parent: Some(parent),
            children: Vec::new(),
            state: None,
            counts: TestCounts::default(),
            duration: None,
            output: Vec::new(),
            location: None,
            suite,
        });
        self.nodes[parent].children.push(index);
        self.by_name.insert((suite, name.to_string()), index);
        let mut at = Some(index);
        while let Some(i) = at {
            self.nodes[i].counts.total += 1;
            at = self.nodes[i].parent;
        }
        self.version += 1;
        index
    }

    fn group(&mut self, suite: usize, path: &str, separator: &str) -> usize {
        let key = format!("{path}{separator}");
        if let Some(&index) = self.by_name.get(&(suite, key.clone())) {
            return index;
        }
        let (parent, label) = match path.rsplit_once(separator) {
            Some((above, last)) => (self.group(suite, above, separator), last.to_string()),
            None => (suite, path.to_string()),
        };
        let index = self.nodes.len();
        self.nodes.push(TestNode {
            kind: NodeKind::Group,
            label,
            name: key.clone(),
            detail: None,
            parent: Some(parent),
            children: Vec::new(),
            state: None,
            counts: TestCounts::default(),
            duration: None,
            output: Vec::new(),
            location: None,
            suite,
        });
        self.nodes[parent].children.push(index);
        self.by_name.insert((suite, key), index);
        index
    }

    fn set_state(&mut self, index: usize, state: TestState) {
        let old = self.nodes[index].state;
        if old == Some(state) {
            return;
        }
        self.nodes[index].state = Some(state);
        if self.nodes[index].kind == NodeKind::Test {
            let mut at = Some(index);
            while let Some(i) = at {
                let counts = &mut self.nodes[i].counts;
                if let Some(old) = old {
                    counts.bump(old, -1);
                }
                counts.bump(state, 1);
                at = self.nodes[i].parent;
            }
        }
        self.version += 1;
    }

    fn output(&mut self, index: usize, kind: ConsoleKind, text: &str) {
        self.nodes[index].output.push(ConsoleLine {
            kind,
            text: text.to_string(),
            complete: true,
        });
        self.version += 1;
    }

    fn line(&mut self, kind: ConsoleKind, line: &str, shown: &mut Vec<(ConsoleKind, String)>) {
        let mut parser = std::mem::replace(&mut self.parser, Parser::GoJson);
        match &mut parser {
            Parser::Libtest(state) => state.line(self, kind, line),
            Parser::GoJson => go_json_line(self, kind, line, shown),
            Parser::GoVerbose(running) => go_verbose_line(self, running, kind, line),
            Parser::Odin(state) => state.line(self, kind, line),
            Parser::Jest(_) => {
                self.parser = parser;
                self.jest_line(kind, line, shown);
                return;
            }
        }
        self.parser = parser;
    }
}

enum Parser {
    Libtest(Libtest),
    GoJson,
    /// The tests running, innermost last.
    GoVerbose(Vec<usize>),
    Odin(Odin),
    /// The reporter's events as tests finish, or for vitest, the report at the end.
    Jest(Jest),
}

#[derive(Default)]
struct Jest {
    cwd: PathBuf,
    filtered: bool,
}

/// A stack frame inside the test runner or Node itself, which says nothing about the test.
fn is_runner_frame(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("at ")
        && (line.contains("/node_modules/")
            || line.contains("node:internal")
            || line.contains("(<anonymous>)"))
}

// Rust's libtest.

/// Cargo names each test binary on stderr as it starts it, and the binary announces itself on
/// stdout. The two streams arrive separately, so they're paired up in order.
#[derive(Default)]
struct Libtest {
    /// Suites announced on stdout, in order.
    suites: Vec<usize>,
    /// Their names from stderr, in order: label, crate and source.
    names: Vec<(String, Suite, Option<String>)>,
    current: Option<usize>,
    /// The test whose captured output is being printed, in the failures section.
    capturing: Option<usize>,
    /// A test whose result hasn't been printed yet, as when output isn't captured.
    pending: Option<usize>,
}

impl Libtest {
    fn line(&mut self, run: &mut TestRun, kind: ConsoleKind, line: &str) {
        if kind == ConsoleKind::Stderr {
            let trimmed = line.trim_start();
            if let Some(named) = cargo_suite_line(trimmed) {
                self.names.push(named);
                self.pair(run);
                return;
            }
            if trimmed.starts_with("error: test failed")
                || trimmed.starts_with("Finished ")
                || trimmed.starts_with("Compiling ")
            {
                return;
            }
        }
        if line.starts_with("running ") && (line.ends_with(" test") || line.ends_with(" tests")) {
            let index = run.add_suite("tests", Suite::default(), None);
            self.suites.push(index);
            self.current = Some(index);
            self.capturing = None;
            self.pair(run);
            return;
        }
        let suite = match self.current {
            Some(suite) => suite,
            None => {
                // Output before any binary started, like the build's.
                run.output(ROOT, kind, line);
                return;
            }
        };
        if let Some(rest) = line.strip_prefix("test ")
            && let Some((name, result)) = rest.rsplit_once(" ...")
        {
            let doc = run.suites.get(&suite).and_then(|s| s.source.as_deref()) == Some("doc")
                || name.contains(" - ");
            let separator = (!doc).then_some("::");
            let index = run.test(suite, name, separator, true);
            // Without capturing, the test's output can follow on the same line.
            let result = result.trim();
            let word = result.split_whitespace().next().unwrap_or("");
            if word == "ok" || word == "FAILED" || word.starts_with("ignored") {
                self.result(run, index, word);
            } else {
                self.result(run, index, "");
                if !result.is_empty() {
                    run.output(index, kind, result);
                }
            }
            return;
        }
        if let Some(pending) = self.pending {
            match line.trim() {
                "ok" | "FAILED" => return self.result(run, pending, line.trim()),
                _ => {}
            }
        }
        if line == "failures:" {
            self.capturing = None;
            return;
        }
        if let Some(rest) = line.strip_prefix("---- ")
            && let Some(name) = rest
                .strip_suffix(" stdout ----")
                .or_else(|| rest.strip_suffix(" stderr ----"))
        {
            self.capturing = run.by_name.get(&(suite, name.to_string())).copied();
            return;
        }
        if let Some(rest) = line.strip_prefix("test result: ") {
            self.capturing = None;
            if let Some(seconds) = rest
                .rsplit_once("finished in ")
                .and_then(|(_, s)| s.trim_end_matches('s').parse::<f64>().ok())
            {
                run.nodes[suite].duration = Some(Duration::from_secs_f64(seconds));
            }
            return;
        }
        match self.capturing.or(self.pending) {
            Some(test) => run.output(test, kind, line),
            None if !line.is_empty() => run.output(suite, kind, line),
            None => {}
        }
    }

    fn result(&mut self, run: &mut TestRun, index: usize, result: &str) {
        let state = if result == "ok" {
            TestState::Passed
        } else if result == "FAILED" {
            TestState::Failed
        } else if result.starts_with("ignored") {
            TestState::Skipped
        } else if result.is_empty() {
            self.pending = Some(index);
            TestState::Running
        } else {
            // A benchmark's result, or something this doesn't know.
            TestState::Passed
        };
        if state != TestState::Running && self.pending == Some(index) {
            self.pending = None;
        }
        run.set_state(index, state);
    }

    /// Names the suites announced so far with the names Cargo gave them.
    fn pair(&mut self, run: &mut TestRun) {
        for (index, (label, suite, detail)) in self.suites.iter().zip(&self.names) {
            if run.nodes[*index].name == suite.name && run.suites.get(index) == Some(suite) {
                continue;
            }
            run.nodes[*index].label = label.clone();
            run.nodes[*index].name = suite.name.clone();
            run.nodes[*index].detail = detail.clone();
            run.suites.insert(*index, suite.clone());
            run.version += 1;
        }
    }
}

/// `Running unittests src/lib.rs (target/debug/deps/app-1a2b)`, `Running tests/api.rs (...)`, or
/// `Doc-tests app`: the label, the suite and its source.
fn cargo_suite_line(line: &str) -> Option<(String, Suite, Option<String>)> {
    if let Some(name) = line.strip_prefix("Doc-tests ") {
        let suite = Suite {
            name: name.trim().to_string(),
            source: Some("doc".into()),
        };
        return Some((format!("Doc-tests {}", name.trim()), suite, None));
    }
    let rest = line.strip_prefix("Running ")?;
    let rest = rest.strip_prefix("unittests ").unwrap_or(rest);
    let (source, binary) = rest.rsplit_once(" (")?;
    let binary = binary.trim_end_matches(')');
    let file = binary.rsplit('/').next()?;
    let crate_name = match file.rsplit_once('-') {
        Some((name, hash)) if hash.chars().all(|c| c.is_ascii_hexdigit()) => name,
        _ => file,
    };
    let suite = Suite {
        name: crate_name.to_string(),
        source: Some(source.to_string()),
    };
    Some((crate_name.to_string(), suite, Some(source.to_string())))
}

// Go.

fn go_json_line(
    run: &mut TestRun,
    kind: ConsoleKind,
    line: &str,
    shown: &mut Vec<(ConsoleKind, String)>,
) {
    let event: Value = match serde_json::from_str(line) {
        Ok(value @ Value::Object(_)) => value,
        // Not an event, like `go: downloading ...`.
        _ => {
            shown.push((kind, format!("{line}\n")));
            if !line.is_empty() {
                run.output(ROOT, kind, line);
            }
            return;
        }
    };
    let action = event["Action"].as_str().unwrap_or("");
    let package = event["Package"]
        .as_str()
        .or_else(|| event["ImportPath"].as_str())
        .unwrap_or("");
    let output = event["Output"].as_str();
    if let Some(text) = output {
        shown.push((ConsoleKind::Stdout, text.to_string()));
    }
    if action.starts_with("build-") {
        // A build's output belongs to no package's tests, and may be for one that has none.
        if let Some(text) = output {
            for line in text.lines() {
                run.output(ROOT, ConsoleKind::Stderr, line);
            }
        }
        return;
    }
    if package.is_empty() {
        return;
    }
    let suite = run.suite_named(package);
    let test = event["Test"]
        .as_str()
        .map(|name| run.test(suite, name, Some("/"), false));
    let target = test.unwrap_or(suite);
    let elapsed = event["Elapsed"]
        .as_f64()
        .map(|s| Duration::from_secs_f64(s.max(0.)));
    match action {
        "run" => {
            if let Some(test) = test {
                run.set_state(test, TestState::Running);
            }
        }
        "output" => {
            if let Some(text) = output {
                for line in text.lines() {
                    run.output(target, ConsoleKind::Stdout, line);
                }
            }
        }
        "pass" | "fail" | "skip" => {
            let state = match action {
                "pass" => TestState::Passed,
                "fail" => TestState::Failed,
                _ => TestState::Skipped,
            };
            match test {
                Some(test) => run.set_state(test, state),
                // A package failing without a test failing, like one that doesn't build.
                None if state == TestState::Failed && run.nodes[suite].counts.failed == 0 => {
                    run.nodes[suite].state = Some(TestState::Failed);
                    run.version += 1;
                }
                None => {}
            }
            run.nodes[target].duration = elapsed;
        }
        _ => {}
    }
}

/// `--- PASS: TestA/case (0.01s)`: the state, name and time.
fn go_result_line(line: &str) -> Option<(TestState, &str, Option<Duration>)> {
    let rest = line.trim_start().strip_prefix("--- ")?;
    let (state, rest) = if let Some(rest) = rest.strip_prefix("PASS: ") {
        (TestState::Passed, rest)
    } else if let Some(rest) = rest.strip_prefix("FAIL: ") {
        (TestState::Failed, rest)
    } else {
        (TestState::Skipped, rest.strip_prefix("SKIP: ")?)
    };
    let (name, time) = match rest.rsplit_once(" (") {
        Some((name, time)) => (name, time.trim_end_matches(')').trim_end_matches('s')),
        None => (rest, ""),
    };
    let duration = time.parse::<f64>().ok().map(Duration::from_secs_f64);
    Some((state, name, duration))
}

fn go_verbose_line(run: &mut TestRun, running: &mut Vec<usize>, kind: ConsoleKind, line: &str) {
    let suite = match run.nodes[ROOT].children.first() {
        Some(&suite) => suite,
        None => {
            let label = run.nodes[ROOT].label.clone();
            run.suite_named(&label)
        }
    };
    if let Some(name) = line.strip_prefix("=== RUN ") {
        let test = run.test(suite, name.trim(), Some("/"), false);
        run.set_state(test, TestState::Running);
        running.push(test);
        run.output(test, kind, line);
        return;
    }
    for prefix in ["=== PAUSE ", "=== CONT ", "=== NAME "] {
        if let Some(name) = line.strip_prefix(prefix) {
            if let Some(&test) = run.by_name.get(&(suite, name.trim().to_string())) {
                running.retain(|&t| t != test);
                if prefix != "=== PAUSE " {
                    running.push(test);
                }
            }
            return;
        }
    }
    if let Some((state, name, duration)) = go_result_line(line) {
        let test = run.test(suite, name, Some("/"), false);
        run.set_state(test, state);
        run.nodes[test].duration = duration;
        run.output(test, kind, line);
        running.retain(|&t| t != test);
        return;
    }
    match running.last() {
        Some(&test) => run.output(test, kind, line),
        None if !line.is_empty() => run.output(suite, kind, line),
        None => {}
    }
}

// Odin.

/// Odin's runner logs each test's change of state from its main thread, after the fact, while
/// tests log from threads of their own, so the order of the two says little. A failure names the
/// procedure it happened in, though, and the summary at the end names each failed test.
#[derive(Default)]
struct Odin {
    /// The test that most recently started, for output that names none.
    current: Option<usize>,
    /// Failures naming a procedure that's not a test yet, kept until it is.
    unclaimed: Vec<(String, ConsoleKind, String)>,
    /// Whether the summary after the last test has begun.
    summary: bool,
}

impl Odin {
    fn find(run: &TestRun, name: &str) -> Option<usize> {
        run.nodes[ROOT]
            .children
            .iter()
            .find_map(|&suite| run.by_name.get(&(suite, name.to_string())).copied())
    }

    fn line(&mut self, run: &mut TestRun, kind: ConsoleKind, line: &str) {
        // `[INFO ] --- [2026-11-06 10:45:56] Test #0 pkg.adds changed state to Running.`
        let message = line
            .strip_prefix('[')
            .and_then(|rest| rest.split_once("] --- ["))
            .and_then(|(_, rest)| rest.split_once("] "))
            .map(|(_, message)| message);
        if let Some(message) = message
            && let Some(rest) = message.strip_prefix("Test #")
            && let Some((_, rest)) = rest.split_once(' ')
            && let Some((full, state)) = rest.split_once(" changed state to ")
        {
            let (package, name) = full.split_once('.').unwrap_or(("tests", full));
            let suite = run.suite_named(package);
            let test = run.test(suite, name, None, false);
            let state = match state.trim_end_matches('.') {
                "Running" => {
                    self.current = Some(test);
                    TestState::Running
                }
                "Successful" => TestState::Passed,
                "Failed" => TestState::Failed,
                "Skipped" => TestState::Skipped,
                _ => return,
            };
            if state != TestState::Running && self.current == Some(test) {
                self.current = None;
            }
            run.set_state(test, state);
            let (claimed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.unclaimed)
                .into_iter()
                .partition(|(procedure, _, _)| procedure == name);
            self.unclaimed = kept;
            for (_, kind, line) in claimed {
                run.output(test, kind, &line);
            }
            return;
        }
        // A log line's header, whose message is on the next line.
        if line.is_empty() || message.is_some_and(str::is_empty) {
            return;
        }
        // `/p/sums_test.odin(12:2):fails() expected 2 + 2 to be 5, got 4`
        if let Some((_, rest)) = line.split_once(".odin(")
            && let Some((_, rest)) = rest.split_once("):")
            && let Some((procedure, _)) = rest.split_once("()")
        {
            match Self::find(run, procedure) {
                Some(test) => run.output(test, kind, line),
                None => self
                    .unclaimed
                    .push((procedure.to_string(), kind, line.to_string())),
            }
            return;
        }
        if message.is_some_and(|m| m.starts_with("Finished ")) || line.starts_with("Finished ") {
            self.summary = true;
        }
        // ` - sums.fails \texpected 2 + 2 to be 5, got 4`: a failed test's last message, which
        // it gets if nothing of its own reached it.
        if self.summary
            && let Some(rest) = line.strip_prefix(" - ")
            && let Some((full, said)) = rest.split_once('\t')
        {
            let name = full.trim().split_once('.').map_or(full.trim(), |(_, n)| n);
            if let Some(test) = Self::find(run, name)
                && run.nodes[test].output.is_empty()
            {
                run.output(test, kind, said.trim());
            }
            run.output(ROOT, kind, line);
            return;
        }
        match self.current.filter(|_| !self.summary) {
            Some(test) => run.output(test, kind, line),
            None => run.output(ROOT, kind, line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(run: &mut TestRun, kind: ConsoleKind, text: &str) -> String {
        run.feed(kind, text)
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    }

    fn find(run: &TestRun, name: &str) -> usize {
        run.nodes()
            .iter()
            .position(|n| n.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
    }

    fn text(run: &TestRun, node: usize) -> String {
        run.output_of(node)
            .iter()
            .map(|l| format!("{}\n", l.text))
            .collect()
    }

    const CARGO_OUT: &str = "\nrunning 3 tests\ntest tests::skipped ... ignored\ntest tests::adds ... ok\ntest tests::fails ... FAILED\n\nfailures:\n\n---- tests::fails stdout ----\nsome output\n\nthread 'tests::fails' (13584530) panicked at ws_a/src/lib.rs:11:42:\nassertion `left == right` failed: math\n\n\nfailures:\n    tests::fails\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.25s\n\n\nrunning 1 test\ntest integ ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n\nrunning 1 test\ntest ws_a/src/lib.rs - doc (line 2) ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    const CARGO_ERR: &str = "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.00s\n     Running unittests src/lib.rs (target/debug/deps/ws_a-8eb566cb7a440010)\nerror: test failed, to rerun pass `-p ws_a --lib`\n     Running tests/it.rs (target/debug/deps/it-2757fcfa8c8384a7)\n   Doc-tests ws_a\n";

    #[test]
    fn libtest_output_becomes_suites_modules_and_tests() {
        let mut run = TestRun::new(TestFormat::Libtest, "tests in ws_a");
        // stderr arriving late, after all of stdout, still names the suites in order.
        let shown = feed(&mut run, ConsoleKind::Stdout, CARGO_OUT);
        assert_eq!(shown, CARGO_OUT);
        feed(&mut run, ConsoleKind::Stderr, CARGO_ERR);
        run.finish(Some(101), false);

        let suites: Vec<(&str, Option<&str>)> = run.nodes()[ROOT]
            .children
            .iter()
            .map(|&i| {
                (
                    run.nodes()[i].label.as_str(),
                    run.nodes()[i].detail.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            suites,
            [
                ("ws_a", Some("src/lib.rs")),
                ("it", Some("tests/it.rs")),
                ("Doc-tests ws_a", None)
            ]
        );
        let lib = run.nodes()[ROOT].children[0];
        assert_eq!(
            run.suite(lib).unwrap().source.as_deref(),
            Some("src/lib.rs")
        );
        let group = find(&run, "tests::");
        assert_eq!(run.nodes()[group].kind, NodeKind::Group);
        assert_eq!(run.nodes()[group].counts.total, 3);
        let fails = find(&run, "tests::fails");
        assert_eq!(run.state(fails), TestState::Failed);
        assert_eq!(run.state(group), TestState::Failed);
        assert_eq!(run.state(find(&run, "tests::skipped")), TestState::Skipped);
        assert_eq!(run.state(find(&run, "integ")), TestState::Passed);
        assert_eq!(run.nodes()[fails].label, "fails");
        assert!(text(&run, fails).contains("some output\n"));
        assert!(text(&run, fails).contains("panicked at ws_a/src/lib.rs:11:42"));
        assert!(text(&run, find(&run, "tests::adds")).is_empty());
        let doc = find(&run, "ws_a/src/lib.rs - doc (line 2)");
        assert_eq!(run.nodes()[doc].parent, Some(run.nodes()[ROOT].children[2]));
        assert_eq!(run.nodes()[lib].duration, Some(Duration::from_millis(250)));
        let counts = run.nodes()[ROOT].counts;
        assert_eq!(
            (counts.total, counts.passed, counts.failed, counts.skipped),
            (5, 3, 1, 1)
        );
        assert_eq!(run.failed_tests(), [fails]);
        assert_eq!(run.state(ROOT), TestState::Failed);
    }

    #[test]
    fn a_test_whose_result_comes_after_its_output_runs_until_then() {
        let mut run = TestRun::new(TestFormat::Libtest, "t");
        feed(
            &mut run,
            ConsoleKind::Stdout,
            "running 1 test\ntest slow ... hello\n",
        );
        // `test slow ... ` with the output right after, as with --nocapture.
        let slow = find(&run, "slow");
        assert_eq!(run.state(slow), TestState::Running);
        feed(&mut run, ConsoleKind::Stdout, "ok\n");
        assert_eq!(run.state(slow), TestState::Passed);
    }

    #[test]
    fn a_build_that_fails_fails_the_run() {
        let mut run = TestRun::new(TestFormat::Libtest, "t");
        feed(
            &mut run,
            ConsoleKind::Stderr,
            "error[E0425]: cannot find value `x`\n",
        );
        assert_eq!(run.state(ROOT), TestState::Running);
        run.finish(Some(101), false);
        assert_eq!(run.state(ROOT), TestState::Failed);
        assert!(text(&run, ROOT).contains("E0425"));
    }

    const GO_JSON: &str = r#"{"Action":"start","Package":"example.com/gt/empty"}
{"Action":"output","Package":"example.com/gt/empty","Output":"?   \texample.com/gt/empty\t[no test files]\n"}
{"Action":"skip","Package":"example.com/gt/empty","Elapsed":0}
{"Action":"start","Package":"example.com/gt/sub"}
{"Action":"run","Package":"example.com/gt/sub","Test":"TestOk"}
{"Action":"output","Package":"example.com/gt/sub","Test":"TestOk","Output":"=== RUN   TestOk\n"}
{"Action":"output","Package":"example.com/gt/sub","Test":"TestOk","Output":"hi\n"}
{"Action":"output","Package":"example.com/gt/sub","Test":"TestOk","Output":"--- PASS: TestOk (0.00s)\n"}
{"Action":"pass","Package":"example.com/gt/sub","Test":"TestOk","Elapsed":0.5}
{"Action":"run","Package":"example.com/gt/sub","Test":"TestFail"}
{"Action":"run","Package":"example.com/gt/sub","Test":"TestFail/inner_case"}
{"Action":"output","Package":"example.com/gt/sub","Test":"TestFail/inner_case","Output":"    a_test.go:8: bad 1\n"}
{"Action":"fail","Package":"example.com/gt/sub","Test":"TestFail/inner_case","Elapsed":0}
{"Action":"fail","Package":"example.com/gt/sub","Test":"TestFail","Elapsed":0}
{"Action":"run","Package":"example.com/gt/sub","Test":"TestSkip"}
{"Action":"skip","Package":"example.com/gt/sub","Test":"TestSkip","Elapsed":0}
{"Action":"output","Package":"example.com/gt/sub","Output":"FAIL\texample.com/gt/sub\t0.242s\n"}
{"Action":"fail","Package":"example.com/gt/sub","Elapsed":0.242}
"#;

    #[test]
    fn go_events_become_packages_tests_and_subtests() {
        let mut run = TestRun::new(TestFormat::GoJson, "all tests");
        let mut shown = String::new();
        // Split mid-line, as pipes do.
        let (a, b) = GO_JSON.split_at(500);
        shown.push_str(&feed(&mut run, ConsoleKind::Stdout, a));
        shown.push_str(&feed(&mut run, ConsoleKind::Stdout, b));
        run.finish(Some(1), false);
        assert!(
            shown
                .starts_with("?   \texample.com/gt/empty\t[no test files]\n=== RUN   TestOk\nhi\n")
        );
        assert!(!shown.contains("Action"));

        let sub = find(&run, "example.com/gt/sub");
        assert_eq!(run.nodes()[sub].kind, NodeKind::Suite);
        let inner = find(&run, "TestFail/inner_case");
        let parent = find(&run, "TestFail");
        assert_eq!(run.nodes()[inner].parent, Some(parent));
        assert_eq!(run.nodes()[inner].label, "inner_case");
        assert_eq!(run.state(inner), TestState::Failed);
        assert_eq!(run.failed_tests(), [inner]);
        assert_eq!(text(&run, inner), "    a_test.go:8: bad 1\n");
        assert_eq!(text(&run, find(&run, "TestOk")).lines().nth(1), Some("hi"));
        assert_eq!(
            run.nodes()[find(&run, "TestOk")].duration,
            Some(Duration::from_millis(500))
        );
        assert_eq!(run.state(find(&run, "TestSkip")), TestState::Skipped);
        let empty = find(&run, "example.com/gt/empty");
        assert_eq!(run.nodes()[empty].counts.total, 0);
        assert_eq!(run.state(ROOT), TestState::Failed);
    }

    #[test]
    fn go_verbose_output_under_the_debugger_is_followed_too() {
        let mut run = TestRun::new(TestFormat::GoVerbose, "test TestFail");
        feed(
            &mut run,
            ConsoleKind::Stdout,
            "=== RUN   TestFail\n=== RUN   TestFail/inner_case\n    a_test.go:8: bad 1\n--- FAIL: TestFail (0.00s)\n    --- FAIL: TestFail/inner_case (0.01s)\nFAIL\n",
        );
        run.finish(Some(1), false);
        let inner = find(&run, "TestFail/inner_case");
        assert_eq!(run.state(inner), TestState::Failed);
        assert_eq!(run.state(find(&run, "TestFail")), TestState::Failed);
        assert!(text(&run, inner).contains("a_test.go:8: bad 1"));
        assert_eq!(run.nodes()[inner].duration, Some(Duration::from_millis(10)));
    }

    #[test]
    fn odin_state_changes_become_tests_with_their_logs() {
        let mut run = TestRun::new(TestFormat::Odin, "tests in pkg");
        // As it really comes: the failure logged before its test is said to have started.
        let log = "[INFO ] --- [2026-11-06 10:45:56] Starting test runner with 1 thread.\n[INFO ] --- [2026-11-06 10:45:56] Test #0 pkg.adds changed state to Running.\n[ERROR] --- [2026-11-06 10:45:56] \n/p/pkg/a_test.odin(14:2):fails() expected 1 + 1 to be 3, got 2\n[INFO ] --- [2026-11-06 10:45:56] Test #0 pkg.adds changed state to Successful.\n[INFO ] --- [2026-11-06 10:45:56] Test #1 pkg.fails changed state to Running.\n[INFO ] --- [2026-11-06 10:45:56] Test #1 pkg.fails changed state to Failed.\n\nFinished 2 tests in 376µs. 1 test failed.\n - pkg.fails \texpected 1 + 1 to be 3, got 2\n";
        feed(&mut run, ConsoleKind::Stderr, log);
        run.finish(Some(1), false);
        let fails = find(&run, "fails");
        assert_eq!(run.state(fails), TestState::Failed);
        assert_eq!(run.state(find(&run, "adds")), TestState::Passed);
        assert!(text(&run, fails).contains("a_test.odin(14:2)"));
        assert!(text(&run, find(&run, "adds")).is_empty());
        assert_eq!(run.nodes()[run.nodes()[fails].parent.unwrap()].label, "pkg");
        assert!(text(&run, ROOT).contains("Starting test runner"));
    }

    #[test]
    fn an_odin_failure_in_a_helper_reaches_its_test_through_the_summary() {
        let mut run = TestRun::new(TestFormat::Odin, "tests in pkg");
        let log = "[INFO ] --- [t] Test #0 pkg.uses_helper changed state to Running.\n[ERROR] --- [t] \n/p/a.odin(3:2):check() bad value\n[INFO ] --- [t] Test #0 pkg.uses_helper changed state to Failed.\n\nFinished 1 test in 1ms. 1 test failed.\n - pkg.uses_helper \tbad value\n";
        feed(&mut run, ConsoleKind::Stderr, log);
        run.finish(Some(1), false);
        assert_eq!(text(&run, find(&run, "uses_helper")), "bad value\n");
        assert!(text(&run, ROOT).contains("/p/a.odin(3:2):check() bad value"));
    }

    /// Jest 30's report for a file with a passing, a failing and a nested test, trimmed.
    const JEST_REPORT: &str = r#"{"numFailedTests":1,"success":false,"testResults":[{
        "name":"/p/js/src/add.test.js","status":"failed","message":"","startTime":1000,"endTime":1250,
        "assertionResults":[
          {"ancestorTitles":["add"],"title":"adds two numbers","fullName":"add adds two numbers",
           "status":"passed","duration":1,"failureMessages":[],"location":{"column":3,"line":4}},
          {"ancestorTitles":["add"],"title":"fails on purpose","fullName":"add fails on purpose",
           "status":"failed","duration":2,"location":{"column":3,"line":8},
           "failureMessages":["Error: expect(received).toBe(expected) // Object.is equality\n\nExpected: 5\nReceived: 4\n    at Object.toBe (/p/js/src/add.test.js:9:23)\n    at Promise.finally.completed (/p/node_modules/jest-circus/build/jestAdapterInit.js:1834:28)\n    at new Promise (<anonymous>)\n    at processTicksAndRejections (node:internal/process/task_queues:103:5)"]},
          {"ancestorTitles":["add","negatives"],"title":"adds them","fullName":"add negatives adds them",
           "status":"passed","duration":0,"failureMessages":[],"location":{"column":5,"line":13}},
          {"ancestorTitles":["add"],"title":"left out by the pattern","status":"pending","failureMessages":[]}
        ]}]}"#;

    #[test]
    fn a_jest_report_becomes_files_describes_and_tests() {
        let mut run = TestRun::new(TestFormat::Jest, "tests in add.test.js");
        run.set_jest_context(PathBuf::from("/p/js"), true);
        // What jest prints as it goes is only shown.
        assert_eq!(
            feed(&mut run, ConsoleKind::Stderr, "PASS src/add.test.js\n"),
            "PASS src/add.test.js\n"
        );
        run.load_jest_report(JEST_REPORT).unwrap();
        run.finish(Some(1), false);
        let sep = gleipnir_packages::JS_NAME_SEPARATOR;
        let file = find(&run, "src/add.test.js");
        assert_eq!(run.nodes()[file].kind, NodeKind::Suite);
        assert_eq!(
            run.suite(file).unwrap().source.as_deref(),
            Some("/p/js/src/add.test.js")
        );
        let failed = find(&run, &format!("add{sep}fails on purpose"));
        assert_eq!(run.state(failed), TestState::Failed);
        assert_eq!(run.nodes()[failed].label, "fails on purpose");
        assert_eq!(
            run.nodes()[failed].location,
            Some((PathBuf::from("/p/js/src/add.test.js"), 7))
        );
        // The failure, with the runner's own frames left out.
        assert_eq!(
            text(&run, failed),
            "Error: expect(received).toBe(expected) // Object.is equality\n\nExpected: 5\nReceived: 4\n    at Object.toBe (/p/js/src/add.test.js:9:23)\n"
        );
        let nested = find(&run, &format!("add{sep}negatives{sep}adds them"));
        let group = run.nodes()[nested].parent.unwrap();
        assert_eq!(run.nodes()[group].kind, NodeKind::Group);
        assert_eq!(run.nodes()[group].label, "negatives");
        assert_eq!(run.nodes()[group].name, format!("add{sep}negatives{sep}"));
        let counts = run.nodes()[ROOT].counts;
        assert_eq!((counts.total, counts.passed, counts.failed), (3, 2, 1));
        assert_eq!(run.failed_tests(), [failed]);
        assert!(run.unlocated().is_empty());
    }

    #[test]
    fn jest_reporter_events_fill_the_tree_as_tests_finish() {
        let mut run = TestRun::new(TestFormat::Jest, "tests in add.test.js");
        run.set_jest_context(PathBuf::from("/p/js"), true);
        let mark = gleipnir_packages::JS_REPORTER_MARK;
        let event = |json: &str| format!("{mark}{json}\n");
        // Jest's own output still shows; the reporter's lines don't.
        let shown = feed(
            &mut run,
            ConsoleKind::Stdout,
            &event(r#"{"event":"file","path":"/p/js/src/add.test.js"}"#),
        );
        assert_eq!(shown, "");
        let file = find(&run, "src/add.test.js");
        assert_eq!(run.state(file), TestState::Running);
        feed(
            &mut run,
            ConsoleKind::Stdout,
            &event(
                r#"{"event":"test","path":"/p/js/src/add.test.js","ancestorTitles":["add"],"title":"adds","status":"passed","duration":3,"failureMessages":[],"location":{"line":4,"column":3}}"#,
            ),
        );
        assert_eq!(
            run.state(find(
                &run,
                &format!("add{}adds", gleipnir_packages::JS_NAME_SEPARATOR)
            )),
            TestState::Passed
        );
        assert_eq!(
            run.nodes()[ROOT].counts.passed,
            1,
            "counted before the run ends"
        );
        // A test left out by the pattern isn't shown.
        feed(
            &mut run,
            ConsoleKind::Stdout,
            &event(
                r#"{"event":"test","path":"/p/js/src/add.test.js","ancestorTitles":["add"],"title":"other","status":"pending","failureMessages":[]}"#,
            ),
        );
        assert_eq!(run.nodes()[ROOT].counts.total, 1);
        assert_eq!(
            feed(&mut run, ConsoleKind::Stderr, "PASS src/add.test.js\n"),
            "PASS src/add.test.js\n"
        );
        feed(
            &mut run,
            ConsoleKind::Stdout,
            &event(r#"{"event":"fileDone","path":"/p/js/src/add.test.js","duration":120}"#),
        );
        run.finish(Some(0), false);
        assert_eq!(run.state(file), TestState::Passed);
        assert_eq!(run.nodes()[file].duration, Some(Duration::from_millis(120)));
    }

    #[test]
    fn a_jest_file_that_cannot_run_fails_with_its_message() {
        let report = r#"{"testResults":[{"name":"/p/a.test.ts","status":"failed",
            "message":"SyntaxError: Unexpected token (3:4)","assertionResults":[]}]}"#;
        let mut run = TestRun::new(TestFormat::Jest, "tests");
        run.set_jest_context(PathBuf::from("/p"), false);
        run.load_jest_report(report).unwrap();
        run.finish(Some(1), false);
        let file = find(&run, "a.test.ts");
        assert_eq!(run.state(file), TestState::Failed);
        assert_eq!(text(&run, file), "SyntaxError: Unexpected token (3:4)\n");
        assert_eq!(run.state(ROOT), TestState::Failed);
    }

    #[test]
    fn tests_still_running_at_the_end_were_stopped() {
        let mut run = TestRun::new(TestFormat::GoVerbose, "t");
        feed(&mut run, ConsoleKind::Stdout, "=== RUN   TestLong\n");
        run.finish(None, true);
        assert_eq!(run.state(find(&run, "TestLong")), TestState::Stopped);
        assert_eq!(run.state(ROOT), TestState::Stopped);
    }
}
