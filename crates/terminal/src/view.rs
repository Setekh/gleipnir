//! A terminal on screen: its cells drawn in the editor's font, the cursor, the selection, keys
//! sent to the program, the mouse selecting and Cmd-clicking `path:line` to open it, and the
//! wheel scrolling back through the output.

use crate::keys;
use crate::session::{Session, SessionEvent};
use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Point as GridPoint, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::CursorShape;
use gleipnir_ui::Theme;
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, Font,
    FontStyle, FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    Point, ScrollEvent, SharedString, StrikethroughStyle, Subscription, TextRun, UnderlineStyle,
    Window, actions, canvas, div, fill, outline, point, prelude::*, px, size,
};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

actions!(
    terminal,
    [
        /// Copies the terminal's selected text.
        Copy,
        /// Pastes into the terminal, as the program asked for pastes if it did.
        Paste,
        /// Clears the terminal and what's scrolled off it.
        Clear,
        ScrollPageUp,
        ScrollPageDown,
    ]
);

/// A place in a file mentioned in the output was Cmd-clicked: the file, and a line and column
/// from 0.
pub struct OpenLocation(pub PathBuf, pub usize, pub usize);

/// Where the cells were last drawn, for mapping the mouse to them.
#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    origin: Point<Pixels>,
    cell: gpui::Size<Pixels>,
}

pub struct TerminalView {
    session: Entity<Session>,
    focus_handle: FocusHandle,
    layout: Rc<Cell<Layout>>,
    selecting: bool,
    /// The button the program's been told is held, while it hears the mouse.
    reported: Option<crate::mouse::MouseButton>,
    /// Scrolling short of a whole line so far, as a trackpad gives it, in lines.
    wheel_rest: f32,
    /// The cell the program was last told the pointer moved to; moves within it aren't told.
    moved_to: Option<(usize, usize)>,
    _subscription: Subscription,
}

