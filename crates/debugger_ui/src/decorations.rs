//! What the debugger shows in an editor: its file's breakpoints, Run icons on entry points, the
//! execution line, and the values of variables beside the lines that use them.

use gleipnir_core::{BufferModel, Debugger, TestState};
use gleipnir_editor::{BreakpointKind, BreakpointMark, DecorationProvider};
use gleipnir_packages::RunTargetKind;
use gleipnir_syntax::Language;
use gpui::{App, Entity, WeakEntity};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;

/// Values longer than this are cut short, as they're a glance, not the variables view.
const VALUE_CHARS: usize = 40;
/// How far above the execution line values are shown when the function can't be found.
const FALLBACK_LINES: usize = 30;

/// A Run icon's line, and the test declared there, if it's one test.
type RunLine = (usize, Option<String>);

pub struct DebugDecorations {
    debugger: Entity<Debugger>,
    buffer: WeakEntity<BufferModel>,
    /// Entry point lines by buffer version, as finding them reads the whole file, each with the
    /// name of the test declared there, if it's one test.
    run_lines: RefCell<Option<(u64, Vec<RunLine>)>>,
}

impl DebugDecorations {
    pub fn new(debugger: Entity<Debugger>, buffer: WeakEntity<BufferModel>) -> Self {
        Self {
            debugger,
            buffer,
            run_lines: RefCell::new(None),
        }
    }

    fn path(&self, cx: &App) -> Option<PathBuf> {
        let buffer = self.buffer.upgrade()?;
        buffer.read(cx).path().map(PathBuf::from)
    }
}

/// Lines that might declare a test, worth asking the language about, found in one pass: in Rust,
/// a function right under a `#[...test]` attribute or a `mod` under `#[cfg(test)]`; in Go, a
/// `func Test` in a `_test.go` file; in Odin, a procedure taking a `^testing.T`; in a JavaScript
/// or TypeScript test file, each `describe`, `it` and `test`.
pub fn test_candidates(language: Language, path: &std::path::Path, text: &str) -> Vec<usize> {
    match language {
        Language::Rust => {
            // Cheap to rule out, which most files are.
            if !text.contains("test]") && !text.contains("cfg(test)") {
                return Vec::new();
            }
            let mut attributed = false;
            let mut cfg_test = false;
            let mut found = Vec::new();
            for (index, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.starts_with("#[") {
                    attributed |= line.ends_with("test]");
                    cfg_test |= line.contains("cfg(test)");
                } else if line.starts_with("fn ")
                    || line.starts_with("async fn ")
                    || line.starts_with("pub fn ")
                {
                    if attributed {
                        found.push(index);
                    }
                    attributed = false;
                    cfg_test = false;
                } else if line.starts_with("mod ") || line.starts_with("pub mod ") {
                    if cfg_test && line.ends_with('{') {
                        found.push(index);
                    }
                    attributed = false;
                    cfg_test = false;
                } else if !line.is_empty() && !line.starts_with("//") {
                    attributed = false;
                    cfg_test = false;
                }
            }
            found
        }
        Language::Odin if text.contains("testing.T") => text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("::") && l.contains("proc") && l.contains("testing.T"))
            .map(|(i, _)| i)
            .collect(),
        Language::TypeScript | Language::Tsx | Language::JavaScript
            if gleipnir_packages::is_js_test_file(path) =>
        {
            gleipnir_packages::testing::js_test_calls(text)
                .into_iter()
                .map(|call| call.line)
                .collect()
        }
        Language::Go if path.to_string_lossy().ends_with("_test.go") => text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with("func Test"))
            .map(|(i, _)| i)
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a line declares a program's entry point, in the file's language.
pub fn is_entry_point(language: Language, line: &str) -> bool {
    let line = line.trim_start();
    match language {
        Language::Rust => {
            let line = line.strip_prefix("pub ").unwrap_or(line);
            let line = line.strip_prefix("async ").unwrap_or(line);
            line.starts_with("fn main(")
        }
        Language::Go => line.starts_with("func main()"),
        Language::Odin => {
            line.starts_with("main")
                && line["main".len()..].trim_start().starts_with("::")
                && line.contains("proc")
        }
        // Scripts run whole, and C# has no run configurations yet.
        Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::CSharp
        | Language::Json
        | Language::Html
        | Language::Css
        | Language::Markdown => false,
    }
}

