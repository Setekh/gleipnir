//! Lays out and paints the visible part of the document.

use crate::view::{EditorView, HighlightCache};
use gleipnir_syntax::highlights;
use gleipnir_text::{Buffer, TabMap, byte_col_at_display, display_column};
use gleipnir_ui::Theme;
use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollEvent, ShapedLine, SharedString, Size, Style, TextRun, UnderlineStyle, Window, fill,
    font, point, px, relative, size,
};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Lines longer than this are shown truncated. Shaping costs time per character, and a minified
/// file's one enormous line shouldn't be able to stall every frame.
const MAX_DISPLAY_BYTES: usize = 20_000;
const GUTTER_LEFT_PADDING: f32 = 12.0;
const GUTTER_RIGHT_PADDING: f32 = 16.0;
/// Keeps some context visible around the cursor when autoscrolling.
const SCROLL_MARGIN_LINES: f32 = 3.0;
const SCROLL_MARGIN_COLUMNS: f32 = 4.0;

/// The first installed font from a preference list. Fonts differ by platform, and looking one up
/// that doesn't exist falls back to something proportional, which an editor can't work with.
fn editor_font(window: &Window) -> Font {
    static FAMILY: OnceLock<String> = OnceLock::new();
    let family = FAMILY.get_or_init(|| {
        let available = window.text_system().all_font_names();
        const PREFERRED: &[&str] = &[
            "JetBrains Mono",
            "JetBrainsMono Nerd Font",
            "SF Mono",
            "Menlo",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
        ];
        PREFERRED
            .iter()
            .find(|name| available.iter().any(|a| a == *name))
            .map_or_else(|| "monospace".to_string(), |name| name.to_string())
    });
    font(family.clone())
}

/// One line as laid out on screen.
pub(crate) struct LineGeometry {
    pub shaped: ShapedLine,
    pub tabs: TabMap,
    /// Bytes of the source line that were shaped, less than its length if truncated.
    pub shown_bytes: usize,
}

/// Where everything landed in the last frame, for mapping the mouse and the IME back to text.
pub(crate) struct FrameGeometry {
    pub bounds: Bounds<Pixels>,
    pub text_origin_x: Pixels,
    pub line_height: Pixels,
    pub char_width: Pixels,
    pub scroll: Point<Pixels>,
    pub first_line: usize,
    pub lines: Vec<LineGeometry>,
    pub page_lines: usize,
    pub tab_size: usize,
    pub line_count: usize,
}

impl FrameGeometry {
    fn line_at_y(&self, y: Pixels) -> usize {
        let relative = (y - self.bounds.top() + self.scroll.y) / self.line_height;
        (relative.max(0.) as usize).min(self.line_count.saturating_sub(1))
    }

    fn line_top(&self, line: usize) -> Pixels {
        self.bounds.top() + self.line_height * line as f32 - self.scroll.y
    }

    /// The byte offset nearest a window position.
    pub fn offset_at(&self, position: Point<Pixels>, buffer: &Buffer) -> usize {
        let line = self.line_at_y(position.y);
        let x = (position.x - self.text_origin_x + self.scroll.x).max(px(0.));
        let col = match line
            .checked_sub(self.first_line)
            .and_then(|i| self.lines.get(i))
        {
            Some(laid_out) => {
                let display = gleipnir_ui::closest_index(&laid_out.shaped, x);
                let col = laid_out.tabs.from_display(display);
                // Past a truncated line's shown part, go to its end.
                if col >= laid_out.shown_bytes {
                    buffer.line_len(line)
                } else {
                    col
                }
            }
            None => {
                // Dragged off screen, so the line isn't shaped. Assume the font is monospace.
                let text = buffer.line(line);
                byte_col_at_display(&text, (x / self.char_width).round() as usize, self.tab_size)
            }
        };
        buffer.point_to_offset(gleipnir_text::Point::new(line, col))
    }