impl EventEmitter<OpenLocation> for TerminalView {}
impl EventEmitter<SessionEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&session, |_, _, event: &SessionEvent, cx| {
            cx.emit(event.clone());
            cx.notify();
        });
        Self {
            session,
            focus_handle: cx.focus_handle(),
            layout: Default::default(),
            selecting: false,
            reported: None,
            wheel_rest: 0.,
            moved_to: None,
            _subscription: subscription,
        }
    }

    pub fn session(&self) -> &Entity<Session> {
        &self.session
    }

    /// The middle of a cell on screen, where it was last drawn.
    pub fn cell_center(&self, column: usize, row: usize) -> Point<Pixels> {
        let layout = self.layout.get();
        point(
            layout.origin.x + layout.cell.width * (column as f32 + 0.5),
            layout.origin.y + layout.cell.height * (row as f32 + 0.5),
        )
    }

    /// The cell under a window position, as a column and row on screen, and which half of it.
    fn cell_at(&self, position: Point<Pixels>) -> (usize, usize, Side) {
        let layout = self.layout.get();
        let x = ((position.x - layout.origin.x) / layout.cell.width).max(0.);
        let y = ((position.y - layout.origin.y) / layout.cell.height).max(0.);
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        (x as usize, y as usize, side)
    }

    /// Tells the program what the mouse did, if it asked to hear and Shift isn't held, which
    /// keeps the mouse for selecting, as in other terminals. Whether it was told.
    fn report_mouse(
        &mut self,
        action: crate::mouse::MouseAction,
        position: Point<Pixels>,
        modifiers: &gpui::Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let mode = self.session.read(cx).mode();
        if !crate::mouse::wants_mouse(mode) || modifiers.shift {
            return false;
        }
        let (column, row, _) = self.cell_at(position);
        if matches!(action, crate::mouse::MouseAction::Move(_))
            && self.moved_to == Some((column, row))
        {
            return true;
        }
        self.moved_to = Some((column, row));
        let held = crate::mouse::MouseModifiers {
            shift: false,
            alt: modifiers.alt,
            control: modifiers.control,
        };
        if let Some(bytes) = crate::mouse::report(action, column, row, held, mode) {
            self.session.update(cx, |session, _| session.write(bytes));
        }
        true
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let (column, row, side) = self.cell_at(event.position);
        if event.modifiers.secondary(cx) {
            self.open_link_at(column, row, cx);
            return;
        }
        let button = crate::mouse::MouseButton::Left;
        let action = crate::mouse::MouseAction::Press(button);
        if self.report_mouse(action, event.position, &event.modifiers, cx) {
            self.reported = Some(button);
            return;
        }
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.session.update(cx, |session, _| {
            session.start_selection(column, row, side, kind)
        });
        self.selecting = true;
        cx.notify();
    }

    /// Right and middle clicks, which only a program listening to the mouse hears.
    fn other_mouse_down(
        &mut self,
        button: crate::mouse::MouseButton,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let action = crate::mouse::MouseAction::Press(button);
        if self.report_mouse(action, event.position, &event.modifiers, cx) {
            self.reported = Some(button);
        }
    }

    fn mouse_up(&mut self, event: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if let Some(button) = self.reported.take() {
            let action = crate::mouse::MouseAction::Release(button);
            self.report_mouse(action, event.position, &event.modifiers, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            // A program listening hears moves as it asked: while a button's held, or all.
            let action = crate::mouse::MouseAction::Move(self.reported);
            self.report_mouse(action, event.position, &event.modifiers, cx);
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (column, row, side) = self.cell_at(event.position);
        self.session
            .update(cx, |session, _| session.extend_selection(column, row, side));
        cx.notify();
    }

    /// Opens a `path:line` under the mouse, resolved against where the terminal started.
    fn open_link_at(&mut self, column: usize, row: usize, cx: &mut Context<Self>) {
        let (text, cwd) = {
            let session = self.session.read(cx);
            let lines = session.screen_lines();
            let Some(text) = lines.get(row).cloned() else {
                return;
            };
            (text, session.launch().cwd.clone())
        };
        // Columns count characters; links count bytes.
        let byte = text
            .char_indices()
            .nth(column)
            .map_or(text.len(), |(i, _)| i);
        let Some(link) = gleipnir_ui::links::find_links(&text)
            .into_iter()
            .find(|l| l.range.contains(&byte))
        else {
            return;
        };
        if let Some(found) = gleipnir_ui::links::resolve(&link.path, &[cwd]) {
            cx.emit(OpenLocation(
                found,
                link.line.saturating_sub(1) as usize,
                link.column.unwrap_or(1).saturating_sub(1) as usize,
            ));
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout.get();
        // A trackpad scrolls a little at a time; it adds up to lines.
        let exact =
            self.wheel_rest + event.delta.pixel_delta(layout.cell.height).y / layout.cell.height;
        let lines = exact.trunc();
        self.wheel_rest = exact - lines;
        if lines == 0. {
            return;
        }
        let mode = self.session.read(cx).mode();
        // A program listening to the mouse, like Claude Code or vim, scrolls itself.
        if crate::mouse::wants_mouse(mode) && !event.modifiers.shift {
            let action = if lines > 0. {
                crate::mouse::MouseAction::WheelUp
            } else {
                crate::mouse::MouseAction::WheelDown
            };
            for _ in 0..lines.abs() as usize {
                self.report_mouse(action, event.position, &event.modifiers, cx);
            }
            return;
        }
        self.session.update(cx, |session, _| {
            // Full-screen programs like `less` scroll themselves, given arrow keys.
            if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
                let key = if lines > 0. { "\x1b[A" } else { "\x1b[B" };
                session.write(key.repeat(lines.abs() as usize).into_bytes());
            } else {
                session.scroll(Scroll::Delta(lines as i32));
            }
        });
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let mode = self.session.read(cx).mode();
        if let Some(bytes) = keys::to_bytes(&event.keystroke, mode) {
            self.session
                .update(cx, |session, _| session.type_bytes(bytes));
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.read(cx).selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.session.update(cx, |session, _| session.paste(&text));
        }
    }
}

/// A run of cells drawn the same way, from a column.
struct Piece {
    column: usize,
    text: String,
    run: TextRun,
}

/// What's drawn of one frame, from the terminal's state.
struct Frame {
    rows: Vec<Vec<Piece>>,
    /// Backgrounds that aren't the terminal's own: row, columns, color.
    backgrounds: Vec<(usize, std::ops::Range<usize>, Hsla)>,
    cursor: Option<(usize, usize, CursorShape, bool)>,
}

fn build_frame(session: &Session, font: &Font, theme: &Theme) -> Frame {
    let term = session.term().lock();
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let columns = term.columns();
    let default_background = theme.editor_background;
    let mut rows: Vec<Vec<Piece>> = (0..term.screen_lines()).map(|_| Vec::new()).collect();
    let mut backgrounds: Vec<(usize, std::ops::Range<usize>, Hsla)> = Vec::new();
    let selection = content.selection;
    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let row = (indexed.point.line.0 + offset) as usize;
        let column = indexed.point.column.0;
        let Some(pieces) = rows.get_mut(row) else {
            continue;
        };
        let mut fg = crate::colors::resolve(cell.fg, content.colors, theme);
        let mut bg = crate::colors::resolve(cell.bg, content.colors, theme);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg.a *= 0.66;
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        let selected = selection.is_some_and(|s| s.contains(indexed.point));
        if selected {
            bg = theme.editor_selection;
        }
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        let width = if wide { 2 } else { 1 };
        if bg != default_background {
            match backgrounds.last_mut() {
                Some((r, range, color)) if *r == row && range.end == column && *color == bg => {
                    range.end = column + width
                }
                _ => backgrounds.push((row, column..column + width, bg)),
            }
        }
        let mut text = String::from(cell.c);
        if let Some(extra) = cell.zerowidth() {
            text.extend(extra);
        }
        let mut cell_font = font.clone();
        if cell.flags.contains(Flags::BOLD) {
            cell_font.weight = FontWeight::BOLD;
        }
        if cell.flags.contains(Flags::ITALIC) {
            cell_font.style = FontStyle::Italic;
        }
        let run = TextRun {
            len: 0,
            font: cell_font,
            color: fg,
            background_color: None,
            underline: cell
                .flags
                .intersects(Flags::ALL_UNDERLINES)
                .then_some(UnderlineStyle {
                    color: Some(fg),
                    thickness: px(1.),
                    wavy: cell.flags.contains(Flags::UNDERCURL),
                }),
            strikethrough: cell
                .flags
                .contains(Flags::STRIKEOUT)
                .then_some(StrikethroughStyle {
                    color: Some(fg),
                    thickness: px(1.),
                }),
        };
        // A wide character gets its own piece, so the ones after it stay on their columns
        // whatever width the font gives it.
        let joins = pieces.last().is_some_and(|last: &Piece| {
            !wide
                && last.run.font == run.font
                && last.run.color == run.color
                && last.run.underline == run.underline
                && last.run.strikethrough == run.strikethrough
                && last.column + last.text.chars().count() == column
                && !last.text.chars().any(unicode_wide)
        });
        if joins {
            pieces.last_mut().unwrap().text.push_str(&text);
        } else {
            pieces.push(Piece { column, text, run });
        }
    }
    // Spaces with nothing under them needn't be drawn.
    for pieces in &mut rows {
        pieces.retain(|p| !p.text.trim().is_empty() || p.run.underline.is_some());
    }
    let cursor = content.cursor;
    let shown = term.mode().contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;
    let cursor_row = cursor.point.line.0 + offset;
    let cursor = (shown && cursor_row >= 0 && (cursor_row as usize) < rows.len()).then(|| {
        let wide = term.grid()[GridPoint::new(cursor.point.line, cursor.point.column)]
            .flags
            .contains(Flags::WIDE_CHAR);
        (
            cursor.point.column.0.min(columns.saturating_sub(1)),
            cursor_row as usize,
            cursor.shape,
            wide,
        )
    });
    Frame {
        rows,
        backgrounds,
        cursor,
    }
}