impl DecorationProvider for DebugDecorations {
    fn breakpoints(&self, cx: &App) -> Vec<BreakpointMark> {
        let Some(path) = self.path(cx) else {
            return Vec::new();
        };
        let debugger = self.debugger.read(cx);
        let debugging = debugger
            .launches()
            .iter()
            .any(|l| l.debug && l.status.is_live());
        debugger
            .breakpoints()
            .in_file(&path)
            .iter()
            .map(|b| BreakpointMark {
                line: b.line as usize,
                kind: if b.is_logpoint() {
                    BreakpointKind::Log
                } else if b.is_conditional() {
                    BreakpointKind::Conditional
                } else {
                    BreakpointKind::Line
                },
                enabled: b.enabled,
                verified: debugging
                    .then(|| {
                        debugger
                            .breakpoint_status(&path, b.line)
                            .map(|s| s.verified)
                    })
                    .flatten(),
            })
            .collect()
    }

    fn execution_line(&self, cx: &App) -> Option<(usize, bool)> {
        let path = self.path(cx)?;
        let (stopped_in, line, innermost) = self.debugger.read(cx).execution_point()?;
        (stopped_in == path).then_some((line as usize, innermost))
    }

    fn run_lines(&self, cx: &App) -> Vec<usize> {
        self.named_run_lines(cx)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    fn run_results(&self, cx: &App) -> Vec<(usize, bool)> {
        let Some(path) = self.path(cx) else {
            return Vec::new();
        };
        let debugger = self.debugger.read(cx);
        self.named_run_lines(cx)
            .into_iter()
            .filter_map(
                |(line, name)| match debugger.test_result(&path, name.as_deref()?)? {
                    TestState::Passed => Some((line, true)),
                    TestState::Failed => Some((line, false)),
                    _ => None,
                },
            )
            .collect()
    }

    fn inline_values(&self, lines: Range<usize>, cx: &App) -> Vec<(usize, String)> {
        self.values_inline(lines, cx)
    }
}

impl DebugDecorations {
    /// The Run icons' lines, each with the test declared there, if it's one test.
    fn named_run_lines(&self, cx: &App) -> Vec<RunLine> {
        let (Some(path), Some(buffer)) = (self.path(cx), self.buffer.upgrade()) else {
            return Vec::new();
        };
        let debugger = self.debugger.read(cx);
        let model = buffer.read(cx);
        let Some(language) = model.language() else {
            return Vec::new();
        };
        let text = model.buffer();
        let version = text.version();
        if let Some((cached, lines)) = &*self.run_lines.borrow()
            && *cached == version
        {
            return lines.clone();
        }
        // Asked only when the text has changed, as for a test file it means looking for its
        // runner on disk.
        let runs = debugger
            .target_for_file(&path)
            .is_some_and(|t| t.kind != RunTargetKind::File);
        // Tests, which are worth a Run icon wherever they are.
        let mut tests: Vec<RunLine> = Vec::new();
        // Only these languages have tests to find, or entry points to mark.
        if !runs && !matches!(language, Language::Rust | Language::Go | Language::Odin) {
            *self.run_lines.borrow_mut() = Some((version, Vec::new()));
            return Vec::new();
        }
        let whole = text.text();
        let candidates = test_candidates(language, &path, &whole);
        if !candidates.is_empty() {
            tests = candidates
                .into_iter()
                .filter_map(|line| {
                    let target = debugger.test_at(&path, &whole, line)?;
                    Some((line, target.single_test().map(String::from)))
                })
                .collect();
        }
        if !runs {
            *self.run_lines.borrow_mut() = Some((version, tests.clone()));
            return tests;
        }
        // Only lines mentioning `main` can declare it, which keeps this quick in a large file.
        let mut lines: Vec<RunLine> = whole
            .match_indices("main")
            .map(|(offset, _)| text.offset_to_point(offset).line)
            .filter(|&line| is_entry_point(language, &text.line(line)))
            .map(|line| (line, None))
            .collect();
        lines.extend(tests);
        lines.sort();
        lines.dedup_by_key(|(line, _)| *line);
        *self.run_lines.borrow_mut() = Some((version, lines.clone()));
        lines
    }

