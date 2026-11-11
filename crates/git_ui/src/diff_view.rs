//! A file's two versions side by side, as IntelliJ's diff shows them: the older on the left, the
//! newer on the right, highlighted as code, changed lines tinted, and their numbers beside them.
//! A diff of the working copy can put each change back as it was, one at a time. A change someone
//! else proposes, like an AI agent, is reviewed in the same view: the file as it is on the left,
//! the proposal on the right, each change accepted into the file or rejected.
//!
//! Its toolbar has IntelliJ's controls: the previous and next difference, jumping to the code,
//! side-by-side or unified, what whitespace to pass over, highlighting changed words or lines, and
//! folding unchanged lines away. The choices are kept in the settings.

use gleipnir_core::{BufferModel, settings, update_settings};
use gleipnir_index::{DiffRow, DiffRowKind, Whitespace, merge3, side_by_side_with, word_changes};
use gleipnir_packages::settings::{DiffHighlight, DiffSettings, DiffViewer, DiffWhitespace};
use gleipnir_syntax::{HighlightKind, Language, highlight_text};
use gleipnir_ui::{ContextMenu, MenuDismissed, Theme};
use gleipnir_workspace::{ModalView, Workspace};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, Hsla, KeyBinding, Pixels, Point, ScrollEvent, SharedString, StyledText,
    Subscription, UniformListScrollHandle, WeakEntity, Window, actions, anchored, deferred, div,
    prelude::*, px, uniform_list,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

actions!(
    diff_view,
    [CloseDiff, NextChange, PreviousChange, JumpToSource]
);

/// Unchanged lines kept beside each change when the rest are folded away.
const CONTEXT: usize = 4;
const ROW_HEIGHT: Pixels = px(20.);

pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some("DiffView");
    cx.bind_keys([
        KeyBinding::new("escape", CloseDiff, context),
        // IntelliJ's next and previous difference, and Jump to Source.
        KeyBinding::new("f7", NextChange, context),
        KeyBinding::new("shift-f7", PreviousChange, context),
        KeyBinding::new("f4", JumpToSource, context),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-down"
            } else {
                "ctrl-enter"
            },
            JumpToSource,
            context,
        ),
    ]);
}

/// The file a diff is of, to jump to: which side is that file as it is now.
struct Source {
    workspace: WeakEntity<Workspace>,
    path: PathBuf,
    right: bool,
}

/// One row of the view.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Display {
    /// A row of the diff, its two sides in columns.
    Pair(usize),
    /// In the unified view, a row's old line, its new line, or an unchanged line.
    Old(usize),
    New(usize),
    Both(usize),
    /// Unchanged rows folded away, until clicked.
    Fold(Range<usize>),
}

impl Display {
    /// The diff row it starts at.
    fn row(&self) -> usize {
        match self {
            Self::Pair(r) | Self::Old(r) | Self::New(r) | Self::Both(r) => *r,
            Self::Fold(rows) => rows.start,
        }
    }
}

/// Changed words in each side's lines: byte ranges by line number.
#[derive(Default)]
struct WordMarks {
    left: HashMap<usize, Vec<Range<usize>>>,
    right: HashMap<usize, Vec<Range<usize>>>,
}

/// Which columns a list of rows draws: both, or with the sides scrolling apart, one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    All,
    Left,
    Right,
}

/// Which toolbar menu is open.
#[derive(Clone, Copy)]
enum Menu {
    Viewer,
    Whitespace,
    Highlight,
}

/// Where the newer side lives, when changes can be put back: an open file's buffer, so it's
/// undoable, or the file on disk.
#[derive(Clone)]
pub enum Revertible {
    Buffer(Entity<BufferModel>),
    File(PathBuf),
}

/// One change decided on while reviewing a proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewDecision {
    pub accepted: bool,
    /// The change's first line in the file, from 1, when it was decided.
    pub line: usize,
    /// The file's lines it takes out, and the lines it puts in.
    pub removed: String,
    pub added: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewEvent {
    /// Every change has been decided, or the review was closed with `undecided` changes left,
    /// which stay out of the file.
    Finished {
        decisions: Vec<ReviewDecision>,
        undecided: usize,
    },
}

/// A proposal being reviewed: the open file the left side is, which accepted changes go into.
struct Review {
    buffer: Entity<BufferModel>,
    decisions: Vec<ReviewDecision>,
    /// Changes that stopped fitting the file when it changed underneath, left out undecided.
    left_out: usize,
    finished: bool,
    /// Said in the title bar, like that the file changed underneath.
    notice: Option<SharedString>,
}

/// One change: the lines it spans on each side, and their text.
struct Hunk {
    left_bytes: Range<usize>,
    right_bytes: Range<usize>,
    left_line: usize,
    left_text: String,
    right_text: String,
}

/// One side of the diff: its lines, and each line's highlights, relative to the line.
struct Side {
    text: String,
    lines: Vec<String>,
    highlights: Vec<Vec<(Range<usize>, HighlightKind)>>,
    /// The longest line, in columns, for how far it scrolls sideways.
    width: usize,
}

