//! What the test runner needs from each language: how its tests report, how to run a chosen few of
//! them, and where each one is declared.

use crate::adapters::{RunTarget, RunTargetKind};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How a test run reports its results, which decides how its output is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestFormat {
    /// Rust's libtest: `test name ... ok`, then each failure's captured output.
    Libtest,
    /// `go test -json`: one JSON event per line.
    GoJson,
    /// A Go test binary run with `-test.v`, as delve runs it: `=== RUN` and `--- PASS:` lines.
    GoVerbose,
    /// Odin's test runner, logging each test's change of state.
    Odin,
    /// Jest's, or vitest's, JSON report, written to a file when the run ends; the console gets
    /// the usual output meanwhile.
    JestJson,
}

/// A suite of tests, as the run reported it: a Rust test binary, a Go package, an Odin package.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Suite {
    /// What the tool calls it: a crate name, an import path, an Odin package name.
    pub name: String,
    /// For Rust, the source the binary was built from, relative to its package, like
    /// `src/lib.rs` or `tests/api.rs`; `doc` for doc tests.
    pub source: Option<String>,
}

/// Tests to run: a whole suite, or a test or group of tests within it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestPick {
    pub suite: Suite,
    /// The test's full name in its suite, or a group's prefix, like `parser::tests::`.
    /// `None` runs the whole suite.
    pub name: Option<String>,
    /// Whether `name` is one test, rather than every test under a group.
    pub exact: bool,
}

/// The flags Odin's test runner gets, to log in a way that can be followed test by test.
pub const ODIN_TEST_DEFINES: &[&str] = &[
    "-define:ODIN_TEST_FANCY=false",
    "-define:ODIN_TEST_LOG_STATE_CHANGES=true",
    "-define:ODIN_TEST_GO_TO_ERROR=true",
    // One at a time, so what's logged between a test's start and end is its own.
    "-define:ODIN_TEST_THREADS=1",
];

impl RunTarget {
    /// The one test a target runs, for targets made from a test's line.
    pub fn single_test(&self) -> Option<&str> {
        if self.kind != RunTargetKind::Test {
            return None;
        }
        self.name.strip_prefix("test ")
    }

    /// How the target's tests report, when run or, with `debug`, debugged.
    pub fn test_format(&self, debug: bool) -> Option<TestFormat> {
        if self.kind != RunTargetKind::Test {
            return None;
        }
        match (self.package_id.as_str(), debug) {
            ("rust", _) => Some(TestFormat::Libtest),
            ("go", false) => Some(TestFormat::GoJson),
            ("go", true) => Some(TestFormat::GoVerbose),
            ("odin", _) => Some(TestFormat::Odin),
            ("typescript", _) => Some(TestFormat::JestJson),
            _ => None,
        }
    }
}

// Rust.

/// The Cargo flag that picks one test binary, for a suite built from `source`.
fn cargo_selector(suite: &Suite) -> Option<Vec<String>> {
    let source = suite.source.as_deref()?;
    let stem = |path: &str| {
        Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
    };
    let flags = if source == "doc" {
        vec!["--doc".to_string()]
    } else if source == "src/lib.rs" {
        vec!["--lib".to_string()]
    } else if source == "src/main.rs" {
        vec!["--bin".to_string(), suite.name.replace('_', "-")]
    } else if let Some(rest) = source.strip_prefix("src/bin/") {
        let name = match rest.strip_suffix("/main.rs") {
            Some(folder) => folder.to_string(),
            None => stem(rest)?,
        };
        vec!["--bin".to_string(), name]
    } else if source.starts_with("tests/") {
        vec!["--test".to_string(), suite.name.clone()]
    } else if source.starts_with("benches/") {
        vec!["--bench".to_string(), suite.name.clone()]
    } else {
        return None;
    };
    Some(flags)
}

/// A Rust test target narrowed to the tests picked.
pub(crate) fn rust_select(target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
    let cut = target.run.iter().position(|a| a == "--");
    let mut base: Vec<String> = target.run[..cut.unwrap_or(target.run.len())].to_vec();
    // One binary when every pick is in the same one, so a debugger has one program to launch.
    let suites: Vec<&Suite> = picks.iter().map(|p| &p.suite).collect();
    let same = suites.windows(2).all(|w| w[0] == w[1]);
    let selector = if same {
        suites.first().and_then(|s| cargo_selector(s))
    } else {
        None
    };
    let doc = selector.as_ref().is_some_and(|s| s[0] == "--doc");
    if let Some(selector) = &selector {
        // The binary picked replaces the one the target had, if it had one.
        let mut kept = Vec::new();
        let mut args = base.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--lib" | "--doc" => {}
                "--bin" | "--test" | "--bench" => {
                    args.next();
                }
                _ => kept.push(arg),
            }
        }
        base = kept;
        base.extend(selector.iter().cloned());
    }
    let names: Vec<String> = picks.iter().filter_map(|p| p.name.clone()).collect();
    let exact = !names.is_empty() && picks.iter().all(|p| p.exact);
    let mut filters = Vec::new();
    if exact {
        filters.push("--exact".to_string());
    }
    filters.extend(names.iter().cloned());

    let mut run = base.clone();
    if !filters.is_empty() {
        run.push("--".into());
        run.extend(filters.iter().cloned());
    }
    let mut build: Vec<String> = base
        .iter()
        .filter(|a| *a != "--no-fail-fast")
        .cloned()
        .collect();
    if let Some(at) = build.iter().position(|a| a == "test") {
        build.insert(at + 1, "--no-run".into());
    }
    let name = match (picks, names.as_slice()) {
        ([one], [name]) if one.exact => format!("test {}", name.rsplit("::").next()?),
        ([_], [name]) => {
            let group = name.trim_end_matches("::");
            match target.run.iter().skip_while(|a| *a != "-p").nth(1) {
                Some(package) => format!("tests in {package}::{group}"),
                None => format!("tests in {group}"),
            }
        }
        ([one], []) => format!("tests in {}", suite_label(&one.suite)),
        _ => format!("{} tests", names.len().max(picks.len())),
    };
    Some(RunTarget {
        name,
        // Doc tests aren't a binary that can be built ahead or debugged.
        build: (!doc).then_some(build),
        run,
        program: None,
        source: None,
        args: filters,
        ..target.clone()
    })
}

