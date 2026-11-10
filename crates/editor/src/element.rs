//! Lays out and paints the visible part of the document.

use crate::decorations::{BreakpointKind, BreakpointMark, GutterZone, MARKS_WIDTH};
use crate::view::{EditorView, HighlightCache};
use gleipnir_core::Severity;
use gleipnir_syntax::highlights;
use gleipnir_text::{Buffer, TabMap, byte_col_at_display, display_column};
use gleipnir_ui::Theme;
use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollEvent, ShapedLine, SharedString, Size, Style, TextRun, TransformationMatrix,
    UnderlineStyle, Window, fill, font, point, px, relative, size,
};
use std::ops::Range;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Lines longer than this are shown truncated. Shaping costs time per character, and a minified
/// file's one enormous line shouldn't be able to stall every frame.
const MAX_DISPLAY_BYTES: usize = 20_000;
const GUTTER_LEFT_PADDING: f32 = 12.0;
const GUTTER_RIGHT_PADDING: f32 = 16.0;
/// Space either side of the text beside each line, like who changed it.
const GUTTER_TEXT_PADDING: f32 = 6.0;
/// Keeps some context visible around the cursor when autoscrolling.
const SCROLL_MARGIN_LINES: f32 = 3.0;
const SCROLL_MARGIN_COLUMNS: f32 = 4.0;

/// The editor's font: the one the settings name if it's installed, or else the first installed
/// from a preference list. Fonts differ by platform, and looking one up that doesn't exist falls
/// back to something proportional, which an editor can't work with.
pub fn editor_font(window: &Window, cx: &gpui::App) -> Font {
    font(editor_font_family(window, cx))
}

/// The family `editor_font` picks.
pub fn editor_font_family(window: &Window, cx: &gpui::App) -> String {
    static AVAILABLE: OnceLock<Vec<String>> = OnceLock::new();
    let available = AVAILABLE.get_or_init(|| window.text_system().all_font_names());
    let installed = |name: &str| available.iter().any(|a| a == name);
    if let Some(chosen) = &gleipnir_core::settings(cx).editor.font_family
        && installed(chosen)
    {
        return chosen.clone();
    }
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
        .find(|name| installed(name))
        .map_or_else(|| "monospace".to_string(), |name| name.to_string())
}

/// One line as laid out on screen.
pub(crate) struct LineGeometry {
    pub shaped: ShapedLine,
    pub tabs: TabMap,
    /// Bytes of the source line that were shaped, less than its length if truncated.
    pub shown_bytes: usize,
}

/// How lines map to rows on screen: one each, but for the rows a multi-line suggestion adds
/// after its line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rows {
    /// The line rows are added after, and how many.
    pub added: Option<(usize, usize)>,
}

impl Rows {
    /// The row a line is on.
    pub fn row_of(self, line: usize) -> usize {
        match self.added {
            Some((after, rows)) if line > after => line + rows,
            _ => line,
        }
    }

    /// The line on a row, where an added row counts as the line it's after.
    pub fn line_of_row(self, row: usize) -> usize {
        match self.added {
            Some((after, rows)) if row > after => row.saturating_sub(rows).max(after),
            _ => row,
        }
    }

    pub fn extra(self) -> usize {
        self.added.map_or(0, |(_, rows)| rows)
    }
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
    pub rows: Rows,
}

impl FrameGeometry {
    fn line_at_y(&self, y: Pixels) -> usize {
        let relative = (y - self.bounds.top() + self.scroll.y) / self.line_height;
        let row = relative.max(0.) as usize;
        self.rows
            .line_of_row(row)
            .min(self.line_count.saturating_sub(1))
    }

