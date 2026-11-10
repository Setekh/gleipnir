//! Editing commands as pure functions over a buffer and a selection set.
//!
//! The editor view is a thin layer that maps input to these, which is what makes every command
//! testable without a window, for one cursor and for many.

use crate::{
    AppliedEdit, Bias, Buffer, Selection, SelectionSet, TextEdit, UndoGrouping,
    byte_col_at_display, display_column, map_offset_through_edits,
};
use std::ops::Range;
use std::time::Instant;

/// The language-specific behaviour a command needs.
#[derive(Clone, Debug)]
pub struct EditConfig {
    pub tab_size: usize,
    pub hard_tabs: bool,
    pub line_comment: Option<String>,
    /// What opens and closes a comment, for languages with no line comment, like HTML and CSS.
    pub block_comment: Option<(String, String)>,
    /// Characters that auto-close, as (open, close). Quotes have open == close.
    pub pairs: Vec<(char, char)>,
}

impl Default for EditConfig {
    fn default() -> Self {
        Self {
            tab_size: 4,
            hard_tabs: false,
            line_comment: None,
            block_comment: None,
            pairs: vec![('(', ')'), ('[', ']'), ('{', '}'), ('"', '"'), ('\'', '\'')],
        }
    }
}

impl EditConfig {
    fn indent_unit(&self) -> String {
        if self.hard_tabs {
            "\t".to_string()
        } else {
            " ".repeat(self.tab_size)
        }
    }

    fn closer_for(&self, open: char) -> Option<char> {
        self.pairs.iter().find(|(o, _)| *o == open).map(|(_, c)| *c)
    }

    fn is_closer(&self, c: char) -> bool {
        self.pairs.iter().any(|(_, close)| *close == c)
    }
}

/// The state every command works on. A short-lived view over the editor's fields.
pub struct EditContext<'a> {
    pub buffer: &'a mut Buffer,
    pub selections: &'a mut SelectionSet,
    pub config: &'a EditConfig,
    pub now: Instant,
}

/// What one selection wants to do: replace `range` with `text`, then select `anchor..head`, both
/// relative to the start of the new text.
struct Plan {
    range: Range<usize>,
    text: String,
    anchor: usize,
    head: usize,
}

impl Plan {
    /// Replaces `range` and leaves a cursor `head` bytes into the new text.
    fn replace(range: Range<usize>, text: impl Into<String>, head: usize) -> Self {
        Self {
            range,
            text: text.into(),
            anchor: head,
            head,
        }
    }

    /// Replaces `range` and leaves the cursor after the new text.
    fn insert(range: Range<usize>, text: impl Into<String>) -> Self {
        let text = text.into();
        let head = text.len();
        Self {
            range,
            text,
            anchor: head,
            head,
        }
    }

    fn delete(range: Range<usize>) -> Self {
        Self {
            range,
            text: String::new(),
            anchor: 0,
            head: 0,
        }
    }
}