fn suite_label(suite: &Suite) -> String {
    match &suite.source {
        Some(source) if source != "doc" => format!("{} ({source})", suite.name),
        _ => suite.name.clone(),
    }
}

/// The folders of a Cargo project's packages: the root, and each workspace member.
fn cargo_packages(root: &Path) -> Vec<(String, PathBuf)> {
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(text) = fs::read_to_string(root.join("Cargo.toml"))
        && let Ok(value) = text.parse::<toml::Table>()
        && let Some(members) = value
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
    {
        for member in members.iter().filter_map(|m| m.as_str()) {
            match member.strip_suffix("/*") {
                Some(parent) => {
                    if let Ok(entries) = fs::read_dir(root.join(parent)) {
                        let mut found: Vec<PathBuf> =
                            entries.filter_map(Result::ok).map(|e| e.path()).collect();
                        found.sort();
                        dirs.extend(found);
                    }
                }
                None => dirs.push(root.join(member)),
            }
        }
    }
    dirs.into_iter()
        .filter_map(|dir| {
            let text = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
            let value = text.parse::<toml::Table>().ok()?;
            let name = value.get("package")?.get("name")?.as_str()?.to_string();
            Some((name, dir))
        })
        .collect()
}

/// Where Rust tests are declared, found by following their module path from the crate's root.
pub(crate) fn rust_locate(
    target: &RunTarget,
    suite: &Suite,
    names: &[String],
) -> Vec<Option<(PathBuf, u32)>> {
    let source = suite.source.as_deref().unwrap_or("src/lib.rs");
    if source == "doc" {
        // `crates/x/src/lib.rs - item (line 12)`, relative to where Cargo ran.
        return names
            .iter()
            .map(|name| {
                let (path, rest) = name.split_once(" - ")?;
                let line = rest.rsplit_once("(line ")?.1.trim_end_matches(')');
                let line: u32 = line.parse().ok()?;
                Some((target.cwd.join(path), line.saturating_sub(1)))
            })
            .collect();
    }
    let wanted = target.run.iter().skip_while(|a| *a != "-p").nth(1).cloned();
    let crate_name = suite.name.replace('-', "_");
    let packages = cargo_packages(&target.cwd);
    let package = packages
        .iter()
        .filter(|(_, dir)| dir.join(source).is_file())
        .max_by_key(|(name, _)| {
            (
                Some(name) == wanted.as_ref(),
                name.replace('-', "_") == crate_name,
            )
        });
    let Some((_, dir)) = package else {
        return vec![None; names.len()];
    };
    let root = dir.join(source);
    let mut files = FileCache::default();
    names
        .iter()
        .map(|name| rust_test_location(&root, name, &mut files))
        .collect()
}

#[derive(Default)]
struct FileCache(HashMap<PathBuf, Option<String>>);

impl FileCache {
    fn get(&mut self, path: &Path) -> Option<&str> {
        self.0
            .entry(path.to_path_buf())
            .or_insert_with(|| fs::read_to_string(path).ok())
            .as_deref()
    }
}

/// The line declaring `mod name`, at or after `from`, and whether its body is inline.
fn find_mod(text: &str, from: usize, name: &str) -> Option<(usize, bool)> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if start < from {
            continue;
        }
        let mut rest = line.trim_start();
        if let Some(after) = rest.strip_prefix("pub") {
            rest = match after.trim_start().strip_prefix('(') {
                Some(scoped) => scoped.split_once(')').map_or("", |(_, r)| r),
                None => after,
            }
            .trim_start();
        }
        let Some(after) = rest.strip_prefix("mod ") else {
            continue;
        };
        let after = after.trim_start();
        let Some(after) = after.strip_prefix(name) else {
            continue;
        };
        let after = after.trim_start();
        if after.starts_with('{') {
            return Some((offset, true));
        }
        if after.starts_with(';') {
            return Some((offset, false));
        }
    }
    None
}

/// The line declaring `fn name`, at or after `from`.
fn find_fn(text: &str, from: usize, name: &str) -> Option<u32> {
    let mut offset = 0;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += line.len();
        if start < from {
            continue;
        }
        let Some(at) = line.find("fn ") else { continue };
        let before = &line[..at];
        if !before
            .split_whitespace()
            .all(|w| matches!(w, "pub" | "async" | "unsafe" | "const") || w.starts_with("pub("))
        {
            continue;
        }
        let after = line[at + 3..].trim_start();
        if let Some(rest) = after.strip_prefix(name)
            && rest.starts_with(['(', '<'])
        {
            return Some(index as u32);
        }
    }
    None
}

fn rust_test_location(root: &Path, name: &str, files: &mut FileCache) -> Option<(PathBuf, u32)> {
    let mut segments: Vec<&str> = name.split("::").collect();
    let function = segments.pop()?;
    let mut file = root.to_path_buf();
    // Where a `mod x;` in this file would find x's file.
    let mut dir = root.parent()?.to_path_buf();
    let is_root = |path: &Path| {
        path.file_name()
            .is_some_and(|n| n == "lib.rs" || n == "main.rs" || n == "mod.rs")
            || path.parent().and_then(Path::file_name) == Some("tests".as_ref())
    };
    if !is_root(root) {
        dir = dir.join(root.file_stem()?);
    }
    let mut from = 0;
    for module in segments {
        let text = files.get(&file)?;
        let (after, inline) = find_mod(text, from, module)?;
        dir = dir.join(module);
        if inline {
            from = after;
        } else {
            let flat = dir.with_extension("rs");
            file = if flat.is_file() {
                flat
            } else {
                dir.join("mod.rs")
            };
            from = 0;
        }
    }
    let text = files.get(&file)?;
    let line = find_fn(text, from, function)?;
    Some((file, line))
}