    pub(crate) fn line_top(&self, line: usize) -> Pixels {
        self.bounds.top() + self.line_height * self.rows.row_of(line) as f32 - self.scroll.y
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

/// The debugger's marks for the lines on screen, placed.
struct Decorations {
    breakpoints: Vec<(Pixels, BreakpointMark)>,
    /// The execution line's top, and whether it's the innermost frame.
    execution: Option<(Pixels, bool)>,
    /// Each Run icon's top, and for a test with a result, whether it passed.
    run_tops: Vec<(Pixels, Option<bool>)>,
    inline_values: Vec<(Point<Pixels>, ShapedLine)>,
}

pub(crate) struct Prepainted {
    geometry: Rc<FrameGeometry>,
    usage_rects: Vec<Bounds<Pixels>>,
    decorations: Option<Decorations>,
    gutter_numbers: Vec<(Pixels, ShapedLine, Pixels)>,
    /// The text beside each line on screen: its top, the text, and how strongly to tint it.
    gutter_labels: Vec<(Pixels, ShapedLine, f32)>,
    active_lines: Vec<Pixels>,
    selection_rects: Vec<Bounds<Pixels>>,
    cursors: Vec<Bounds<Pixels>>,
    hitbox: Hitbox,
    gutter_width: Pixels,
    /// How much of the gutter, at its left, the text beside lines takes.
    gutter_text_width: Pixels,
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
    // Text after the last source byte, like an inlay at the end of the line, still needs a run.
    if display_at < display_len && !runs.is_empty() {
        runs.push(run(display_len - display_at, default, false));
    }
    // An empty run list is invalid for shaping, even with no text.
    if runs.is_empty() {
        runs.push(run(display_len, default, false));
    }
    runs
}

/// Recolors the parts of a line's runs that are inserted text, like inlay hints, splitting runs at
/// their edges.
fn color_inserted(runs: Vec<TextRun>, inserted: &[Range<usize>], color: Hsla) -> Vec<TextRun> {
    let mut out = Vec::with_capacity(runs.len() + inserted.len() * 2);
    let mut at = 0;
    for run in runs {
        let end = at + run.len;
        let mut start = at;
        // Cut points inside this run, from inserted ranges' edges.
        let mut cuts: Vec<usize> = inserted
            .iter()
            .flat_map(|r| [r.start, r.end])
            .filter(|&c| c > start && c < end)
            .collect();
        cuts.push(end);
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            let inside = inserted.iter().any(|r| r.start <= start && cut <= r.end);
            let mut piece = run.clone();
            piece.len = cut - start;
            if inside {
                piece.color = color;
                piece.underline = None;
            }
            out.push(piece);
            start = cut;
        }
        at = end;
    }
    out
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
        let font = editor_font(window, cx);
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
            let provider = view.decorations.clone();
            let marks_width = if provider.is_some() { MARKS_WIDTH } else { 0. };
            let plain = view.plain;
            let gutter_text_width = match &view.gutter_text {
                Some(_) if !plain => {
                    char_width * view.gutter_text_chars as f32 + px(GUTTER_TEXT_PADDING * 2.)
                }
                _ => px(0.),
            };
            let gutter_width = if plain {
                px(8.)
            } else {
                gutter_text_width
                    + px(GUTTER_LEFT_PADDING + marks_width + GUTTER_RIGHT_PADDING)
                    + char_width * digits
            };
            let text_origin_x = bounds.left() + gutter_width;
            let text_width = (bounds.size.width - gutter_width).max(px(0.));
            let viewport_lines = (bounds.size.height / line_height).max(1.);
            let rows = Rows {
                added: if view.focus_handle.is_focused(window) {
                    view.suggestion_rows(cx)
                } else {
                    None
                },
            };

            if view.autoscroll {
                view.autoscroll = false;
                let head = view.selections.primary().head;
                let point = buffer.offset_to_point(head);
                let margin =
                    line_height * SCROLL_MARGIN_LINES.min(((viewport_lines - 1.) / 2.).max(0.));
                let top = line_height * rows.row_of(point.line) as f32;
                if std::mem::take(&mut view.autoscroll_center) {
                    view.scroll.y = top - (bounds.size.height - line_height) / 2.;
                } else if top - margin < view.scroll.y {
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
            let max_scroll_y = line_height * (line_count.saturating_sub(1) + rows.extra()) as f32;
            view.scroll.y = view.scroll.y.clamp(px(0.), max_scroll_y);
            view.scroll.x = view.scroll.x.max(px(0.));
            let scroll = view.scroll;

            let first_line = rows.line_of_row((scroll.y / line_height).floor() as usize);
            let last_row = ((scroll.y + bounds.size.height) / line_height).ceil() as usize;
            let last_line = (rows.line_of_row(last_row.saturating_sub(1)) + 1).min(line_count);
            let top_of =
                |line: usize| bounds.top() + line_height * rows.row_of(line) as f32 - scroll.y;
            let visible = first_line..last_line;

            let snapshot = model.snapshot();
            let generation = model.syntax().map_or(0, |s| s.generation());
            let version = buffer.version();
            let tokens_generation = view.annotations.tokens_generation;
            let cached = view.highlights.as_ref().filter(|c| {
                c.generation == generation
                    && c.version == version
                    && c.lines == visible
                    && c.tokens == tokens_generation
            });
            let spans = match cached {
                Some(cache) => cache.spans.clone(),
                None => {
                    let range = buffer.line_start(first_line)
                        ..buffer.line_start(last_line.max(first_line + 1));
                    let spans = model
                        .syntax()
                        .and_then(|s| Some((s.language(), s.tree()?)))
                        .map(|(language, tree)| {
                            highlights(language, tree, &snapshot, range.clone())
                        })
                        .unwrap_or_default();
                    Rc::new(crate::annotations::overlay(
                        &spans,
                        &view.annotations.tokens,
                        range,
                    ))
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

            let inlays = if gleipnir_core::settings(cx).editor.inlay_hints {
                view.annotations.inlays.clone()
            } else {
                Default::default()
            };
            let mut lines = Vec::with_capacity(visible.len());
            let mut gutter_numbers = Vec::with_capacity(visible.len());
            let mut gutter_labels = Vec::new();
            let mut active_lines = Vec::new();
            for line_index in visible.clone() {
                let top = top_of(line_index);
                let line_start = buffer.line_start(line_index);
                let mut text = buffer.line(line_index);
                if text.len() > MAX_DISPLAY_BYTES {
                    text.truncate(floor_boundary(&text, MAX_DISPLAY_BYTES));
                }
                let line_inlays: Vec<(usize, &str)> = inlays
                    .iter()
                    .skip_while(|h| h.offset < line_start)
                    .take_while(|h| h.offset <= line_start + text.len())
                    .map(|h| (h.offset - line_start, h.label.as_str()))
                    .collect();
                let (display, tabs) = TabMap::expand_with(&text, tab_size, &line_inlays);
                let marked = view.marked.as_ref().and_then(|m| {
                    let line_end = line_start + text.len();
                    (m.start < line_end && m.end > line_start).then(|| {
                        (
                            m.start.saturating_sub(line_start),
                            (m.end - line_start).min(text.len()),
                        )
                    })
                });
                let mut runs = runs_for_line(
                    &text,
                    display.len(),
                    line_start,
                    &tabs,
                    &spans,
                    marked,
                    &font,
                    &theme,
                );
                if !line_inlays.is_empty() {
                    runs = color_inserted(runs, &tabs.inserted(), theme.inline_value);
                }
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
                if active_heads.contains(&line_index) && !plain {
                    active_lines.push(top);
                }
                if plain {
                    continue;
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
                if let Some(label) = view
                    .gutter_text
                    .as_ref()
                    .and_then(|t| t.lines.get(line_index)?.as_ref())
                {
                    let run = TextRun {
                        len: label.text.len(),
                        font: font.clone(),
                        color: theme.text_muted,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let shaped = window.text_system().shape_line(
                        label.text.clone(),
                        font_size,
                        &[run],
                        None,
                    );
                    gutter_labels.push((top, shaped, label.shade));
                }
            }

            let decorations = provider.map(|provider| {
                let on_screen = |line: &usize| visible.contains(line);
                let breakpoints = provider
                    .breakpoints(cx)
                    .into_iter()
                    .filter(|b| on_screen(&b.line))
                    .map(|b| (top_of(b.line), b))
                    .collect();
                let execution = provider
                    .execution_line(cx)
                    .filter(|(line, _)| on_screen(line))
                    .map(|(line, top)| (top_of(line), top));
                let results: std::collections::HashMap<usize, bool> =
                    provider.run_results(cx).into_iter().collect();
                let run_tops = provider
                    .run_lines(cx)
                    .into_iter()
                    .filter(on_screen)
                    .map(|line| (top_of(line), results.get(&line).copied()))
                    .collect();
                let inline_values = provider
                    .inline_values(visible.clone(), cx)
                    .into_iter()
                    .filter(|(line, _)| on_screen(line))
                    .map(|(line, text)| {
                        let laid_out = &lines[line - first_line];
                        let x = text_origin_x + laid_out.shaped.width + char_width * 2. - scroll.x;
                        let text = SharedString::from(text);
                        let run = TextRun {
                            len: text.len(),
                            font: font.clone(),
                            color: theme.inline_value,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let shaped = window
                            .text_system()
                            .shape_line(text, font_size, &[run], None);
                        (point(x, top_of(line)), shaped)
                    })
                    .collect();
                Decorations {
                    breakpoints,
                    execution,
                    run_tops,
                    inline_values,
                }
            });

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
                rows,
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
            let mut usage_rects = Vec::new();
            for range in &view.annotations.highlights {
                let start = buffer.offset_to_point(range.start);
                let end = buffer.offset_to_point(range.end);
                if start.line != end.line || !(first_line..last_line).contains(&start.line) {
                    continue;
                }
                let top = top_of(start.line);
                let left = x_of(start.line, start.col);
                let right = x_of(start.line, end.col);
                usage_rects.push(Bounds::new(
                    point(left, top),
                    size((right - left).max(px(0.)), line_height),
                ));
            }
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
                        let top = top_of(line_index);
                        selection_rects.push(Bounds::new(
                            point(left, top),
                            size((right - left).max(px(0.)), line_height),
                        ));
                    }
                }
                let head = buffer.offset_to_point(selection.head);
                if (first_line..last_line).contains(&head.line) {
                    let top = top_of(head.line);
                    cursors.push(Bounds::new(
                        point(x_of(head.line, head.col), top),
                        size(px(2.), line_height),
                    ));
                }
            }

            view.highlights = Some(HighlightCache {
                generation,
                tokens: tokens_generation,
                version,
                lines: visible,
                spans,
            });
            view.geometry = Some(geometry.clone());

            Prepainted {
                geometry,
                decorations,
                usage_rects,
                gutter_numbers,
                gutter_labels,
                active_lines,
                selection_rects,
                cursors,
                hitbox,
                gutter_width,
                gutter_text_width,
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

        if let Some((top, _)) = prepainted.decorations.as_ref().and_then(|d| d.execution) {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), top),
                    size(bounds.size.width, line_height),
                ),
                theme.execution_line,
            ));
        }

        let text_bounds = Bounds::new(
            point(bounds.left() + gutter_width, bounds.top()),
            Size {
                width: (bounds.size.width - gutter_width).max(px(0.)),
                height: bounds.size.height,
            },
        );
        let link = self.view.read(cx).semantics.link.clone();
        let text_hitbox_cursor = if link.is_some() {
            CursorStyle::PointingHand
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(text_hitbox_cursor, &prepainted.hitbox);

        window.with_content_mask(
            Some(ContentMask {
                bounds: text_bounds,
            }),
            |window| {
                for rect in &prepainted.usage_rects {
                    window.paint_quad(fill(*rect, theme.usage_highlight).corner_radii(px(2.)));
                }
                for rect in &prepainted.selection_rects {
                    window.paint_quad(fill(*rect, theme.editor_selection));
                }
                for (index, line) in geometry.lines.iter().enumerate() {
                    let top = geometry.line_top(geometry.first_line + index);
                    let origin = point(geometry.text_origin_x - geometry.scroll.x, top);
                    line.shaped
                        .paint(origin, line_height, gpui::TextAlign::Left, None, window, cx)
                        .ok();
                }
                paint_diagnostics(&self.view, &geometry, link, &theme, window, cx);
                if let Some(decorations) = &prepainted.decorations {
                    for (origin, value) in &decorations.inline_values {
                        value
                            .paint(
                                *origin,
                                line_height,
                                gpui::TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                    }
                }
                if focused {
                    paint_suggestion(&self.view, &geometry, &theme, window, cx);
                }
                if focused && cursor_visible {
                    for cursor in &prepainted.cursors {
                        window.paint_quad(fill(*cursor, theme.editor_cursor));
                    }
                }
            },
        );

        let gutter_text_width = prepainted.gutter_text_width;
        // What the gutter had before the text beside lines was added moves right of it.
        let marks_bounds = Bounds::new(
            point(bounds.left() + gutter_text_width, bounds.top()),
            size(
                (bounds.size.width - gutter_text_width).max(px(0.)),
                bounds.size.height,
            ),
        );
        // Clipped, as a partly scrolled-off line would otherwise draw over whatever's above.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (top, label, shade) in &prepainted.gutter_labels {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left(), *top),
                        size(gutter_text_width - px(2.), line_height),
                    ),
                    theme.accent.opacity(0.04 + 0.2 * shade.clamp(0., 1.)),
                ));
                label
                    .paint(
                        point(bounds.left() + px(GUTTER_TEXT_PADDING), *top),
                        line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .ok();
            }
            paint_gutter_marks(&self.view, &geometry, marks_bounds, &theme, window, cx);
            paint_change_marks(
                &self.view,
                &geometry,
                bounds,
                gutter_width,
                &theme,
                window,
                cx,
            );
            if let Some(decorations) = &prepainted.decorations {
                paint_debug_marks(
                    decorations,
                    marks_bounds,
                    gutter_width - gutter_text_width,
                    line_height,
                    &theme,
                    window,
                    cx,
                );
            }
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
        });

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
                if event.position.x < hit_geometry.bounds.left() + gutter_text_width {
                    let line = hit_geometry.line_at_y(event.position.y);
                    view.gutter_text_clicked(line, cx);
                } else if event.position.x < hit_geometry.text_origin_x {
                    let line = hit_geometry.line_at_y(event.position.y);
                    let zone = if event.position.x
                        >= hit_geometry.text_origin_x - px(GUTTER_RIGHT_PADDING)
                    {
                        GutterZone::Run
                    } else {
                        GutterZone::Marks
                    };
                    view.gutter_mouse_down(line, zone, window, cx);
                } else {
                    let buffer = view.buffer.read(cx).buffer();
                    let offset = hit_geometry.offset_at(event.position, buffer);
                    view.mouse_down(offset, event.click_count, event.modifiers, window, cx);
                }
            });
        });

        let view = self.view.clone();
        let hitbox = prepainted.hitbox.clone();
        let hit_geometry = geometry.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Right
                || !hitbox.is_hovered(window)
                || event.position.x >= hit_geometry.text_origin_x
            {
                return;
            }
            let line = hit_geometry.line_at_y(event.position.y);
            view.update(cx, |view, cx| {
                if event.position.x < hit_geometry.bounds.left() + gutter_text_width {
                    cx.emit(crate::view::EditorEvent::GutterTextMenu {
                        line,
                        position: event.position,
                    });
                } else if view.decorations.is_some() {
                    cx.emit(crate::view::EditorEvent::GutterMenu {
                        line,
                        position: event.position,
                    });
                }
            });
        });

        let view = self.view.clone();
        let hit_geometry = geometry.clone();
        let hover_hitbox = prepainted.hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            view.update(cx, |view, cx| {
                if view.drag.is_none() {
                    let over_label = hover_hitbox.is_hovered(window)
                        && event.position.x < hit_geometry.bounds.left() + gutter_text_width;
                    let line = over_label.then(|| hit_geometry.line_at_y(event.position.y));
                    view.gutter_text_hovered(line, event.position, cx);
                    let over_text = hover_hitbox.is_hovered(window)
                        && event.position.x >= hit_geometry.text_origin_x;
                    let buffer = view.buffer.read(cx).buffer();
                    let offset = over_text
                        .then(|| hit_geometry.offset_at(event.position, buffer))
                        // Past the end of a line isn't over anything.
                        .filter(|o| {
                            hit_geometry
                                .bounds_for_offset(*o, buffer)
                                .is_some_and(|b| b.right() > event.position.x)
                        });
                    let secondary = event.modifiers.secondary(cx);
                    view.mouse_over(offset, event.position, secondary, cx);
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
        let hover_hitbox = prepainted.hitbox.clone();
        let hit_geometry = geometry.clone();
        window.on_modifiers_changed(move |event: &gpui::ModifiersChangedEvent, window, cx| {
            let position = window.mouse_position();
            view.update(cx, |view, cx| {
                if view.drag.is_some() {
                    return;
                }
                let over_text =
                    hover_hitbox.is_hovered(window) && position.x >= hit_geometry.text_origin_x;
                let buffer = view.buffer.read(cx).buffer();
                let offset = over_text.then(|| hit_geometry.offset_at(position, buffer));
                view.update_link(offset, event.modifiers.secondary(cx), cx);
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

/// The x span of `range` on one line, clipped to what's on screen.
fn span_on_line(
    geometry: &FrameGeometry,
    buffer: &gleipnir_text::Buffer,
    line: usize,
    range: &Range<usize>,
) -> Option<(Pixels, Pixels, Pixels)> {
    let line_start = buffer.line_start(line);
    let line_end = buffer.line_end(line);
    let start = range.start.max(line_start);
    let end = range.end.min(line_end);
    if start > end {
        return None;
    }
    let left = geometry.bounds_for_offset(start, buffer)?;
    let right = if end > start {
        geometry.bounds_for_offset(end, buffer)?.left()
    } else {
        left.left() + geometry.char_width
    };
    Some((
        left.left(),
        right.max(left.left() + geometry.char_width / 2.),
        left.top(),
    ))
}

fn severity_color(severity: Severity, theme: &Theme) -> Hsla {
    match severity {
        Severity::Error => theme.error,
        Severity::Warning => theme.warning,
        Severity::Information | Severity::Hint => theme.text_faint,
    }
}

fn paint_diagnostics(
    view: &Entity<EditorView>,
    geometry: &FrameGeometry,
    link: Option<Range<usize>>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let model = view.read(cx).buffer.read(cx);
    let buffer = model.buffer();
    let last_line = geometry.first_line + geometry.lines.len();
    let visible = buffer.line_start(geometry.first_line)
        ..buffer.line_end(last_line.saturating_sub(1).max(geometry.first_line));
    let mut underlines = Vec::new();
    for diagnostic in model.diagnostics().iter().rev() {
        if diagnostic.range.end < visible.start || diagnostic.range.start > visible.end {
            continue;
        }
        let first = buffer
            .offset_to_point(diagnostic.range.start)
            .line
            .max(geometry.first_line);
        let last = buffer
            .offset_to_point(diagnostic.range.end)
            .line
            .min(last_line.saturating_sub(1));
        for line in first..=last {
            if let Some((left, right, top)) =
                span_on_line(geometry, buffer, line, &diagnostic.range)
            {
                let style = UnderlineStyle {
                    thickness: px(1.),
                    color: Some(severity_color(diagnostic.severity, theme)),
                    wavy: diagnostic.severity <= Severity::Warning,
                };
                underlines.push((
                    point(left, top + geometry.line_height - px(3.)),
                    right - left,
                    style,
                ));
            }
        }
    }
    if let Some(link) = link {
        let line = buffer.offset_to_point(link.start).line;
        if let Some((left, right, top)) = span_on_line(geometry, buffer, line, &link) {
            let style = UnderlineStyle {
                thickness: px(1.),
                color: Some(theme.accent),
                wavy: false,
            };
            underlines.push((
                point(left, top + geometry.line_height - px(2.)),
                right - left,
                style,
            ));
        }
    }
    for (origin, width, style) in underlines {
        window.paint_underline(origin, width, &style);
    }
}

/// The grey suggestion after the cursor, and how likely it is, faint beside it.
fn paint_suggestion(
    view: &Entity<EditorView>,
    geometry: &FrameGeometry,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(suggestion) = view.read(cx).suggestion().cloned() else {
        return;
    };
    let at = {
        let buffer = view.read(cx).buffer.read(cx).buffer();
        geometry.bounds_for_offset(suggestion.offset, buffer)
    };
    let Some(at) = at else {
        return;
    };
    let font = editor_font(window, cx);
    let font_size = px(theme.editor_font_size);
    let shape = |text: String, color: Hsla, size: Pixels, window: &mut Window| {
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_line(SharedString::from(text), size, &[run], None)
    };
    // The rest of this line after the cursor, and any lines after it in the rows made for them.
    let spaces = " ".repeat(geometry.tab_size.max(1));
    let mut lines = suggestion.text.split('\n');
    let first = lines.next().unwrap_or_default().replace('\t', &spaces);
    let ghost = shape(first, theme.text_faint, font_size, window);
    ghost
        .paint(
            at.origin,
            geometry.line_height,
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
    for (row, line) in lines.enumerate() {
        let origin = point(
            geometry.text_origin_x - geometry.scroll.x,
            at.origin.y + geometry.line_height * (row + 1) as f32,
        );
        shape(
            line.replace('\t', &spaces),
            theme.text_faint,
            font_size,
            window,
        )
        .paint(
            origin,
            geometry.line_height,
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
    }
    if let Some(score) = suggestion.score {
        let label = format!("  {:.0}% · Tab", (score * 100.).clamp(0., 100.));
        let badge = shape(
            label,
            theme.text_faint.opacity(0.7),
            font_size * 0.8,
            window,
        );
        let origin = point(at.origin.x + ghost.width, at.origin.y + font_size * 0.12);
        badge
            .paint(
                origin,
                geometry.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
    }
}

/// How the lines differ from the last commit, at the gutter's inner edge beside the code, as
/// IntelliJ marks them: a bar for added and changed lines, a wedge between lines where some
/// were deleted.
fn paint_change_marks(
    view: &Entity<EditorView>,
    geometry: &FrameGeometry,
    bounds: Bounds<Pixels>,
    gutter_width: Pixels,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    use gleipnir_index::LineChangeKind;
    let changes = view.read(cx).buffer.read(cx).line_changes().clone();
    let first = geometry.first_line;
    let last = first + geometry.lines.len();
    let x = bounds.left() + gutter_width - px(3.);
    for change in changes.iter() {
        match change.kind {
            LineChangeKind::Deleted => {
                let line = change.lines.start;
                if line < first || line > last {
                    continue;
                }
                window.paint_quad(fill(
                    Bounds::new(
                        point(x - px(3.), geometry.line_top(line) - px(2.)),
                        size(px(6.), px(4.)),
                    ),
                    theme.git_deleted,
                ));
            }
            kind => {
                let start = change.lines.start.max(first);
                let end = change.lines.end.min(last);
                if start >= end {
                    continue;
                }
                let color = if kind == LineChangeKind::Added {
                    theme.git_added
                } else {
                    theme.git_modified
                };
                window.paint_quad(fill(
                    Bounds::new(
                        point(x, geometry.line_top(start)),
                        size(
                            px(3.),
                            geometry.line_top(end - 1) + geometry.line_height
                                - geometry.line_top(start),
                        ),
                    ),
                    color,
                ));
            }
        }
    }
}

/// A bar at the gutter's edge on lines with problems, in the color of the worst one.
fn paint_gutter_marks(
    view: &Entity<EditorView>,
    geometry: &FrameGeometry,
    bounds: Bounds<Pixels>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let model = view.read(cx).buffer.read(cx);
    let buffer = model.buffer();
    let mut worst: Vec<Option<Severity>> = vec![None; geometry.lines.len()];
    for diagnostic in model.diagnostics() {
        let line = buffer.offset_to_point(diagnostic.range.start).line;
        if let Some(slot) = line
            .checked_sub(geometry.first_line)
            .and_then(|i| worst.get_mut(i))
        {
            *slot = Some(slot.map_or(diagnostic.severity, |s| s.min(diagnostic.severity)));
        }
    }
    for (index, severity) in worst.into_iter().enumerate() {
        let Some(severity) = severity else { continue };
        let top = geometry.line_top(geometry.first_line + index);
        window.paint_quad(fill(
            Bounds::new(
                point(bounds.left() + px(3.), top + px(3.)),
                size(px(3.), geometry.line_height - px(6.)),
            ),
            severity_color(severity, theme),
        ));
    }
}

/// Breakpoints in the marks column, Run icons beside the numbers, and the execution arrow.
fn paint_debug_marks(
    decorations: &Decorations,
    bounds: Bounds<Pixels>,
    gutter_width: Pixels,
    line_height: Pixels,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let diameter = (line_height - px(6.)).min(px(12.));
    let center_x = bounds.left() + px(GUTTER_LEFT_PADDING + MARKS_WIDTH / 2.);
    let dot = |top: Pixels, diameter: Pixels| {
        Bounds::new(
            point(
                center_x - diameter / 2.,
                top + (line_height - diameter) / 2.,
            ),
            size(diameter, diameter),
        )
    };
    for (top, mark) in &decorations.breakpoints {
        let color = match mark.kind {
            BreakpointKind::Log => theme.warning,
            _ => theme.breakpoint,
        };
        let circle = dot(*top, diameter);
        if !mark.enabled || mark.verified == Some(false) {
            let color = if mark.enabled {
                color
            } else {
                theme.text_faint
            };
            window.paint_quad(
                gpui::outline(circle, color, gpui::BorderStyle::Solid)
                    .corner_radii(diameter / 2.)
                    .border_widths(px(1.5)),
            );
        } else {
            window.paint_quad(fill(circle, color).corner_radii(diameter / 2.));
        }
        if mark.kind == BreakpointKind::Conditional {
            let inner = diameter / 3.;
            window.paint_quad(
                fill(dot(*top, inner), theme.editor_background).corner_radii(inner / 2.),
            );
        }
    }
    let icon = |top: Pixels, x: Pixels, side: Pixels| {
        Bounds::new(point(x, top + (line_height - side) / 2.), size(side, side))
    };
    if let Some((top, innermost)) = decorations.execution {
        let side = px(10.);
        let color = if innermost {
            theme.accent
        } else {
            theme.text_muted
        };
        window
            .paint_svg(
                icon(top, center_x - side / 2., side),
                "icons/run.svg".into(),
                None,
                TransformationMatrix::unit(),
                color,
                cx,
            )
            .ok();
    }
    for (top, result) in &decorations.run_tops {
        let side = px(11.);
        let x = bounds.left() + gutter_width - px(GUTTER_RIGHT_PADDING / 2.) - side / 2.;
        let (path, color) = match result {
            Some(true) => ("icons/run-passed.svg", theme.git_added),
            Some(false) => ("icons/run-failed.svg", theme.error),
            None => ("icons/run.svg", theme.git_added),
        };
        window
            .paint_svg(
                icon(*top, x, side),
                path.into(),
                None,
                TransformationMatrix::unit(),
                color,
                cx,
            )
            .ok();
    }
}

fn record_frame(view: &mut EditorView, elapsed: Duration) {
    if let Some(times) = &mut view.frame_times {
        times.push(elapsed);
    }
}
