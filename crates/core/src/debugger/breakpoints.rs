//! Breakpoints belong to the project, not to a debug session: they're kept between sessions and
//! restarts, and move with the text as it's edited.

use gleipnir_dap::SourceBreakpoint;
use gleipnir_text::AppliedEdit;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breakpoint {
    /// Zero-based, like every line in the editor.
    pub line: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Stop only when this expression is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Stop only on some hits, in the adapter's syntax, like `>= 3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    /// A logpoint: print this instead of stopping. `{expression}` is replaced by its value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

fn yes() -> bool {
    true
}

impl Breakpoint {
    pub fn at(line: u32) -> Self {
        Self {
            line,
            enabled: true,
            ..Default::default()
        }
    }

    pub fn is_conditional(&self) -> bool {
        self.condition.is_some() || self.hit_condition.is_some()
    }

    pub fn is_logpoint(&self) -> bool {
        self.log_message.is_some()
    }

    pub fn to_source(&self) -> SourceBreakpoint {
        SourceBreakpoint {
            line: self.line + 1,
            condition: self.condition.clone(),
            hit_condition: self.hit_condition.clone(),
            log_message: self.log_message.clone(),
        }
    }
}

/// Every breakpoint in a project, by file and then line, at most one per line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Breakpoints {
    files: BTreeMap<PathBuf, Vec<Breakpoint>>,
}

impl Breakpoints {
    pub fn files(&self) -> impl Iterator<Item = (&Path, &[Breakpoint])> {
        self.files.iter().map(|(p, b)| (p.as_path(), b.as_slice()))
    }

    pub fn in_file(&self, path: &Path) -> &[Breakpoint] {
        self.files.get(path).map_or(&[], Vec::as_slice)
    }

    pub fn get(&self, path: &Path, line: u32) -> Option<&Breakpoint> {
        self.in_file(path).iter().find(|b| b.line == line)
    }