    /// The line and display column under a position, which column selection works in.
    pub fn display_position_at(&self, position: Point<Pixels>) -> (usize, usize) {
        let x = (position.x - self.text_origin_x + self.scroll.x).max(px(0.));
        (
            self.line_at_y(position.y),
            (x / self.char_width).round() as usize,
        )
    }

    /// Where a piece of text is on screen, for placing the IME's candidate window.
    pub fn bounds_for_offset(&self, offset: usize, buffer: &Buffer) -> Option<Bounds<Pixels>> {
        let point = buffer.offset_to_point(offset);
        let laid_out = self.lines.get(point.line.checked_sub(self.first_line)?)?;
        let x = laid_out
            .shaped
            .x_for_index(laid_out.tabs.to_display(point.col));
        Some(Bounds::new(
            gpui::point(
                self.text_origin_x + x - self.scroll.x,
                self.line_top(point.line),
            ),
            size(self.char_width, self.line_height),
        ))
    }
}

pub(crate) struct EditorElement {
    view: Entity<EditorView>,
}

impl EditorElement {
    pub fn new(view: Entity<EditorView>) -> Self {
        Self { view }
    }
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub(crate) struct Prepainted {
    geometry: Rc<FrameGeometry>,
    gutter_numbers: Vec<(Pixels, ShapedLine, Pixels)>,
    active_lines: Vec<Pixels>,
    selection_rects: Vec<Bounds<Pixels>>,
    cursors: Vec<Bounds<Pixels>>,
    hitbox: Hitbox,
    gutter_width: Pixels,
    started: Instant,
}

/// Builds the styled runs for one line from the highlights on it, in the expanded text's bytes.
#[allow(clippy::too_many_arguments)]
fn runs_for_line(
    line: &str,
    display_len: usize,
    line_start: usize,
    tabs: &TabMap,
    spans: &[gleipnir_syntax::HighlightSpan],
    marked: Option<(usize, usize)>,
    font: &Font,
    theme: &Theme,
) -> Vec<TextRun> {
    let run = |len: usize, color: Hsla, underlined: bool| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: underlined.then_some(UnderlineStyle {
            color: Some(color),
            thickness: px(1.),
            wavy: false,
        }),
        strikethrough: None,
    };

    // Boundaries in source bytes, which keep color changes and composition marks separate.
    let default = theme.text;
    let mut pieces: Vec<(usize, usize, Hsla)> = Vec::new();
    let mut at = 0;
    for span in spans {
        if span.range.end <= line_start || span.range.start >= line_start + line.len() {
            continue;
        }
        let start = floor_boundary(line, span.range.start.saturating_sub(line_start));
        let end = floor_boundary(line, (span.range.end - line_start).min(line.len()));
        if start < at || end <= start {
            continue;
        }
        if at < start {
            pieces.push((at, start, default));
        }
        pieces.push((start, end, theme.syntax.color(span.kind)));
        at = end;
    }
    if at < line.len() {
        pieces.push((at, line.len(), default));
    }

