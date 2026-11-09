//! Code intelligence in the editor: completion, hover, signature help and Cmd+click links. The
//! editor knows nothing of LSP: a `SemanticsProvider` answers its questions in the editor's own
//! terms, and the language server crate supplies one.

use crate::actions::*;
use crate::view::{EditorEvent, EditorView};
use anyhow::Result;
use gleipnir_core::{BufferModel, Severity};
use gleipnir_index::fuzzy::{Pattern, fuzzy_match};
use gleipnir_text::TextEdit;
use gleipnir_ui::Theme;
use gleipnir_ui::markdown::{Block, blocks};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FontWeight, HighlightStyle, Pixels, Point,
    ScrollStrategy, SharedString, StyledText, Task, UniformListScrollHandle, Window, anchored,
    deferred, div, point, prelude::*, px, uniform_list,
};
use std::any::Any;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// How long the mouse has to rest on something before its documentation shows.
const HOVER_DELAY: Duration = Duration::from_millis(400);
const MENU_ROWS: usize = 10;
const MENU_ROW_HEIGHT: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Function,
    Method,
    Field,
    Variable,
    Type,
    Keyword,
    Module,
    Constant,
    Snippet,
    Other,
}

#[derive(Clone)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub kind: CompletionKind,
    pub filter_text: String,
    pub sort_text: String,
    /// Where the replaced text starts. It runs to the cursor, however much has been typed since.
    pub replace_start: usize,
    pub new_text: String,
    /// What to select afterwards, as byte offsets into `new_text`, for snippets with a tab stop.
    /// An empty range is a cursor; without one, the cursor goes after the text.
    pub selection: Option<Range<usize>>,
    /// Edits elsewhere, like an import at the top, in the document as it was when requested.
    pub additional_edits: Vec<TextEdit>,
    pub documentation: Option<String>,
    /// The provider's own data, for resolving the item before it's applied.
    pub data: Rc<dyn Any>,
}

pub struct Completions {
    pub items: Vec<CompletionItem>,
    /// The list depends on more of the word than was typed, so it's asked for again as typing
    /// continues instead of just being filtered.
    pub incomplete: bool,
}

pub struct HoverInfo {
    pub markdown: String,
}

pub struct SignatureInfo {
    pub label: String,
    /// The active parameter, as a byte range into `label`.
    pub active_parameter: Option<Range<usize>>,
    pub documentation: Option<String>,
}

#[derive(Clone, Default)]
pub struct Triggers {
    pub completion: Vec<char>,
    pub signature: Vec<char>,
}

/// Byte ranges and the colors a server gives them, sorted.
pub type SemanticTokens = Vec<(Range<usize>, gleipnir_syntax::HighlightKind)>;