impl EditContext<'_> {
    /// Runs one plan per selection as a single batch. Plans whose ranges overlap are merged,
    /// which happens when, say, two cursors in one word both delete the word.
    fn apply_plans(&mut self, plans: Vec<Plan>, grouping: UndoGrouping) -> Vec<AppliedEdit> {
        let primary_index = self.selections.primary_index();
        let mut merged: Vec<(Plan, usize)> = Vec::with_capacity(plans.len());
        for (index, plan) in plans.into_iter().enumerate() {
            match merged.last_mut() {
                Some((last, last_index)) if plan.range.start < last.range.end => {
                    last.range.end = last.range.end.max(plan.range.end);
                    if index == primary_index {
                        *last_index = index;
                    }
                }
                _ => merged.push((plan, index)),
            }
        }

        let mut edits = Vec::with_capacity(merged.len());
        let mut after = Vec::with_capacity(merged.len());
        let mut primary = 0;
        let mut delta = 0isize;
        for (position, (plan, index)) in merged.into_iter().enumerate() {
            let base = (plan.range.start as isize + delta) as usize;
            after.push(Selection::new(base + plan.anchor, base + plan.head));
            delta += plan.text.len() as isize - plan.range.len() as isize;
            if index <= primary_index {
                primary = position;
            }
            edits.push(TextEdit {
                range: plan.range,
                text: plan.text,
            });
        }
        let after = SelectionSet::from_vec(after, primary);
        let applied = self
            .buffer
            .apply_edits(edits, self.selections, &after, grouping, self.now);
        *self.selections = after;
        applied
    }

    /// Runs a batch that isn't one-per-selection, such as editing every line in a selection.
    /// Selections follow the edits; a cursor at an insertion moves past it.
    fn apply_line_edits(&mut self, edits: Vec<TextEdit>) -> Vec<AppliedEdit> {
        let mut edits = edits;
        edits.sort_by_key(|e| e.range.start);
        let mut after = self.selections.clone();
        after.update(|s| {
            Selection::new(
                map_offset_through_edits(s.anchor, &edits, Bias::Right),
                map_offset_through_edits(s.head, &edits, Bias::Right),
            )
        });
        let applied = self.buffer.apply_edits(
            edits,
            self.selections,
            &after,
            UndoGrouping::Distinct,
            self.now,
        );
        *self.selections = after;
        applied
    }

    /// The distinct lines touched by any selection, ascending. A selection ending at column 0
    /// doesn't count that last line, matching what the highlight shows the user.
    fn selected_lines(&self) -> Vec<usize> {
        let mut lines = Vec::new();
        for selection in self.selections.iter() {
            let range = selection.range();
            let first = self.buffer.offset_to_point(range.start).line;
            let end = self.buffer.offset_to_point(range.end);
            let last = if end.col == 0 && end.line > first {
                end.line - 1
            } else {
                end.line
            };
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Applies edits that came from elsewhere, like a completion or a language server, as one undo
    /// step. Ranges are in the document as it is now, and mustn't overlap. Selections follow the
    /// edits, except that `cursor`, if given, is where the primary cursor ends up, in the document
    /// as it will be after the edits.
    pub fn apply_edits(
        &mut self,
        edits: Vec<TextEdit>,
        cursor: Option<usize>,
        grouping: UndoGrouping,
    ) -> Vec<AppliedEdit> {
        let mut edits = edits;
        edits.sort_by_key(|e| e.range.start);
        let mut after = self.selections.clone();
        after.update(|s| {
            Selection::new(
                map_offset_through_edits(s.anchor, &edits, Bias::Left),
                map_offset_through_edits(s.head, &edits, Bias::Right),
            )
        });
        if let Some(cursor) = cursor {
            after = SelectionSet::cursor(cursor);
        }
        let applied = self
            .buffer
            .apply_edits(edits, self.selections, &after, grouping, self.now);
        after.clip(self.buffer);
        *self.selections = after;
        applied
    }

    /// Inserts text at every selection, as for an IME commit or multi-character input.
    pub fn insert(&mut self, text: &str, grouping: UndoGrouping) -> Vec<AppliedEdit> {
        let plans = self
            .selections
            .iter()
            .map(|s| Plan::insert(s.range(), text))
            .collect();
        self.apply_plans(plans, grouping)
    }

    /// Handles one typed character, with auto-closing, overtyping and selection wrapping.
    pub fn type_char(&mut self, c: char) -> Vec<AppliedEdit> {
        if c == '\n' {
            return self.newline();
        }
        let plans = self
            .selections
            .iter()
            .map(|s| self.plan_typed_char(s, c))
            .collect();
        self.apply_plans(plans, UndoGrouping::Typing)
    }

    fn plan_typed_char(&self, selection: &Selection, c: char) -> Plan {
        let range = selection.range();
        let closer = self.config.closer_for(c);

        if let Some(close) = closer
            && !selection.is_empty()
        {
            // Wrap the selection, keeping the original text selected.
            let inner = self.buffer.slice(range.clone());
            let inner_len = inner.len();
            let open_len = c.len_utf8();
            return Plan {
                text: format!("{c}{inner}{close}"),
                range,
                anchor: open_len,
                head: open_len + inner_len,
            };
        }

        if selection.is_empty() {
            let next = self.buffer.char_at(range.start);
            // Typing a closer that's already there steps over it.
            if self.config.is_closer(c) && next == Some(c) {
                return Plan::replace(range.start..range.start, "", c.len_utf8());
            }
            if let Some(close) = closer
                && self.should_auto_close(range.start, c, close, next)
            {
                return Plan::replace(range, format!("{c}{close}"), c.len_utf8());
            }
        }
        Plan::insert(range, c.to_string())
    }

    fn should_auto_close(
        &self,
        offset: usize,
        open: char,
        close: char,
        next: Option<char>,
    ) -> bool {
        // Don't close in front of code: `(|foo` should stay as typed.
        let next_allows = match next {
            None => true,
            Some(n) => n.is_whitespace() || self.config.is_closer(n) || n == ',' || n == ';',
        };
        if !next_allows {
            return false;
        }
        if open == close {
            // A quote after a word is an apostrophe or a closing quote, not an opening one.
            let before = self.buffer.char_before(offset);
            return !before.is_some_and(|b| b.is_alphanumeric() || b == '_' || b == open);
        }
        true
    }

    /// Splits the line, keeping indentation and adding a level after an opening bracket.
    pub fn newline(&mut self) -> Vec<AppliedEdit> {
        let unit = self.config.indent_unit();
        let plans = self
            .selections
            .iter()
            .map(|selection| {
                let range = selection.range();
                let point = self.buffer.offset_to_point(range.start);
                let whitespace = self.buffer.leading_whitespace(point.line);
                let indent = &whitespace[..whitespace.len().min(point.col)];

                // Leave no trailing whitespace behind when pressing enter on a blank line.
                let line_start = self.buffer.line_start(point.line);
                let only_whitespace_before = point.col <= whitespace.len();
                let replace_start = if only_whitespace_before
                    && selection.is_empty()
                    && point.col == self.buffer.line_len(point.line)
                {
                    line_start
                } else {
                    range.start
                };
                let replaced = replace_start..range.end;

                let before = self.buffer.char_before(range.start);
                let after = self.buffer.char_at(range.end);
                let opens = before.and_then(|b| {
                    (b != '"' && b != '\'')
                        .then(|| self.config.closer_for(b))
                        .flatten()
                });
                match opens {
                    Some(close) if after == Some(close) => {
                        let first = format!("\n{indent}{unit}");
                        let head = first.len();
                        Plan::replace(replaced, format!("{first}\n{indent}"), head)
                    }
                    Some(_) => Plan::insert(replaced, format!("\n{indent}{unit}")),
                    None => Plan::insert(replaced, format!("\n{indent}")),
                }
            })
            .collect();
        self.apply_plans(plans, UndoGrouping::Distinct)
    }

    pub fn backspace(&mut self) -> Vec<AppliedEdit> {
        let plans = self
            .selections
            .iter()
            .map(|selection| {
                if !selection.is_empty() {
                    return Plan::delete(selection.range());
                }
                let at = selection.head;
                let point = self.buffer.offset_to_point(at);
                let before = self.buffer.char_before(at);

                // Deleting an opener with its auto-inserted closer.
                if let Some(open) = before
                    && let Some(close) = self.config.closer_for(open)
                    && self.buffer.char_at(at) == Some(close)
                {
                    return Plan::delete(at - open.len_utf8()..at + close.len_utf8());
                }

                // In leading spaces, go back to the previous tab stop.
                let line = self.buffer.line(point.line);
                if point.col > 0 && line[..point.col].bytes().all(|b| b == b' ') {
                    let width = ((point.col - 1) % self.config.tab_size) + 1;
                    return Plan::delete(at - width..at);
                }
                Plan::delete(self.buffer.prev_boundary(at)..at)
            })
            .collect();
        self.apply_plans(plans, UndoGrouping::Typing)
    }

    pub fn delete_forward(&mut self) -> Vec<AppliedEdit> {
        self.delete_each(|buffer, at| at..buffer.next_boundary(at))
    }

    pub fn delete_word_left(&mut self) -> Vec<AppliedEdit> {
        self.delete_each(|buffer, at| buffer.prev_word(at)..at)
    }

    pub fn delete_word_right(&mut self) -> Vec<AppliedEdit> {
        self.delete_each(|buffer, at| at..buffer.next_word(at))
    }

    fn delete_each(
        &mut self,
        range_at: impl Fn(&Buffer, usize) -> Range<usize>,
    ) -> Vec<AppliedEdit> {
        let plans = self
            .selections
            .iter()
            .map(|s| {
                let range = if s.is_empty() {
                    range_at(self.buffer, s.head)
                } else {
                    s.range()
                };
                Plan::delete(range)
            })
            .collect();
        self.apply_plans(plans, UndoGrouping::Distinct)
    }

    /// Deletes every line touched by a selection.
    pub fn delete_lines(&mut self) -> Vec<AppliedEdit> {
        let lines = self.selected_lines();
        let mut edits: Vec<TextEdit> = Vec::new();
        for block in contiguous_blocks(&lines) {
            edits.push(TextEdit::new(self.whole_lines_range(block), ""));
        }
        let mut after = self.selections.clone();
        after.update(|s| {
            let head = map_offset_through_edits(s.head, &edits, Bias::Left);
            Selection::cursor(head)
        });
        let applied = self.buffer.apply_edits(
            edits,
            self.selections,
            &after,
            UndoGrouping::Distinct,
            self.now,
        );
        *self.selections = after;
        applied
    }

    /// The bytes to remove to delete these lines entirely. The last line has no trailing newline
    /// to take with it, so it takes the preceding one instead.
    fn whole_lines_range(&self, block: Range<usize>) -> Range<usize> {
        let start = self.buffer.line_start(block.start);
        if block.end < self.buffer.line_count() {
            start..self.buffer.line_start(block.end)
        } else if start > 0 {
            start - 1..self.buffer.len()
        } else {
            start..self.buffer.len()
        }
    }

    /// Duplicates selected text, or the whole line for a bare cursor, and moves onto the copy.
    pub fn duplicate(&mut self) -> Vec<AppliedEdit> {
        let plans = self
            .selections
            .iter()
            .map(|selection| {
                let range = selection.range();
                if selection.is_empty() {
                    let point = self.buffer.offset_to_point(selection.head);
                    let line = self.buffer.line(point.line);
                    let end = self.buffer.line_end(point.line);
                    let text = format!("\n{line}");
                    let head = 1 + point.col;
                    Plan::replace(end..end, text, head)
                } else {
                    let text = self.buffer.slice(range.clone());
                    let len = text.len();
                    Plan {
                        range: range.end..range.end,
                        text,
                        anchor: 0,
                        head: len,
                    }
                }
            })
            .collect();
        self.apply_plans(plans, UndoGrouping::Distinct)
    }

    /// Moves every line touched by a selection up or down by one, keeping the selections on it.
    pub fn move_lines(&mut self, up: bool) -> Vec<AppliedEdit> {
        let lines = self.selected_lines();
        // Blocks that would collide move together.
        let mut blocks: Vec<Range<usize>> = Vec::new();
        for block in contiguous_blocks(&lines) {
            match blocks.last_mut() {
                Some(last) if last.end + 1 >= block.start => last.end = block.end,
                _ => blocks.push(block),
            }
        }
        let line_count = self.buffer.line_count();
        let at_edge = if up {
            blocks.first().is_some_and(|b| b.start == 0)
        } else {
            blocks.last().is_some_and(|b| b.end >= line_count)
        };
        if at_edge {
            return Vec::new();
        }

        let mut edits = Vec::new();
        let mut shifts: Vec<(Range<usize>, isize)> = Vec::new();
        for block in &blocks {
            let block_start = self.buffer.line_start(block.start);
            let block_end = self.buffer.line_end(block.end - 1);
            let text = self.buffer.slice(block_start..block_end);
            if up {
                let previous = block.start - 1;
                let previous_start = self.buffer.line_start(previous);
                let previous_text = self.buffer.line(previous);
                edits.push(TextEdit::new(
                    previous_start..block_end,
                    format!("{text}\n{previous_text}"),
                ));
                shifts.push((block_start..block_end, -(previous_text.len() as isize + 1)));
            } else {
                let next_text = self.buffer.line(block.end);
                let next_end = self.buffer.line_end(block.end);
                edits.push(TextEdit::new(
                    block_start..next_end,
                    format!("{next_text}\n{text}"),
                ));
                shifts.push((block_start..block_end, next_text.len() as isize + 1));
            }
        }

        let document_len = self.buffer.len();
        let shift_of = |offset: usize| {
            shifts
                .iter()
                // One past the end: a selection ending at column 0 of the next line ends with its
                // block's newline, so that end moves with the block.
                .find(|(range, _)| range.start <= offset && offset <= range.end + 1)
                .map_or(offset, |(_, shift)| {
                    // Moved to the end of the file, the block has no newline left to end after.
                    ((offset as isize + shift) as usize).min(document_len)
                })
        };
        let mut after = self.selections.clone();
        after.update(|s| Selection::new(shift_of(s.anchor), shift_of(s.head)));
        let applied = self.buffer.apply_edits(
            edits,
            self.selections,
            &after,
            UndoGrouping::Distinct,
            self.now,
        );
        *self.selections = after;
        applied
    }

    /// Tab: inserts indentation at cursors, or indents whole lines when a selection spans lines.
    pub fn indent(&mut self) -> Vec<AppliedEdit> {
        let spans_lines = self.selections.iter().any(|s| {
            let range = s.range();
            self.buffer.offset_to_point(range.start).line
                != self.buffer.offset_to_point(range.end).line
        });
        if spans_lines {
            let unit = self.config.indent_unit();
            let edits = self
                .selected_lines()
                .into_iter()
                .filter(|&line| self.buffer.line_len(line) > 0)
                .map(|line| {
                    let start = self.buffer.line_start(line);
                    TextEdit::new(start..start, unit.clone())
                })
                .collect();
            return self.apply_line_edits(edits);
        }
        let plans = self
            .selections
            .iter()
            .map(|selection| {
                let range = selection.range();
                if self.config.hard_tabs {
                    return Plan::insert(range, "\t");
                }
                let point = self.buffer.offset_to_point(range.start);
                let line = self.buffer.line(point.line);
                let column = display_column(&line, point.col, self.config.tab_size);
                let width = self.config.tab_size - column % self.config.tab_size;
                Plan::insert(range, " ".repeat(width))
            })
            .collect();
        self.apply_plans(plans, UndoGrouping::Distinct)
    }

    /// Shift+Tab: removes one level of indentation from every touched line.
    pub fn outdent(&mut self) -> Vec<AppliedEdit> {
        let tab_size = self.config.tab_size;
        let edits = self
            .selected_lines()
            .into_iter()
            .filter_map(|line| {
                let whitespace = self.buffer.leading_whitespace(line);
                let remove = if whitespace.starts_with('\t') {
                    1
                } else {
                    whitespace
                        .bytes()
                        .take(tab_size)
                        .take_while(|b| *b == b' ')
                        .count()
                };
                let start = self.buffer.line_start(line);
                (remove > 0).then(|| TextEdit::new(start..start + remove, ""))
            })
            .collect();
        self.apply_line_edits(edits)
    }

    /// Comments every touched line, or uncomments if they're all already commented. The marker
    /// goes at the shallowest indent, so a block keeps its shape. Blank lines are left alone.
    pub fn toggle_line_comment(&mut self) -> Vec<AppliedEdit> {
        let Some(marker) = self.config.line_comment.clone() else {
            return match self.config.block_comment.clone() {
                Some((open, close)) => self.toggle_block_comment_per_line(&open, &close),
                None => Vec::new(),
            };
        };
        let lines: Vec<usize> = self
            .selected_lines()
            .into_iter()
            .filter(|&line| !self.buffer.line(line).trim().is_empty())
            .collect();
        if lines.is_empty() {
            return Vec::new();
        }
        let all_commented = lines
            .iter()
            .all(|&line| self.buffer.line(line).trim_start().starts_with(&marker));

        let edits = if all_commented {
            lines
                .iter()
                .map(|&line| {
                    let text = self.buffer.line(line);
                    let indent = text.len() - text.trim_start().len();
                    let after_marker = indent + marker.len();
                    let end = if text[after_marker..].starts_with(' ') {
                        after_marker + 1
                    } else {
                        after_marker
                    };
                    let start = self.buffer.line_start(line);
                    TextEdit::new(start + indent..start + end, "")
                })
                .collect()
        } else {
            let column = lines
                .iter()
                .map(|&line| self.buffer.leading_whitespace(line).len())
                .min()
                .unwrap_or(0);
            lines
                .iter()
                .map(|&line| {
                    let at = self.buffer.line_start(line) + column;
                    TextEdit::new(at..at, format!("{marker} "))
                })
                .collect()
        };
        self.apply_line_edits(edits)
    }

    /// Comments each touched line on its own with a block comment, as IntelliJ does in HTML, or
    /// uncomments them if they all already are. Blank lines are left alone.
    fn toggle_block_comment_per_line(&mut self, open: &str, close: &str) -> Vec<AppliedEdit> {
        let lines: Vec<usize> = self
            .selected_lines()
            .into_iter()
            .filter(|&line| !self.buffer.line(line).trim().is_empty())
            .collect();
        if lines.is_empty() {
            return Vec::new();
        }
        let wrapped = |text: &str| {
            let inner = text.trim();
            inner.len() >= open.len() + close.len()
                && inner.starts_with(open)
                && inner.ends_with(close)
        };
        let all_commented = lines.iter().all(|&line| wrapped(&self.buffer.line(line)));
        let mut edits = Vec::new();
        for &line in &lines {
            let text = self.buffer.line(line);
            let start = self.buffer.line_start(line);
            let indent = text.len() - text.trim_start().len();
            let end = text.trim_end().len();
            if all_commented {
                let mut open_end = indent + open.len();
                if text[open_end..].starts_with(' ') {
                    open_end += 1;
                }
                let mut close_start = end - close.len();
                if close_start > open_end && text[..close_start].ends_with(' ') {
                    close_start -= 1;
                }
                edits.push(TextEdit::new(start + indent..start + open_end, ""));
                edits.push(TextEdit::new(start + close_start..start + end, ""));
            } else {
                edits.push(TextEdit::new(
                    start + indent..start + indent,
                    format!("{open} "),
                ));
                edits.push(TextEdit::new(start + end..start + end, format!(" {close}")));
            }
        }
        self.apply_line_edits(edits)
    }

    /// Pastes clipboard text. If there's one clipboard line per cursor, each cursor gets its own.
    /// Text copied as whole lines goes in above the cursor's line, rather than splitting it.
    pub fn paste(&mut self, text: &str, linewise: bool) -> Vec<AppliedEdit> {
        let count = self.selections.len();
        let pieces: Option<Vec<&str>> = (count > 1)
            .then(|| {
                text.strip_suffix('\n')
                    .unwrap_or(text)
                    .split('\n')
                    .collect::<Vec<_>>()
            })
            .filter(|parts: &Vec<&str>| parts.len() == count);

        // Whole lines go above each cursor's line, once per line however many cursors are on it.
        if linewise && pieces.is_none() && !self.selections.has_non_empty() {
            let mut lines: Vec<usize> = self
                .selections
                .iter()
                .map(|s| self.buffer.offset_to_point(s.head).line)
                .collect();
            lines.dedup();
            let edits = lines
                .into_iter()
                .map(|line| {
                    let start = self.buffer.line_start(line);
                    TextEdit::new(start..start, text)
                })
                .collect();
            return self.apply_line_edits(edits);
        }

        let plans = self
            .selections
            .iter()
            .enumerate()
            .map(|(index, selection)| {
                let piece = pieces.as_ref().map_or(text, |parts| parts[index]);
                Plan::insert(selection.range(), piece)
            })
            .collect();
        self.apply_plans(plans, UndoGrouping::Distinct)
    }

    /// Cuts selected text, or whole lines for bare cursors. Returns what to put on the clipboard.
    pub fn cut(&mut self) -> (String, bool, Vec<AppliedEdit>) {
        let (text, linewise) = copy_text(self.buffer, self.selections);
        let applied = if linewise {
            self.delete_lines()
        } else {
            let plans = self
                .selections
                .iter()
                .map(|s| Plan::delete(s.range()))
                .collect();
            self.apply_plans(plans, UndoGrouping::Distinct)
        };
        (text, linewise, applied)
    }
}

/// Groups sorted line numbers into half-open runs of consecutive lines.
fn contiguous_blocks(lines: &[usize]) -> Vec<Range<usize>> {
    let mut blocks: Vec<Range<usize>> = Vec::new();
    for &line in lines {
        match blocks.last_mut() {
            Some(last) if last.end == line => last.end = line + 1,
            _ => blocks.push(line..line + 1),
        }
    }
    blocks
}

/// The clipboard text for the current selections, and whether it's whole lines. Bare cursors copy
/// their lines; if any selection covers text, only selected text is copied.
pub fn copy_text(buffer: &Buffer, selections: &SelectionSet) -> (String, bool) {
    if selections.has_non_empty() {
        let parts: Vec<String> = selections
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| buffer.slice(s.range()))
            .collect();
        return (parts.join("\n"), false);
    }
    let mut lines: Vec<usize> = selections
        .iter()
        .map(|s| buffer.offset_to_point(s.head).line)
        .collect();
    lines.dedup();
    let mut text = String::new();
    for line in lines {
        text.push_str(&buffer.line(line));
        text.push('\n');
    }
    (text, true)
}

