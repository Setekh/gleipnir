//! A single-line text field, with the same selection and motion behaviour as the editor because
//! it runs on the same commands.

use crate::theme::Theme;
use gleipnir_text::ops::{EditConfig, EditContext, Motion, copy_text, move_selections};
use gleipnir_text::{Buffer, Selection, SelectionSet, UndoGrouping};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ShapedLine, SharedString, Style, TextRun, UTF16Selection, Window,
    actions, div, fill, point, prelude::*, px, relative, size,
};
use std::ops::Range;
use std::time::Instant;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        Left,
        Right,
        WordLeft,
        WordRight,
        Home,
        End,
        SelectLeft,
        SelectRight,
        SelectWordLeft,
        SelectWordRight,
        SelectHome,
        SelectEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
    ]
);

/// Registers the field's key bindings. Enter and Escape are left for the owner to handle, as what
/// they mean depends on where the field is.
pub fn bind_keys(cx: &mut App) {
    let context = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-home", SelectHome, context),
        KeyBinding::new("shift-end", SelectEnd, context),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("alt-backspace", DeleteWordLeft, context),
        KeyBinding::new("alt-left", WordLeft, context),
        KeyBinding::new("alt-right", WordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("cmd-left", Home, context),
        KeyBinding::new("cmd-right", End, context),
        KeyBinding::new("cmd-shift-left", SelectHome, context),
        KeyBinding::new("cmd-shift-right", SelectEnd, context),
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, context),
        KeyBinding::new("ctrl-left", WordLeft, context),
        KeyBinding::new("ctrl-right", WordRight, context),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, context),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, context),
        KeyBinding::new("ctrl-a", SelectAll, context),
        KeyBinding::new("ctrl-c", Copy, context),
        KeyBinding::new("ctrl-x", Cut, context),
        KeyBinding::new("ctrl-v", Paste, context),
        KeyBinding::new("ctrl-z", Undo, context),
        KeyBinding::new("ctrl-shift-z", Redo, context),
    ]);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputEvent {
    Changed,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    buffer: Buffer,
    selections: SelectionSet,
    config: EditConfig,
    placeholder: SharedString,
    marked: Option<Range<usize>>,
    selecting: bool,
    last_line: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: Buffer::new(""),
            selections: SelectionSet::cursor(0),
            // Nothing auto-closes in a search field: typing `(` usually means looking for one.
            config: EditConfig {
                pairs: Vec::new(),
                ..EditConfig::default()
            },
            placeholder: placeholder.into(),
            marked: None,
            selecting: false,
            last_line: None,
            last_bounds: None,
        }
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    /// Replaces the contents, for pre-filling a field, with everything selected so that typing
    /// starts over.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let line = text.replace('\n', " ");
        self.buffer = Buffer::new(&line);
        self.selections = SelectionSet::single(Selection::new(0, self.buffer.len()));
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>, command: impl FnOnce(&mut EditContext)) {
        let before = self.buffer.version();
        let mut edit_cx = EditContext {
            buffer: &mut self.buffer,
            selections: &mut self.selections,
            config: &self.config,
            now: Instant::now(),
        };
        command(&mut edit_cx);
        if self.buffer.version() != before {
            cx.emit(TextInputEvent::Changed);
        }
        cx.notify();
    }

    fn motion(&mut self, motion: Motion, extend: bool, cx: &mut Context<Self>) {
        move_selections(&self.buffer, &mut self.selections, motion, extend, 4);
        self.buffer.break_undo_group();
        cx.notify();
    }

    fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.run(cx, |e| drop(e.backspace()));
    }
    fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.run(cx, |e| drop(e.delete_forward()));
    }
    fn on_delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.run(cx, |e| drop(e.delete_word_left()));
    }
    fn on_left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Left, false, cx);
    }
    fn on_right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Right, false, cx);
    }
    fn on_word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::WordLeft, false, cx);
    }
    fn on_word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::WordRight, false, cx);
    }
    fn on_home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocStart, false, cx);
    }
    fn on_end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocEnd, false, cx);
    }
    fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Left, true, cx);
    }
    fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::Right, true, cx);
    }
    fn on_select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::WordLeft, true, cx);
    }
    fn on_select_word_right(
        &mut self,
        _: &SelectWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.motion(Motion::WordRight, true, cx);
    }
    fn on_select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocStart, true, cx);
    }
    fn on_select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.motion(Motion::DocEnd, true, cx);
    }
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selections = SelectionSet::single(Selection::new(0, self.buffer.len()));
        cx.notify();
    }
    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.selections.has_non_empty() {
            let (text, _) = copy_text(&self.buffer, &self.selections);
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
    fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.selections.has_non_empty() {
            let (text, _) = copy_text(&self.buffer, &self.selections);
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.run(cx, |e| drop(e.backspace()));
        }
    }
    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let line = text.trim_end_matches('\n').replace('\n', " ");
            self.run(cx, |e| drop(e.insert(&line, UndoGrouping::Distinct)));
        }
    }
    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, selections)) = self.buffer.undo() {
            self.selections = selections;
            cx.emit(TextInputEvent::Changed);
            cx.notify();
        }
    }
    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, selections)) = self.buffer.redo() {
            self.selections = selections;
            cx.emit(TextInputEvent::Changed);
            cx.notify();
        }
    }

    fn offset_at(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (&self.last_bounds, &self.last_line) else {
            return 0;
        };
        if self.buffer.is_empty() {
            return 0;
        }
        self.buffer
            .clip(line.closest_index_for_x(position.x - bounds.left()))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let offset = self.offset_at(event.position);
        self.selecting = true;
        let anchor = if event.modifiers.shift {
            self.selections.primary().anchor
        } else {
            offset
        };
        self.selections = SelectionSet::single(Selection::new(anchor, offset));
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            let anchor = self.selections.primary().anchor;
            self.selections =
                SelectionSet::single(Selection::new(anchor, self.offset_at(event.position)));
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = self.buffer.doc_utf16_to_offset(range_utf16.start);
        let end = self.buffer.doc_utf16_to_offset(range_utf16.end);
        actual_range
            .replace(self.buffer.offset_to_doc_utf16(start)..self.buffer.offset_to_doc_utf16(end));
        Some(self.buffer.slice(start..end))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.selections.primary();
        let range = selection.range();
        Some(UTF16Selection {
            range: self.buffer.offset_to_doc_utf16(range.start)
                ..self.buffer.offset_to_doc_utf16(range.end),
            reversed: selection.is_reversed(),
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selections = SelectionSet::single(Selection::new(
            self.buffer.doc_utf16_to_offset(range_utf16.start),
            self.buffer.doc_utf16_to_offset(range_utf16.end),
        ));
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.buffer.len_utf16())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| {
            self.buffer.offset_to_doc_utf16(r.start)..self.buffer.offset_to_doc_utf16(r.end)
        })
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|r| {
                self.buffer.doc_utf16_to_offset(r.start)..self.buffer.doc_utf16_to_offset(r.end)
            })
            .or(self.marked.take());
        if let Some(range) = range {
            self.selections = SelectionSet::single(Selection::new(range.start, range.end));
        }
        self.marked = None;
        let line = new_text.replace('\n', " ");
        self.run(cx, |e| drop(e.insert(&line, UndoGrouping::Typing)));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|r| {
                self.buffer.doc_utf16_to_offset(r.start)..self.buffer.doc_utf16_to_offset(r.end)
            })
            .or(self.marked.clone())
            .unwrap_or_else(|| self.selections.primary().range());
        self.selections = SelectionSet::single(Selection::new(range.start, range.end));
        self.run(cx, |e| drop(e.insert(new_text, UndoGrouping::Typing)));
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        if let Some(selected) = new_selected_range_utf16 {
            let base = self.buffer.offset_to_doc_utf16(range.start);
            self.selections = SelectionSet::single(Selection::new(
                self.buffer.doc_utf16_to_offset(base + selected.start),
                self.buffer.doc_utf16_to_offset(base + selected.end),
            ));
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_line.as_ref()?;
        let start = self.buffer.doc_utf16_to_offset(range_utf16.start);
        let end = self.buffer.doc_utf16_to_offset(range_utf16.end);
        Some(Bounds::from_corners(
            point(
                element_bounds.left() + line.x_for_index(start),
                element_bounds.top(),
            ),
            point(
                element_bounds.left() + line.x_for_index(end),
                element_bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.buffer.offset_to_doc_utf16(self.offset_at(position)))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .w_full()
            .on_action(cx.listener(Self::on_backspace))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_delete_word_left))
            .on_action(cx.listener(Self::on_left))
            .on_action(cx.listener(Self::on_right))
            .on_action(cx.listener(Self::on_word_left))
            .on_action(cx.listener(Self::on_word_right))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_select_left))
            .on_action(cx.listener(Self::on_select_right))
            .on_action(cx.listener(Self::on_select_word_left))
            .on_action(cx.listener(Self::on_select_word_right))
            .on_action(cx.listener(Self::on_select_home))
            .on_action(cx.listener(Self::on_select_end))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(InputElement { input: cx.entity() })
    }
}