    let mut runs = Vec::with_capacity(pieces.len());
    let mut display_at = 0;
    for (start, end, color) in pieces {
        let display_end = tabs.to_display(end).min(display_len);
        // Split around any composition mark, which is shown underlined.
        let mut piece_start = start;
        if let Some((mark_start, mark_end)) = marked {
            for boundary in [mark_start, mark_end] {
                if piece_start < boundary && boundary < end {
                    let boundary_display = tabs.to_display(boundary);
                    let underlined = piece_start >= mark_start && boundary <= mark_end;
                    runs.push(run(boundary_display - display_at, color, underlined));
                    display_at = boundary_display;
                    piece_start = boundary;
                }
            }
        }
        if display_end > display_at {
            let underlined = marked.is_some_and(|(s, e)| piece_start >= s && end <= e);
            runs.push(run(display_end - display_at, color, underlined));
            display_at = display_end;
        }
    }
    // An empty run list is invalid for shaping, even with no text.
    if runs.is_empty() {
        runs.push(run(display_len, default, false));
    }
    runs
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

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
        style.size.height = relative(1.).into();
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
    ) -> Prepainted {
        let started = Instant::now();
        let theme = cx.global::<Theme>().clone();
        let font_size = px(theme.editor_font_size);
        let line_height = px(theme.editor_line_height);
        let font = editor_font(window);
        let font_id = window.text_system().resolve_font(&font);
        let char_width = window
            .text_system()
            .em_advance(font_id, font_size)
            .unwrap_or(px(8.));
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        self.view.update(cx, |view, cx| {
            let model = view.buffer.read(cx);
            let buffer = model.buffer();
            let line_count = buffer.line_count();
            let tab_size = model.edit_config().tab_size;

            // The gutter fits the largest line number, so widths don't jump around while typing.
            let digits = (line_count.max(1).ilog10() + 1).max(3) as f32;
            let gutter_width = px(GUTTER_LEFT_PADDING + GUTTER_RIGHT_PADDING) + char_width * digits;
            let text_origin_x = bounds.left() + gutter_width;
            let text_width = (bounds.size.width - gutter_width).max(px(0.));
            let viewport_lines = (bounds.size.height / line_height).max(1.);

            if view.autoscroll {
                view.autoscroll = false;
                let head = view.selections.primary().head;
                let point = buffer.offset_to_point(head);
                let margin =
                    line_height * SCROLL_MARGIN_LINES.min(((viewport_lines - 1.) / 2.).max(0.));
                let top = line_height * point.line as f32;
                if top - margin < view.scroll.y {
                    view.scroll.y = top - margin;
                } else if top + line_height + margin > view.scroll.y + bounds.size.height {
                    view.scroll.y = top + line_height + margin - bounds.size.height;
                }
                let line = buffer.line(point.line);
                let column = display_column(&line, point.col, tab_size) as f32;
                let x = char_width * column;
                let margin = char_width * SCROLL_MARGIN_COLUMNS;
                if x - margin < view.scroll.x {
                    view.scroll.x = x - margin;
                } else if x + margin > view.scroll.x + text_width {
                    view.scroll.x = x + margin - text_width;
                }
            }

            // The last line may scroll up to the top of the viewport, but no further.
            let max_scroll_y = line_height * line_count.saturating_sub(1) as f32;
            view.scroll.y = view.scroll.y.clamp(px(0.), max_scroll_y);
            view.scroll.x = view.scroll.x.max(px(0.));
            let scroll = view.scroll;

            let first_line = (scroll.y / line_height).floor() as usize;
            let last_line = ((scroll.y + bounds.size.height) / line_height).ceil() as usize;
            let last_line = last_line.min(line_count);
            let visible = first_line..last_line;

            let snapshot = model.snapshot();
            let generation = model.syntax().map_or(0, |s| s.generation());
            let version = buffer.version();
            let cached = view.highlights.as_ref().filter(|c| {
                c.generation == generation && c.version == version && c.lines == visible
            });
            let spans = match cached {
                Some(cache) => cache.spans.clone(),
                None => {
                    let range = buffer.line_start(first_line)
                        ..buffer.line_start(last_line.max(first_line + 1));
                    let spans = model
                        .syntax()
                        .and_then(|s| Some((s.language(), s.tree()?)))
                        .map(|(language, tree)| highlights(language, tree, &snapshot, range))
                        .unwrap_or_default();
                    Rc::new(spans)
                }
            };

            let active_heads: Vec<usize> = view
                .selections
                .iter()
                .filter(|s| s.is_empty())
                .map(|s| buffer.offset_to_point(s.head).line)
                .collect();
            let head_lines: Vec<usize> = view
                .selections
                .iter()
                .map(|s| buffer.offset_to_point(s.head).line)
                .collect();

            let mut lines = Vec::with_capacity(visible.len());
            let mut gutter_numbers = Vec::with_capacity(visible.len());
            let mut active_lines = Vec::new();
            for line_index in visible.clone() {
                let top = bounds.top() + line_height * line_index as f32 - scroll.y;
                let line_start = buffer.line_start(line_index);
                let mut text = buffer.line(line_index);
                if text.len() > MAX_DISPLAY_BYTES {
                    text.truncate(floor_boundary(&text, MAX_DISPLAY_BYTES));
                }
                let (display, tabs) = TabMap::expand(&text, tab_size);
                let marked = view.marked.as_ref().and_then(|m| {
                    let line_end = line_start + text.len();
                    (m.start < line_end && m.end > line_start).then(|| {
                        (
                            m.start.saturating_sub(line_start),
                            (m.end - line_start).min(text.len()),
                        )
                    })
                });
                let runs = runs_for_line(
                    &text,
                    display.len(),
                    line_start,
                    &tabs,
                    &spans,
                    marked,
                    &font,
                    &theme,
                );
                let shaped = window.text_system().shape_line(
                    SharedString::from(display),
                    font_size,
                    &runs,
                    None,
                );
                lines.push(LineGeometry {
                    shaped,
                    tabs,
                    shown_bytes: text.len(),
                });

                let is_active = head_lines.contains(&line_index);
                if active_heads.contains(&line_index) {
                    active_lines.push(top);
                }
                let number = SharedString::from((line_index + 1).to_string());
                let color = if is_active {
                    theme.editor_gutter_text_active
                } else {
                    theme.editor_gutter_text
                };
                let number_run = TextRun {
                    len: number.len(),
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped_number =
                    window
                        .text_system()
                        .shape_line(number, font_size, &[number_run], None);
                let x =
                    bounds.left() + gutter_width - px(GUTTER_RIGHT_PADDING) - shaped_number.width();
                gutter_numbers.push((x, shaped_number, top));
            }

            let geometry = Rc::new(FrameGeometry {
                bounds,
                text_origin_x,
                line_height,
                char_width,
                scroll,
                first_line,
                lines,
                page_lines: viewport_lines as usize,
                tab_size,
                line_count,
            });

            // Selections, split across the lines they cover. Only what's on screen.
            let mut selection_rects = Vec::new();
            let mut cursors = Vec::new();
            let x_of = |line_index: usize, col: usize| -> Pixels {
                let laid_out = &geometry.lines[line_index - first_line];
                let col = col.min(laid_out.shown_bytes);
                text_origin_x + laid_out.shaped.x_for_index(laid_out.tabs.to_display(col))
                    - scroll.x
            };
            for selection in view.selections.iter() {
                let range = selection.range();
                let start = buffer.offset_to_point(range.start);
                let end = buffer.offset_to_point(range.end);
                if !selection.is_empty() {
                    let from = start.line.max(first_line);
                    let to = end.line.min(last_line.saturating_sub(1));
                    for line_index in from..=to {
                        if line_index < first_line || line_index >= last_line {
                            continue;
                        }
                        let col_start = if line_index == start.line {
                            start.col
                        } else {
                            0
                        };
                        let left = x_of(line_index, col_start);
                        let right = if line_index == end.line {
                            x_of(line_index, end.col)
                        } else {
                            // Show the newline is selected too, so an empty line still registers.
                            x_of(line_index, usize::MAX) + char_width * 0.5
                        };
                        let top = bounds.top() + line_height * line_index as f32 - scroll.y;
                        selection_rects.push(Bounds::new(
                            point(left, top),
                            size((right - left).max(px(0.)), line_height),
                        ));
                    }
                }
                let head = buffer.offset_to_point(selection.head);
                if (first_line..last_line).contains(&head.line) {
                    let top = bounds.top() + line_height * head.line as f32 - scroll.y;
                    cursors.push(Bounds::new(
                        point(x_of(head.line, head.col), top),
                        size(px(2.), line_height),
                    ));
                }
            }

            view.highlights = Some(HighlightCache {
                generation,
                version,
                lines: visible,
                spans,
            });
            view.geometry = Some(geometry.clone());

            Prepainted {
                geometry,
                gutter_numbers,
                active_lines,
                selection_rects,
                cursors,
                hitbox,
                gutter_width,
                started,
            }
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepainted: &mut Prepainted,
        window: &mut Window,
        cx: &mut App,
    ) {
        let theme = cx.global::<Theme>().clone();
        let focus_handle = self.view.read(cx).focus_handle.clone();
        let focused = focus_handle.is_focused(window);
        let cursor_visible = self.view.read(cx).cursor_visible;
        let geometry = prepainted.geometry.clone();
        let gutter_width = prepainted.gutter_width;
        let line_height = geometry.line_height;

        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        window.paint_quad(fill(bounds, theme.editor_background));
        for top in &prepainted.active_lines {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), *top),
                    size(bounds.size.width, line_height),
                ),
                theme.editor_active_line,
            ));
        }

        let text_bounds = Bounds::new(
            point(bounds.left() + gutter_width, bounds.top()),
            Size {
                width: (bounds.size.width - gutter_width).max(px(0.)),
                height: bounds.size.height,
            },
        );
        let text_hitbox_cursor = CursorStyle::IBeam;
        window.set_cursor_style(text_hitbox_cursor, &prepainted.hitbox);

        window.with_content_mask(
            Some(ContentMask {
                bounds: text_bounds,
            }),
            |window| {
                for rect in &prepainted.selection_rects {
                    window.paint_quad(fill(*rect, theme.editor_selection));
                }
                for (index, line) in geometry.lines.iter().enumerate() {
                    let top = bounds.top() + line_height * (geometry.first_line + index) as f32
                        - geometry.scroll.y;
                    let origin = point(geometry.text_origin_x - geometry.scroll.x, top);
                    line.shaped
                        .paint(origin, line_height, gpui::TextAlign::Left, None, window, cx)
                        .ok();
                }
                if focused && cursor_visible {
                    for cursor in &prepainted.cursors {
                        window.paint_quad(fill(*cursor, theme.editor_cursor));
                    }
                }
            },
        );

        for (x, number, top) in &prepainted.gutter_numbers {
            number
                .paint(
                    point(*x, *top),
                    line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
        }

        // Mouse handling. Registered per frame, and independent of what's under the pointer once a
        // drag has started, so that dragging can leave the editor and keep selecting.
        let view = self.view.clone();
        let hitbox = prepainted.hitbox.clone();
        let hit_geometry = geometry.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            view.update(cx, |view, cx| {
                if event.position.x < hit_geometry.text_origin_x {
                    let line = hit_geometry.line_at_y(event.position.y);
                    view.gutter_mouse_down(line, window, cx);
                } else {
                    let buffer = view.buffer.read(cx).buffer();
                    let offset = hit_geometry.offset_at(event.position, buffer);
                    view.mouse_down(offset, event.click_count, event.modifiers, window, cx);
                }
            });
        });

        let view = self.view.clone();
        let hit_geometry = geometry.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            view.update(cx, |view, cx| {
                if view.drag.is_none() {
                    return;
                }
                if !event.dragging() {
                    // The button was released somewhere we weren't told about.
                    view.mouse_up();
                    return;
                }
                let buffer = view.buffer.read(cx).buffer();
                let offset = hit_geometry.offset_at(event.position, buffer);
                let display = hit_geometry.display_position_at(event.position);
                view.mouse_dragged(offset, display, cx);
            });
        });

        let view = self.view.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, _| view.mouse_up());
            }
        });

        let view = self.view.clone();
        let hitbox = prepainted.hitbox.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                view.update(cx, |view, cx| {
                    view.scroll_wheel(event.delta, line_height, cx)
                });
            }
        });

        let elapsed = prepainted.started.elapsed();
        self.view.update(cx, |view, _| record_frame(view, elapsed));
    }
}

fn record_frame(view: &mut EditorView, elapsed: Duration) {
    if let Some(times) = &mut view.frame_times {
        times.push(elapsed);
    }
}
