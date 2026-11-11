use crate::actions::*;
use crate::element::{EditorElement, FrameGeometry};
use gleipnir_core::{BufferEvent, BufferModel, DiskStatus};
use gleipnir_syntax::HighlightSpan;
use gleipnir_text::ops::{self, EditContext, Motion};
use gleipnir_text::{AppliedEdit, Selection, SelectionSet};
use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, Modifiers, MouseButton, Pixels, Point, ScrollDelta, Subscription, Task,
    UTF16Selection, Window, div, point, prelude::*, px,
};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

const CURSOR_BLINK: Duration = Duration::from_millis(530);

/// Marks clipboard text as whole lines, so it pastes above the cursor line instead of inline.
#[derive(Serialize, Deserialize)]
struct ClipboardMetadata {
    linewise: bool,
}

/// What a drag is doing, decided by the click that started it.
#[derive(Clone, Debug)]
pub(crate) enum Drag {
    /// Extends from a fixed anchor, by characters.
    Chars { anchor: usize },
    /// Started with a double or triple click: grows by words or lines, always keeping the
    /// originally clicked word or line selected.
    Units { origin: Range<usize>, lines: bool },
    /// Alt+drag: a rectangular selection, from an anchor (line, display column).
    Column { anchor: (usize, usize) },
    /// The selections from before an additive click, which the drag adds to.
    Adding { base: SelectionSet, anchor: usize },
}

/// Highlights for the lines on screen. Stays valid until the tree or the visible range changes,
/// so a blinking cursor or a mouse move doesn't re-run the query.
pub(crate) struct HighlightCache {
    pub generation: u64,
    /// The semantic tokens' generation the spans include.
    pub tokens: u64,
    pub version: u64,
    pub lines: Range<usize>,
    pub spans: Rc<Vec<HighlightSpan>>,
}

/// A plain description of the editor, for the test driver to check against.
#[derive(Clone, Debug, Serialize)]
pub struct EditorState {
    pub path: Option<String>,
    pub language: Option<String>,
    pub text: String,
    pub dirty: bool,
    pub disk_status: String,
    pub selections: Vec<(usize, usize)>,
    pub primary: usize,
    pub scroll_y: f32,
    pub visible_lines: Option<(usize, usize)>,
    pub highlight_spans: usize,
    pub parse_pending: bool,
    /// The primary cursor's zero-based line and byte column.
    pub cursor: (usize, usize),
    /// What the completion menu lists, if it's open.
    pub completions: Vec<String>,
    pub hover: Option<String>,
    pub signature: Option<String>,
    /// Each as `line:column severity: message`, one-based.
    pub diagnostics: Vec<String>,
    /// Inlay hints, as `line: text`, one-based.
    pub inlay_hints: Vec<String>,
    /// The other uses of the name under the cursor, as their text.
    pub usage_highlights: Vec<String>,
    /// How many ranges the server colored.
    pub semantic_tokens: usize,
    /// Lines with breakpoints, one-based.
    pub breakpoints: Vec<usize>,
    /// The line execution is stopped on, one-based.
    pub execution_line: Option<usize>,
    /// What's shown beside lines, as `line: text`, one-based, for the lines on screen.
    pub inline_values: Vec<String>,
    /// How lines differ from the last commit, as `line: added`, one-based.
    pub line_changes: Vec<String>,
    /// The grey suggestion, as `text (87%, source)`.
    pub suggestion: Option<String>,
    /// Tests' last results in the gutter, as `line: passed` or `line: failed`, one-based.
    pub run_results: Vec<String>,
    /// The text beside each line in the gutter, like who changed it, as `line: text`, one-based.
    pub gutter_text: Vec<String>,
}

pub enum EditorEvent {
    /// The user asked to save a buffer that has no path yet.
    SaveAsRequested,
    /// The user asked to save, but the file changed on disk since it was opened, so saving would
    /// lose those changes. The host should ask what to do.
    SaveConflict,
    /// A save failed, with the reason.
    SaveFailed(String),
    /// A Cmd+click, or Ctrl+click on Linux, on a name: go to where it's defined.
    GoToDefinition { offset: usize },
    /// A click on a line number or beside it, with decorations shown: toggle a breakpoint.
    GutterClicked { line: usize },
    /// A click on a line's Run icon.
    RunClicked { line: usize },
    /// A right-click in the gutter, at a position in the window, for a menu.
    GutterMenu {
        line: usize,
        position: gpui::Point<Pixels>,
    },
    /// A click on the text shown beside a line in the gutter, like who last changed it.
    GutterTextClicked { line: usize },
    /// A right-click on that text, at a position in the window, for a menu.
    GutterTextMenu {
        line: usize,
        position: gpui::Point<Pixels>,
    },
}