    pub fn len(&self) -> usize {
        self.files.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Adds a breakpoint, or removes the one that's there. Returns whether one was added.
    pub fn toggle(&mut self, path: &Path, line: u32) -> bool {
        if self.get(path, line).is_some() {
            self.remove(path, line);
            false
        } else {
            self.set(path, Breakpoint::at(line));
            true
        }
    }

    /// Adds or replaces the breakpoint on its line.
    pub fn set(&mut self, path: &Path, breakpoint: Breakpoint) {
        let list = self.files.entry(path.to_path_buf()).or_default();
        match list.binary_search_by_key(&breakpoint.line, |b| b.line) {
            Ok(index) => list[index] = breakpoint,
            Err(index) => list.insert(index, breakpoint),
        }
    }

    pub fn remove(&mut self, path: &Path, line: u32) -> Option<Breakpoint> {
        let list = self.files.get_mut(path)?;
        let index = list.iter().position(|b| b.line == line)?;
        let removed = list.remove(index);
        if list.is_empty() {
            self.files.remove(path);
        }
        Some(removed)
    }

    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// What an adapter is told about a file: the enabled breakpoints.
    pub fn for_adapter(&self, path: &Path) -> Vec<SourceBreakpoint> {
        self.in_file(path)
            .iter()
            .filter(|b| b.enabled)
            .map(Breakpoint::to_source)
            .collect()
    }

    /// Moves a file's breakpoints with an edit, as given in the coordinates before it. Returns
    /// whether any moved.
    ///
    /// Breakpoints after the edit shift by the lines it added or removed. One on the line where
    /// the edit starts stays, unless text was inserted before everything on that line but its
    /// indentation, as typing Enter at its start does: then it moves down with the line.
    /// `blank_before` says whether only whitespace precedes an edit on its line. One on a line the
    /// edit removed joins the line where it started, merging with any that's already there.
    pub fn apply_edits(
        &mut self,
        path: &Path,
        edits: &[AppliedEdit],
        blank_before: impl Fn(&AppliedEdit) -> bool,
    ) -> bool {
        let Some(list) = self.files.get_mut(path) else {
            return false;
        };
        let before = list.clone();
        // A batch's edits come last first, each in the coordinates of the document as it stood
        // when it was made, so applying them in turn is right.
        for edit in edits {
            let (start, old_end) = (edit.start.line as i64, edit.old_end.line as i64);
            let added = edit.new_text.matches('\n').count() as i64;
            let delta = added - (old_end - start);
            let at_line_start =
                edit.old_end == edit.start && (edit.start.col == 0 || blank_before(edit));
            // An edit ending at the very start of a line leaves that line's text alone.
            let spares_end_line = edit.old_end.col == 0 && old_end > start;
            for breakpoint in list.iter_mut() {
                let line = breakpoint.line as i64;
                let untouched = line > old_end || (line == old_end && spares_end_line);
                let moved = if untouched || (line == start && at_line_start) {
                    line + delta
                } else if line > start {
                    start
                } else {
                    line
                };
                breakpoint.line = moved.max(0) as u32;
            }
            // Two that landed on one line become one, keeping the first.
            list.sort_by_key(|b| b.line);
            list.dedup_by_key(|b| b.line);
        }
        *list != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gleipnir_text::{Buffer, SelectionSet, TextEdit, UndoGrouping};
    use proptest::prelude::*;

    fn path() -> PathBuf {
        PathBuf::from("/p/main.rs")
    }

    fn lines(breakpoints: &Breakpoints) -> Vec<u32> {
        breakpoints
            .in_file(&path())
            .iter()
            .map(|b| b.line)
            .collect()
    }

    /// Edits a buffer and moves the breakpoints with it, as the debugger does.
    fn edit(
        buffer: &mut Buffer,
        breakpoints: &mut Breakpoints,
        range: std::ops::Range<usize>,
        text: &str,
    ) {
        let cursor = SelectionSet::cursor(0);
        let applied = buffer.apply_edits(
            vec![TextEdit::new(range, text)],
            &cursor,
            &cursor,
            UndoGrouping::Distinct,
            std::time::Instant::now(),
        );
        breakpoints.apply_edits(&path(), &applied, |e| {
            buffer.line(e.start.line)[..e.start.col].trim().is_empty()
        });
    }

    fn with_breakpoints(at: &[u32]) -> Breakpoints {
        let mut breakpoints = Breakpoints::default();
        for line in at {
            breakpoints.toggle(&path(), *line);
        }
        breakpoints
    }

    #[test]
    fn toggling_adds_then_removes_and_lines_stay_sorted() {
        let mut breakpoints = with_breakpoints(&[5, 1, 3]);
        assert_eq!(lines(&breakpoints), [1, 3, 5]);
        assert!(!breakpoints.toggle(&path(), 3));
        assert_eq!(lines(&breakpoints), [1, 5]);
        breakpoints.toggle(&path(), 1);
        breakpoints.toggle(&path(), 5);
        assert!(breakpoints.is_empty());
    }

    #[test]
    fn adapters_get_enabled_ones_counted_from_one_with_their_conditions() {
        let mut breakpoints = with_breakpoints(&[0, 2]);
        breakpoints.set(
            &path(),
            Breakpoint {
                line: 4,
                condition: Some("i > 3".into()),
                log_message: Some("i is {i}".into()),
                ..Breakpoint::at(4)
            },
        );
        breakpoints.set(
            &path(),
            Breakpoint {
                enabled: false,
                ..Breakpoint::at(2)
            },
        );
        let sent = breakpoints.for_adapter(&path());
        assert_eq!(sent.iter().map(|b| b.line).collect::<Vec<_>>(), [1, 5]);
        assert_eq!(sent[1].condition.as_deref(), Some("i > 3"));
        assert_eq!(sent[1].log_message.as_deref(), Some("i is {i}"));
    }

    #[test]
    fn breakpoints_follow_lines_added_and_removed_above_them() {
        let mut buffer = Buffer::new("a\nb\nc\nd\n");
        let mut breakpoints = with_breakpoints(&[1, 3]);
        // A new line at the top pushes both down.
        edit(&mut buffer, &mut breakpoints, 0..0, "new\n");
        assert_eq!(lines(&breakpoints), [2, 4]);
        // Typing within a line moves nothing.
        edit(&mut buffer, &mut breakpoints, 5..5, "xyz");
        assert_eq!(lines(&breakpoints), [2, 4]);
        // Removing the first line pulls both up.
        edit(&mut buffer, &mut breakpoints, 0..4, "");
        assert_eq!(lines(&breakpoints), [1, 3]);
    }

    #[test]
    fn enter_at_the_start_of_a_line_takes_its_breakpoint_down_but_at_the_end_does_not() {
        let mut buffer = Buffer::new("a\nb\nc\n");
        let mut breakpoints = with_breakpoints(&[1]);
        edit(&mut buffer, &mut breakpoints, 2..2, "\n");
        assert_eq!(lines(&breakpoints), [2]);
        // The end of the breakpoint's line.
        let end = buffer.line_end(2);
        edit(&mut buffer, &mut breakpoints, end..end, "\n");
        assert_eq!(lines(&breakpoints), [2]);
    }

    #[test]
    fn enter_after_only_indentation_also_takes_it_down() {
        let mut buffer = Buffer::new("fn main() {\n    let a = 1;\n}\n");
        let mut breakpoints = with_breakpoints(&[1]);
        let at = buffer.line_start(1) + 4;
        edit(&mut buffer, &mut breakpoints, at..at, "\n    ");
        assert_eq!(lines(&breakpoints), [2]);
        assert_eq!(buffer.line(2), "    let a = 1;");
    }

    #[test]
    fn deleting_a_breakpoints_line_moves_it_to_where_the_deletion_started_and_merges() {
        let mut buffer = Buffer::new("a\nb\nc\nd\n");
        let mut breakpoints = with_breakpoints(&[1, 2]);
        // Delete from the end of line 0 through line 2's end: lines 1 and 2 go.
        let (from, to) = (buffer.line_end(0), buffer.line_end(2));
        edit(&mut buffer, &mut breakpoints, from..to, "");
        assert_eq!(lines(&breakpoints), [0]);
        assert_eq!(buffer.text(), "a\nd\n");
    }

    #[test]
    fn saved_and_restored_as_plain_json() {
        let mut breakpoints = with_breakpoints(&[3]);
        breakpoints.set(
            Path::new("/p/other.go"),
            Breakpoint {
                condition: Some("x == 1".into()),
                ..Breakpoint::at(9)
            },
        );
        let json = serde_json::to_value(&breakpoints).unwrap();
        assert_eq!(json["/p/main.rs"][0]["line"], 3);
        let restored: Breakpoints = serde_json::from_value(json).unwrap();
        assert_eq!(restored, breakpoints);
        // Older or hand-written state without the optional fields still loads.
        let minimal: Breakpoints =
            serde_json::from_value(serde_json::json!({ "/p/a.rs": [{ "line": 2 }] })).unwrap();
        assert!(minimal.get(Path::new("/p/a.rs"), 2).unwrap().enabled);
    }

    proptest! {
        /// After random edits, each breakpoint is still on a line with the same text as the one
        /// it was set on, unless that line itself was edited.
        #[test]
        fn breakpoints_stay_on_their_lines_through_edits_elsewhere(
            line_count in 3usize..12,
            marked in 0usize..12,
            edits in proptest::collection::vec((0usize..12, 0usize..3, 0usize..3), 1..8),
        ) {
            let marked = marked % line_count;
            let text: String = (0..line_count).map(|i| format!("line{i}\n")).collect();
            let mut buffer = Buffer::new(&text);
            let mut breakpoints = with_breakpoints(&[marked as u32]);
            let mark_text = format!("line{marked}");
            for (at, removed, inserted) in edits {
                let lines_now = buffer.line_count().saturating_sub(1).max(1);
                let at = at % lines_now;
                let bp = breakpoints.in_file(&path())[0].line as usize;
                // Edit whole lines other than the breakpoint's.
                let removed = removed.min(lines_now - at);
                if (at..at + removed).contains(&bp) || (at == bp && inserted == 0 && removed == 0) {
                    continue;
                }
                let start = buffer.line_start(at);
                let end = buffer.line_start(at + removed);
                let new: String = (0..inserted).map(|i| format!("new{i}\n")).collect();
                edit(&mut buffer, &mut breakpoints, start..end, &new);
                let bp = breakpoints.in_file(&path())[0].line as usize;
                prop_assert_eq!(buffer.line(bp), mark_text.clone(), "{}", buffer.text());
            }
        }
    }
}