impl Side {
    fn new(text: &str, language: Option<Language>) -> Self {
        let text = text.replace("\r\n", "\n");
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let mut highlights = vec![Vec::new(); lines.len()];
        if let Some(language) = language {
            let mut starts = Vec::with_capacity(lines.len());
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                starts.push(offset);
                offset += line.len();
            }
            for span in highlight_text(language, &text) {
                // The line holding the span's start; spans across lines are cut at its end.
                let line = starts
                    .partition_point(|&s| s <= span.range.start)
                    .saturating_sub(1);
                let (Some(&start), Some(content)) = (starts.get(line), lines.get(line)) else {
                    continue;
                };
                let from = span.range.start - start;
                let to = (span.range.end - start).min(content.len());
                if from < to {
                    highlights[line].push((from..to, span.kind));
                }
            }
        }
        let width = lines
            .iter()
            .map(|l| {
                l.chars()
                    .map(|c| if c == '\t' { 4 } else { 1 })
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        Self {
            text,
            lines,
            highlights,
            width,
        }
    }
}

pub struct DiffView {
    focus_handle: FocusHandle,
    title: SharedString,
    left_title: SharedString,
    right_title: SharedString,
    language: Option<Language>,
    left: Rc<Side>,
    right: Rc<Side>,
    rows: Rc<Vec<DiffRow>>,
    display: Rc<Vec<Display>>,
    words: Rc<WordMarks>,
    options: DiffSettings,
    /// Folds opened by clicking, by their first row.
    unfolded: HashSet<usize>,
    scroll: UniformListScrollHandle,
    /// The left side's, when the sides scroll apart; `scroll` is then the right's.
    left_scroll: UniformListScrollHandle,
    left_scroll_x: Pixels,
    /// How far the code is scrolled sideways, and how far it can be.
    scroll_x: Pixels,
    max_scroll_x: Pixels,
    /// The change last moved to with F7, as its first row.
    current: Option<usize>,
    revertible: Option<Revertible>,
    review: Option<Review>,
    source: Option<Source>,
    menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
}

impl EventEmitter<DismissEvent> for DiffView {}
impl EventEmitter<ReviewEvent> for DiffView {}
impl ModalView for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DiffView {
    pub fn new(
        title: impl Into<SharedString>,
        (left_title, left): (impl Into<SharedString>, &str),
        (right_title, right): (impl Into<SharedString>, &str),
        cx: &mut Context<Self>,
    ) -> Self {
        let title = title.into();
        // The title starts with the file's path, which says its language.
        let path = title
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let language = gleipnir_packages::registry()
            .language_for_path(std::path::Path::new(&path))
            .map(|l| l.grammar);
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            title,
            left_title: left_title.into(),
            right_title: right_title.into(),
            language,
            left: Rc::new(Side::new(left, language)),
            right: Rc::new(Side::new(right, language)),
            rows: Rc::default(),
            display: Rc::default(),
            words: Rc::default(),
            options: settings(cx).diff.clone(),
            unfolded: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
            left_scroll: UniformListScrollHandle::new(),
            left_scroll_x: px(0.),
            scroll_x: px(0.),
            max_scroll_x: px(0.),
            current: None,
            revertible: None,
            review: None,
            source: None,
            menu: None,
        };
        this.diff_again();
        // Opens at the first change, as there's nothing to see before it.
        this.move_to_change(true);
        this
    }

    /// Lets F4 open the file at the change, closing the diff: the side that's the file as it is.
    pub fn jumps_to(
        mut self,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        right_is_file: bool,
    ) -> Self {
        self.source = Some(Source {
            workspace,
            path,
            right: right_is_file,
        });
        self
    }

    /// Works out the rows again from the two sides, as the options say.
    fn diff_again(&mut self) {
        let whitespace = match self.options.whitespace {
            DiffWhitespace::Compare => Whitespace::Compare,
            DiffWhitespace::Trim => Whitespace::Trim,
            DiffWhitespace::Ignore => Whitespace::Ignore,
            DiffWhitespace::IgnoreWithBlankLines => Whitespace::IgnoreWithBlankLines,
        };
        self.rows = Rc::new(side_by_side_with(
            &self.left.text,
            &self.right.text,
            whitespace,
        ));
        self.unfolded.clear();
        self.words = Rc::new(self.word_marks(whitespace != Whitespace::Compare));
        self.lay_out();
    }

    /// The changed words in each change that has lines on both sides.
    fn word_marks(&self, ignore_space: bool) -> WordMarks {
        let mut marks = WordMarks::default();
        if self.options.highlight != DiffHighlight::Words {
            return marks;
        }
        for run in self.changes() {
            let (left_lines, right_lines) =
                (self.lines_of(run.clone(), false), self.lines_of(run, true));
            if left_lines.is_empty() || right_lines.is_empty() {
                continue;
            }
            let join = |side: &Side, lines: &Range<usize>| side.lines[lines.clone()].join("\n");
            let (old, new) = (
                join(&self.left, &left_lines),
                join(&self.right, &right_lines),
            );
            let (old_marks, new_marks) = word_changes(&old, &new, ignore_space);
            split_by_line(&old, &old_marks, left_lines.start, &mut marks.left);
            split_by_line(&new, &new_marks, right_lines.start, &mut marks.right);
        }
        marks
    }

    /// Lays the rows out as the viewer and folding say.
    fn lay_out(&mut self) {
        let rows = &self.rows;
        let unified = self.options.viewer == DiffViewer::Unified;
        let fold =
            self.options.collapse_unchanged && rows.iter().any(|r| r.kind != DiffRowKind::Same);
        let same = |r: usize| {
            if unified {
                Display::Both(r)
            } else {
                Display::Pair(r)
            }
        };
        let mut out = Vec::with_capacity(rows.len());
        let mut i = 0;
        while i < rows.len() {
            let start = i;
            if rows[i].kind == DiffRowKind::Same {
                while i < rows.len() && rows[i].kind == DiffRowKind::Same {
                    i += 1;
                }
                // Context after the change before, and before the change after.
                let from = if start == 0 { 0 } else { start + CONTEXT };
                let to = if i == rows.len() {
                    i
                } else {
                    i.saturating_sub(CONTEXT)
                };
                if fold && to >= from + 2 && !self.unfolded.contains(&from) {
                    out.extend((start..from).map(same));
                    out.push(Display::Fold(from..to));
                    out.extend((to..i).map(same));
                } else {
                    out.extend((start..i).map(same));
                }
            } else {
                while i < rows.len() && rows[i].kind != DiffRowKind::Same {
                    i += 1;
                }
                if unified {
                    out.extend(
                        (start..i)
                            .filter(|&r| rows[r].left.is_some())
                            .map(Display::Old),
                    );
                    out.extend(
                        (start..i)
                            .filter(|&r| rows[r].right.is_some())
                            .map(Display::New),
                    );
                } else {
                    out.extend((start..i).map(Display::Pair));
                }
            }
        }
        self.display = Rc::new(out);
    }