struct InputElement {
    input: Entity<TextInput>,
}

struct InputPrepaint {
    line: ShapedLine,
    selection: Option<Bounds<Pixels>>,
    cursor: Option<Bounds<Pixels>>,
}

impl IntoElement for InputElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for InputElement {
    type RequestLayoutState = ();
    type PrepaintState = InputPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> InputPrepaint {
        let theme = cx.global::<Theme>();
        let input = self.input.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let text = input.buffer.text();
        let (display, color): (SharedString, _) = if text.is_empty() {
            (input.placeholder.clone(), theme.text_faint)
        } else {
            (text.into(), style.color)
        };
        let run = TextRun {
            len: display.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(display, font_size, &[run], None);

        let primary = input.selections.primary();
        let range = primary.range();
        let height = bounds.size.height;
        // With only a placeholder showing, the offsets don't refer to the displayed text.
        let (start_x, end_x, head_x) = if input.buffer.is_empty() {
            (px(0.), px(0.), px(0.))
        } else {
            (
                line.x_for_index(range.start),
                line.x_for_index(range.end),
                line.x_for_index(primary.head),
            )
        };
        InputPrepaint {
            selection: (!primary.is_empty()).then(|| {
                Bounds::new(
                    point(bounds.left() + start_x, bounds.top()),
                    size(end_x - start_x, height),
                )
            }),
            cursor: primary.is_empty().then(|| {
                Bounds::new(
                    point(bounds.left() + head_x, bounds.top()),
                    size(px(1.5), height),
                )
            }),
            line,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut InputPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        let theme = cx.global::<Theme>().clone();
        if let Some(selection) = prepaint.selection {
            window.paint_quad(fill(selection, theme.editor_selection));
        }
        prepaint
            .line
            .paint(
                bounds.origin,
                window.line_height(),
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor
        {
            window.paint_quad(fill(cursor, theme.editor_cursor));
        }
        let line = prepaint.line.clone();
        self.input.update(cx, |input, _| {
            input.last_line = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    struct Host {
        input: Entity<TextInput>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.input.clone())
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<TextInput>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::theme::init_theme(cx);
            bind_keys(cx);
        });
        let (host, cx) = cx.add_window_view(|_, cx| Host {
            input: cx.new(|cx| TextInput::new("search", cx)),
        });
        let input = host.read_with(cx, |host, _| host.input.clone());
        cx.update(|window, cx| {
            let focus_handle = input.read(cx).focus_handle.clone();
            focus_handle.focus(window, cx);
        });
        (input, cx)
    }

    #[gpui::test]
    fn typing_inserts_text_and_reports_changes(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx);
        let changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = changes.clone();
        cx.update(|_, cx| {
            cx.subscribe(&input, move |_, _: &TextInputEvent, _| {
                counter.set(counter.get() + 1)
            })
            .detach();
        });
        cx.simulate_input("héllo");
        input.read_with(cx, |input, _| assert_eq!(input.text(), "héllo"));
        assert_eq!(changes.get(), 5);
    }

    #[gpui::test]
    fn brackets_do_not_auto_close_in_a_field(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx);
        cx.simulate_input("(");
        input.read_with(cx, |input, _| assert_eq!(input.text(), "("));
    }

    #[gpui::test]
    fn editing_keys_work(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx);
        cx.simulate_input("abc");
        cx.simulate_keystrokes("left backspace");
        input.read_with(cx, |input, _| assert_eq!(input.text(), "ac"));
        cx.simulate_keystrokes("home delete");
        input.read_with(cx, |input, _| assert_eq!(input.text(), "c"));
    }