/// A cursor movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up(usize),
    Down(usize),
    WordLeft,
    WordRight,
    /// Toggles between the first non-whitespace character and column 0.
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
}

/// Moves every selection. With `extend` the anchors stay put, growing the selections. Without it,
/// Left and Right collapse a selection to the matching edge, rather than moving from the head.
pub fn move_selections(
    buffer: &Buffer,
    selections: &mut SelectionSet,
    motion: Motion,
    extend: bool,
    tab_size: usize,
) {
    selections.update(|selection| {
        if !extend && !selection.is_empty() {
            let range = selection.range();
            match motion {
                Motion::Left => return Selection::cursor(range.start),
                Motion::Right => return Selection::cursor(range.end),
                _ => {}
            }
        }
        let (head, goal) = motion_target(buffer, selection, motion, tab_size);
        Selection {
            anchor: if extend { selection.anchor } else { head },
            head,
            goal,
        }
    });
}

fn motion_target(
    buffer: &Buffer,
    selection: Selection,
    motion: Motion,
    tab_size: usize,
) -> (usize, Option<usize>) {
    let head = selection.head;
    match motion {
        Motion::Left => (buffer.prev_boundary(head), None),
        Motion::Right => (buffer.next_boundary(head), None),
        Motion::WordLeft => (buffer.prev_word(head), None),
        Motion::WordRight => (buffer.next_word(head), None),
        Motion::Up(count) | Motion::Down(count) => {
            let point = buffer.offset_to_point(head);
            let text = buffer.line(point.line);
            let goal = selection
                .goal
                .unwrap_or_else(|| display_column(&text, point.col, tab_size));
            let target = if matches!(motion, Motion::Up(_)) {
                point.line.saturating_sub(count)
            } else {
                (point.line + count).min(buffer.line_count() - 1)
            };
            (
                offset_at_display_column(buffer, target, goal, tab_size),
                Some(goal),
            )
        }
        Motion::LineStart => {
            let point = buffer.offset_to_point(head);
            let first = buffer.first_non_whitespace(point.line);
            let target = if head == first {
                buffer.line_start(point.line)
            } else {
                first
            };
            (target, None)
        }
        Motion::LineEnd => (buffer.line_end(buffer.offset_to_point(head).line), None),
        Motion::DocStart => (0, None),
        Motion::DocEnd => (buffer.len(), None),
    }
}