    /// Where a diff row is shown: its line, or the fold it's in.
    fn display_index(&self, row: usize) -> usize {
        self.display
            .iter()
            .position(|d| match d {
                Display::Fold(rows) => rows.end > row,
                d => d.row() >= row,
            })
            .unwrap_or(self.display.len().saturating_sub(1))
    }

    /// Changes the options from the toolbar, and keeps them for next time.
    pub(crate) fn set_options(
        &mut self,
        change: impl FnOnce(&mut DiffSettings),
        cx: &mut Context<Self>,
    ) {
        let mut options = self.options.clone();
        change(&mut options);
        if options == self.options {
            return;
        }
        self.options = options.clone();
        // Should the settings file not save, the choice still holds while Gleipnir runs.
        update_settings(cx, |s| s.diff = options).ok();
        let current = self.current;
        self.diff_again();
        self.current = None;
        if let Some(row) = current {
            self.current = self.change_rows().into_iter().find(|&s| s >= row);
        }
        self.scroll_to_current();
        cx.notify();
    }

    fn unfold(&mut self, first_row: usize, cx: &mut Context<Self>) {
        self.unfolded.insert(first_row);
        self.lay_out();
        cx.notify();
    }

    /// The rows as shown, for tests: `+` and `-` lines in the unified view, `⋯ n` for a fold.
    pub fn debug_display(&self) -> Vec<String> {
        let text = |line: &Option<(usize, String)>| {
            line.as_ref().map(|(_, t)| t.clone()).unwrap_or_default()
        };
        self.display
            .iter()
            .map(|d| match d {
                Display::Pair(r) => {
                    format!(
                        "{} | {}",
                        text(&self.rows[*r].left),
                        text(&self.rows[*r].right)
                    )
                }
                Display::Old(r) => format!("- {}", text(&self.rows[*r].left)),
                Display::New(r) => format!("+ {}", text(&self.rows[*r].right)),
                Display::Both(r) => {
                    let row = &self.rows[*r];
                    format!(
                        "  {}",
                        text(if row.right.is_some() {
                            &row.right
                        } else {
                            &row.left
                        })
                    )
                }
                Display::Fold(rows) => format!("⋯ {}", rows.len()),
            })
            .collect()
    }

    /// The changed words marked on each side, for tests.
    pub fn debug_words(&self) -> (Vec<String>, Vec<String>) {
        let words = |side: &Side, marks: &HashMap<usize, Vec<Range<usize>>>| {
            let mut lines: Vec<_> = marks.iter().collect();
            lines.sort_by_key(|(line, _)| **line);
            lines
                .into_iter()
                .flat_map(|(line, ranges)| {
                    ranges
                        .iter()
                        .map(|r| side.lines[*line][r.clone()].to_string())
                })
                .collect()
        };
        (
            words(&self.left, &self.words.left),
            words(&self.right, &self.words.right),
        )
    }

    pub fn scroll_x(&self) -> Pixels {
        self.scroll_x
    }

    /// How far each side is scrolled down and sideways, left then right, for tests.
    pub fn scroll_offsets(&self) -> [(Pixels, Pixels); 2] {
        let down = |handle: &UniformListScrollHandle| -handle.0.borrow().base_handle.offset().y;
        let left = if self.split() {
            (down(&self.left_scroll), self.left_scroll_x)
        } else {
            (down(&self.scroll), self.scroll_x)
        };
        [left, (down(&self.scroll), self.scroll_x)]
    }