    #[gpui::test]
    fn selecting_then_typing_replaces(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx);
        cx.simulate_input("abc");
        cx.simulate_keystrokes("shift-left shift-left");
        cx.simulate_input("x");
        input.read_with(cx, |input, _| assert_eq!(input.text(), "ax"));
    }

    #[gpui::test]
    fn set_text_selects_everything_so_typing_replaces_it(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx);
        input.update(cx, |input, cx| input.set_text("old\nquery", cx));
        input.read_with(cx, |input, _| assert_eq!(input.text(), "old query"));
        cx.simulate_input("n");
        input.read_with(cx, |input, _| assert_eq!(input.text(), "n"));
    }

    #[gpui::test]
    fn the_field_stays_on_one_line_when_given_newlines(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx);
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "a\nb", window, cx);
            });
        });
        input.read_with(cx, |input, _| assert_eq!(input.text(), "a b"));
    }

    #[gpui::test]
    fn ime_composition_replaces_the_marked_text_on_commit(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx);
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "に", None, window, cx);
                assert_eq!(input.marked_text_range(window, cx), Some(0..1));
                input.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
                input.replace_text_in_range(None, "日本", window, cx);
                assert_eq!(input.text(), "日本");
                assert_eq!(input.marked_text_range(window, cx), None);
            });
        });
    }
}