impl EventEmitter<EditorEvent> for EditorView {}

pub struct EditorView {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) buffer: Entity<BufferModel>,
    pub(crate) selections: SelectionSet,
    /// Scroll position in pixels: `x` to the right, `y` downward.
    pub(crate) scroll: Point<Pixels>,
    /// Set when the next frame should bring the primary cursor into view. Laying out knows the
    /// viewport and metrics, so that's where it happens.
    pub(crate) autoscroll: bool,
    /// Set with `autoscroll` when the cursor should land mid-screen, as after a jump.
    pub(crate) autoscroll_center: bool,
    pub(crate) geometry: Option<Rc<FrameGeometry>>,
    pub(crate) highlights: Option<HighlightCache>,
    pub(crate) cursor_visible: bool,
    pub(crate) drag: Option<Drag>,
    pub(crate) marked: Option<Range<usize>>,
    /// Selections to return to on Shrink Selection, innermost first.
    expand_stack: Vec<SelectionSet>,
    /// Whether occurrence matching started from a bare cursor, and so matches whole words.
    occurrence_whole_word: bool,
    /// Durations of recent frames, when measuring.
    pub(crate) frame_times: Option<Vec<Duration>>,
    blink: Option<Task<()>>,
    pub(crate) semantics: crate::semantics::SemanticsState,
    pub(crate) suggestion: crate::suggestion::SuggestionState,
    pub(crate) decorations: Option<Rc<dyn crate::decorations::DecorationProvider>>,
    /// A text box, like a commit message: no line numbers, and no current line shown.
    pub(crate) plain: bool,
    pub(crate) annotations: crate::annotations::Annotations,
    /// Text beside each line in the gutter, like who last changed it.
    pub(crate) gutter_text: Option<crate::gutter_text::GutterText>,
    /// The line whose label the mouse is on, and where, for its details.
    pub(crate) gutter_hover: Option<(usize, Point<Pixels>)>,
    /// The widest label, in characters.
    pub(crate) gutter_text_chars: usize,
    pub(crate) caret_gesture: crate::caret_gesture::CaretGesture,
    /// Find and Replace, once it's been opened.
    pub(crate) find: Option<crate::find::FindState>,
    _subscriptions: Vec<Subscription>,
}