/// Whether a character takes two cells, as CJK and most emoji do.
fn unicode_wide(c: char) -> bool {
    (c as u32) >= 0x1100
        && matches!(c as u32,
            0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F
            | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD)
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let session = self.session.clone();
        let layout = self.layout.clone();
        let focused = self.focus_handle.is_focused(window);
        let font = gleipnir_editor::editor_font(window, cx);
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.editor_background)
            .px(px(6.))
            .py(px(4.))
            .cursor(gpui::CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &Clear, _, cx| {
                this.session.update(cx, |s, _| s.clear());
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollPageUp, _, cx| {
                this.session.update(cx, |s, _| s.scroll(Scroll::PageUp));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollPageDown, _, cx| {
                this.session.update(cx, |s, _| s.scroll(Scroll::PageDown));
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event, _, cx| {
                    this.other_mouse_down(crate::mouse::MouseButton::Right, event, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event, _, cx| {
                    this.other_mouse_down(crate::mouse::MouseButton::Middle, event, cx)
                }),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let font_size = px(theme.editor_font_size);
                        let line_height = px(theme.editor_line_height);
                        let font_id = window.text_system().resolve_font(&font);
                        let cell_width = window
                            .text_system()
                            .em_advance(font_id, font_size)
                            .unwrap_or(px(8.));
                        let columns = (bounds.size.width / cell_width).floor().max(2.) as u16;
                        let lines = (bounds.size.height / line_height).floor().max(1.) as u16;
                        session.update(cx, |session, _| {
                            session.resize(
                                columns,
                                lines,
                                f32::from(cell_width) as u16,
                                f32::from(line_height) as u16,
                            )
                        });
                        layout.set(Layout {
                            origin: bounds.origin,
                            cell: size(cell_width, line_height),
                        });
                        let frame = build_frame(session.read(cx), &font, &theme);
                        (frame, cell_width, line_height, font_size, theme)
                    },
                    move |bounds,
                          (frame, cell_width, line_height, font_size, theme),
                          window,
                          cx| {
                        let at = |column: usize, row: usize| {
                            point(
                                bounds.origin.x + cell_width * column as f32,
                                bounds.origin.y + line_height * row as f32,
                            )
                        };
                        for (row, columns, color) in &frame.backgrounds {
                            window.paint_quad(fill(
                                Bounds::new(
                                    at(columns.start, *row),
                                    size(cell_width * columns.len() as f32, line_height),
                                ),
                                *color,
                            ));
                        }
                        for (row, pieces) in frame.rows.iter().enumerate() {
                            for piece in pieces {
                                let run = TextRun {
                                    len: piece.text.len(),
                                    ..piece.run.clone()
                                };
                                let shaped = window.text_system().shape_line(
                                    SharedString::from(piece.text.clone()),
                                    font_size,
                                    &[run],
                                    None,
                                );
                                shaped
                                    .paint(
                                        at(piece.column, row),
                                        line_height,
                                        gpui::TextAlign::Left,
                                        None,
                                        window,
                                        cx,
                                    )
                                    .ok();
                            }
                        }
                        if let Some((column, row, shape, wide)) = frame.cursor {
                            let width = cell_width * if wide { 2. } else { 1. };
                            let origin = at(column, row);
                            let cell = Bounds::new(origin, size(width, line_height));
                            let color = theme.editor_cursor;
                            match (focused, shape) {
                                (false, _) | (_, CursorShape::HollowBlock) => window
                                    .paint_quad(outline(cell, color, gpui::BorderStyle::Solid)),
                                (_, CursorShape::Beam) => window.paint_quad(fill(
                                    Bounds::new(origin, size(px(2.), line_height)),
                                    color,
                                )),
                                (_, CursorShape::Underline) => window.paint_quad(fill(
                                    Bounds::new(
                                        point(origin.x, origin.y + line_height - px(2.)),
                                        size(width, px(2.)),
                                    ),
                                    color,
                                )),
                                _ => {
                                    let mut block = color;
                                    block.a *= 0.7;
                                    window.paint_quad(fill(cell, block));
                                }
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}

/// The text a test sees on screen, as lines.
pub fn screen_text(view: &Entity<TerminalView>, cx: &App) -> Vec<String> {
    view.read(cx).session.read(cx).screen_lines()
}