    /// Opens the file at the change being looked at, and closes the diff.
    fn jump_to_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let (workspace, path, right) =
            (source.workspace.clone(), source.path.clone(), source.right);
        // The change moved to, or else the first line shown.
        let row = self.current.unwrap_or_else(|| {
            let handle = if self.split() && !right {
                &self.left_scroll
            } else {
                &self.scroll
            };
            let top = (-handle.0.borrow().base_handle.offset().y / ROW_HEIGHT).max(0.) as usize;
            self.display.get(top).map_or(0, Display::row)
        });
        let line = self.rows[row.min(self.rows.len().saturating_sub(1))..]
            .iter()
            .find_map(|r| {
                if right {
                    r.right.as_ref()
                } else {
                    r.left.as_ref()
                }
            })
            .map_or(0, |(n, _)| *n);
        self.close(cx);
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace
                        .open_path_at(path, Some((line, 0)), window, cx)
                        .detach_and_log_err(cx);
                })
                .ok();
        });
    }

    fn deploy_menu(
        &mut self,
        which: Menu,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.weak_entity();
        let choice = |change: fn(&mut DiffSettings)| {
            let view = view.clone();
            move |_: &mut Window, cx: &mut App| {
                view.update(cx, |view, cx| view.set_options(change, cx))
                    .ok();
            }
        };
        let o = &self.options;
        let mark = |on: bool, label: &str| format!("{}{label}", if on { "✓ " } else { "    " });
        let menu = cx.new(|cx| match which {
            Menu::Viewer => ContextMenu::new(cx)
                .item(
                    mark(o.viewer == DiffViewer::SideBySide, "Side-by-side viewer"),
                    None,
                    choice(|o| o.viewer = DiffViewer::SideBySide),
                )
                .item(
                    mark(o.viewer == DiffViewer::Unified, "Unified viewer"),
                    None,
                    choice(|o| o.viewer = DiffViewer::Unified),
                ),
            Menu::Whitespace => ContextMenu::new(cx)
                .item(
                    mark(o.whitespace == DiffWhitespace::Compare, "Do not ignore"),
                    None,
                    choice(|o| o.whitespace = DiffWhitespace::Compare),
                )
                .item(
                    mark(o.whitespace == DiffWhitespace::Trim, "Trim whitespaces"),
                    None,
                    choice(|o| o.whitespace = DiffWhitespace::Trim),
                )
                .item(
                    mark(o.whitespace == DiffWhitespace::Ignore, "Ignore whitespaces"),
                    None,
                    choice(|o| o.whitespace = DiffWhitespace::Ignore),
                )
                .item(
                    mark(
                        o.whitespace == DiffWhitespace::IgnoreWithBlankLines,
                        "Ignore whitespaces and empty lines",
                    ),
                    None,
                    choice(|o| o.whitespace = DiffWhitespace::IgnoreWithBlankLines),
                ),
            Menu::Highlight => ContextMenu::new(cx)
                .item(
                    mark(o.highlight == DiffHighlight::Words, "Highlight words"),
                    None,
                    choice(|o| o.highlight = DiffHighlight::Words),
                )
                .item(
                    mark(o.highlight == DiffHighlight::Lines, "Highlight lines"),
                    None,
                    choice(|o| o.highlight = DiffHighlight::Lines),
                )
                .item(
                    mark(o.highlight == DiffHighlight::None, "Do not highlight"),
                    None,
                    choice(|o| o.highlight = DiffHighlight::None),
                ),
        });
        let menu_focus = menu.read(cx).focus_handle(cx);
        window.focus(&menu_focus, cx);
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &MenuDismissed, window, cx| {
                this.menu = None;
                window.focus(&this.focus_handle, cx);
                cx.notify();
            });
        self.menu = Some((menu, position, subscription));
        cx.notify();
    }

    /// Lets each change be put back as it was on the left, in the file the right side is.
    pub fn revertible(mut self, target: Revertible) -> Self {
        self.revertible = Some(target);
        self
    }

    /// The rows, as `left | right` with a mark for how they differ, for tests.
    pub fn debug_rows(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let mark = match row.kind {
                    DiffRowKind::Same => ' ',
                    DiffRowKind::Added => '+',
                    DiffRowKind::Deleted => '-',
                    DiffRowKind::Changed => '~',
                };
                fn side(line: &Option<(usize, String)>) -> &str {
                    line.as_ref().map(|(_, t)| t.as_str()).unwrap_or("")
                }
                format!("{mark} {} | {}", side(&row.left), side(&row.right))
            })
            .collect()
    }

    /// How many highlighted pieces each side has, for tests.
    pub fn highlight_count(&self) -> (usize, usize) {
        let count = |side: &Side| side.highlights.iter().map(Vec::len).sum();
        (count(&self.left), count(&self.right))
    }

    /// Each run of changed rows, as the rows it spans.
    fn changes(&self) -> Vec<Range<usize>> {
        let mut runs = Vec::new();
        let mut start = None;
        for (i, row) in self.rows.iter().enumerate() {
            match (row.kind == DiffRowKind::Same, start) {
                (false, None) => start = Some(i),
                (true, Some(s)) => {
                    runs.push(s..i);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            runs.push(s..self.rows.len());
        }
        runs
    }

    /// The first row of each change, for tests.
    pub fn change_rows(&self) -> Vec<usize> {
        self.changes().into_iter().map(|r| r.start).collect()
    }

    fn move_to_change(&mut self, forward: bool) {
        let starts = self.change_rows();
        let next = match (self.current, forward) {
            (None, _) => starts.first().copied(),
            (Some(at), true) => starts.iter().copied().find(|&s| s > at),
            (Some(at), false) => starts.iter().rev().copied().find(|&s| s < at),
        };
        if let Some(row) = next {
            self.current = Some(row);
            self.scroll_to_current();
        }
    }

    fn scroll_to_current(&self) {
        if let Some(row) = self.current {
            let at = self.display_index(row).saturating_sub(3);
            self.scroll.scroll_to_item(at, gpui::ScrollStrategy::Top);
            self.left_scroll
                .scroll_to_item(at, gpui::ScrollStrategy::Top);
        }
    }

    /// Whether the two sides are drawn as lists that scroll apart.
    fn split(&self) -> bool {
        self.options.viewer == DiffViewer::SideBySide && !self.options.sync_scroll
    }

    /// Scrolls one side, or both, sideways by a wheel's or trackpad's `dx`.
    fn scroll_sideways(&mut self, left: bool, dx: Pixels, cx: &mut Context<Self>) {
        let offset = if left && self.split() {
            &mut self.left_scroll_x
        } else {
            &mut self.scroll_x
        };
        let next = (*offset - dx).clamp(px(0.), self.max_scroll_x);
        if next != *offset {
            *offset = next;
            cx.notify();
        }
    }

    /// The lines a run of rows covers on one side; for a side with none, where they'd go.
    fn lines_of(&self, rows: Range<usize>, right: bool) -> Range<usize> {
        let line = |row: &DiffRow| {
            if right {
                row.right.as_ref().map(|l| l.0)
            } else {
                row.left.as_ref().map(|l| l.0)
            }
        };
        let found: Vec<usize> = self.rows[rows.clone()].iter().filter_map(line).collect();
        match (found.first(), found.last()) {
            (Some(&first), Some(&last)) => first..last + 1,
            _ => {
                let at = self.rows[rows.end..]
                    .iter()
                    .find_map(line)
                    .unwrap_or(if right {
                        self.right.lines.len()
                    } else {
                        self.left.lines.len()
                    });
                at..at
            }
        }
    }

    /// The change starting at a row, with where it is in each side's text.
    fn hunk(&self, first_row: usize) -> Option<Hunk> {
        let run = self.changes().into_iter().find(|r| r.start == first_row)?;
        let bytes = |side: &Side, lines: Range<usize>| {
            let all: Vec<&str> = side.text.split_inclusive('\n').collect();
            let lines = lines.start.min(all.len())..lines.end.min(all.len());
            let start: usize = all[..lines.start].iter().map(|l| l.len()).sum();
            let end = start + all[lines].iter().map(|l| l.len()).sum::<usize>();
            start..end
        };
        let left_lines = self.lines_of(run.clone(), false);
        let left_bytes = bytes(&self.left, left_lines.clone());
        let right_bytes = bytes(&self.right, self.lines_of(run, true));
        Some(Hunk {
            left_text: self.left.text[left_bytes.clone()].to_string(),
            right_text: self.right.text[right_bytes.clone()].to_string(),
            left_bytes,
            right_bytes,
            left_line: left_lines.start + 1,
        })
    }

    /// Replaces one side's text, and works out the diff again.
    fn set_side(&mut self, right: bool, text: &str) {
        let side = Rc::new(Side::new(text, self.language));
        if right {
            self.right = side;
        } else {
            self.left = side;
        }
        self.diff_again();
        self.current = None;
    }

    /// Puts the change starting at a row back as it is on the left.
    pub fn revert_change(&mut self, first_row: usize, cx: &mut Context<Self>) {
        let Some(target) = self.revertible.clone() else {
            return;
        };
        let Some(hunk) = self.hunk(first_row) else {
            return;
        };
        let mut text = self.right.text.clone();
        text.replace_range(hunk.right_bytes.clone(), &hunk.left_text);
        match target {
            Revertible::Buffer(buffer) => buffer.update(cx, |buffer, cx| {
                buffer.apply_external_edits(
                    vec![gleipnir_text::TextEdit::new(
                        hunk.right_bytes,
                        hunk.left_text,
                    )],
                    cx,
                )
            }),
            Revertible::File(path) => {
                std::fs::write(path, &text).ok();
            }
        }
        self.set_side(true, &text);
        cx.notify();
    }

    /// Reviews a proposed change to an open file, which is the left side, with the proposal on
    /// the right. Accepting a change edits the file, as a step undo takes back, unsaved;
    /// rejecting it leaves the file alone. Once every change is decided, or the view is closed,
    /// it says what was decided with `ReviewEvent::Finished`.
    pub fn reviewing(mut self, buffer: Entity<BufferModel>) -> Self {
        self.review = Some(Review {
            buffer,
            decisions: Vec::new(),
            left_out: 0,
            finished: false,
            notice: None,
        });
        self
    }

    pub fn is_reviewing(&self) -> bool {
        self.review.is_some()
    }

    /// What's been decided so far, in the order it was.
    pub fn decisions(&self) -> &[ReviewDecision] {
        self.review.as_ref().map_or(&[], |r| &r.decisions)
    }

    /// Checks the file is still what the left side shows. If it changed, as from another edit
    /// landing while the person looks, the proposal's changes are carried over to the file as it
    /// is now, as git merges, and nothing's decided this time. Changes that clash with the
    /// file's are left out.
    fn file_unchanged(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(review) = &self.review else {
            return false;
        };
        let now = review.buffer.read(cx).buffer().text();
        if now == self.left.text {
            return true;
        }
        let mut clashed = 0;
        let proposed: String = merge3(&self.left.text, &now, &self.right.text)
            .iter()
            .map(|chunk| match chunk.merged() {
                Some(lines) => lines.concat(),
                None => {
                    clashed += 1;
                    chunk.ours.concat()
                }
            })
            .collect();
        self.set_side(false, &now);
        self.set_side(true, &proposed);
        if let Some(review) = &mut self.review {
            review.left_out += clashed;
            review.notice = Some(if clashed == 0 {
                "The file changed, so the proposal was carried over to it".into()
            } else {
                format!(
                    "The file changed, and {clashed} proposed change(s) no longer fit it, so \
                     they were left out"
                )
                .into()
            });
        }
        if self.change_rows().is_empty() {
            self.finish_review(cx);
        }
        cx.notify();
        false
    }

    /// Takes the change starting at a row into the file.
    pub fn accept_change(&mut self, first_row: usize, cx: &mut Context<Self>) {
        self.decide(Some(first_row), true, cx);
    }

    /// Leaves the change starting at a row out of the file.
    pub fn reject_change(&mut self, first_row: usize, cx: &mut Context<Self>) {
        self.decide(Some(first_row), false, cx);
    }

    /// Takes every change left into the file, as one step undo takes back.
    pub fn accept_all(&mut self, cx: &mut Context<Self>) {
        self.decide(None, true, cx);
    }

    /// Leaves every change left out of the file.
    pub fn reject_all(&mut self, cx: &mut Context<Self>) {
        self.decide(None, false, cx);
    }

    /// Decides one change, by its first row, or with `None`, all that are left.
    fn decide(&mut self, first_row: Option<usize>, accept: bool, cx: &mut Context<Self>) {
        if self.review.as_ref().is_none_or(|r| r.finished) || !self.file_unchanged(cx) {
            return;
        }
        let rows = match first_row {
            Some(row) => vec![row],
            None => self.change_rows(),
        };
        let hunks: Vec<Hunk> = rows.into_iter().filter_map(|row| self.hunk(row)).collect();
        if hunks.is_empty() {
            return;
        }
        let mut left = self.left.text.clone();
        let mut right = self.right.text.clone();
        // From the end back, so earlier ranges stay where they are.
        for hunk in hunks.iter().rev() {
            if accept {
                left.replace_range(hunk.left_bytes.clone(), &hunk.right_text);
            } else {
                right.replace_range(hunk.right_bytes.clone(), &hunk.left_text);
            }
        }
        let review = self.review.as_mut().expect("reviewing");
        if accept {
            let edits = hunks
                .iter()
                .map(|h| gleipnir_text::TextEdit::new(h.left_bytes.clone(), h.right_text.clone()))
                .collect();
            review
                .buffer
                .update(cx, |buffer, cx| buffer.apply_external_edits(edits, cx));
        }
        review.notice = None;
        review
            .decisions
            .extend(hunks.into_iter().map(|h| ReviewDecision {
                accepted: accept,
                line: h.left_line,
                removed: h.left_text,
                added: h.right_text,
            }));
        if accept {
            self.set_side(false, &left);
        } else {
            self.set_side(true, &right);
        }
        if self.change_rows().is_empty() {
            self.finish_review(cx);
        } else {
            self.move_to_change(true);
        }
        cx.notify();
    }

    /// Ends the review, leaving out whatever wasn't decided, and closes the view.
    fn finish_review(&mut self, cx: &mut Context<Self>) {
        let mut undecided = self.change_rows().len();
        let Some(review) = &mut self.review else {
            return;
        };
        undecided += review.left_out;
        if !review.finished {
            review.finished = true;
            cx.emit(ReviewEvent::Finished {
                decisions: review.decisions.clone(),
                undecided,
            });
        }
        cx.emit(DismissEvent);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.review.is_some() {
            self.finish_review(cx);
        } else {
            cx.emit(DismissEvent);
        }
    }
}

/// Byte ranges in lines joined by `\n`, cut into ranges within each line, by line number.
fn split_by_line(
    text: &str,
    ranges: &[Range<usize>],
    first_line: usize,
    out: &mut HashMap<usize, Vec<Range<usize>>>,
) {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    for range in ranges {
        let mut line = starts
            .partition_point(|&s| s <= range.start)
            .saturating_sub(1);
        let mut from = range.start;
        while from < range.end && line < starts.len() {
            let line_end = starts.get(line + 1).map_or(text.len(), |next| next - 1);
            let to = range.end.min(line_end);
            if to > from {
                out.entry(first_line + line)
                    .or_default()
                    .push(from - starts[line]..to - starts[line]);
            }
            line += 1;
            from = starts.get(line).copied().unwrap_or(range.end);
        }
    }
}

fn change_color(kind: DiffRowKind, theme: &Theme) -> Option<Hsla> {
    match kind {
        DiffRowKind::Same => None,
        DiffRowKind::Added => Some(theme.git_added),
        DiffRowKind::Deleted => Some(theme.git_deleted),
        DiffRowKind::Changed => Some(theme.git_modified),
    }
}

/// A line as code, colored by its highlights, with its changed words marked.
fn code_line(
    side: &Side,
    line: usize,
    marks: Option<&Vec<Range<usize>>>,
    mark: Hsla,
    theme: &Theme,
) -> StyledText {
    let text = side.lines.get(line).cloned().unwrap_or_default();
    let valid = |range: &Range<usize>| {
        range.start < range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)
    };
    let syntax: Vec<(Range<usize>, Hsla)> = side
        .highlights
        .get(line)
        .into_iter()
        .flatten()
        .filter(|(range, _)| valid(range))
        .map(|(range, kind)| (range.clone(), theme.syntax.color(*kind)))
        .collect();
    let marks: Vec<Range<usize>> = marks
        .into_iter()
        .flatten()
        .filter(|r| valid(r))
        .cloned()
        .collect();
    // Cut at every edge of either, so the pieces don't overlap.
    let mut edges: Vec<usize> = syntax
        .iter()
        .map(|(r, _)| r)
        .chain(&marks)
        .flat_map(|r| [r.start, r.end])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = edges
        .windows(2)
        .filter_map(|pair| {
            let piece = pair[0]..pair[1];
            let color = syntax
                .iter()
                .find(|(r, _)| r.start <= piece.start && piece.end <= r.end)
                .map(|(_, c)| *c);
            let marked = marks
                .iter()
                .any(|r| r.start <= piece.start && piece.end <= r.end);
            (color.is_some() || marked).then(|| {
                (
                    piece,
                    HighlightStyle {
                        color,
                        background_color: marked.then_some(mark),
                        ..Default::default()
                    },
                )
            })
        })
        .collect();
    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let font = gleipnir_editor::editor_font(window, cx);
        let font_size = px(theme.editor_font_size);
        let char_width = window
            .text_system()
            .em_advance(window.text_system().resolve_font(&font), font_size)
            .unwrap_or(px(8.));
        let digits = self
            .left
            .lines
            .len()
            .max(self.right.lines.len())
            .max(1)
            .to_string()
            .len();
        let number_width = px(10. + 8. * digits as f32);
        let change_starts = self.change_rows();
        let can_revert = self.revertible.is_some();
        let reviewing = self.review.is_some();
        let unified = self.options.viewer == DiffViewer::Unified;
        let highlight = self.options.highlight;
        let middle = if reviewing { px(46.) } else { px(18.) };
        let size = window.viewport_size();
        let width = size.width - px(80.);

        // How far the code can scroll sideways: the widest line past the columns' room for it.
        let room = if unified {
            width - number_width * 2. - middle
        } else {
            (width - middle) / 2. - number_width
        };
        let widest = self.left.width.max(self.right.width) as f32 * char_width + px(24.);
        self.max_scroll_x = (widest - room).max(px(0.));
        self.scroll_x = self.scroll_x.min(self.max_scroll_x);
        self.left_scroll_x = self.left_scroll_x.min(self.max_scroll_x);
        let split = self.split();

        let starts: Rc<HashSet<usize>> = Rc::new(change_starts.iter().copied().collect());
        let rows_of = |part: Part| {
            let starts = starts.clone();
            move |this: &mut Self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>| {
                let theme = cx.global::<Theme>().clone();
                let (left, right, rows, display, words) = (
                    this.left.clone(),
                    this.right.clone(),
                    this.rows.clone(),
                    this.display.clone(),
                    this.words.clone(),
                );
                let scroll_x = if part == Part::Left && this.split() {
                    this.left_scroll_x
                } else {
                    this.scroll_x
                };
                let tinted = highlight != DiffHighlight::None;
                let number = |line: Option<usize>| {
                    div()
                        .w(number_width)
                        .h_full()
                        .flex_none()
                        .pr(px(8.))
                        .text_right()
                        .text_color(theme.editor_gutter_text)
                        .child(line.map(|n| (n + 1).to_string()).unwrap_or_default())
                };
                // The code of one line, scrolled sideways.
                let code = |side: &Side,
                            line: Option<usize>,
                            marks: &HashMap<usize, Vec<Range<usize>>>,
                            mark: Hsla| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(div().ml(-scroll_x).children(
                            line.map(|n| code_line(side, n, marks.get(&n), mark, &theme)),
                        ))
                };
                // Between the columns: putting a change back, or deciding on a proposed one.
                let gutter = |first_row: Option<usize>, cx: &mut Context<Self>| {
                    let review_button =
                        |label: &'static str, accept: bool, row: usize, cx: &mut Context<Self>| {
                            let kind = if accept { "accept" } else { "reject" };
                            div()
                                .id((kind, row))
                                .debug_selector(move || format!("review-{kind}-{row}"))
                                .px(px(4.))
                                .rounded(px(3.))
                                .cursor_pointer()
                                .font_weight(FontWeight::BOLD)
                                .text_color(if accept {
                                    theme.git_added
                                } else {
                                    theme.git_deleted
                                })
                                .hover(|s| s.bg(theme.hover))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if accept {
                                        this.accept_change(row, cx)
                                    } else {
                                        this.reject_change(row, cx)
                                    }
                                }))
                        };
                    let id = first_row.unwrap_or(usize::MAX);
                    div()
                        .id(("diff-revert", id))
                        .debug_selector(move || format!("diff-revert-{id}"))
                        .w(middle)
                        .h_full()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(2.))
                        .border_l_1()
                        .border_r_1()
                        .border_color(theme.border)
                        .when_some(first_row.filter(|_| reviewing), |d, row| {
                            d.child(review_button("✓", true, row, cx))
                                .child(review_button("✕", false, row, cx))
                        })
                        .when_some(first_row.filter(|_| can_revert), |d, row| {
                            d.cursor_pointer()
                                .text_color(theme.accent)
                                .font_weight(FontWeight::BOLD)
                                .hover(|s| s.bg(theme.hover))
                                .child("⟲")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.revert_change(row, cx)),
                                )
                        })
                };
                range
                .map(|index| {
                    let entry = &display[index];
                    // Buttons go on the first line shown of each change.
                    let first_row = (!matches!(entry, Display::Fold(_))
                        && starts.contains(&entry.row())
                        && (index == 0 || display[index - 1].row() != entry.row()
                            || matches!(display[index - 1], Display::Fold(_))))
                    .then(|| entry.row());
                    let base = div()
                        .id(("diff-row", index))
                        .w_full()
                        .h(ROW_HEIGHT)
                        .flex()
                        .flex_row();
                    match entry {
                        Display::Fold(folded) => {
                            let first = folded.start;
                            let count = folded.len();
                            base.debug_selector(move || format!("diff-fold-{first}"))
                                .items_center()
                                .pl(number_width)
                                .bg(theme.surface)
                                .text_color(theme.text_muted)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.hover))
                                .child(format!("⋯ {count} unchanged lines"))
                                .on_click(cx.listener(move |this, _, _, cx| this.unfold(first, cx)))
                                .into_any_element()
                        }
                        Display::Pair(r) => {
                            let row = &rows[*r];
                            let tint = change_color(row.kind, &theme)
                                .filter(|_| tinted)
                                .map(|c| (c.opacity(0.16), c.opacity(0.4)));
                            let side = |line: &Option<(usize, String)>, side: &Side, marks: &HashMap<usize, Vec<Range<usize>>>| {
                                let n = line.as_ref().map(|(n, _)| *n);
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_row()
                                    .when_some(tint.filter(|_| n.is_some()), |d, (bg, _)| d.bg(bg))
                                    .child(number(n))
                                    .child(code(side, n, marks, tint.map_or(theme.hover, |t| t.1)))
                            };
                            match part {
                                Part::All => base
                                    .child(side(&row.left, &left, &words.left))
                                    .child(gutter(first_row, cx))
                                    .child(side(&row.right, &right, &words.right)),
                                Part::Left => base.child(side(&row.left, &left, &words.left)),
                                Part::Right => base
                                    .child(gutter(first_row, cx))
                                    .child(side(&row.right, &right, &words.right)),
                            }
                            .into_any_element()
                        }
                        Display::Old(r) | Display::New(r) | Display::Both(r) => {
                            let row = &rows[*r];
                            let (old, new) = (
                                row.left.as_ref().map(|(n, _)| *n),
                                row.right.as_ref().map(|(n, _)| *n),
                            );
                            let (numbers, from_right, color) = match entry {
                                Display::Old(_) => ((old, None), false, Some(theme.git_deleted)),
                                Display::New(_) => ((None, new), true, Some(theme.git_added)),
                                _ => ((old, new), new.is_some(), None),
                            };
                            let color = color.filter(|_| tinted);
                            let (side, line, marks) = if from_right {
                                (&right, new, &words.right)
                            } else {
                                (&left, old, &words.left)
                            };
                            base.when_some(color, |d, c| d.bg(c.opacity(0.16)))
                                .child(number(numbers.0))
                                .child(number(numbers.1))
                                .child(gutter(first_row, cx))
                                .child(code(
                                    side,
                                    line,
                                    marks,
                                    color.map_or(theme.hover, |c| c.opacity(0.4)),
                                ))
                                .into_any_element()
                        }
                    }
                })
                .collect::<Vec<_>>()
            }
        };

        // A list of rows, which a sideways swipe scrolls sideways rather than down.
        let count = self.display.len();
        let list = |id: &'static str,
                    part: Part,
                    handle: &UniformListScrollHandle,
                    cx: &mut Context<Self>| {
            let mut list = uniform_list(id, count, cx.processor(rows_of(part)))
                .track_scroll(handle)
                .size_full();
            list.style().restrict_scroll_to_axis = Some(true);
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .h_full()
                .min_w_0()
                .on_scroll(cx.listener(move |this, event: &ScrollEvent, _, cx| {
                    let dx = event.delta.pixel_delta(ROW_HEIGHT).x;
                    if dx != px(0.) {
                        this.scroll_sideways(part == Part::Left, dx, cx);
                    }
                }))
                .child(list)
        };
        let (left_handle, right_handle) = (self.left_scroll.clone(), self.scroll.clone());
        let lists = if split {
            let side_width = (width - middle) / 2.;
            div()
                .size_full()
                .flex()
                .flex_row()
                .child(
                    list("diff-rows-left", Part::Left, &left_handle, cx)
                        .w(side_width)
                        .flex_none(),
                )
                .child(list("diff-rows", Part::Right, &right_handle, cx).flex_1())
        } else {
            div()
                .size_full()
                .child(list("diff-rows", Part::All, &right_handle, cx).size_full())
        };

        let changes = match change_starts.len() {
            0 => "No differences".to_string(),
            1 => "1 difference".to_string(),
            n => format!("{n} differences"),
        };
        let hint = if reviewing {
            "✓ accepts a change, ✕ rejects it"
        } else if can_revert {
            "⟲ puts a change back"
        } else {
            ""
        };
        let notice = self.review.as_ref().and_then(|r| r.notice.clone());
        let header_button = |id: &'static str, label: &'static str, accept: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .text_color(theme.text)
                .hover(|s| s.bg(theme.hover))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if accept {
                        this.accept_all(cx)
                    } else {
                        this.reject_all(cx)
                    }
                }))
        };
        let tool = |id: &'static str, label: SharedString, enabled: bool, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .when(on, |d| d.bg(theme.selection))
                .text_color(if enabled {
                    theme.text
                } else {
                    theme.text_faint
                })
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme.hover)))
                .child(label)
        };
        let dropdown = |id: &'static str, label: &'static str, which: Menu| {
            tool(id, format!("{label} ▾").into(), true, false).on_click(cx.listener(
                move |this, event: &gpui::ClickEvent, window, cx| {
                    this.deploy_menu(which, event.position(), window, cx)
                },
            ))
        };
        let separator = || div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(theme.border);
        let options = &self.options;
        let viewer = match options.viewer {
            DiffViewer::SideBySide => "Side-by-side viewer",
            DiffViewer::Unified => "Unified viewer",
        };
        let whitespace = match options.whitespace {
            DiffWhitespace::Compare => "Do not ignore",
            DiffWhitespace::Trim => "Trim whitespaces",
            DiffWhitespace::Ignore => "Ignore whitespaces",
            DiffWhitespace::IgnoreWithBlankLines => "Ignore whitespaces and empty lines",
        };
        let highlighting = match options.highlight {
            DiffHighlight::Words => "Highlight words",
            DiffHighlight::Lines => "Highlight lines",
            DiffHighlight::None => "Do not highlight",
        };
        let has_changes = !change_starts.is_empty();
        let has_source = self.source.is_some();
        let collapse = options.collapse_unchanged;
        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .py(px(3.))
            .text_size(px(12.5))
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .child(
                tool("diff-previous", "↑".into(), has_changes, false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.move_to_change(false);
                        cx.notify();
                    },
                )),
            )
            .child(
                tool("diff-next", "↓".into(), has_changes, false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.move_to_change(true);
                        cx.notify();
                    },
                )),
            )
            .when(has_source, |d| {
                d.child(separator()).child(
                    tool("diff-jump", "Jump to Source".into(), true, false).on_click(
                        cx.listener(|this, _, window, cx| this.jump_to_source(window, cx)),
                    ),
                )
            })
            .child(separator())
            .child(dropdown("diff-viewer", viewer, Menu::Viewer))
            .child(dropdown("diff-whitespace", whitespace, Menu::Whitespace))
            .child(dropdown("diff-highlight", highlighting, Menu::Highlight))
            .child(
                tool("diff-collapse", "Collapse unchanged".into(), true, collapse).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.set_options(|o| o.collapse_unchanged = !o.collapse_unchanged, cx)
                    }),
                ),
            )
            .when(!unified, |d| {
                d.child(
                    tool(
                        "diff-sync",
                        "Sync scrolling".into(),
                        true,
                        options.sync_scroll,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_options(|o| o.sync_scroll = !o.sync_scroll, cx)
                    })),
                )
            })
            .child(div().flex_1())
            .child(div().text_color(theme.text_muted).child(changes));

        let menu = self.menu.as_ref().map(|(menu, position, _)| {
            deferred(
                anchored()
                    .position(*position)
                    .snap_to_window()
                    .child(menu.clone()),
            )
            .with_priority(3)
        });

        let column_titles = if unified {
            div().flex().flex_row().child(
                div()
                    .px(px(14.))
                    .py(px(4.))
                    .child(format!("{} → {}", self.left_title, self.right_title)),
            )
        } else {
            div()
                .flex()
                .flex_row()
                .child(
                    div()
                        .flex_1()
                        .px(px(14.))
                        .py(px(4.))
                        .child(self.left_title.clone()),
                )
                .child(div().w(middle).flex_none())
                .child(
                    div()
                        .flex_1()
                        .px(px(14.))
                        .py(px(4.))
                        .child(self.right_title.clone()),
                )
        };

        div()
            .key_context("DiffView")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(width)
            .h(size.height - px(140.))
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_color(theme.text)
            .on_action(cx.listener(|this, _: &CloseDiff, _, cx| this.close(cx)))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| {
                this.move_to_change(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| {
                this.move_to_change(false);
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &JumpToSource, window, cx| this.jump_to_source(window, cx)),
            )
            // A review isn't thrown away by a stray click outside, nor is anything by a click in
            // the toolbar's menus.
            .when(!reviewing, |d| {
                d.on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if this.menu.is_none() {
                        cx.emit(DismissEvent)
                    }
                }))
            })
            .child(
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .bg(theme.surface)
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(13.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .children(notice.map(|n| div().text_color(theme.warning).child(n)))
                            .when(!hint.is_empty(), |d| {
                                d.child(div().text_color(theme.text_muted).child(hint))
                            })
                            .when(reviewing, |d| {
                                d.child(header_button("review-accept-all", "Accept All", true))
                                    .child(header_button("review-reject-all", "Reject All", false))
                            }),
                    ),
            )
            .child(toolbar)
            .child(
                column_titles
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .bg(theme.surface)
                    .border_b_1()
                    .border_color(theme.border),
            )
            .child(
                div()
                    .id("diff-code")
                    .flex_1()
                    .min_h_0()
                    .font(font)
                    .text_size(font_size)
                    .child(lists),
            )
            .children(menu)
    }
}