/// The module path a Rust file adds to its tests' names, like `parser::` for `src/parser.rs`.
pub(crate) fn rust_module_path(file: &Path) -> Option<String> {
    let src = file
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|n| n == "src"))?;
    let relative = file.strip_prefix(src).ok()?;
    let mut parts: Vec<String> = relative
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let last = parts.pop()?;
    let stem = last.strip_suffix(".rs")?;
    if parts.first().is_some_and(|p| p == "bin") {
        return None;
    }
    if !matches!(stem, "lib" | "main" | "mod") {
        parts.push(stem.to_string());
    }
    Some(parts.iter().map(|p| format!("{p}::")).collect())
}

// Go.

/// The module path from a `go.mod`, which a package's import path starts with.
fn go_module(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("go.mod")).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("module "))
        .map(|m| m.trim().trim_matches('"').to_string())
}

/// The folder of a Go package, from its import path.
fn go_package_dir(root: &Path, import: &str) -> PathBuf {
    match go_module(root) {
        Some(module) if import == module => root.to_path_buf(),
        Some(module) => match import.strip_prefix(&format!("{module}/")) {
            Some(rest) => root.join(rest),
            None => root.to_path_buf(),
        },
        None => root.to_path_buf(),
    }
}

/// The folder of a suite's package. Under delve, results don't name their package, and the suite
/// is the target's own.
fn go_suite_dir(target: &RunTarget, suite: &Suite) -> PathBuf {
    match go_module(&target.cwd) {
        Some(module) if suite.name == module || suite.name.starts_with(&format!("{module}/")) => {
            go_package_dir(&target.cwd, &suite.name)
        }
        _ => target.program.clone().unwrap_or_else(|| target.cwd.clone()),
    }
}

/// A Go test name as a `-run` pattern: each level of subtest matched exactly.
fn go_run_pattern(name: &str) -> String {
    name.split('/')
        .map(|level| format!("^{}$", regex_escape(level)))
        .collect::<Vec<_>>()
        .join("/")
}

fn regex_escape(text: &str) -> String {
    let mut escaped = String::new();
    for c in text.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

pub(crate) fn go_select(target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for pick in picks {
        let dir = go_suite_dir(target, &pick.suite);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    let whole = picks.iter().any(|p| p.name.is_none());
    let names: Vec<&str> = picks.iter().filter_map(|p| p.name.as_deref()).collect();
    let pattern = match names.as_slice() {
        _ if whole => None,
        [] => None,
        [one] => Some(go_run_pattern(one)),
        // Several tests run by their top level, as subtests can't be listed side by side.
        many => {
            let mut top: Vec<&str> = many.iter().map(|n| n.split('/').next().unwrap()).collect();
            top.dedup();
            Some(format!(
                "^({})$",
                top.iter()
                    .map(|t| regex_escape(t))
                    .collect::<Vec<_>>()
                    .join("|")
            ))
        }
    };
    let mut run = vec!["go".to_string(), "test".into(), "-json".into()];
    let mut args = Vec::new();
    if let Some(pattern) = &pattern {
        run.extend(["-run".to_string(), pattern.clone()]);
        args.extend(["-test.run".to_string(), pattern.clone()]);
    }
    args.push("-test.v".into());
    for dir in &dirs {
        let relative = dir.strip_prefix(&target.cwd).unwrap_or(dir);
        run.push(if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            format!("./{}", relative.display())
        });
    }
    let name = match (picks, names.as_slice()) {
        ([one], [name]) if one.exact => format!("test {name}"),
        ([one], _) => format!("tests in {}", one.suite.name),
        _ => format!("{} tests", picks.len()),
    };
    Some(RunTarget {
        name,
        run,
        // delve debugs one package.
        program: dirs.first().cloned(),
        source: None,
        args,
        ..target.clone()
    })
}

pub(crate) fn go_locate(
    target: &RunTarget,
    suite: &Suite,
    names: &[String],
) -> Vec<Option<(PathBuf, u32)>> {
    let dir = go_suite_dir(target, suite);
    let mut files: Vec<(PathBuf, String)> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with("_test.go"))
        .filter_map(|p| Some((p.clone(), fs::read_to_string(&p).ok()?)))
        .collect();
    files.sort();
    names
        .iter()
        .map(|name| {
            let top = name.split('/').next()?;
            let declared = format!("func {top}(");
            files.iter().find_map(|(path, text)| {
                let line = text.lines().position(|l| l.starts_with(&declared))?;
                Some((path.clone(), line as u32))
            })
        })
        .collect()
}

// JavaScript and TypeScript.

/// What joins a test's `describe` titles and its own in the names the results use, as titles can
/// hold any other character, spaces included.
pub const JS_NAME_SEPARATOR: char = '\x1f';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsTestKind {
    /// A `describe`.
    Group,
    /// An `it` or a `test`.
    Test,
}

/// A `describe`, `it` or `test` call with a plain string title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsTestCall {
    pub kind: JsTestKind,
    pub title: String,
    /// The line it starts on, from 0.
    pub line: usize,
    /// Where its name starts and its closing parenthesis is, so calls inside it can be found.
    pub span: std::ops::Range<usize>,
}

/// Whether a file holds tests, by the conventions jest and vitest look for by default.
pub fn is_js_test_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let script = ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"]
        .iter()
        .any(|e| name.ends_with(&format!(".{e}")));
    script
        && (name.contains(".test.")
            || name.contains(".spec.")
            || path.components().any(|c| c.as_os_str() == "__tests__"))
}

