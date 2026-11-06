//! What the debugger shows in an editor: its file's breakpoints, Run icons on entry points, the
//! execution line, and the values of variables beside the lines that use them.

use gleipnir_core::{BufferModel, Debugger};
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

pub struct DebugDecorations {
    debugger: Entity<Debugger>,
    buffer: WeakEntity<BufferModel>,
    /// Entry point lines by buffer version, as finding them reads the whole file.
    run_lines: RefCell<Option<(u64, Vec<usize>)>>,
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
        Language::TypeScript | Language::Tsx | Language::JavaScript => false,
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
        let (Some(path), Some(buffer)) = (self.path(cx), self.buffer.upgrade()) else {
            return Vec::new();
        };
        let runs = self
            .debugger
            .read(cx)
            .target_for_file(&path)
            .is_some_and(|t| t.kind != RunTargetKind::File);
        if !runs {
            return Vec::new();
        }
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
        // Only lines mentioning `main` can declare it, which keeps this quick in a large file.
        let whole = text.text();
        let mut lines: Vec<usize> = whole
            .match_indices("main")
            .map(|(offset, _)| text.offset_to_point(offset).line)
            .filter(|&line| is_entry_point(language, &text.line(line)))
            .collect();
        lines.dedup();
        *self.run_lines.borrow_mut() = Some((version, lines.clone()));
        lines
    }

    fn inline_values(&self, lines: Range<usize>, cx: &App) -> Vec<(usize, String)> {
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
    fn long_values_are_cut_to_one_short_line() {
        assert_eq!(
            shorten("Point {\n  x: 1,\n  y: 2\n}"),
            "Point { x: 1, y: 2 }"
        );
        let long = "x".repeat(60);
        assert_eq!(shorten(&long).chars().count(), VALUE_CHARS + 1);
    }
}