fn offset_at_display_column(buffer: &Buffer, line: usize, column: usize, tab_size: usize) -> usize {
    let text = buffer.line(line);
    buffer.line_start(line) + byte_col_at_display(&text, column, tab_size)
}

/// Adds a cursor on the line above or below the outermost cursor, in the same column.
pub fn add_cursor_vertically(
    buffer: &Buffer,
    selections: &mut SelectionSet,
    up: bool,
    tab_size: usize,
) {
    let outermost = if up {
        selections.as_slice()[0]
    } else {
        *selections
            .as_slice()
            .last()
            .expect("a selection set is never empty")
    };
    let point = buffer.offset_to_point(outermost.head);
    let target_line = if up {
        match point.line.checked_sub(1) {
            Some(line) => line,
            None => return,
        }
    } else {
        if point.line + 1 >= buffer.line_count() {
            return;
        }
        point.line + 1
    };
    let text = buffer.line(point.line);
    let goal = outermost
        .goal
        .unwrap_or_else(|| display_column(&text, point.col, tab_size));
    let offset = offset_at_display_column(buffer, target_line, goal, tab_size);
    let mut cursor = Selection::cursor(offset);
    cursor.goal = Some(goal);
    selections.push(cursor);
}

/// Selects the word under each bare cursor. Returns false if nothing was changed.
pub fn select_words_under_cursors(buffer: &Buffer, selections: &mut SelectionSet) -> bool {
    let mut changed = false;
    selections.update(|selection| {
        if !selection.is_empty() {
            return selection;
        }
        match buffer.word_at(selection.head) {
            Some(word) => {
                changed = true;
                Selection::new(word.start, word.end)
            }
            None => selection,
        }
    });
    changed
}