/// Skips a quoted string or template literal starting at `i`, returning where it ends.
fn skip_js_string(bytes: &[u8], mut i: usize) -> usize {
    let quote = bytes[i];
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'$' if quote == b'`' && bytes.get(i + 1) == Some(&b'{') => {
                // A template's expression, which can hold strings of its own.
                let mut depth = 0;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        b'\'' | b'"' | b'`' => {
                            i = skip_js_string(bytes, i);
                            continue;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                i += 1;
            }
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The text of a string literal starting at `i`, if it's a plain one: a template with
/// expressions in it has no fixed title.
fn js_string_at(text: &str, i: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let quote = *bytes.get(i)?;
    if !matches!(quote, b'\'' | b'"' | b'`') {
        return None;
    }
    let end = skip_js_string(bytes, i);
    let inner = text.get(i + 1..end.checked_sub(1)?)?;
    if quote == b'`' && inner.contains("${") {
        return None;
    }
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Every `describe`, `it` and `test` call in a file with a plain string title, in order, with
/// `.only`, `.skip` and the like allowed. Parametrised ones, with `.each`, are left out, as their
/// titles are templates.
pub fn js_test_calls(text: &str) -> Vec<JsTestCall> {
    let bytes = text.as_bytes();
    let mut calls: Vec<(JsTestCall, usize)> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut closes: HashMap<usize, usize> = HashMap::new();
    let mut line = 0;
    let mut i = 0;
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    line += usize::from(bytes[i] == b'\n');
                    i += 1;
                }
                i += 2;
            }
            b'\'' | b'"' | b'`' => {
                let end = skip_js_string(bytes, i).min(bytes.len());
                line += bytes[i..end].iter().filter(|&&c| c == b'\n').count();
                i = end;
            }
            b'(' => {
                open.push(i);
                i += 1;
            }
            b')' => {
                if let Some(start) = open.pop() {
                    closes.insert(start, i);
                }
                i += 1;
            }
            _ if ident(b) && (i == 0 || !ident(bytes[i - 1])) => {
                let start = i;
                while i < bytes.len() && ident(bytes[i]) {
                    i += 1;
                }
                let word = &text[start..i];
                let before = text[..start].trim_end();
                let kind = match word {
                    "describe" | "fdescribe" | "xdescribe" | "suite" => JsTestKind::Group,
                    "it" | "test" | "fit" | "xit" => JsTestKind::Test,
                    _ => continue,
                };
                // `foo.it(` is someone else's method.
                if before.ends_with('.') {
                    continue;
                }
                // Modifiers, like `.only` and `.skip`; `.each` titles are templates.
                let mut j = i;
                let mut parametrised = false;
                while bytes.get(j) == Some(&b'.') {
                    let from = j + 1;
                    let mut k = from;
                    while k < bytes.len() && ident(bytes[k]) {
                        k += 1;
                    }
                    parametrised |= &text[from..k] == "each";
                    j = k;
                }
                if parametrised || bytes.get(j) != Some(&b'(') {
                    continue;
                }
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                if let Some(title) = js_string_at(text, k) {
                    calls.push((
                        JsTestCall {
                            kind,
                            title,
                            line,
                            span: start..start,
                        },
                        j,
                    ));
                }
                // The parenthesis is seen next, so it's matched like any other.
                i = j;
            }
            _ => i += 1,
        }
    }
    calls
        .into_iter()
        .map(|(mut call, paren)| {
            call.span.end = closes.get(&paren).copied().unwrap_or(bytes.len());
            call
        })
        .collect()
}

/// The titles from the outermost `describe` down to the call on a line, and what that call is.
pub fn js_test_chain(text: &str, line: usize) -> Option<(Vec<String>, JsTestKind)> {
    let calls = js_test_calls(text);
    let target = calls.iter().find(|c| c.line == line)?;
    let mut chain: Vec<String> = calls
        .iter()
        .filter(|c| {
            c.kind == JsTestKind::Group
                && c.span.start < target.span.start
                && c.span.end > target.span.start
        })
        .map(|c| c.title.clone())
        .collect();
    chain.push(target.title.clone());
    Some((chain, target.kind))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsRunner {
    Jest,
    Vitest,
}

/// How a project's tests run: the runner, the folder it runs in, and its command and script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsTestSetup {
    pub runner: JsRunner,
    pub cwd: PathBuf,
    /// `node_modules/.bin/jest` or `vitest`, to run.
    pub bin: PathBuf,
    /// The runner's own script, for Node to start under the debugger.
    pub script: PathBuf,
}

/// The runner a package.json declares, by its config or its dependencies.
fn js_runner_in_package(dir: &Path) -> Option<JsRunner> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    if json.get("jest").is_some() {
        return Some(JsRunner::Jest);
    }
    let depends = |name: &str| {
        ["devDependencies", "dependencies"]
            .iter()
            .any(|key| json.get(key).and_then(|d| d.get(name)).is_some())
    };
    if depends("vitest") {
        Some(JsRunner::Vitest)
    } else if depends("jest") {
        Some(JsRunner::Jest)
    } else {
        None
    }
}

/// The runner configured in a folder itself, by its config file.
fn js_runner_config(dir: &Path) -> Option<JsRunner> {
    let has = |stem: &str, extensions: &[&str]| {
        extensions
            .iter()
            .any(|e| dir.join(format!("{stem}.{e}")).is_file())
    };
    if has("vitest.config", &["ts", "mts", "cts", "js", "mjs", "cjs"]) {
        Some(JsRunner::Vitest)
    } else if has(
        "jest.config",
        &["ts", "mts", "cts", "js", "mjs", "cjs", "json"],
    ) {
        Some(JsRunner::Jest)
    } else {
        None
    }
}