pub trait SemanticsProvider {
    fn triggers(&self, buffer: &Entity<BufferModel>, cx: &App) -> Triggers;
    fn completions(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        trigger: Option<char>,
        cx: &mut App,
    ) -> Task<Result<Completions>>;
    /// Fills in what a server leaves out of the list for speed, like auto-import edits.
    fn resolve(
        &self,
        buffer: &Entity<BufferModel>,
        item: CompletionItem,
        cx: &mut App,
    ) -> Task<Result<CompletionItem>>;
    fn hover(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Result<Option<HoverInfo>>>;
    fn signature_help(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Result<Option<SignatureInfo>>>;
    /// The uses of the name at `offset` in this file, to highlight while the cursor's on it.
    fn document_highlights(
        &self,
        _buffer: &Entity<BufferModel>,
        _offset: usize,
        _cx: &mut App,
    ) -> Task<Result<Vec<Range<usize>>>> {
        Task::ready(Ok(Vec::new()))
    }
    /// Types and parameter names to show in the text, like `: i32` after a variable.
    fn inlay_hints(
        &self,
        _buffer: &Entity<BufferModel>,
        _cx: &mut App,
    ) -> Task<Result<Vec<crate::InlayHint>>> {
        Task::ready(Ok(Vec::new()))
    }
    /// Colors from the server's understanding of names, laid over the syntax tree's.
    fn semantic_tokens(
        &self,
        _buffer: &Entity<BufferModel>,
        _cx: &mut App,
    ) -> Task<Result<SemanticTokens>> {
        Task::ready(Ok(Vec::new()))
    }
}

pub(crate) struct CompletionMenu {
    items: Vec<CompletionItem>,
    /// (item index, matched byte offsets in its label), best first.
    visible: Vec<(usize, Vec<usize>)>,
    selected: usize,
    /// Where the word being completed starts.
    word_start: usize,
    incomplete: bool,
    scroll: UniformListScrollHandle,
}

pub(crate) struct HoverPopup {
    anchor: usize,
    range: Range<usize>,
    diagnostics: Vec<(Severity, String)>,
    blocks: Vec<Block>,
    /// Where it was drawn, so the mouse moving onto it doesn't close it.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

pub(crate) struct SignaturePopup {
    anchor: usize,
    info: SignatureInfo,
}

/// The popups an editor can show, and the requests in flight for them.
#[derive(Default)]
pub(crate) struct SemanticsState {
    pub provider: Option<Rc<dyn SemanticsProvider>>,
    pub completion: Option<CompletionMenu>,
    completion_task: Option<Task<()>>,
    /// Where the word starts that the request in flight is for, so typing more of the same word
    /// waits for it rather than asking again.
    completion_requested_at: Option<usize>,
    pub hover: Option<HoverPopup>,
    hover_task: Option<Task<()>>,
    hover_target: Option<usize>,
    pub signature: Option<SignaturePopup>,
    signature_task: Option<Task<()>>,
    /// The name under the mouse while the secondary modifier is held, shown as a link.
    pub link: Option<Range<usize>>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl EditorView {
    pub fn set_semantics(&mut self, provider: Rc<dyn SemanticsProvider>) {
        self.semantics.provider = Some(provider);
    }

    pub fn has_popups(&self) -> bool {
        self.suggestion.current.is_some()
            || self.semantics.completion.is_some()
            || self.semantics.hover.is_some()
            || self.semantics.signature.is_some()
    }

    pub fn completion_labels(&self) -> Vec<String> {
        self.semantics
            .completion
            .as_ref()
            .map_or_else(Vec::new, |menu| {
                menu.visible
                    .iter()
                    .map(|(index, _)| menu.items[*index].label.clone())
                    .collect()
            })
    }

    pub fn hover_text(&self) -> Option<String> {
        let hover = self.semantics.hover.as_ref()?;
        let mut parts: Vec<String> = hover.diagnostics.iter().map(|(_, m)| m.clone()).collect();
        parts.extend(hover.blocks.iter().filter_map(|b| match b {
            Block::Code(t) | Block::Text(t) => Some(t.clone()),
            Block::Rule => None,
        }));
        Some(parts.join("\n"))
    }

    pub fn signature_label(&self) -> Option<String> {
        self.semantics
            .signature
            .as_ref()
            .map(|s| s.info.label.clone())
    }

    pub(crate) fn close_popups(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.has_popups();
        self.clear_suggestion(cx);
        self.semantics.completion = None;
        self.semantics.completion_task = None;
        self.semantics.completion_requested_at = None;
        self.semantics.hover = None;
        self.semantics.hover_task = None;
        self.semantics.signature = None;
        self.semantics.signature_task = None;
        if had {
            cx.notify();
        }
        had
    }

    /// Called after a character is typed, to start or update completion and signature help.
    pub(crate) fn after_typing(&mut self, typed: char, cx: &mut Context<Self>) {
        self.typed_for_suggestion(typed, cx);
        let Some(provider) = self.semantics.provider.clone() else {
            return;
        };
        let triggers = provider.triggers(&self.buffer, cx);
        self.semantics.hover = None;
        let editor_settings = gleipnir_core::settings(cx).editor.clone();
        if self.semantics.completion.is_some() {
            self.update_completions(cx);
        } else if !editor_settings.completion_while_typing {
            // Only Ctrl+Space asks.
        } else if is_word(typed) {
            self.request_completions(None, cx);
        } else if triggers.completion.contains(&typed) {
            self.request_completions(Some(typed), cx);
        }
        self.check_signature(cx);
        if typed == ')' {
            self.semantics.signature = None;
        } else if (editor_settings.parameter_info_while_typing
            && triggers.signature.contains(&typed))
            || (typed == ',' && self.semantics.signature.is_some())
        {
            self.request_signature(cx);
        }
    }

    /// Called after deleting, which narrows or ends a completion.
    pub(crate) fn after_deleting(&mut self, cx: &mut Context<Self>) {
        self.request_suggestion(cx);
        if self.semantics.completion.is_some() {
            self.update_completions(cx);
        }
        self.check_signature(cx);
    }

    /// Parameter Info closes once the cursor leaves the call: before where it opened, onto
    /// another line, or past the parenthesis that closes it.
    pub(crate) fn check_signature(&mut self, cx: &mut Context<Self>) {
        let Some(signature) = &self.semantics.signature else {
            return;
        };
        let head = self.selections.primary().head;
        let buffer = self.buffer.read(cx).buffer();
        let anchor = signature.anchor.min(buffer.len());
        let inside = head >= anchor
            && buffer.offset_to_point(head).line == buffer.offset_to_point(anchor).line
            && {
                let mut depth = 0i32;
                buffer.slice(anchor..head).chars().all(|c| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    depth >= 0
                })
            };
        if !inside {
            self.semantics.signature = None;
            cx.notify();
        }
    }

    fn word_start(&self, cx: &App) -> usize {
        let buffer = self.buffer.read(cx).buffer();
        let mut start = self.selections.primary().head;
        while let Some(c) = buffer.char_before(start) {
            if !is_word(c) {
                break;
            }
            start -= c.len_utf8();
        }
        start
    }

    pub(crate) fn request_completions(&mut self, trigger: Option<char>, cx: &mut Context<Self>) {
        let Some(provider) = self.semantics.provider.clone() else {
            return;
        };
        let offset = self.selections.primary().head;
        let word_start = self.word_start(cx);
        // Typing on through a word while its list is on the way: that list, filtered by what's
        // been typed since, is as good, and asking again on every key would only queue up work
        // for a slow server.
        if trigger.is_none() && self.semantics.completion_requested_at == Some(word_start) {
            return;
        }
        let buffer = self.buffer.clone();
        let task = provider.completions(&buffer, offset, trigger, cx);
        self.semantics.completion_requested_at = Some(word_start);
        self.semantics.completion_task = Some(cx.spawn(async move |this, cx| {
            let completions = task.await;
            this.update(cx, |this, cx| {
                this.semantics.completion_requested_at = None;
                let Ok(completions) = completions else { return };
                // The caret has left the word since: the list is for somewhere else.
                if this.word_start(cx) != word_start || this.selections.primary().head < word_start
                {
                    return;
                }
                let scroll = this
                    .semantics
                    .completion
                    .take()
                    .map(|m| m.scroll)
                    .unwrap_or_default();
                this.semantics.completion = Some(CompletionMenu {
                    items: completions.items,
                    visible: Vec::new(),
                    selected: 0,
                    word_start,
                    incomplete: completions.incomplete,
                    scroll,
                });
                this.filter_completions(cx);
            })
            .ok();
        }));
    }

    /// Re-filters after typing or deleting, or asks again if the list was incomplete.
    fn update_completions(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = &self.semantics.completion else {
            return;
        };
        let head = self.selections.primary().head;
        let still_in_word = head >= menu.word_start && self.word_start(cx) == menu.word_start;
        if !still_in_word || self.selections.len() > 1 {
            self.semantics.completion = None;
            cx.notify();
            return;
        }
        if menu.incomplete {
            self.request_completions(None, cx);
        }
        self.filter_completions(cx);
    }

    fn filter_completions(&mut self, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        let Some(menu) = &mut self.semantics.completion else {
            return;
        };
        let query = self.buffer.read(cx).buffer().slice(menu.word_start..head);
        let pattern = Pattern::new(&query);
        let mut visible: Vec<(i32, usize, Vec<usize>)> = menu
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let found = fuzzy_match(&pattern, &item.filter_text)?;
                // Highlights go on the label, which usually is the filter text.
                let positions = if item.filter_text == item.label {
                    found.positions
                } else {
                    Vec::new()
                };
                Some((found.score, index, positions))
            })
            .collect();
        if pattern.is_empty() {
            visible.sort_by(|a, b| menu.items[a.1].sort_text.cmp(&menu.items[b.1].sort_text));
        } else {
            visible.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| menu.items[a.1].sort_text.cmp(&menu.items[b.1].sort_text))
            });
        }
        menu.visible = visible
            .into_iter()
            .map(|(_, index, positions)| (index, positions))
            .collect();
        menu.selected = 0;
        menu.scroll.scroll_to_item(0, ScrollStrategy::Top);
        if menu.visible.is_empty() {
            self.semantics.completion = None;
        }
        cx.notify();
    }

    pub(crate) fn move_completion(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.semantics.completion else {
            return;
        };
        let count = menu.visible.len() as isize;
        if count == 0 {
            return;
        }
        menu.selected = (menu.selected as isize + delta).rem_euclid(count) as usize;
        menu.scroll
            .scroll_to_item(menu.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    pub(crate) fn confirm_completion(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.semantics.completion.take() else {
            return;
        };
        let Some((index, _)) = menu.visible.get(menu.selected) else {
            return;
        };
        let item = menu.items[*index].clone();
        let Some(provider) = self.semantics.provider.clone() else {
            return;
        };
        let buffer = self.buffer.clone();
        let resolve = provider.resolve(&buffer, item.clone(), cx);
        self.semantics.completion_requested_at = None;
        self.semantics.completion_task = Some(cx.spawn(async move |this, cx| {
            let item = resolve.await.unwrap_or(item);
            this.update(cx, |this, cx| this.apply_completion(item, cx))
                .ok();
        }));
        cx.notify();
    }

    /// Replaces the word with the completion, and makes any other edits it carries, as one undo
    /// step. The cursor goes where the snippet says, or after the inserted text.
    fn apply_completion(&mut self, item: CompletionItem, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        let start = item.replace_start.min(head);
        let mut edits = vec![TextEdit::new(start..head, item.new_text.clone())];
        // An additional edit overlapping the main one would make the batch invalid; servers don't
        // send those on purpose, so drop it rather than fail the whole completion.
        let shift: isize = item
            .additional_edits
            .iter()
            .filter(|e| e.range.end <= start)
            .map(|e| e.text.len() as isize - e.range.len() as isize)
            .sum();
        edits.extend(
            item.additional_edits
                .iter()
                .filter(|e| e.range.end <= start || e.range.start >= head)
                .cloned(),
        );
        let base = (start as isize + shift) as usize;
        let selection = item
            .selection
            .clone()
            .unwrap_or(item.new_text.len()..item.new_text.len());
        self.edit_for_semantics(edits, base + selection.start, cx);
        if !selection.is_empty() {
            self.selections = gleipnir_text::SelectionSet::single(gleipnir_text::Selection::new(
                base + selection.start,
                base + selection.end,
            ));
            cx.notify();
        }
        // Inside a call's parentheses, show what it takes.
        if selection.start < item.new_text.len() && item.new_text[..selection.start].contains('(') {
            self.request_signature(cx);
        }
    }

    pub(crate) fn request_signature(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.semantics.provider.clone() else {
            return;
        };
        let offset = self.selections.primary().head;
        let task = provider.signature_help(&self.buffer, offset, cx);
        self.semantics.signature_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.semantics.signature = match result {
                    Ok(Some(info)) => Some(SignaturePopup {
                        anchor: offset,
                        info,
                    }),
                    _ => None,
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// Shows documentation for an offset now, as the keyboard shortcut does.
    pub(crate) fn request_hover(&mut self, offset: usize, cx: &mut Context<Self>) {
        let model = self.buffer.read(cx);
        let range = model.buffer().word_at(offset).unwrap_or(offset..offset);
        let diagnostics: Vec<(Severity, String)> = model
            .diagnostics()
            .iter()
            .filter(|d| d.range.start <= offset && offset <= d.range.end.max(d.range.start + 1))
            .map(|d| (d.severity, d.message.clone()))
            .collect();
        let task = self
            .semantics
            .provider
            .clone()
            .map(|p| p.hover(&self.buffer, offset, cx));
        self.semantics.hover_task = Some(cx.spawn(async move |this, cx| {
            let info = match task {
                Some(task) => task.await.ok().flatten(),
                None => None,
            };
            this.update(cx, |this, cx| {
                let blocks = info.map(|i| blocks(&i.markdown)).unwrap_or_default();
                this.semantics.hover =
                    (!blocks.is_empty() || !diagnostics.is_empty()).then(|| HoverPopup {
                        anchor: range.start,
                        range: range.clone(),
                        diagnostics: diagnostics.clone(),
                        blocks,
                        bounds: Rc::default(),
                    });
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn update_link(
        &mut self,
        offset: Option<usize>,
        secondary: bool,
        cx: &mut Context<Self>,
    ) {
        let link = offset
            .filter(|_| secondary)
            .and_then(|o| self.buffer.read(cx).buffer().word_at(o));
        if link != self.semantics.link {
            self.semantics.link = link;
            cx.notify();
        }
    }

    /// Called as the mouse moves over text, with the offset under it, if any.
    pub(crate) fn mouse_over(
        &mut self,
        offset: Option<usize>,
        position: Point<Pixels>,
        secondary: bool,
        cx: &mut Context<Self>,
    ) {
        self.update_link(offset, secondary, cx);

        if let Some(hover) = &self.semantics.hover {
            let over_popup = hover.bounds.get().is_some_and(|b| b.contains(&position));
            let over_range = offset.is_some_and(|o| hover.range.start <= o && o <= hover.range.end);
            if over_popup || over_range {
                return;
            }
            self.semantics.hover = None;
            cx.notify();
        }
        if offset == self.semantics.hover_target {
            return;
        }
        self.semantics.hover_target = offset;
        self.semantics.hover_task = None;
        let Some(offset) = offset else { return };
        // Only over a word or a diagnostic is there anything to show.
        let model = self.buffer.read(cx);
        let interesting = model.buffer().word_at(offset).is_some()
            || model
                .diagnostics()
                .iter()
                .any(|d| d.range.start <= offset && offset < d.range.end);
        if !interesting {
            return;
        }
        self.semantics.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            this.update(cx, |this, cx| {
                if this.semantics.hover_target == Some(offset) {
                    this.request_hover(offset, cx);
                }
            })
            .ok();
        }));
    }

    /// F2 and Shift+F2: the next or previous error or warning, wrapping around, with its message.
    pub(crate) fn go_to_problem(&mut self, forward: bool, cx: &mut Context<Self>) {
        let head = self.selections.primary().head;
        let starts: Vec<usize> = self
            .buffer
            .read(cx)
            .diagnostics()
            .iter()
            .filter(|d| d.severity <= Severity::Warning)
            .map(|d| d.range.start)
            .collect();
        let target = if forward {
            starts
                .iter()
                .copied()
                .find(|s| *s > head)
                .or(starts.first().copied())
        } else {
            starts
                .iter()
                .rev()
                .copied()
                .find(|s| *s < head)
                .or(starts.last().copied())
        };
        let Some(target) = target else { return };
        self.selections = gleipnir_text::SelectionSet::cursor(target);
        self.did_move(cx);
        self.request_hover(target, cx);
    }

    /// A click with the secondary modifier held: go to what's under it.
    pub(crate) fn link_clicked(&mut self, offset: usize, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::GoToDefinition { offset });
    }

    pub(crate) fn render_popups(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.global::<Theme>().clone();
        let Some(geometry) = self.geometry.clone() else {
            return Vec::new();
        };
        let mut popups = Vec::new();
        let font = crate::element::editor_font(window, cx);
        let (menu_at, signature_at, hover_at) = {
            let buffer = self.buffer.read(cx).buffer();
            let below = |offset: usize| {
                geometry
                    .bounds_for_offset(offset, buffer)
                    .map(|b| point(b.left(), b.bottom()))
            };
            let above = |offset: usize| {
                geometry
                    .bounds_for_offset(offset, buffer)
                    .map(|b| point(b.left(), b.top()))
            };
            (
                self.semantics
                    .completion
                    .as_ref()
                    .and_then(|m| below(m.word_start)),
                self.semantics
                    .signature
                    .as_ref()
                    .and_then(|s| above(s.anchor)),
                self.semantics.hover.as_ref().and_then(|h| above(h.anchor)),
            )
        };

        if let Some(menu) = &self.semantics.completion
            && let Some(position) = menu_at
        {
            popups.push(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(8.))
                        .child(self.render_completion_menu(menu, &theme, font.clone(), cx)),
                )
                .with_priority(3)
                .into_any_element(),
            );
        }
        if self.semantics.completion.is_none()
            && let Some(signature) = &self.semantics.signature
            && let Some(position) = signature_at
        {
            popups.push(
                deferred(
                    anchored()
                        .anchor(gpui::Anchor::BottomLeft)
                        .position(position)
                        .snap_to_window_with_margin(px(8.))
                        .child(Self::render_signature(
                            &signature.info,
                            &theme,
                            font.clone(),
                        )),
                )
                .with_priority(3)
                .into_any_element(),
            );
        }
        if let Some(hover) = &self.semantics.hover
            && let Some(position) = hover_at
        {
            popups.push(
                deferred(
                    anchored()
                        .anchor(gpui::Anchor::BottomLeft)
                        .position(position)
                        .snap_to_window_with_margin(px(8.))
                        .child(Self::render_hover(hover, &theme, font)),
                )
                .with_priority(3)
                .into_any_element(),
            );
        }
        popups
    }

    fn render_completion_menu(
        &self,
        menu: &CompletionMenu,
        theme: &Theme,
        font: gpui::Font,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = cx.processor(move |this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            let Some(menu) = &this.semantics.completion else {
                return Vec::new();
            };
            range
                .map(|row| {
                    let (index, positions) = &menu.visible[row];
                    let item = &menu.items[*index];
                    let selected = row == menu.selected;
                    let (badge, color) = kind_badge(item.kind, &theme);
                    let highlight = HighlightStyle {
                        color: Some(theme.accent),
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    };
                    let ranges: Vec<(Range<usize>, HighlightStyle)> = positions
                        .iter()
                        .filter_map(|p| {
                            Some((
                                *p..*p + item.label.get(*p..)?.chars().next()?.len_utf8(),
                                highlight,
                            ))
                        })
                        .collect();
                    div()
                        .id(("completion", row))
                        .debug_selector(|| format!("completion-{row}"))
                        .h(px(MENU_ROW_HEIGHT))
                        .w_full()
                        .px(px(8.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .when(selected, |r| r.bg(theme.selection))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(menu) = &mut this.semantics.completion {
                                menu.selected = row;
                            }
                            this.confirm_completion(cx);
                        }))
                        .child(
                            div()
                                .w(px(14.))
                                .flex_none()
                                .text_size(px(10.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(color)
                                .child(badge),
                        )
                        .child(
                            div().flex_none().text_color(theme.text).child(
                                StyledText::new(SharedString::from(item.label.clone()))
                                    .with_highlights(ranges),
                            ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text_faint)
                                .child(item.detail.clone().unwrap_or_default()),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        });
        let count = menu.visible.len();
        let documentation = menu
            .visible
            .get(menu.selected)
            .and_then(|(index, _)| menu.items[*index].documentation.clone())
            .map(|text| blocks(&text))
            .filter(|b| !b.is_empty());
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(4.))
            .occlude()
            .child(
                div()
                    .w(px(420.))
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(6.))
                    .shadow_lg()
                    .overflow_hidden()
                    .font(font.clone())
                    .text_size(px(12.5))
                    .child(
                        uniform_list("completions", count, rows)
                            .track_scroll(&menu.scroll)
                            .h(px(MENU_ROW_HEIGHT * count.min(MENU_ROWS) as f32)),
                    ),
            )
            .children(
                documentation.map(|blocks| Self::render_blocks(&blocks, theme, font, px(360.))),
            )
            .into_any_element()
    }

    fn render_blocks(
        blocks: &[Block],
        theme: &Theme,
        font: gpui::Font,
        width: Pixels,
    ) -> AnyElement {
        div()
            .max_w(width)
            .max_h(px(320.))
            .overflow_hidden()
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.))
            .shadow_lg()
            .text_size(px(12.5))
            .text_color(theme.text)
            .children(blocks.iter().map(|block| {
                match block {
                    Block::Code(code) => div()
                        .font(font.clone())
                        .text_color(theme.syntax.function)
                        .whitespace_normal()
                        .child(code.clone())
                        .into_any_element(),
                    Block::Text(text) => div()
                        .text_color(theme.text_muted)
                        .child(text.clone())
                        .into_any_element(),
                    Block::Rule => div().h(px(1.)).bg(theme.border).into_any_element(),
                }
            }))
            .into_any_element()
    }

    fn render_hover(hover: &HoverPopup, theme: &Theme, font: gpui::Font) -> AnyElement {
        let bounds = hover.bounds.clone();
        let diagnostics = hover.diagnostics.iter().map(|(severity, message)| {
            let color = match severity {
                Severity::Error => theme.error,
                Severity::Warning => theme.warning,
                _ => theme.text_muted,
            };
            div()
                .text_color(color)
                .child(message.clone())
                .into_any_element()
        });
        let mut blocks_view: Vec<AnyElement> = diagnostics.collect();
        if !hover.blocks.is_empty() {
            if !blocks_view.is_empty() {
                blocks_view.push(div().h(px(1.)).bg(theme.border).into_any_element());
            }
            blocks_view.push(Self::render_blocks(&hover.blocks, theme, font, px(560.)));
        }
        div()
            .occlude()
            .max_w(px(580.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(if hover.diagnostics.is_empty() { 0. } else { 8. }))
            .when(!hover.diagnostics.is_empty(), |d| {
                d.bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(6.))
                    .shadow_lg()
            })
            .text_size(px(12.5))
            .child(
                gpui::canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(blocks_view)
            .into_any_element()
    }

    fn render_signature(info: &SignatureInfo, theme: &Theme, font: gpui::Font) -> AnyElement {
        let highlight = HighlightStyle {
            color: Some(theme.accent),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let label = StyledText::new(SharedString::from(info.label.clone()))
            .with_highlights(info.active_parameter.clone().map(|r| (r, highlight)));
        div()
            .occlude()
            .max_w(px(640.))
            .px(px(10.))
            .py(px(5.))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.))
            .shadow_lg()
            .font(font)
            .text_size(px(12.5))
            .text_color(theme.text)
            .child(label)
            .into_any_element()
    }
}

fn kind_badge(kind: CompletionKind, theme: &Theme) -> (&'static str, gpui::Hsla) {
    let s = &theme.syntax;
    match kind {
        CompletionKind::Function => ("f", s.function),
        CompletionKind::Method => ("m", s.function),
        CompletionKind::Field => ("p", s.property),
        CompletionKind::Variable => ("v", s.variable),
        CompletionKind::Type => ("T", s.type_),
        CompletionKind::Keyword => ("k", s.keyword),
        CompletionKind::Module => ("M", s.namespace),
        CompletionKind::Constant => ("c", s.constant),
        CompletionKind::Snippet => ("s", s.string),
        CompletionKind::Other => ("·", theme.text_muted),
    }
}

/// Binds the keys that act on popups. Added after the editor's own, so they win while a menu is open.
pub(crate) fn popup_bindings(cx: &mut App) {
    use gpui::KeyBinding;
    let menu = Some("Editor && showing_completions");
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousCompletion, menu),
        KeyBinding::new("down", SelectNextCompletion, menu),
        KeyBinding::new("pageup", SelectPreviousCompletionPage, menu),
        KeyBinding::new("pagedown", SelectNextCompletionPage, menu),
        KeyBinding::new("enter", ConfirmCompletion, menu),
        KeyBinding::new("tab", ConfirmCompletion, menu),
        KeyBinding::new("escape", DismissPopups, Some("Editor && showing_popups")),
        // A suggestion is taken with Tab, as Copilot's are, unless a completion list is open.
        KeyBinding::new(
            "tab",
            AcceptSuggestion,
            Some("Editor && has_suggestion && !showing_completions"),
        ),
        KeyBinding::new("ctrl-space", ShowCompletions, Some("Editor")),
        KeyBinding::new("f2", NextProblem, Some("Editor")),
        KeyBinding::new("shift-f2", PreviousProblem, Some("Editor")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("f1", ShowHover, Some("Editor")),
        KeyBinding::new("ctrl-j", ShowHover, Some("Editor")),
        KeyBinding::new("cmd-p", ShowSignatureHelp, Some("Editor")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-q", ShowHover, Some("Editor")),
        KeyBinding::new("ctrl-p", ShowSignatureHelp, Some("Editor")),
    ]);
}
