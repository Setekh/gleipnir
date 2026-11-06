//! The text model: a rope-backed buffer with undo, plus the coordinate conversions that the
//! rest of the IDE depends on.
//!
//! Three coordinate systems meet here, and mixing them up is the classic editor bug:
//! - byte offsets: what tree-sitter and GPUI's shaped lines use. Our canonical form.
//! - UTF-16 line/character: what LSP uses on the wire.
//! - display columns: what the screen uses once tabs are expanded.

pub mod diff;
pub mod ops;
mod selection;

pub use selection::{Bias, Selection, SelectionSet, map_offset_through_edits, transform_offset};

use ropey::Rope;
use std::ops::Range;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

/// A line and a byte column within that line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: usize,
    pub col: usize,
}

impl Point {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// An LSP-style position: zero-based line and UTF-16 code unit offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Utf16Point {
    pub line: u32,
    pub character: u32,
}

impl Utf16Point {
    pub fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// Everything a downstream consumer needs to know about one edit. Tree-sitter wants bytes and
/// points, LSP wants UTF-16 positions, so both are computed while the old text is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start: Point,
    pub old_end: Point,
    pub new_end: Point,
    pub start_utf16: Utf16Point,
    pub old_end_utf16: Utf16Point,
    pub new_text: String,
}

/// One replacement in a batch. All ranges in a batch refer to the document before the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

impl TextEdit {
    pub fn new(range: Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }
}

#[derive(Clone, Debug)]
struct EditRecord {
    start: usize,
    old_text: String,
    new_text: String,
}

#[derive(Clone, Debug)]
struct Transaction {
    id: u64,
    /// In the order they were applied, so undo walks them backwards.
    edits: Vec<EditRecord>,
    selections_before: SelectionSet,
    selections_after: SelectionSet,
    last_edit_at: Instant,
    /// Only runs of plain typing merge; a paste or a newline starts a new undo step.
    mergeable: bool,
    /// A sealed transaction accepts no more edits. Set when the cursor moves or a save starts.
    sealed: bool,
}

/// How an edit should behave in the undo history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoGrouping {
    /// Merge with the previous edit if it was also typing and recent enough.
    Typing,
    /// Always a distinct undo step.
    Distinct,
    /// Join the transaction that is already open, for multi-part operations like toggling
    /// comments on many lines or applying a completion and its auto-import.
    Join,
}

const TYPING_MERGE_WINDOW: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

/// The document. Internally always LF, with no byte order mark. Both are restored on save, so
/// every other component can assume plain `\n`-separated text.
pub struct Buffer {
    rope: Rope,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_transaction_id: u64,
    /// Bumped on every change, including undo. Monotonic, so it can serve as the LSP version.
    version: u64,
    /// The transaction on top of the undo stack when the file was last saved. Comparing against
    /// this means undoing back to the saved state correctly reads as clean.
    saved_state: Option<u64>,
    line_ending: LineEnding,
    has_bom: bool,
}

/// Captures a document state being written to disk. Saving is asynchronous, so the buffer may
/// have moved on by the time the write finishes; this records what was actually written.
#[derive(Clone, Debug)]
pub struct SaveToken {
    state: Option<u64>,
    pub text: String,
    pub version: u64,
}

/// A cheap, immutable copy of the text for use on background threads.
#[derive(Clone)]
pub struct Snapshot {
    rope: Rope,
    version: u64,
}

impl Snapshot {
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    /// The contiguous chunk of text containing `offset`, and the chunk's starting byte. This is
    /// the shape tree-sitter's parse callback wants.
    pub fn chunk_at(&self, offset: usize) -> (&str, usize) {
        if offset >= self.rope.len_bytes() {
            return ("", self.rope.len_bytes());
        }
        let (chunk, start, _, _) = self.rope.chunk_at_byte(offset);
        (chunk, start)
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        self.rope.byte_slice(range).to_string()
    }
}