/// How the tests in a file run: the nearest folder above it with a jest or vitest config, or
/// failing that a package.json that depends on one, and the runner installed nearest to it.
pub fn js_test_setup(file: &Path) -> Option<JsTestSetup> {
    let folders: Vec<&Path> = file.ancestors().skip(1).collect();
    let (cwd, runner) = folders
        .iter()
        .find_map(|dir| Some((*dir, js_runner_config(dir)?)))
        .or_else(|| {
            folders
                .iter()
                .find_map(|dir| Some((*dir, js_runner_in_package(dir)?)))
        })?;
    let (bin_name, script) = match runner {
        JsRunner::Jest => ("jest", "jest/bin/jest.js"),
        JsRunner::Vitest => ("vitest", "vitest/vitest.mjs"),
    };
    let modules = cwd
        .ancestors()
        .map(|dir| dir.join("node_modules"))
        .find(|modules| modules.join(".bin").join(bin_name).exists())?;
    Some(JsTestSetup {
        runner,
        cwd: cwd.to_path_buf(),
        bin: modules.join(".bin").join(bin_name),
        script: modules.join(script),
    })
}

/// Where a run's JSON report goes: a file of its own, by what it runs, so runs side by side
/// don't share one.
fn js_report_path(cwd: &Path, files: &[String], pattern: Option<&str>) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (cwd, files, pattern).hash(&mut hasher);
    std::env::temp_dir()
        .join("gleipnir-tests")
        .join(format!("{:016x}.json", hasher.finish()))
}

/// The JSON report a target writes, if it's a JavaScript test run.
pub fn js_report_of(target: &RunTarget) -> Option<PathBuf> {
    target
        .run
        .iter()
        .find_map(|arg| arg.strip_prefix("--outputFile="))
        .map(PathBuf::from)
}

/// A test target running some files, narrowed by a title pattern, with its report and, for the
/// debugger, the runner's script.
fn js_target(
    setup: &JsTestSetup,
    name: String,
    files: Vec<String>,
    pattern: Option<String>,
    source: Option<PathBuf>,
) -> RunTarget {
    let report = js_report_path(&setup.cwd, &files, pattern.as_deref());
    let mut args = Vec::new();
    if setup.runner == JsRunner::Vitest {
        args.push("run".to_string());
    }
    args.extend(files);
    if let Some(pattern) = pattern {
        args.extend(["-t".to_string(), pattern]);
    }
    match setup.runner {
        JsRunner::Jest => args.extend([
            "--json".to_string(),
            format!("--outputFile={}", report.display()),
            "--testLocationInResults".to_string(),
        ]),
        JsRunner::Vitest => args.extend([
            "--reporter=default".to_string(),
            "--reporter=json".to_string(),
            format!("--outputFile={}", report.display()),
            "--includeTaskLocation".to_string(),
        ]),
    }
    let mut run = vec![setup.bin.to_string_lossy().into_owned()];
    run.extend(args.iter().cloned());
    // Under the debugger, everything in one process, so breakpoints are where Node is.
    match setup.runner {
        JsRunner::Jest => args.push("--runInBand".to_string()),
        JsRunner::Vitest => args.push("--no-file-parallelism".to_string()),
    }
    RunTarget {
        package_id: "typescript".into(),
        name,
        kind: RunTargetKind::Test,
        build: None,
        run,
        program: Some(setup.script.clone()),
        cwd: setup.cwd.clone(),
        source,
        args,
    }
}

fn js_relative(setup: &JsTestSetup, file: &Path) -> String {
    file.strip_prefix(&setup.cwd)
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned()
}

/// A file's tests, or with a line, the `describe`, `it` or `test` on it.
pub fn js_test_target(file: &Path, chain: Option<(Vec<String>, JsTestKind)>) -> Option<RunTarget> {
    let setup = js_test_setup(file)?;
    let files = vec![js_relative(&setup, file)];
    let file_name = file.file_name()?.to_string_lossy().into_owned();
    Some(match chain {
        None => js_target(
            &setup,
            format!("tests in {file_name}"),
            files,
            None,
            Some(file.to_path_buf()),
        ),
        Some((chain, kind)) => {
            let full = regex_escape(&chain.join(" "));
            let (name, pattern) = match kind {
                JsTestKind::Test => (format!("test {}", chain.join(" › ")), format!("^{full}$")),
                JsTestKind::Group => (
                    format!("tests in {}", chain.join(" › ")),
                    format!("^{full} "),
                ),
            };
            js_target(&setup, name, files, Some(pattern), Some(file.to_path_buf()))
        }
    })
}

/// A JavaScript test run narrowed to some of its files, groups and tests.
pub(crate) fn js_select(target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
    let program = target.program.clone()?;
    let bin = PathBuf::from(target.run.first()?);
    let runner = if bin.file_name()? == "vitest" {
        JsRunner::Vitest
    } else {
        JsRunner::Jest
    };
    let setup = JsTestSetup {
        runner,
        cwd: target.cwd.clone(),
        bin,
        script: program,
    };
    let mut files: Vec<String> = Vec::new();
    let mut pieces: Vec<String> = Vec::new();
    let mut whole = false;
    for pick in picks {
        let file = pick
            .suite
            .source
            .clone()
            .unwrap_or_else(|| pick.suite.name.clone());
        let file = js_relative(&setup, Path::new(&file));
        if !files.contains(&file) {
            files.push(file);
        }
        match &pick.name {
            None => whole = true,
            Some(name) => {
                let titles = name
                    .trim_end_matches(JS_NAME_SEPARATOR)
                    .replace(JS_NAME_SEPARATOR, " ");
                let escaped = regex_escape(&titles);
                pieces.push(if pick.exact {
                    format!("{escaped}$")
                } else {
                    format!("{escaped} ")
                });
            }
        }
    }
    let pattern = (!whole && !pieces.is_empty()).then(|| match pieces.as_slice() {
        [one] => format!("^{one}"),
        many => format!("^(?:{})", many.join("|")),
    });
    let label = |name: &str| {
        name.trim_end_matches(JS_NAME_SEPARATOR)
            .replace(JS_NAME_SEPARATOR, " › ")
    };
    let name = match picks {
        [one] => match (&one.name, one.exact) {
            (Some(name), true) => format!("test {}", label(name)),
            (Some(name), false) => format!("tests in {}", label(name)),
            (None, _) => format!("tests in {}", one.suite.name),
        },
        many => format!("{} tests", many.len()),
    };
    let source = picks
        .first()
        .and_then(|p| p.suite.source.clone())
        .map(PathBuf::from);
    Some(js_target(&setup, name, files, pattern, source))
}