/// Finds occurrences of `needle`, optionally only where it stands as a whole word.
fn occurrences(buffer: &Buffer, needle: &str, whole_word: bool) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let haystack = buffer.text();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(position) = haystack[from..].find(needle) {
        let start = from + position;
        let end = start + needle.len();
        let bounded = !whole_word
            || (!haystack[..start].chars().next_back().is_some_and(is_word)
                && !haystack[end..].chars().next().is_some_and(is_word));
        if bounded {
            found.push(start..end);
        }
        // Advance one character, so that overlapping matches in `aaaa` are all found.
        from = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    found
}

/// Adds the next occurrence of the primary selection's text, wrapping at the end of the document.
/// A bare cursor first selects its word; `whole_word` should then stay set for later calls, which
/// is how Ctrl+G avoids matching `foo` inside `foobar` when started from a cursor.
pub fn add_next_occurrence(buffer: &Buffer, selections: &mut SelectionSet, whole_word: bool) {
    if !selections.has_non_empty() {
        select_words_under_cursors(buffer, selections);
        return;
    }
    let primary = selections.primary();
    let needle = buffer.slice(primary.range());
    let found = occurrences(buffer, &needle, whole_word);
    let taken = |range: &Range<usize>| {
        selections
            .iter()
            .any(|s| s.range().start < range.end && range.start < s.range().end)
    };
    let next = found
        .iter()
        .find(|range| range.start >= primary.range().end && !taken(range))
        .or_else(|| found.iter().find(|range| !taken(range)))
        .cloned();
    if let Some(range) = next {
        selections.push(Selection::new(range.start, range.end));
    }
}