/// Splits a file's text into normalized content and how to restore it. Files with mixed line
/// endings adopt whichever is more common, as there's no way to keep both through an edit.
fn normalize_text(text: &str) -> (String, LineEnding, bool) {
    let (text, has_bom) = match text.strip_prefix('\u{feff}') {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    if crlf == 0 {
        return (text.to_string(), LineEnding::Lf, has_bom);
    }
    let ending = if crlf >= lf {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    };
    (text.replace("\r\n", "\n"), ending, has_bom)
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let (normalized, line_ending, has_bom) = normalize_text(text);
        Self {
            rope: Rope::from_str(&normalized),
            undo: Vec::new(),
            redo: Vec::new(),
            next_transaction_id: 0,
            version: 0,
            saved_state: None,
            line_ending,
            has_bom,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            rope: self.rope.clone(),
            version: self.version,
        }
    }

    pub fn is_crlf(&self) -> bool {
        self.line_ending == LineEnding::Crlf
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// The text as it should be written to disk, with line endings and BOM restored.
    pub fn text_for_save(&self) -> String {
        let text = self.rope.to_string();
        let text = match self.line_ending {
            LineEnding::Lf => text,
            LineEnding::Crlf => text.replace('\n', "\r\n"),
        };
        if self.has_bom {
            format!("\u{feff}{text}")
        } else {
            text
        }
    }

    pub fn len(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_bytes() == 0
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn is_dirty(&self) -> bool {
        self.undo.last().map(|t| t.id) != self.saved_state
    }

    /// Starts a save. Seals the current undo step, so that typing during the write creates a new
    /// state rather than silently changing the one being saved.
    pub fn begin_save(&mut self) -> SaveToken {
        self.break_undo_group();
        SaveToken {
            state: self.undo.last().map(|t| t.id),
            text: self.text_for_save(),
            version: self.version,
        }
    }

    /// Records that the state captured by `token` is now what's on disk.
    pub fn finish_save(&mut self, token: &SaveToken) {
        self.saved_state = token.state;
    }

    /// Ropey considers the empty string after a trailing newline to be a line, as do we.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn clip(&self, offset: usize) -> usize {
        let offset = offset.min(self.len());
        // Snap to a char boundary: ropey maps a mid-char byte to its containing char.
        self.rope.char_to_byte(self.rope.byte_to_char(offset))
    }

    pub fn line_start(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return self.len();
        }
        self.rope.line_to_byte(line)
    }

    /// Byte length of a line, not counting its newline.
    pub fn line_len(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return 0;
        }
        let slice = self.rope.line(line);
        let mut len = slice.len_bytes();
        if len > 0 && slice.byte(len - 1) == b'\n' {
            len -= 1;
        }
        len
    }

    pub fn line_end(&self, line: usize) -> usize {
        self.line_start(line) + self.line_len(line)
    }

    /// A line's content, without its newline.
    pub fn line(&self, line: usize) -> String {
        if line >= self.line_count() {
            return String::new();
        }
        let mut text = self.rope.line(line).to_string();
        if text.ends_with('\n') {
            text.pop();
        }
        text
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        let start = self.clip(range.start);
        let end = self.clip(range.end.max(range.start));
        self.rope.byte_slice(start..end).to_string()
    }

    pub fn offset_to_point(&self, offset: usize) -> Point {
        let offset = self.clip(offset);
        let line = self.rope.byte_to_line(offset);
        Point {
            line,
            col: offset - self.rope.line_to_byte(line),
        }
    }

    pub fn point_to_offset(&self, point: Point) -> usize {
        if point.line >= self.line_count() {
            return self.len();
        }
        self.clip(self.line_start(point.line) + point.col.min(self.line_len(point.line)))
    }

    pub fn offset_to_utf16(&self, offset: usize) -> Utf16Point {
        let offset = self.clip(offset);
        let line = self.rope.byte_to_line(offset);
        let line_char = self.rope.line_to_char(line);
        let line_cu = self.rope.char_to_utf16_cu(line_char);
        let cu = self.rope.char_to_utf16_cu(self.rope.byte_to_char(offset));
        Utf16Point {
            line: line as u32,
            character: (cu - line_cu) as u32,
        }
    }

    /// Converts an LSP position. Per the spec, a character past the end of the line clamps to
    /// the end of the line, rather than spilling onto the next one.
    pub fn utf16_to_offset(&self, position: Utf16Point) -> usize {
        let line = position.line as usize;
        if line >= self.line_count() {
            return self.len();
        }
        let line_char = self.rope.line_to_char(line);
        let line_cu = self.rope.char_to_utf16_cu(line_char);
        let target = (line_cu + position.character as usize).min(self.rope.len_utf16_cu());
        let char_idx = self.rope.utf16_cu_to_char(target);
        self.rope.char_to_byte(char_idx).min(self.line_end(line))
    }

    /// Length of the document in UTF-16 code units, which is how the platform IME addresses it.
    pub fn len_utf16(&self) -> usize {
        self.rope.len_utf16_cu()
    }

    pub fn offset_to_doc_utf16(&self, offset: usize) -> usize {
        self.rope
            .char_to_utf16_cu(self.rope.byte_to_char(self.clip(offset)))
    }

    pub fn doc_utf16_to_offset(&self, utf16: usize) -> usize {
        let utf16 = utf16.min(self.rope.len_utf16_cu());
        self.rope.char_to_byte(self.rope.utf16_cu_to_char(utf16))
    }

    /// Applies a batch of non-overlapping edits as one change.
    ///
    /// Edits are applied from the end of the document backwards, so each returned `AppliedEdit`
    /// is valid against the document as it stood at that moment. Consumers such as LSP and
    /// tree-sitter process the returned list in order. An empty batch records nothing.
    pub fn apply_edits(
        &mut self,
        mut edits: Vec<TextEdit>,
        selections_before: &SelectionSet,
        selections_after: &SelectionSet,
        grouping: UndoGrouping,
        now: Instant,
    ) -> Vec<AppliedEdit> {
        edits.retain(|e| !(e.range.is_empty() && e.text.is_empty()));
        if edits.is_empty() {
            return Vec::new();
        }
        edits.sort_by_key(|e| e.range.start);
        debug_assert!(
            edits.windows(2).all(|w| w[0].range.end <= w[1].range.start),
            "edits in a batch must not overlap: {edits:?}"
        );

        let mut applied = Vec::with_capacity(edits.len());
        let mut records = Vec::with_capacity(edits.len());
        for edit in edits.iter().rev() {
            let start = self.clip(edit.range.start);
            let end = self.clip(edit.range.end.max(start));
            let old_text = self.slice(start..end);
            applied.push(self.apply(start, end, &edit.text));
            records.push(EditRecord {
                start,
                old_text,
                new_text: edit.text.clone(),
            });
        }

        self.redo.clear();
        let merged = match self.undo.last_mut() {
            Some(last) if !last.sealed => {
                let window_open =
                    now.saturating_duration_since(last.last_edit_at) < TYPING_MERGE_WINDOW;
                let merge = match grouping {
                    UndoGrouping::Join => true,
                    UndoGrouping::Typing => last.mergeable && window_open,
                    UndoGrouping::Distinct => false,
                };
                if merge {
                    last.edits.extend(records.iter().cloned());
                    last.selections_after = selections_after.clone();
                    last.last_edit_at = now;
                    last.mergeable &= grouping == UndoGrouping::Typing;
                }
                merge
            }
            _ => false,
        };
        if !merged {
            let id = self.next_transaction_id;
            self.next_transaction_id += 1;
            self.undo.push(Transaction {
                id,
                edits: records,
                selections_before: selections_before.clone(),
                selections_after: selections_after.clone(),
                last_edit_at: now,
                mergeable: grouping == UndoGrouping::Typing,
                sealed: false,
            });
        }
        applied
    }

    /// Stops further edits joining the current undo step. Called when the cursor moves, so that
    /// typing, moving and typing again are two separate undos.
    pub fn break_undo_group(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.sealed = true;
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Reverts the last transaction, returning the edits made and the selections to restore.
    /// The id of the step `undo` would revert, which stays the same until that step is undone or
    /// another is added.
    pub fn undo_top_id(&self) -> Option<u64> {
        self.undo.last().map(|t| t.id)
    }

    pub fn undo(&mut self) -> Option<(Vec<AppliedEdit>, SelectionSet)> {
        let mut transaction = self.undo.pop()?;
        transaction.sealed = true;
        let mut applied = Vec::with_capacity(transaction.edits.len());
        for record in transaction.edits.iter().rev() {
            applied.push(self.apply(
                record.start,
                record.start + record.new_text.len(),
                &record.old_text,
            ));
        }
        let selections = transaction.selections_before.clone();
        self.redo.push(transaction);
        // A later edit must not merge into whatever step is now on top.
        self.break_undo_group();
        Some((applied, selections))
    }

    pub fn redo(&mut self) -> Option<(Vec<AppliedEdit>, SelectionSet)> {
        let transaction = self.redo.pop()?;
        let mut applied = Vec::with_capacity(transaction.edits.len());
        for record in &transaction.edits {
            applied.push(self.apply(
                record.start,
                record.start + record.old_text.len(),
                &record.new_text,
            ));
        }
        let selections = transaction.selections_after.clone();
        self.undo.push(transaction);
        Some((applied, selections))
    }

    /// Replaces the document's text, for reloading a file that changed on disk. Only the lines that
    /// differ are replaced, so whatever follows positions through edits, like cursors, breakpoints
    /// and language servers, keeps its place. Clears history, as the file was changed by
    /// something else. The edits come last first, like `apply_edits`'.
    pub fn reset(&mut self, text: &str) -> Vec<AppliedEdit> {
        let (normalized, line_ending, has_bom) = normalize_text(text);
        let current = self.text();
        let hunks = diff::diff_lines(&current, &normalized);
        let old_lines = diff::split_lines(&current);
        let new_lines = diff::split_lines(&normalized);
        let mut starts = Vec::with_capacity(old_lines.len() + 1);
        let mut offset = 0;
        for line in &old_lines {
            starts.push(offset);
            offset += line.len();
        }
        starts.push(offset);
        let mut applied = Vec::with_capacity(hunks.len());
        for hunk in hunks.iter().rev() {
            let replacement: String = new_lines[hunk.new.clone()].concat();
            applied.push(self.apply(starts[hunk.old.start], starts[hunk.old.end], &replacement));
        }
        self.line_ending = line_ending;
        self.has_bom = has_bom;
        self.undo.clear();
        self.redo.clear();
        self.saved_state = None;
        applied
    }

    fn apply(&mut self, start: usize, end: usize, new_text: &str) -> AppliedEdit {
        let start_point = self.offset_to_point(start);
        let old_end_point = self.offset_to_point(end);
        let start_utf16 = self.offset_to_utf16(start);
        let old_end_utf16 = self.offset_to_utf16(end);

        let start_char = self.rope.byte_to_char(start);
        let end_char = self.rope.byte_to_char(end);
        self.rope.remove(start_char..end_char);
        self.rope.insert(start_char, new_text);
        self.version += 1;

        let new_end_byte = start + new_text.len();
        AppliedEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte,
            start: start_point,
            old_end: old_end_point,
            new_end: self.offset_to_point(new_end_byte),
            start_utf16,
            old_end_utf16,
            new_text: new_text.to_string(),
        }
    }

    pub fn char_at(&self, offset: usize) -> Option<char> {
        if offset >= self.len() {
            return None;
        }
        Some(self.rope.char(self.rope.byte_to_char(offset)))
    }

    pub fn char_before(&self, offset: usize) -> Option<char> {
        let offset = self.clip(offset);
        if offset == 0 {
            return None;
        }
        Some(self.rope.char(self.rope.byte_to_char(offset) - 1))
    }

    pub fn prev_boundary(&self, offset: usize) -> usize {
        let offset = self.clip(offset);
        let point = self.offset_to_point(offset);
        if point.col == 0 {
            return if point.line == 0 {
                0
            } else {
                self.line_end(point.line - 1)
            };
        }
        let line = self.line(point.line);
        let col = line[..point.col]
            .grapheme_indices(true)
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.line_start(point.line) + col
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        let offset = self.clip(offset);
        let point = self.offset_to_point(offset);
        let line = self.line(point.line);
        if point.col >= line.len() {
            return if point.line + 1 >= self.line_count() {
                self.len()
            } else {
                self.line_start(point.line + 1)
            };
        }
        let rest = &line[point.col..];
        let advance = rest
            .grapheme_indices(true)
            .nth(1)
            .map(|(idx, _)| idx)
            .unwrap_or(rest.len());
        offset + advance
    }

    /// The start of the previous word, skipping any whitespace before the cursor first.
    pub fn prev_word(&self, offset: usize) -> usize {
        let mut pos = self.clip(offset);
        while let Some(c) = self.char_before(pos) {
            if !c.is_whitespace() {
                break;
            }
            pos -= c.len_utf8();
        }
        let Some(first) = self.char_before(pos) else {
            return 0;
        };
        let class = char_class(first);
        while let Some(c) = self.char_before(pos) {
            if char_class(c) != class {
                break;
            }
            pos -= c.len_utf8();
        }
        pos
    }

    /// The end of the next word, skipping any whitespace after the cursor first.
    pub fn next_word(&self, offset: usize) -> usize {
        let mut pos = self.clip(offset);
        while let Some(c) = self.char_at(pos) {
            if !c.is_whitespace() {
                break;
            }
            pos += c.len_utf8();
        }
        let Some(first) = self.char_at(pos) else {
            return self.len();
        };
        let class = char_class(first);
        while let Some(c) = self.char_at(pos) {
            if char_class(c) != class {
                break;
            }
            pos += c.len_utf8();
        }
        pos
    }

    /// The identifier under or immediately before `offset`, if any.
    pub fn word_at(&self, offset: usize) -> Option<Range<usize>> {
        let offset = self.clip(offset);
        let is_word = |c: char| char_class(c) == CharClass::Word;
        let on_word = self.char_at(offset).is_some_and(is_word);
        let after_word = self.char_before(offset).is_some_and(is_word);
        if !on_word && !after_word {
            return None;
        }
        let mut start = offset;
        while let Some(c) = self.char_before(start) {
            if !is_word(c) {
                break;
            }
            start -= c.len_utf8();
        }
        let mut end = offset;
        while let Some(c) = self.char_at(end) {
            if !is_word(c) {
                break;
            }
            end += c.len_utf8();
        }
        Some(start..end)
    }

    pub fn leading_whitespace(&self, line: usize) -> String {
        self.line(line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    /// The first non-whitespace column, which is where Home goes first.
    pub fn first_non_whitespace(&self, line: usize) -> usize {
        self.line_start(line) + self.leading_whitespace(line).len()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Whitespace,
    Punctuation,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Whitespace
    } else {
        CharClass::Punctuation
    }
}

/// Maps between byte columns and display columns in a line containing tabs. GPUI's text shaper
/// has no notion of tab stops, so we shape an expanded copy of the line and translate through
/// this map whenever we need to go between the document and the screen.
#[derive(Clone, Debug, Default)]
pub struct TabMap {
    /// Byte column of each tab in the source line, with its expanded width.
    tabs: Vec<(usize, usize)>,
}

impl TabMap {
    /// Expands tabs to spaces, returning the display text and a map back to the source.
    pub fn expand(line: &str, tab_size: usize) -> (String, TabMap) {
        if !line.contains('\t') {
            return (line.to_string(), TabMap::default());
        }
        let mut out = String::with_capacity(line.len() + 8);
        let mut tabs = Vec::new();
        let mut column = 0;
        for (idx, c) in line.char_indices() {
            if c == '\t' {
                let width = tab_size - column % tab_size;
                tabs.push((idx, width));
                out.extend(std::iter::repeat_n(' ', width));
                column += width;
            } else {
                out.push(c);
                column += 1;
            }
        }
        (out, TabMap { tabs })
    }

    /// Converts a byte index in the source line to a byte index in the expanded line.
    pub fn to_display(&self, col: usize) -> usize {
        let mut extra = 0;
        for &(tab, width) in &self.tabs {
            if tab >= col {
                break;
            }
            extra += width - 1;
        }
        col + extra
    }

    /// Converts a byte index in the expanded line back to the source. An index inside a tab's
    /// expansion snaps to whichever edge of the tab is closer.
    pub fn from_display(&self, display: usize) -> usize {
        let mut extra = 0;
        for &(tab, width) in &self.tabs {
            let display_start = tab + extra;
            if display <= display_start {
                break;
            }
            if display < display_start + width {
                return if display - display_start < width.div_ceil(2) {
                    tab
                } else {
                    tab + 1
                };
            }
            extra += width - 1;
        }
        display - extra
    }
}

/// The on-screen column of a byte column, which is what vertical cursor movement preserves.
pub fn display_column(line: &str, byte_col: usize, tab_size: usize) -> usize {
    let mut column = 0;
    for (idx, c) in line.char_indices() {
        if idx >= byte_col {
            break;
        }
        column += if c == '\t' {
            tab_size - column % tab_size
        } else {
            1
        };
    }
    column
}

/// The inverse of `display_column`: the byte column at or just before a display column.
pub fn byte_col_at_display(line: &str, target: usize, tab_size: usize) -> usize {
    let mut column = 0;
    for (idx, c) in line.char_indices() {
        if column >= target {
            return idx;
        }
        let width = if c == '\t' {
            tab_size - column % tab_size
        } else {
            1
        };
        if column + width > target {
            return idx;
        }
        column += width;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit_with(
        buffer: &mut Buffer,
        edits: Vec<TextEdit>,
        grouping: UndoGrouping,
        now: Instant,
    ) -> Vec<AppliedEdit> {
        let selections = SelectionSet::cursor(0);
        buffer.apply_edits(edits, &selections, &selections, grouping, now)
    }

    fn typed(buffer: &mut Buffer, offset: usize, text: &str, now: Instant) -> AppliedEdit {
        buffer
            .apply_edits(
                vec![TextEdit::new(offset..offset, text)],
                &SelectionSet::cursor(offset),
                &SelectionSet::cursor(offset + text.len()),
                UndoGrouping::Typing,
                now,
            )
            .remove(0)
    }

    #[test]
    fn lines_exclude_their_newline_and_trailing_newline_makes_a_line() {
        let b = Buffer::new("one\ntwo\n");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line(0), "one");
        assert_eq!(b.line(1), "two");
        assert_eq!(b.line(2), "");
        assert_eq!(b.line_len(1), 3);
        assert_eq!(b.line_start(1), 4);
        assert_eq!(b.line_end(1), 7);
    }

    #[test]
    fn point_conversion_round_trips_and_clamps() {
        let b = Buffer::new("héllo\nworld");
        for offset in [0, 1, 3, 6, 7, 12] {
            assert_eq!(b.point_to_offset(b.offset_to_point(offset)), offset);
        }
        assert_eq!(b.point_to_offset(Point::new(0, 999)), b.line_end(0));
        assert_eq!(b.point_to_offset(Point::new(99, 0)), b.len());
    }

    #[test]
    fn clip_snaps_into_char_boundaries() {
        let b = Buffer::new("é");
        assert_eq!(b.clip(1), 0);
        assert_eq!(b.clip(2), 2);
        assert_eq!(b.clip(50), 2);
    }

    #[test]
    fn utf16_positions_count_surrogate_pairs_as_two() {
        // 😀 is 4 UTF-8 bytes and 2 UTF-16 code units; é is 2 bytes and 1 code unit.
        let b = Buffer::new("a😀é!\nnext");
        let bang = "a😀é".len();
        assert_eq!(b.offset_to_utf16(bang), Utf16Point::new(0, 4));
        assert_eq!(b.utf16_to_offset(Utf16Point::new(0, 4)), bang);
        assert_eq!(
            b.utf16_to_offset(Utf16Point::new(1, 2)),
            b.line_start(1) + 2
        );
    }

    #[test]
    fn utf16_past_end_of_line_clamps_to_that_line() {
        let b = Buffer::new("ab\ncd");
        assert_eq!(b.utf16_to_offset(Utf16Point::new(0, 99)), 2);
        assert_eq!(b.utf16_to_offset(Utf16Point::new(9, 0)), b.len());
    }

    #[test]
    fn applied_edit_reports_all_coordinates_from_before_the_change() {
        let mut b = Buffer::new("a😀b\nc");
        let start = "a😀".len();
        let e = edit_with(
            &mut b,
            vec![TextEdit::new(start..start + 1, "xy\nz")],
            UndoGrouping::Distinct,
            Instant::now(),
        )
        .remove(0);
        assert_eq!(e.start_byte, 5);
        assert_eq!(e.old_end_byte, 6);
        assert_eq!(e.new_end_byte, 9);
        assert_eq!(e.start_utf16, Utf16Point::new(0, 3));
        assert_eq!(e.old_end_utf16, Utf16Point::new(0, 4));
        assert_eq!(e.new_end, Point::new(1, 1));
        assert_eq!(b.text(), "a😀xy\nz\nc");
    }

    #[test]
    fn batches_apply_back_to_front_so_each_applied_edit_is_valid_in_sequence() {
        let mut b = Buffer::new("aaa bbb ccc");
        let applied = edit_with(
            &mut b,
            vec![TextEdit::new(0..3, "X"), TextEdit::new(8..11, "ZZZZ")],
            UndoGrouping::Distinct,
            Instant::now(),
        );
        assert_eq!(b.text(), "X bbb ZZZZ");
        assert_eq!(applied[0].start_byte, 8);
        assert_eq!(applied[1].start_byte, 0);

        // Replaying the applied edits in order on the original must give the same result.
        let mut replay = String::from("aaa bbb ccc");
        for edit in &applied {
            replay.replace_range(edit.start_byte..edit.old_end_byte, &edit.new_text);
        }
        assert_eq!(replay, b.text());
    }

    #[test]
    fn a_batch_is_a_single_undo_step() {
        let mut b = Buffer::new("one two");
        edit_with(
            &mut b,
            vec![TextEdit::new(0..0, ">"), TextEdit::new(4..4, ">")],
            UndoGrouping::Distinct,
            Instant::now(),
        );
        assert_eq!(b.text(), ">one >two");
        b.undo().unwrap();
        assert_eq!(b.text(), "one two");
        assert!(!b.can_undo());
    }

    #[test]
    fn inserts_at_the_same_offset_keep_their_order() {
        let mut b = Buffer::new("");
        edit_with(
            &mut b,
            vec![TextEdit::new(0..0, "a"), TextEdit::new(0..0, "b")],
            UndoGrouping::Distinct,
            Instant::now(),
        );
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn an_empty_batch_records_nothing() {
        let mut b = Buffer::new("x");
        let applied = edit_with(
            &mut b,
            vec![TextEdit::new(1..1, "")],
            UndoGrouping::Distinct,
            Instant::now(),
        );
        assert!(applied.is_empty());
        assert!(!b.can_undo());
        assert_eq!(b.version(), 0);
    }

    #[test]
    fn rapid_typing_is_one_undo_step() {
        let mut b = Buffer::new("");
        let t = Instant::now();
        typed(&mut b, 0, "a", t);
        typed(&mut b, 1, "b", t + Duration::from_millis(100));
        typed(&mut b, 2, "c", t + Duration::from_millis(200));
        let (_, selections) = b.undo().unwrap();
        assert_eq!(b.text(), "");
        assert_eq!(selections, SelectionSet::cursor(0));
        assert!(!b.can_undo());
    }

    #[test]
    fn a_typing_pause_starts_a_new_undo_step() {
        let mut b = Buffer::new("");
        let t = Instant::now();
        typed(&mut b, 0, "a", t);
        typed(&mut b, 1, "b", t + Duration::from_secs(2));
        b.undo();
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn moving_the_cursor_breaks_the_typing_group() {
        let mut b = Buffer::new("");
        let t = Instant::now();
        typed(&mut b, 0, "a", t);
        b.break_undo_group();
        typed(&mut b, 1, "b", t);
        b.undo();
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn joined_edits_undo_in_reverse_and_redo_forward() {
        let mut b = Buffer::new("abc");
        let t = Instant::now();
        edit_with(
            &mut b,
            vec![TextEdit::new(0..1, "XXXX")],
            UndoGrouping::Distinct,
            t,
        );
        edit_with(
            &mut b,
            vec![TextEdit::new(4..4, "-")],
            UndoGrouping::Join,
            t,
        );
        edit_with(
            &mut b,
            vec![TextEdit::new(0..0, "!")],
            UndoGrouping::Join,
            t,
        );
        assert_eq!(b.text(), "!XXXX-bc");
        let (edits, _) = b.undo().unwrap();
        assert_eq!(edits.len(), 3);
        assert_eq!(b.text(), "abc");
        b.redo().unwrap();
        assert_eq!(b.text(), "!XXXX-bc");
        assert!(b.undo().is_some());
        assert!(!b.can_undo());
    }

    #[test]
    fn typing_after_undo_does_not_merge_into_the_earlier_step() {
        let mut b = Buffer::new("");
        let t = Instant::now();
        typed(&mut b, 0, "a", t);
        b.break_undo_group();
        typed(&mut b, 1, "b", t);
        b.undo();
        typed(&mut b, 1, "c", t);
        assert_eq!(b.text(), "ac");
        b.undo();
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut b = Buffer::new("");
        let t = Instant::now();
        typed(&mut b, 0, "a", t);
        b.undo();
        assert!(b.can_redo());
        typed(&mut b, 0, "b", t);
        assert!(!b.can_redo());
    }

    #[test]
    fn undoing_back_to_the_saved_state_is_clean_and_redoing_is_dirty() {
        let mut b = Buffer::new("x");
        assert!(!b.is_dirty());
        typed(&mut b, 1, "y", Instant::now());
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty());
        b.redo();
        assert!(b.is_dirty());
    }

    #[test]
    fn saving_marks_clean_and_undoing_past_a_save_is_dirty() {
        let mut b = Buffer::new("");
        typed(&mut b, 0, "a", Instant::now());
        let token = b.begin_save();
        b.finish_save(&token);
        assert!(!b.is_dirty());
        b.undo();
        assert!(b.is_dirty());
        b.redo();
        assert!(!b.is_dirty());
    }

    #[test]
    fn typing_during_a_save_leaves_the_buffer_dirty() {
        let mut b = Buffer::new("");
        let t = Instant::now();
        typed(&mut b, 0, "a", t);
        let token = b.begin_save();
        assert_eq!(token.text, "a");
        // Within the merge window, but the save sealed the step, so this is a new state.
        typed(&mut b, 1, "b", t);
        b.finish_save(&token);
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty());
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn crlf_is_normalized_internally_and_restored_on_save() {
        let b = Buffer::new("one\r\ntwo\r\n");
        assert_eq!(b.text(), "one\ntwo\n");
        assert_eq!(b.line(0), "one");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.text_for_save(), "one\r\ntwo\r\n");
    }

    #[test]
    fn mixed_line_endings_adopt_the_majority() {
        assert!(Buffer::new("a\r\nb\r\nc\n").is_crlf());
        let b = Buffer::new("a\nb\nc\r\n");
        assert!(!b.is_crlf());
        assert_eq!(b.text(), "a\nb\nc\n");
    }

    #[test]
    fn a_byte_order_mark_is_hidden_and_restored() {
        let b = Buffer::new("\u{feff}abc");
        assert_eq!(b.text(), "abc");
        assert_eq!(b.len(), 3);
        assert_eq!(b.text_for_save(), "\u{feff}abc");
    }

    #[test]
    fn only_newlines_break_lines() {
        // Ropey would split on these with its default features, disagreeing with every other
        // tool and language server. A form feed is common in old source files.
        let b = Buffer::new("a\u{c}b\u{2028}c\u{b}d\u{85}e\rf");
        assert_eq!(b.line_count(), 1);
    }

    #[test]
    fn reset_replaces_everything_and_clears_history() {
        let mut b = Buffer::new("old");
        typed(&mut b, 3, "!", Instant::now());
        b.reset("new\r\ntext");
        assert_eq!(b.text(), "new\ntext");
        assert!(b.is_crlf());
        assert!(!b.can_undo());
        assert!(!b.is_dirty());
    }

    #[test]
    fn reset_replaces_only_the_lines_that_changed() {
        let mut b = Buffer::new("one\ntwo\nthree\nfour\n");
        let edits = b.reset("one\nTWO\nthree\nfour\nfive\n");
        assert_eq!(b.text(), "one\nTWO\nthree\nfour\nfive\n");
        // Last first, each a line.
        let lines: Vec<(usize, usize)> = edits
            .iter()
            .map(|e| (e.start.line, e.old_end.line))
            .collect();
        assert_eq!(lines, [(4, 4), (1, 2)]);
        assert!(b.reset("one\nTWO\nthree\nfour\nfive\n").is_empty());
    }

    #[test]
    fn snapshots_are_unaffected_by_later_edits_and_expose_chunks() {
        let mut b = Buffer::new("hello world");
        let snapshot = b.snapshot();
        typed(&mut b, 0, "XX", Instant::now());
        assert_eq!(snapshot.slice(0..5), "hello");
        let (chunk, start) = snapshot.chunk_at(6);
        assert_eq!(&chunk[6 - start..], "world");
        assert_eq!(snapshot.chunk_at(99), ("", 11));
    }

    #[test]
    fn document_wide_utf16_offsets_round_trip() {
        let b = Buffer::new("a😀\nbé");
        assert_eq!(b.len_utf16(), 6);
        for offset in [0, 1, 5, 6, 7, 9] {
            assert_eq!(b.doc_utf16_to_offset(b.offset_to_doc_utf16(offset)), offset);
        }
    }

    #[test]
    fn word_motion_treats_identifiers_and_punctuation_as_separate_words() {
        let b = Buffer::new("foo_bar.baz  qux");
        assert_eq!(b.next_word(0), 7);
        assert_eq!(b.next_word(7), 8);
        assert_eq!(b.next_word(8), 11);
        assert_eq!(b.next_word(11), 16);
        assert_eq!(b.prev_word(16), 13);
        assert_eq!(b.prev_word(13), 8);
        assert_eq!(b.prev_word(8), 7);
        assert_eq!(b.prev_word(7), 0);
    }

    #[test]
    fn word_at_finds_identifiers_touching_the_cursor() {
        let b = Buffer::new("let foo_bar = 1;");
        assert_eq!(b.word_at(5), Some(4..11));
        assert_eq!(b.word_at(11), Some(4..11));
        assert_eq!(b.word_at(12), None);
    }

    #[test]
    fn grapheme_boundaries_keep_combined_characters_together() {
        // e + combining acute is one grapheme of three bytes.
        let b = Buffer::new("e\u{301}x\ny");
        assert_eq!(b.next_boundary(0), 3);
        assert_eq!(b.prev_boundary(3), 0);
        // Crossing a line break is one step in either direction.
        assert_eq!(b.next_boundary(4), 5);
        assert_eq!(b.prev_boundary(5), 4);
    }

    #[test]
    fn tab_map_translates_both_ways() {
        let (text, map) = TabMap::expand("\ta\tb", 4);
        assert_eq!(text, "    a   b");
        assert_eq!(map.to_display(0), 0);
        assert_eq!(map.to_display(1), 4);
        assert_eq!(map.to_display(2), 5);
        assert_eq!(map.to_display(3), 8);
        assert_eq!(map.from_display(4), 1);
        assert_eq!(map.from_display(5), 2);
        assert_eq!(map.from_display(8), 3);
        // Inside the first tab: closer to the left edge, then closer to the right.
        assert_eq!(map.from_display(1), 0);
        assert_eq!(map.from_display(3), 1);
    }

    #[test]
    fn tab_map_is_identity_without_tabs() {
        let (text, map) = TabMap::expand("plain", 4);
        assert_eq!(text, "plain");
        assert_eq!(map.to_display(3), 3);
        assert_eq!(map.from_display(3), 3);
    }

    #[test]
    fn display_columns_respect_tab_stops() {
        assert_eq!(display_column("\tab", 1, 4), 4);
        assert_eq!(display_column("a\tb", 2, 4), 4);
        assert_eq!(byte_col_at_display("\tab", 4, 4), 1);
        assert_eq!(byte_col_at_display("\tab", 5, 4), 2);
        assert_eq!(byte_col_at_display("\tab", 2, 4), 0);
        assert_eq!(byte_col_at_display("ab", 99, 4), 2);
    }
}
