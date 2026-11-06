//! A popup list to choose from: where to go when there's more than one place, which fix to apply,
//! which symbol in the file. Long lists get a filter field.

use gleipnir_index::fuzzy::{Pattern, fuzzy_match};
use gleipnir_ui::Theme;
use gleipnir_ui::list::ListNav;
use gleipnir_ui::text_input::{TextInput, TextInputEvent};
use gleipnir_workspace::ModalView;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, KeyBinding, SharedString, StyledText, Subscription, Window, actions, div,
    prelude::*, px, uniform_list,
};
use std::ops::Range;

actions!(picker, [SelectNext, SelectPrevious, Confirm, Dismiss]);

const ROW_HEIGHT: f32 = 26.;
const VISIBLE_ROWS: usize = 14;
/// Lists longer than this get a filter field.
const FILTER_FROM: usize = 8;

pub fn bind_keys(cx: &mut App) {
    let context = Some("Picker");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

#[derive(Clone, Debug, Default)]
pub struct PickerItem {
    pub label: String,
    /// Shown faintly after the label, like a file and line.
    pub detail: String,
    /// Nesting, for showing a tree of symbols.
    pub indent: usize,
}

type OnConfirm = Box<dyn FnOnce(usize, &mut Window, &mut App)>;

pub struct Picker {
    focus_handle: FocusHandle,
    title: SharedString,
    input: Option<Entity<TextInput>>,
    items: Vec<PickerItem>,
    /// Item indices shown, with matched label bytes.
    visible: Vec<(usize, Vec<usize>)>,
    nav: ListNav,
    on_confirm: Option<OnConfirm>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for Picker {}
impl ModalView for Picker {}

impl Focusable for Picker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.input {
            Some(input) => input.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Picker {
    pub fn new(
        title: impl Into<SharedString>,
        items: Vec<PickerItem>,
        selected: usize,
        on_confirm: impl FnOnce(usize, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let input = (items.len() > FILTER_FROM).then(|| {
            let input = cx.new(|cx| TextInput::new("Type to filter", cx));
            subscriptions
                .push(cx.subscribe(&input, |this, _, _: &TextInputEvent, cx| this.filter(cx)));
            input
        });
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            title: title.into(),
            input,
            items,
            visible: Vec::new(),
            nav: ListNav::new(),
            on_confirm: Some(Box::new(on_confirm)),
            _subscriptions: subscriptions,
        };
        this.filter(cx);
        this.nav
            .select(selected.min(this.visible.len().saturating_sub(1)));
        this
    }

    pub fn labels(&self) -> Vec<String> {
        self.visible
            .iter()
            .map(|(i, _)| self.items[*i].label.clone())
            .collect()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self
            .input
            .as_ref()
            .map(|i| i.read(cx).text())
            .unwrap_or_default();
        let pattern = Pattern::new(&query);
        self.visible = if pattern.is_empty() {
            (0..self.items.len()).map(|i| (i, Vec::new())).collect()
        } else {
            let mut found: Vec<_> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    fuzzy_match(&pattern, &item.label).map(|m| (m.score, i, m.positions))
                })
                .collect();
            found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            found.into_iter().map(|(_, i, p)| (i, p)).collect()
        };
        self.nav.set_count(self.visible.len());
        self.nav.select(0);
        cx.notify();
    }

    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&(index, _)) = self.nav.selected().and_then(|s| self.visible.get(s)) else {
            return;
        };
        let Some(on_confirm) = self.on_confirm.take() else {
            return;
        };
        cx.emit(DismissEvent);
        on_confirm(index, window, cx);
    }

    fn render_row(&self, row: usize, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (index, positions) = &self.visible[row];
        let item = &self.items[*index];
        let selected = self.nav.selected() == Some(row);
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
            .id(("picker-row", row))
            .debug_selector(|| format!("picker-row-{row}"))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(10.))
            .pl(px(10. + 14. * item.indent as f32))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .when(selected, |r| r.bg(theme.selection))
            .hover(|r| r.bg(theme.hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.nav.select(row);
                this.confirm(window, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(theme.text)
                    .child(
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
                    .child(item.detail.clone()),
            )
            .into_any_element()
    }
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let count = self.visible.len();
        let rows = cx.processor(|this, range: Range<usize>, _, cx| {
            let theme = cx.global::<Theme>().clone();
            range
                .map(|row| this.render_row(row, &theme, cx))
                .collect::<Vec<_>>()
        });
        div()
            .key_context("Picker")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(620.))
            .flex()
            .flex_col()
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(theme.ui_font_size))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.nav.select_next();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.nav.select_previous();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_color(theme.text_muted)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            )
            .children(self.input.clone().map(|input| {
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(input)
            }))
            .child(if count == 0 {
                div()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.text_faint)
                    .child("Nothing found")
                    .into_any_element()
            } else {
                uniform_list("picker", count, rows)
                    .track_scroll(self.nav.scroll_handle())
                    .h(px(ROW_HEIGHT * count.min(VISIBLE_ROWS) as f32))
                    .into_any_element()
            })
    }
}