    fn values_inline(&self, lines: Range<usize>, cx: &App) -> Vec<(usize, String)> {
        let (Some(path), Some(buffer)) = (self.path(cx), self.buffer.upgrade()) else {
            return Vec::new();
        };
        let debugger = self.debugger.read(cx);
        let Some((stopped_in, execution, _)) = debugger.execution_point() else {
            return Vec::new();
        };
        let Some((frame_path, values)) = debugger.frame_values() else {
            return Vec::new();
        };
        if stopped_in != path || frame_path != path {
            return Vec::new();
        }
        let values: HashMap<&str, &str> = values
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let model = buffer.read(cx);
        let Some(syntax) = model.syntax() else {
            return Vec::new();
        };
        let text = model.buffer();
        let execution = execution as usize;
        if execution >= text.line_count() {
            return Vec::new();
        }
        // From the start of the function that's running, down to where it stopped.
        let at = text.line_start(execution) + text.leading_whitespace(execution).len();
        let first = syntax
            .enclosing_function(at)
            .map(|f| text.offset_to_point(f.start).line)
            .unwrap_or_else(|| execution.saturating_sub(FALLBACK_LINES));
        let first = first.max(lines.start);
        let last = execution.min(lines.end.saturating_sub(1));
        if first > last {
            return Vec::new();
        }
        let mut by_line: Vec<(usize, Vec<String>)> = Vec::new();
        for range in syntax.identifiers(text.line_start(first)..text.line_end(last)) {
            let name = text.slice(range.clone());
            let Some(value) = values.get(name.as_str()) else {
                continue;
            };
            let line = text.offset_to_point(range.start).line;
            let shown = format!("{name}: {}", shorten(value));
            match by_line.last_mut() {
                Some((at, list)) if *at == line => {
                    if !list.contains(&shown) {
                        list.push(shown);
                    }
                }
                _ => by_line.push((line, vec![shown])),
            }
        }
        by_line
            .into_iter()
            .map(|(line, list)| (line, list.join(", ")))
            .collect()
    }
}

fn shorten(value: &str) -> String {
    let single: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= VALUE_CHARS {
        return single;
    }
    let mut cut: String = single.chars().take(VALUE_CHARS).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_points_per_language() {
        assert!(is_entry_point(Language::Rust, "fn main() {"));
        assert!(is_entry_point(
            Language::Rust,
            "async fn main() -> Result<()> {"
        ));
        assert!(!is_entry_point(Language::Rust, "fn main_loop() {"));
        assert!(is_entry_point(Language::Go, "func main() {"));
        assert!(!is_entry_point(Language::Go, "func mainly() {"));
        assert!(is_entry_point(Language::Odin, "main :: proc() {"));
        assert!(!is_entry_point(Language::Odin, "maintain :: proc() {"));
        assert!(!is_entry_point(Language::TypeScript, "function main() {"));
    }

    #[test]
    fn test_candidates_are_functions_under_a_test_attribute() {
        let rust = "fn main() {}\n#[test]\nfn first() {}\n#[tokio::test]\n// a note\nasync fn second() {}\n#[derive(Debug)]\nfn not_a_test() {}\n";
        assert_eq!(
            test_candidates(Language::Rust, std::path::Path::new("a.rs"), rust),
            [2, 5]
        );
        let go = "func TestA(t *testing.T) {\n";
        assert_eq!(
            test_candidates(Language::Go, std::path::Path::new("a.go"), go),
            Vec::<usize>::new()
        );
        assert_eq!(
            test_candidates(Language::Go, std::path::Path::new("a_test.go"), go),
            [0]
        );
    }

    #[test]
    fn long_values_are_cut_to_one_short_line() {
        assert_eq!(
            shorten("Point {\n  x: 1,\n  y: 2\n}"),
            "Point { x: 1, y: 2 }"
        );
        let long = "x".repeat(60);
        assert_eq!(shorten(&long).chars().count(), VALUE_CHARS + 1);
    }
}