// Odin.

/// An Odin package's tests, built with the test runner's flags for following them.
pub(crate) fn odin_test_target(root: &Path, dir: &Path, names: &[String]) -> RunTarget {
    let package = dir
        .file_name()
        .map_or_else(|| "tests".into(), |n| n.to_string_lossy().into_owned());
    let program = root.join("build/gleipnir").join(format!("{package}-tests"));
    let mut defines: Vec<String> = ODIN_TEST_DEFINES.iter().map(|d| d.to_string()).collect();
    if !names.is_empty() {
        defines.push(format!("-define:ODIN_TEST_NAMES={}", names.join(",")));
    }
    let mut build = vec![
        "odin".to_string(),
        "build".into(),
        dir.to_string_lossy().into_owned(),
        "-build-mode:test".into(),
        "-debug".into(),
        format!("-out:{}", program.display()),
    ];
    build.extend(defines);
    let name = match names {
        [one] => format!("test {}", one.rsplit('.').next().unwrap_or(one)),
        [] => format!("tests in {package}"),
        many => format!("{} tests", many.len()),
    };
    RunTarget {
        package_id: "odin".into(),
        name,
        kind: RunTargetKind::Test,
        build: Some(build),
        run: vec![program.to_string_lossy().into_owned()],
        program: Some(program),
        cwd: root.to_path_buf(),
        source: Some(dir.to_path_buf()),
        args: Vec::new(),
    }
}

/// The package folder an Odin test target was made for.
fn odin_dir(target: &RunTarget) -> Option<PathBuf> {
    let build = target.build.as_ref()?;
    build.get(2).map(PathBuf::from)
}

pub(crate) fn odin_select(target: &RunTarget, picks: &[TestPick]) -> Option<RunTarget> {
    let dir = odin_dir(target)?;
    let names: Vec<String> = picks
        .iter()
        .filter_map(|p| Some(format!("{}.{}", p.suite.name, p.name.as_ref()?)))
        .collect();
    let whole = picks.iter().any(|p| p.name.is_none());
    Some(odin_test_target(
        &target.cwd,
        &dir,
        if whole { &[] } else { &names },
    ))
}

/// Whether a line declares an Odin test procedure, given the lines above it.
pub(crate) fn odin_test_name(lines: &[&str], line: usize) -> Option<String> {
    let declared = lines.get(line)?.trim_start();
    let (name, rest) = declared.split_once("::")?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if !rest.trim_start().starts_with("proc") || !rest.contains("testing.T") {
        return None;
    }
    let attributed = lines[..line]
        .iter()
        .rev()
        .take_while(|l| {
            let l = l.trim();
            l.starts_with("@") || l.starts_with("//") || l.is_empty()
        })
        .any(|l| l.contains("test"));
    attributed.then(|| name.to_string())
}

/// Whether a folder holds Odin tests.
pub(crate) fn has_odin_tests(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| {
            let path = e.path();
            path.extension().is_some_and(|x| x == "odin")
                && fs::read_to_string(&path).is_ok_and(|t| t.contains("@(test)"))
        })
    })
}