/// Selects every occurrence of the primary selection's text.
pub fn select_all_occurrences(buffer: &Buffer, selections: &mut SelectionSet, whole_word: bool) {
    if !selections.has_non_empty() {
        select_words_under_cursors(buffer, selections);
    }
    let needle = buffer.slice(selections.primary().range());
    let primary_start = selections.primary().range().start;
    let found = occurrences(buffer, &needle, whole_word);
    // Overlapping matches can't all be selected, so take them greedily from the left.
    let mut items: Vec<Selection> = Vec::new();
    for range in found {
        if items
            .last()
            .is_none_or(|last| last.range().end <= range.start)
        {
            items.push(Selection::new(range.start, range.end));
        }
    }
    if !items.is_empty() {
        let primary = items
            .iter()
            .position(|s| s.range().start >= primary_start)
            .unwrap_or(0);
        *selections = SelectionSet::from_vec(items, primary);
    }
}

/// A rectangular selection between two display positions, one selection per line. Lines too short
/// to reach the rectangle get a cursor at their end.
pub fn column_selection(
    buffer: &Buffer,
    anchor: (usize, usize),
    head: (usize, usize),
    tab_size: usize,
) -> SelectionSet {
    let (anchor_line, anchor_col) = anchor;
    let (head_line, head_col) = head;
    let reversed_lines = head_line < anchor_line;
    let lines = anchor_line.min(head_line)..=anchor_line.max(head_line);

    let items: Vec<Selection> = lines
        .map(|line| {
            let text = buffer.line(line);
            let start = buffer.line_start(line);
            Selection::new(
                start + byte_col_at_display(&text, anchor_col, tab_size),
                start + byte_col_at_display(&text, head_col, tab_size),
            )
        })
        .collect();
    let primary = if reversed_lines { 0 } else { items.len() - 1 };
    SelectionSet::from_vec(items, primary)
}

/// The range of whole lines around an offset, including the newline, for triple-click.
pub fn line_range_at(buffer: &Buffer, offset: usize) -> Range<usize> {
    let line = buffer.offset_to_point(offset).line;
    let start = buffer.line_start(line);
    let end = if line + 1 < buffer.line_count() {
        buffer.line_start(line + 1)
    } else {
        buffer.len()
    };
    start..end
}

#[cfg(test)]
mod tests;