impl EditorView {
    pub fn new(buffer: Entity<BufferModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.subscribe(&buffer, Self::on_buffer_event),
            cx.observe(&buffer, |_, _, cx| cx.notify()),
            cx.on_focus(&focus_handle, window, |this, _, cx| this.restart_blink(cx)),
            cx.on_blur(&focus_handle, window, |this, _, cx| {
                this.blink = None;
                this.drag = None;
                cx.notify();
            }),
        ];
        Self {
            focus_handle,
            buffer,
            selections: SelectionSet::cursor(0),
            scroll: point(px(0.), px(0.)),
            autoscroll: false,
            autoscroll_center: false,
            geometry: None,
            highlights: None,
            cursor_visible: true,
            drag: None,
            marked: None,
            expand_stack: Vec::new(),
            occurrence_whole_word: false,
            frame_times: None,
            blink: None,
            semantics: Default::default(),
            suggestion: Default::default(),
            decorations: None,
            plain: false,
            annotations: Default::default(),
            gutter_text: None,
            gutter_hover: None,
            gutter_text_chars: 0,
            caret_gesture: Default::default(),
            find: None,
            _subscriptions: subscriptions,
        }
    }

    /// Shows a debugger's breakpoints, Run icons, execution line and values in this editor.
    pub fn set_decorations(
        &mut self,
        provider: Rc<dyn crate::decorations::DecorationProvider>,
        cx: &mut Context<Self>,
    ) {
        self.decorations = Some(provider);
        cx.notify();
    }

    /// Makes this a plain text box, without line numbers or the current line marked.
    pub fn set_plain(&mut self, plain: bool, cx: &mut Context<Self>) {
        self.plain = plain;
        cx.notify();
    }

    pub fn has_decorations(&self) -> bool {
        self.decorations.is_some()
    }

    pub fn buffer(&self) -> &Entity<BufferModel> {
        &self.buffer
    }

    /// Where the primary caret is on screen, in window coordinates, if it's in view.
    pub fn caret_bounds(&self, cx: &App) -> Option<Bounds<Pixels>> {
        let buffer = self.buffer.read(cx).buffer();
        self.geometry
            .as_ref()?
            .bounds_for_offset(self.selections.primary().head, buffer)
    }

    /// Closes the hover, completion and parameter info popups, as a popup opening over them does.
    pub fn dismiss_popups(&mut self, cx: &mut Context<Self>) {
        self.close_popups(cx);
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn set_selections(&mut self, selections: SelectionSet, cx: &mut Context<Self>) {
        self.selections = selections;
        self.did_move(cx);
    }

    /// Moves the cursor to a line and column, both zero-based with the column in bytes, and centres
    /// it on screen. Out-of-range values are clipped.
    pub fn jump_to(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        let offset = self
            .buffer
            .read(cx)
            .buffer()
            .point_to_offset(gleipnir_text::Point::new(line, column));
        self.selections = SelectionSet::cursor(offset);
        self.autoscroll_center = true;
        self.expand_stack.clear();
        self.did_move(cx);
    }

    /// Copies the cursor and scroll position from another view, for a split showing the same place.
    pub fn copy_position_from(&mut self, other: &EditorView, cx: &mut Context<Self>) {
        self.selections = other.selections.clone();
        self.scroll = other.scroll;
        cx.notify();
    }

    pub fn scroll_y(&self) -> f32 {
        self.scroll.y.into()
    }

    /// Restores a saved position. Offsets are clipped, as the file may have changed since.
    pub fn restore_position(&mut self, cursor: usize, scroll_y: f32, cx: &mut Context<Self>) {
        let cursor = self.buffer.read(cx).buffer().clip(cursor);
        self.selections = SelectionSet::cursor(cursor);
        self.scroll.y = px(scroll_y);
        cx.notify();
    }

    pub fn state(&self, cx: &App) -> EditorState {
        let model = self.buffer.read(cx);
        let buffer = model.buffer();
        EditorState {
            path: model.path().map(|p| p.display().to_string()),
            language: model.language().map(|l| l.name().to_string()),
            text: buffer.text(),
            dirty: buffer.is_dirty(),
            disk_status: format!("{:?}", model.disk_status()),
            selections: self.selections.iter().map(|s| (s.anchor, s.head)).collect(),
            primary: self.selections.primary_index(),
            scroll_y: self.scroll.y.into(),
            visible_lines: self
                .geometry
                .as_ref()
                .map(|g| (g.first_line, g.first_line + g.lines.len())),
            highlight_spans: self.highlights.as_ref().map_or(0, |h| h.spans.len()),
            parse_pending: model.is_parse_pending(),
            cursor: {
                let point = buffer.offset_to_point(self.selections.primary().head);
                (point.line, point.col)
            },
            completions: self.completion_labels(),
            hover: self.hover_text(),
            signature: self.signature_label(),
            diagnostics: model
                .diagnostics()
                .iter()
                .map(|d| {
                    let point = buffer.offset_to_point(d.range.start);
                    format!(
                        "{}:{} {:?}: {}",
                        point.line + 1,
                        point.col + 1,
                        d.severity,
                        d.message
                    )
                })
                .collect(),
            inlay_hints: self.inlay_labels(cx),
            usage_highlights: self
                .annotations
                .highlights
                .iter()
                .map(|r| buffer.slice(r.clone()))
                .collect(),
            semantic_tokens: self.semantic_token_count(),
            breakpoints: self.decorations.as_ref().map_or_else(Vec::new, |d| {
                d.breakpoints(cx).iter().map(|b| b.line + 1).collect()
            }),
            execution_line: self
                .decorations
                .as_ref()
                .and_then(|d| d.execution_line(cx))
                .map(|(line, _)| line + 1),
            inline_values: match (&self.decorations, &self.geometry) {
                (Some(d), Some(g)) => d
                    .inline_values(g.first_line..g.first_line + g.lines.len(), cx)
                    .into_iter()
                    .map(|(line, text)| format!("{}: {text}", line + 1))
                    .collect(),
                _ => Vec::new(),
            },
            suggestion: self.suggestion().map(|s| match s.score {
                Some(score) => format!("{} ({:.0}%, {})", s.text, score * 100., s.source),
                None => format!("{} ({})", s.text, s.source),
            }),
            line_changes: model
                .line_changes()
                .iter()
                .map(|c| {
                    let kind = match c.kind {
                        gleipnir_index::LineChangeKind::Added => "added",
                        gleipnir_index::LineChangeKind::Modified => "modified",
                        gleipnir_index::LineChangeKind::Deleted => "deleted",
                    };
                    format!("{}: {kind}", c.lines.start + 1)
                })
                .collect(),
            run_results: self.decorations.as_ref().map_or_else(Vec::new, |d| {
                d.run_results(cx)
                    .into_iter()
                    .map(|(line, passed)| {
                        format!("{}: {}", line + 1, if passed { "passed" } else { "failed" })
                    })
                    .collect()
            }),
            gutter_text: self.gutter_labels(),
        }
    }

    /// Scrolls by whole lines, for measuring scroll performance.
    pub fn scroll_lines(&mut self, lines: f32, cx: &mut Context<Self>) {
        let line_height = self.geometry.as_ref().map_or(px(20.), |g| g.line_height);
        self.scroll.y = (self.scroll.y + line_height * lines).max(px(0.));
        cx.notify();
    }

    pub fn start_measuring(&mut self) {
        self.frame_times = Some(Vec::new());
    }

    pub fn take_frame_times(&mut self) -> Vec<Duration> {
        self.frame_times.take().unwrap_or_default()
    }

    fn on_buffer_event(
        &mut self,
        _: Entity<BufferModel>,
        event: &BufferEvent,
        cx: &mut Context<Self>,
    ) {
        if let BufferEvent::Edited { edits, origin, .. } = event {
            // The view that made the edit has already placed its selections.
            if *origin != Some(cx.entity_id()) {
                self.selections.transform(edits);
                self.marked = None;
                self.expand_stack.clear();
            }
            self.annotations_edited(edits, cx);
            self.gutter_text_edited(edits);
            if let Some(scope) = self.find.as_mut().and_then(|f| f.scope.as_mut()) {
                scope.transform(edits);
            }
            self.refresh_matches(cx);
            cx.notify();
        }
    }

    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        cx.notify();
        self.blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CURSOR_BLINK).await;
                let alive = this.update(cx, |this, cx| {
                    this.cursor_visible = !this.cursor_visible;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// Anything that moves the cursor: keep it visible, make it solid, and end the undo group so
    /// that typing here doesn't merge with typing somewhere else.
    pub(crate) fn did_move(&mut self, cx: &mut Context<Self>) {
        self.clear_suggestion(cx);
        self.semantics.completion = None;
        self.semantics.hover = None;
        self.check_signature(cx);
        self.request_usage_highlights(cx);
        self.autoscroll = true;
        self.buffer
            .update(cx, |buffer, _| buffer.break_undo_group());
        if self.blink.is_some() {
            self.restart_blink(cx);
        }
        cx.notify();
    }

    fn motion(&mut self, motion: Motion, extend: bool, cx: &mut Context<Self>) {
        let model = self.buffer.read(cx);
        let tab_size = model.edit_config().tab_size;
        ops::move_selections(
            model.buffer(),
            &mut self.selections,
            motion,
            extend,
            tab_size,
        );
        self.expand_stack.clear();
        self.did_move(cx);
    }

    fn page_lines(&self) -> usize {
        self.geometry.as_ref().map_or(30, |g| g.page_lines.max(1))
    }

    fn page(&mut self, up: bool, extend: bool, cx: &mut Context<Self>) {
        let lines = self.page_lines();
        let line_height = self.geometry.as_ref().map_or(px(20.), |g| g.line_height);
        self.motion(
            if up {
                Motion::Up(lines)
            } else {
                Motion::Down(lines)
            },
            extend,
            cx,
        );
        let delta = line_height * lines as f32;
        self.scroll.y = if up {
            (self.scroll.y - delta).max(px(0.))
        } else {
            self.scroll.y + delta
        };
    }

    /// Runs an editing command through the buffer.
    fn command<R>(
        &mut self,
        cx: &mut Context<Self>,
        command: impl FnOnce(&mut EditContext) -> (R, Vec<AppliedEdit>),
    ) -> R {
        let origin = cx.entity_id();
        let selections = &mut self.selections;
        let result = self.buffer.update(cx, |buffer, cx| {
            buffer.edit(selections, origin, cx, command)
        });
        self.expand_stack.clear();
        self.autoscroll = true;
        if self.blink.is_some() {
            self.restart_blink(cx);
        }
        cx.notify();
        result
    }

    pub(crate) fn edit(
        &mut self,
        cx: &mut Context<Self>,
        command: impl FnOnce(&mut EditContext) -> Vec<AppliedEdit>,
    ) {
        self.command(cx, |edit| ((), command(edit)));
    }

    fn key_context(&self, cx: &App) -> gpui::KeyContext {
        let mut context = gpui::KeyContext::new_with_defaults(cx.platform_info().os);
        context.add("Editor");
        if self.semantics.completion.is_some() {
            context.add("showing_completions");
        }
        if self.has_popups() {
            context.add("showing_popups");
        }
        if self.suggestion.current.is_some() {
            context.add("has_suggestion");
        }
        if self.caret_gesture.active {
            context.add("cloning_carets");
        }
        context
    }

    /// Applies edits from a completion, placing the cursor, as one undo step.
    pub(crate) fn edit_for_semantics(
        &mut self,
        edits: Vec<gleipnir_text::TextEdit>,
        cursor: usize,
        cx: &mut Context<Self>,
    ) {
        self.edit(cx, |edit| {
            edit.apply_edits(edits, Some(cursor), gleipnir_text::UndoGrouping::Distinct)
        });
    }

    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let origin = cx.entity_id();
        let restored = self.buffer.update(cx, |buffer, cx| buffer.undo(origin, cx));
        if let Some(selections) = restored {
            self.selections = selections;
            self.did_move(cx);
        }
    }

    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let origin = cx.entity_id();
        let restored = self.buffer.update(cx, |buffer, cx| buffer.redo(origin, cx));
        if let Some(selections) = restored {
            self.selections = selections;
            self.did_move(cx);
        }
    }

    fn write_clipboard(text: String, linewise: bool, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
            text,
            ClipboardMetadata { linewise },
        ));
    }

    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let (text, linewise) = ops::copy_text(self.buffer.read(cx).buffer(), &self.selections);
        Self::write_clipboard(text, linewise, cx);
    }

    fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let (text, linewise) = self.command(cx, |edit| {
            let (text, linewise, applied) = edit.cut();
            ((text, linewise), applied)
        });
        Self::write_clipboard(text, linewise, cx);
    }

    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else { return };
        let linewise = item
            .entries()
            .iter()
            .find_map(|entry| match entry {
                ClipboardEntry::String(s) => s.metadata_json::<ClipboardMetadata>(),
                _ => None,
            })
            .is_some_and(|m| m.linewise);
        // Other applications may put CRLF on the clipboard, and the buffer is always LF.
        let text = text.replace("\r\n", "\n");
        self.edit(cx, |edit| edit.paste(&text, linewise));
    }

    fn on_extend_selection(&mut self, _: &ExtendSelection, _: &mut Window, cx: &mut Context<Self>) {
        let model = self.buffer.read(cx);
        let buffer = model.buffer();
        let syntax = model.syntax();
        let before = self.selections.clone();
        let mut next = before.clone();
        next.update(|selection| {
            // A bare cursor goes to its word first, as that's nearly always what's wanted, and is
            // more useful than the single-character node a tree-sitter lookup might find.
            if selection.is_empty()
                && let Some(word) = buffer.word_at(selection.head)
            {
                return Selection::new(word.start, word.end);
            }
            let wider = syntax
                .and_then(|s| s.expand_selection(selection.range()))
                .unwrap_or(0..buffer.len());
            Selection::new(wider.start, wider.end)
        });
        if next != before {
            self.expand_stack.push(before);
            self.selections = next;
            cx.notify();
        }
    }

    fn on_shrink_selection(&mut self, _: &ShrinkSelection, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(previous) = self.expand_stack.pop() {
            self.selections = previous;
            cx.notify();
        }
    }

    fn add_cursor(&mut self, up: bool, cx: &mut Context<Self>) {
        let model = self.buffer.read(cx);
        let tab_size = model.edit_config().tab_size;
        ops::add_cursor_vertically(model.buffer(), &mut self.selections, up, tab_size);
        self.did_move(cx);
    }

    fn on_add_next_occurrence(
        &mut self,
        _: &AddNextOccurrence,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selections.has_non_empty() {
            self.occurrence_whole_word = true;
        }
        let whole_word = self.occurrence_whole_word;
        ops::add_next_occurrence(
            self.buffer.read(cx).buffer(),
            &mut self.selections,
            whole_word,
        );
        self.did_move(cx);
    }

    fn on_remove_last_occurrence(
        &mut self,
        _: &RemoveLastOccurrence,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selections.remove_primary();
        self.did_move(cx);
    }

    fn on_select_all_occurrences(
        &mut self,
        _: &SelectAllOccurrences,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let whole_word = self.occurrence_whole_word || !self.selections.has_non_empty();
        ops::select_all_occurrences(
            self.buffer.read(cx).buffer(),
            &mut self.selections,
            whole_word,
        );
        self.did_move(cx);
    }

    /// Escape: back to one cursor, then to no selection.
    fn on_collapse(&mut self, _: &CollapseSelections, _: &mut Window, cx: &mut Context<Self>) {
        if self.close_popups(cx) {
            return;
        }
        if self.selections.len() > 1 {
            self.selections.collapse_to_primary();
        } else if self.selections.has_non_empty() {
            self.selections = SelectionSet::cursor(self.selections.primary().head);
        } else {
            cx.propagate();
            return;
        }
        self.occurrence_whole_word = false;
        self.did_move(cx);
    }

    fn on_save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let model = self.buffer.read(cx);
        if model.path().is_none() {
            cx.emit(EditorEvent::SaveAsRequested);
            return;
        }
        if model.disk_status() == DiskStatus::Conflict {
            cx.emit(EditorEvent::SaveConflict);
            return;
        }
        let save = self.buffer.update(cx, |buffer, cx| buffer.save(cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = save.await {
                this.update(cx, |_, cx| {
                    cx.emit(EditorEvent::SaveFailed(format!("{error:#}")))
                })
                .ok();
            }
        })
        .detach();
    }

    /// Handles a mouse press in the text area.
    pub(crate) fn mouse_down(
        &mut self,
        offset: usize,
        click_count: usize,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.expand_stack.clear();
        self.occurrence_whole_word = false;
        let model = self.buffer.read(cx);
        let buffer = model.buffer();
        let tab_size = model.edit_config().tab_size;

        if modifiers.secondary(cx) && click_count == 1 && !modifiers.shift && !modifiers.alt {
            self.selections = SelectionSet::cursor(offset);
            self.did_move(cx);
            self.link_clicked(offset, cx);
            return;
        }
        if modifiers.alt && modifiers.shift {
            // Add a cursor, as a drag from here can select more alongside what's there.
            let base = self.selections.clone();
            self.selections.push(Selection::cursor(offset));
            self.drag = Some(Drag::Adding {
                base,
                anchor: offset,
            });
        } else if modifiers.alt {
            let point = buffer.offset_to_point(offset);
            let line = buffer.line(point.line);
            let column = gleipnir_text::display_column(&line, point.col, tab_size);
            self.drag = Some(Drag::Column {
                anchor: (point.line, column),
            });
            self.selections = SelectionSet::cursor(offset);
        } else if modifiers.shift {
            let anchor = self.selections.primary().anchor;
            self.selections = SelectionSet::single(Selection::new(anchor, offset));
            self.drag = Some(Drag::Chars { anchor });
        } else {
            match click_count {
                2 => {
                    let word = buffer.word_at(offset).unwrap_or(offset..offset);
                    self.selections = SelectionSet::single(Selection::new(word.start, word.end));
                    self.drag = Some(Drag::Units {
                        origin: word,
                        lines: false,
                    });
                }
                3.. => {
                    let line = ops::line_range_at(buffer, offset);
                    self.selections = SelectionSet::single(Selection::new(line.start, line.end));
                    self.drag = Some(Drag::Units {
                        origin: line,
                        lines: true,
                    });
                }
                _ => {
                    self.selections = SelectionSet::cursor(offset);
                    self.drag = Some(Drag::Chars { anchor: offset });
                }
            }
        }
        self.did_move(cx);
    }

    /// A click in the line number gutter selects that whole line.
    pub(crate) fn gutter_mouse_down(
        &mut self,
        line: usize,
        zone: crate::decorations::GutterZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        if let Some(decorations) = &self.decorations {
            let line = line.min(self.buffer.read(cx).buffer().line_count().saturating_sub(1));
            let run = zone == crate::decorations::GutterZone::Run
                && decorations.run_lines(cx).contains(&line);
            cx.emit(if run {
                EditorEvent::RunClicked { line }
            } else {
                EditorEvent::GutterClicked { line }
            });
            return;
        }
        let buffer = self.buffer.read(cx).buffer();
        let range = ops::line_range_at(buffer, buffer.line_start(line));
        self.selections = SelectionSet::single(Selection::new(range.start, range.end));
        self.drag = Some(Drag::Units {
            origin: range,
            lines: true,
        });
        self.did_move(cx);
    }

    /// Handles the mouse moving during a drag, with the position under it.
    pub(crate) fn mouse_dragged(
        &mut self,
        offset: usize,
        display: (usize, usize),
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.drag.clone() else {
            return;
        };
        let model = self.buffer.read(cx);
        let buffer = model.buffer();
        let tab_size = model.edit_config().tab_size;
        self.selections = match drag {
            Drag::Chars { anchor } => SelectionSet::single(Selection::new(anchor, offset)),
            Drag::Units { origin, lines } => {
                let unit = if lines {
                    ops::line_range_at(buffer, offset)
                } else {
                    buffer.word_at(offset).unwrap_or(offset..offset)
                };
                if unit.start < origin.start {
                    SelectionSet::single(Selection::new(origin.end, unit.start))
                } else {
                    SelectionSet::single(Selection::new(origin.start, unit.end.max(origin.end)))
                }
            }
            Drag::Column { anchor } => ops::column_selection(buffer, anchor, display, tab_size),
            Drag::Adding { base, anchor } => {
                let mut selections = base;
                selections.push(Selection::new(anchor, offset));
                selections
            }
        };
        self.autoscroll = true;
        cx.notify();
    }

    pub(crate) fn mouse_up(&mut self) {
        self.drag = None;
    }

    pub(crate) fn scroll_wheel(
        &mut self,
        delta: ScrollDelta,
        line_height: Pixels,
        cx: &mut Context<Self>,
    ) {
        let delta = delta.pixel_delta(line_height);
        self.scroll.x = (self.scroll.x - delta.x).max(px(0.));
        self.scroll.y = (self.scroll.y - delta.y).max(px(0.));
        self.autoscroll = false;
        cx.notify();
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let buffer = self.buffer.read(cx).buffer();
        let start = buffer.doc_utf16_to_offset(range_utf16.start);
        let end = buffer.doc_utf16_to_offset(range_utf16.end);
        actual_range.replace(buffer.offset_to_doc_utf16(start)..buffer.offset_to_doc_utf16(end));
        Some(buffer.slice(start..end))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let buffer = self.buffer.read(cx).buffer();
        let primary = self.selections.primary();
        let range = primary.range();
        Some(UTF16Selection {
            range: buffer.offset_to_doc_utf16(range.start)..buffer.offset_to_doc_utf16(range.end),
            reversed: primary.is_reversed(),
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.buffer.read(cx).buffer();
        self.selections = SelectionSet::single(Selection::new(
            buffer.doc_utf16_to_offset(range_utf16.start),
            buffer.doc_utf16_to_offset(range_utf16.end),
        ));
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        Some(self.buffer.read(cx).buffer().len_utf16())
    }

    fn marked_text_range(&self, _: &mut Window, cx: &mut Context<Self>) -> Option<Range<usize>> {
        let buffer = self.buffer.read(cx).buffer();
        self.marked
            .as_ref()
            .map(|r| buffer.offset_to_doc_utf16(r.start)..buffer.offset_to_doc_utf16(r.end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    /// Text input from the platform: ordinary typing and IME commits both arrive here.
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Committing a composition, or the IME replacing a specific range, applies at the primary
        // cursor only. Otherwise this is typing, which goes to every cursor.
        let target = {
            let buffer = self.buffer.read(cx).buffer();
            range_utf16
                .map(|r| buffer.doc_utf16_to_offset(r.start)..buffer.doc_utf16_to_offset(r.end))
                .or_else(|| self.marked.clone())
        };
        self.marked = None;
        if let Some(range) = target {
            self.selections = SelectionSet::single(Selection::new(range.start, range.end));
        }

        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => {
                self.edit(cx, |edit| edit.type_char(c));
                self.after_typing(c, cx);
            }
            (None, _) => {}
            _ => self.edit(cx, |edit| {
                edit.insert(text, gleipnir_text::UndoGrouping::Typing)
            }),
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (range, base_utf16) = {
            let buffer = self.buffer.read(cx).buffer();
            let range = range_utf16
                .map(|r| buffer.doc_utf16_to_offset(r.start)..buffer.doc_utf16_to_offset(r.end))
                .or_else(|| self.marked.clone())
                .unwrap_or_else(|| self.selections.primary().range());
            let base = buffer.offset_to_doc_utf16(range.start);
            (range, base)
        };
        // Composition happens at one place: other cursors would each need their own IME state.
        self.selections = SelectionSet::single(Selection::new(range.start, range.end));
        self.edit(cx, |edit| {
            edit.insert(text, gleipnir_text::UndoGrouping::Typing)
        });
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        if let Some(selected) = selected_utf16 {
            let buffer = self.buffer.read(cx).buffer();
            self.selections = SelectionSet::single(Selection::new(
                buffer.doc_utf16_to_offset(base_utf16 + selected.start),
                buffer.doc_utf16_to_offset(base_utf16 + selected.end),
            ));
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let buffer = self.buffer.read(cx).buffer();
        let start = buffer.doc_utf16_to_offset(range_utf16.start);
        self.geometry.as_ref()?.bounds_for_offset(start, buffer)
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let buffer = self.buffer.read(cx).buffer();
        let offset = self.geometry.as_ref()?.offset_at(position, buffer);
        Some(buffer.offset_to_doc_utf16(offset))
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let popups = self.render_popups(window, cx);
        let tooltip = self.render_gutter_tooltip(cx);
        macro_rules! motions {
            ($el:expr, $($action:ident => ($motion:expr, $extend:expr)),* $(,)?) => {
                $el$(.on_action(cx.listener(|this, _: &$action, _, cx| {
                    this.motion($motion, $extend, cx)
                })))*
            };
        }
        macro_rules! edits {
            ($el:expr, $($action:ident => $method:ident $(($($arg:expr),*))?),* $(,)?) => {
                $el$(.on_action(cx.listener(|this, _: &$action, _, cx| {
                    this.edit(cx, |edit| edit.$method($($($arg),*)?));
                    this.after_deleting(cx);
                })))*
            };
        }

        let root = div()
            .key_context(self.key_context(cx))
            .track_focus(&self.focus_handle)
            .size_full()
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui::ModifiersChangedEvent, _, cx| {
                    let (ours, only) = crate::caret_gesture::gesture_modifier(&event.modifiers);
                    if this
                        .caret_gesture
                        .modifiers_changed(ours, only, std::time::Instant::now())
                    {
                        cx.notify();
                    }
                },
            ))
            .capture_key_down(
                cx.listener(|this, _: &gpui::KeyDownEvent, _, _| this.caret_gesture.key_pressed()),
            )
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                let len = this.buffer.read(cx).buffer().len();
                this.selections = SelectionSet::single(Selection::new(0, len));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(true, false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(false, false, cx)))
            .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| this.page(true, true, cx)))
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| this.page(false, true, cx)))
            .on_action(cx.listener(|this, _: &AddCursorAbove, _, cx| this.add_cursor(true, cx)))
            .on_action(cx.listener(|this, _: &AddCursorBelow, _, cx| this.add_cursor(false, cx)))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_extend_selection))
            .on_action(cx.listener(Self::on_shrink_selection))
            .on_action(cx.listener(Self::on_add_next_occurrence))
            .on_action(cx.listener(Self::on_remove_last_occurrence))
            .on_action(cx.listener(Self::on_select_all_occurrences))
            .on_action(cx.listener(Self::on_collapse))
            .on_action(cx.listener(Self::on_save))
            .on_action(
                cx.listener(|this, _: &ShowCompletions, _, cx| this.request_completions(None, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ConfirmCompletion, _, cx| this.confirm_completion(cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectNextCompletion, _, cx| this.move_completion(1, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPreviousCompletion, _, cx| {
                    this.move_completion(-1, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SelectNextCompletionPage, _, cx| {
                    this.move_completion(8, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SelectPreviousCompletionPage, _, cx| {
                    this.move_completion(-8, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &AcceptSuggestion, _, cx| {
                this.accept_suggestion(cx);
            }))
            .on_action(cx.listener(|this, _: &AcceptSuggestionWord, _, cx| {
                this.accept_suggestion_part(crate::suggestion::SuggestionPart::Word, cx);
            }))
            .on_action(cx.listener(|this, _: &AcceptSuggestionLine, _, cx| {
                this.accept_suggestion_part(crate::suggestion::SuggestionPart::Line, cx);
            }))
            .on_action(cx.listener(|this, _: &DismissPopups, _, cx| {
                this.close_popups(cx);
            }))
            .on_action(cx.listener(|this, _: &ShowHover, _, cx| {
                let head = this.selections.primary().head;
                this.request_hover(head, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowSignatureHelp, _, cx| this.request_signature(cx)))
            .on_action(cx.listener(|this, _: &NextProblem, _, cx| this.go_to_problem(true, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousProblem, _, cx| this.go_to_problem(false, cx)),
            );

        let root = motions!(root,
            MoveLeft => (Motion::Left, false),
            MoveRight => (Motion::Right, false),
            MoveUp => (Motion::Up(1), false),
            MoveDown => (Motion::Down(1), false),
            MoveWordLeft => (Motion::WordLeft, false),
            MoveWordRight => (Motion::WordRight, false),
            MoveLineStart => (Motion::LineStart, false),
            MoveLineEnd => (Motion::LineEnd, false),
            MoveDocStart => (Motion::DocStart, false),
            MoveDocEnd => (Motion::DocEnd, false),
            SelectLeft => (Motion::Left, true),
            SelectRight => (Motion::Right, true),
            SelectUp => (Motion::Up(1), true),
            SelectDown => (Motion::Down(1), true),
            SelectWordLeft => (Motion::WordLeft, true),
            SelectWordRight => (Motion::WordRight, true),
            SelectLineStart => (Motion::LineStart, true),
            SelectLineEnd => (Motion::LineEnd, true),
            SelectDocStart => (Motion::DocStart, true),
            SelectDocEnd => (Motion::DocEnd, true),
        );
        let root = edits!(root,
            Backspace => backspace,
            Delete => delete_forward,
            DeleteWordLeft => delete_word_left,
            DeleteWordRight => delete_word_right,
            DeleteLine => delete_lines,
            Newline => newline,
            Indent => indent,
            Outdent => outdent,
            DuplicateLine => duplicate,
            MoveLinesUp => move_lines(true),
            MoveLinesDown => move_lines(false),
            ToggleComment => toggle_line_comment,
        );
        let root = self.on_find_actions(root, cx);
        // Released outside the window, a drag still has to end.
        let editor = root
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.mouse_up()),
            )
            .child(EditorElement::new(cx.entity()))
            .children(popups)
            .children(tooltip);
        // The find bar sits above the text, outside the editor's keys, so typing in it doesn't
        // edit the file.
        match self.render_find_bar(cx) {
            Some(bar) => div()
                .size_full()
                .flex()
                .flex_col()
                .child(bar)
                .child(div().flex_1().min_h_0().child(editor))
                .into_any_element(),
            None => editor.into_any_element(),
        }
    }
}