pub(crate) fn odin_locate(
    target: &RunTarget,
    _: &Suite,
    names: &[String],
) -> Vec<Option<(PathBuf, u32)>> {
    let Some(dir) = odin_dir(target) else {
        return vec![None; names.len()];
    };
    let mut files: Vec<(PathBuf, String)> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "odin"))
        .filter_map(|p| Some((p.clone(), fs::read_to_string(&p).ok()?)))
        .collect();
    files.sort();
    names
        .iter()
        .map(|name| {
            files.iter().find_map(|(path, text)| {
                let lines: Vec<&str> = text.lines().collect();
                let line = lines.iter().position(|l| {
                    l.trim_start()
                        .strip_prefix(name.as_str())
                        .is_some_and(|r| r.trim_start().starts_with("::"))
                })?;
                Some((path.clone(), line as u32))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JS_TESTS: &str = "import { add } from './add'\n\
// describe('commented out', () => {})\n\
describe('maths (basic)', () => {\n\
  const label = `not a ${'test'}`\n\
  it('adds', () => {\n\
    expect(add(1, 2)).toBe(3)\n\
  })\n\
  describe.only(\"nested\", function () {\n\
    test.skip('skips', async () => {})\n\
  })\n\
  it.each([1, 2])('runs %s', () => {})\n\
  helper.it('not mine', () => {})\n\
})\n\
test(`top level`, () => {})\n";

    #[test]
    fn js_test_calls_are_found_with_their_titles_and_extent() {
        let calls = js_test_calls(JS_TESTS);
        let found: Vec<(&str, JsTestKind, usize)> = calls
            .iter()
            .map(|c| (c.title.as_str(), c.kind, c.line))
            .collect();
        assert_eq!(
            found,
            [
                ("maths (basic)", JsTestKind::Group, 2),
                ("adds", JsTestKind::Test, 4),
                ("nested", JsTestKind::Group, 7),
                ("skips", JsTestKind::Test, 8),
                ("top level", JsTestKind::Test, 13),
            ]
        );
        // The outer describe's call ends at its closing parenthesis, after everything in it.
        let outer = &calls[0];
        assert_eq!(&JS_TESTS[outer.span.end..outer.span.end + 1], ")");
        assert!(outer.span.end > calls[3].span.start);
        assert!(outer.span.end < calls[4].span.start);
    }

    #[test]
    fn a_js_test_is_named_by_the_describes_around_it() {
        assert_eq!(
            js_test_chain(JS_TESTS, 8),
            Some((
                vec!["maths (basic)".into(), "nested".into(), "skips".into()],
                JsTestKind::Test
            ))
        );
        assert_eq!(
            js_test_chain(JS_TESTS, 7),
            Some((
                vec!["maths (basic)".into(), "nested".into()],
                JsTestKind::Group
            ))
        );
        assert_eq!(
            js_test_chain(JS_TESTS, 13),
            Some((vec!["top level".into()], JsTestKind::Test))
        );
        assert_eq!(js_test_chain(JS_TESTS, 5), None);
    }

    #[test]
    fn test_files_are_known_by_name_or_folder() {
        assert!(is_js_test_file(Path::new("/p/src/a.test.ts")));
        assert!(is_js_test_file(Path::new("/p/src/a.spec.tsx")));
        assert!(is_js_test_file(Path::new("/p/src/__tests__/build.ts")));
        assert!(!is_js_test_file(Path::new("/p/src/a.ts")));
        assert!(!is_js_test_file(Path::new("/p/src/a.test.md")));
    }

    /// A monorepo like Nx makes: jest configured per library, installed once at the top.
    fn js_monorepo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &[
                ("package.json", "{\"devDependencies\": {\"jest\": \"30\"}}"),
                ("node_modules/.bin/jest", ""),
                ("node_modules/jest/bin/jest.js", ""),
                ("libs/modules/jest.config.ts", "export default {}"),
                ("libs/modules/src/__tests__/log.test.ts", JS_TESTS),
                ("apps/web/vitest.config.ts", "export default {}"),
                ("apps/web/node_modules/.bin/vitest", ""),
                ("apps/web/src/a.spec.ts", JS_TESTS),
            ],
        );
        dir
    }

    #[test]
    fn js_tests_run_with_the_nearest_configured_runner() {
        let dir = js_monorepo();
        let root = dir.path();
        let file = root.join("libs/modules/src/__tests__/log.test.ts");
        let setup = js_test_setup(&file).unwrap();
        assert_eq!(setup.runner, JsRunner::Jest);
        assert_eq!(setup.cwd, root.join("libs/modules"));
        assert_eq!(setup.bin, root.join("node_modules/.bin/jest"));
        assert_eq!(setup.script, root.join("node_modules/jest/bin/jest.js"));

        let web = js_test_setup(&root.join("apps/web/src/a.spec.ts")).unwrap();
        assert_eq!(web.runner, JsRunner::Vitest);
        assert_eq!(web.bin, root.join("apps/web/node_modules/.bin/vitest"));
    }

    #[test]
    fn a_js_test_target_runs_one_test_by_its_full_title() {
        let dir = js_monorepo();
        let file = dir.path().join("libs/modules/src/__tests__/log.test.ts");
        let target = js_test_target(&file, js_test_chain(JS_TESTS, 4)).unwrap();
        assert_eq!(target.name, "test maths (basic) › adds");
        assert_eq!(target.single_test(), Some("maths (basic) › adds"));
        assert_eq!(target.test_format(false), Some(TestFormat::JestJson));
        let report = js_report_of(&target).unwrap();
        assert_eq!(
            &target.run[1..5],
            [
                "src/__tests__/log.test.ts",
                "-t",
                "^maths \\(basic\\) adds$",
                "--json"
            ]
        );
        assert!(target.run.contains(&"--testLocationInResults".to_string()));
        assert!(report.starts_with(std::env::temp_dir()));
        // Under the debugger, Node runs jest's script, in one process.
        assert!(
            target
                .program
                .as_ref()
                .unwrap()
                .ends_with("jest/bin/jest.js")
        );
        assert_eq!(target.args.last().map(String::as_str), Some("--runInBand"));

        let group = js_test_target(&file, js_test_chain(JS_TESTS, 7)).unwrap();
        assert_eq!(group.name, "tests in maths (basic) › nested");
        assert_eq!(group.run[3], "^maths \\(basic\\) nested ");
        let whole = js_test_target(&file, None).unwrap();
        assert_eq!(whole.name, "tests in log.test.ts");
        assert!(!whole.run.contains(&"-t".to_string()));

        let vitest = js_test_target(&dir.path().join("apps/web/src/a.spec.ts"), None).unwrap();
        assert_eq!(&vitest.run[1..3], ["run", "src/a.spec.ts"]);
    }

    #[test]
    fn js_tests_are_narrowed_to_the_picked_ones() {
        let dir = js_monorepo();
        let file = dir.path().join("libs/modules/src/__tests__/log.test.ts");
        let target = js_test_target(&file, None).unwrap();
        let suite = Suite {
            name: "src/__tests__/log.test.ts".into(),
            source: Some(file.to_string_lossy().into_owned()),
        };
        let sep = JS_NAME_SEPARATOR;
        let picks = [
            TestPick {
                suite: suite.clone(),
                name: Some(format!("maths (basic){sep}adds")),
                exact: true,
            },
            TestPick {
                suite: suite.clone(),
                name: Some(format!("maths (basic){sep}nested{sep}")),
                exact: false,
            },
        ];
        let narrowed = js_select(&target, &picks).unwrap();
        assert_eq!(narrowed.name, "2 tests");
        assert_eq!(
            &narrowed.run[1..4],
            [
                "src/__tests__/log.test.ts",
                "-t",
                "^(?:maths \\(basic\\) adds$|maths \\(basic\\) nested )"
            ]
        );
        let one = js_select(&target, &picks[..1]).unwrap();
        assert_eq!(one.name, "test maths (basic) › adds");
        assert_eq!(one.run[3], "^maths \\(basic\\) adds$");
    }

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    fn rust_target(root: &Path) -> RunTarget {
        RunTarget {
            package_id: "rust".into(),
            name: "tests in app".into(),
            kind: RunTargetKind::Test,
            build: None,
            run: ["cargo", "test", "-p", "app", "--no-fail-fast"]
                .map(String::from)
                .to_vec(),
            program: None,
            cwd: root.to_path_buf(),
            source: None,
            args: Vec::new(),
        }
    }

    #[test]
    fn rust_tests_are_found_through_their_modules() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &[
                ("Cargo.toml", "[workspace]\nmembers = [\"app\"]\n"),
                ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
                (
                    "app/src/lib.rs",
                    "mod parser;\npub mod util {\n    #[cfg(test)]\n    mod tests {\n        #[test]\n        fn inline() {}\n    }\n}\n#[test]\nfn top() {}\n",
                ),
                (
                    "app/src/parser.rs",
                    "fn parse() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    pub fn parses() {}\n}\n",
                ),
                ("app/tests/api.rs", "mod common;\n#[test]\nfn api() {}\n"),
                ("app/tests/common/mod.rs", "#[test]\nfn shared() {}\n"),
            ],
        );
        let target = rust_target(dir.path());
        let lib = Suite {
            name: "app".into(),
            source: Some("src/lib.rs".into()),
        };
        let found = rust_locate(
            &target,
            &lib,
            &[
                "top".into(),
                "util::tests::inline".into(),
                "parser::tests::parses".into(),
                "missing::x".into(),
            ],
        );
        let app = dir.path().join("app");
        assert_eq!(
            found,
            [
                Some((app.join("src/lib.rs"), 9)),
                Some((app.join("src/lib.rs"), 5)),
                Some((app.join("src/parser.rs"), 4)),
                None
            ]
        );
        let api = Suite {
            name: "api".into(),
            source: Some("tests/api.rs".into()),
        };
        let found = rust_locate(&target, &api, &["api".into(), "common::shared".into()]);
        assert_eq!(
            found,
            [
                Some((app.join("tests/api.rs"), 2)),
                Some((app.join("tests/common/mod.rs"), 1))
            ]
        );
        let doc = Suite {
            name: "Doc-tests app".into(),
            source: Some("doc".into()),
        };
        let found = rust_locate(&target, &doc, &["app/src/lib.rs - util (line 3)".into()]);
        assert_eq!(found, [Some((dir.path().join("app/src/lib.rs"), 2))]);
    }

    #[test]
    fn rust_picks_narrow_the_command_to_one_binary_and_exact_names() {
        let dir = tempfile::tempdir().unwrap();
        let target = rust_target(dir.path());
        let lib = Suite {
            name: "app".into(),
            source: Some("src/lib.rs".into()),
        };
        let picks = [
            TestPick {
                suite: lib.clone(),
                name: Some("a::b".into()),
                exact: true,
            },
            TestPick {
                suite: lib.clone(),
                name: Some("c".into()),
                exact: true,
            },
        ];
        let narrowed = rust_select(&target, &picks).unwrap();
        assert_eq!(
            narrowed.run.join(" "),
            "cargo test -p app --no-fail-fast --lib -- --exact a::b c"
        );
        // Narrowing again keeps one binary flag.
        let again = rust_select(&narrowed, &picks[..1]).unwrap();
        assert_eq!(
            again.run.join(" "),
            "cargo test -p app --no-fail-fast --lib -- --exact a::b"
        );
        assert_eq!(
            narrowed.build.unwrap().join(" "),
            "cargo test --no-run -p app --lib"
        );
        assert_eq!(narrowed.args, ["--exact", "a::b", "c"]);

        let module = TestPick {
            suite: Suite {
                name: "api".into(),
                source: Some("tests/api.rs".into()),
            },
            name: Some("common::".into()),
            exact: false,
        };
        let narrowed = rust_select(&target, &[module]).unwrap();
        assert_eq!(
            narrowed.run.join(" "),
            "cargo test -p app --no-fail-fast --test api -- common::"
        );
        assert_eq!(narrowed.name, "tests in app::common");
    }

    #[test]
    fn module_paths_follow_the_file_layout() {
        let path = |p: &str| rust_module_path(Path::new(p));
        assert_eq!(path("/p/src/lib.rs").as_deref(), Some(""));
        assert_eq!(path("/p/src/parser.rs").as_deref(), Some("parser::"));
        assert_eq!(path("/p/src/a/b/mod.rs").as_deref(), Some("a::b::"));
        assert_eq!(path("/p/src/a/c.rs").as_deref(), Some("a::c::"));
    }

    #[test]
    fn go_picks_become_run_patterns() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &[("go.mod", "module example.com/m\n\ngo 1.22\n")],
        );
        let target = RunTarget {
            package_id: "go".into(),
            name: "all tests".into(),
            kind: RunTargetKind::Test,
            build: None,
            run: ["go", "test", "-json", "./..."].map(String::from).to_vec(),
            program: None,
            cwd: dir.path().to_path_buf(),
            source: None,
            args: Vec::new(),
        };
        let sub = Suite {
            name: "example.com/m/sub".into(),
            source: None,
        };
        let one = TestPick {
            suite: sub.clone(),
            name: Some("TestA/case one".into()),
            exact: true,
        };
        let narrowed = go_select(&target, &[one]).unwrap();
        assert_eq!(
            narrowed.run.join(" "),
            "go test -json -run ^TestA$/^case one$ ./sub"
        );
        assert_eq!(narrowed.program, Some(dir.path().join("sub")));
        let two = [
            TestPick {
                suite: sub.clone(),
                name: Some("TestA/x".into()),
                exact: true,
            },
            TestPick {
                suite: sub,
                name: Some("TestB".into()),
                exact: true,
            },
        ];
        let narrowed = go_select(&target, &two).unwrap();
        assert_eq!(narrowed.args, ["-test.run", "^(TestA|TestB)$", "-test.v"]);
    }

    #[test]
    fn odin_test_procedures_are_recognised() {
        let text = "package p\n\n@(test)\nadds :: proc(t: ^testing.T) {\n}\n\nhelper :: proc(t: ^testing.T) {}\n";
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(odin_test_name(&lines, 3).as_deref(), Some("adds"));
        assert_eq!(odin_test_name(&lines, 6), None);
    }
}
