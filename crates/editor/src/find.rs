//! Find and Replace in a file, as IntelliJ has them: a bar over the editor that finds as you
//! type, marks every match, steps through them, and replaces one or all, with Match Case, Words
//! and Regex. The last search carries over to other files, and to Find Next once the bar's closed.

use crate::EditorView;
use gleipnir_index::find::{FindOptions, Regex, compile, replacement};
use gleipnir_text::{Selection, SelectionSet, TextEdit, UndoGrouping};
use gleipnir_ui::{Theme, text_input::TextInput, text_input::TextInputEvent};
use gpui::{
    App, AppContext as _, Context, Entity, Focusable, FontWeight, Global, KeyBinding, SharedString,
    Subscription, Window, actions, div, prelude::*, px,
};
use std::ops::Range;
use std::rc::Rc;

actions!(
    editor,
    [
        /// Opens the find bar, with the selection to find.
        Find,
        /// Opens the find bar with a replacement too.
        Replace,
        FindNext,
        FindPrevious,
        CloseFind,
        ToggleMatchCase,
        ToggleWholeWords,
        ToggleRegex,
        /// Replaces the match the bar's on, and moves to the next.
        ReplaceMatch,
        ReplaceAll,
        /// Puts a cursor on every match, and closes the bar.
        SelectAllMatches,
        /// Up and Down in the fields: earlier and later searches, or replacements.
        OlderSearch,
        NewerSearch,
        OlderReplacement,
        NewerReplacement,
    ]
);

/// Searches and replacements remembered for Up and Down.
const HISTORY_LIMIT: usize = 30;

/// Past this many matches, no more are marked: nobody steps through them one by one.
const MATCH_LIMIT: usize = 10_000;

pub(crate) fn bind_keys(cx: &mut App) {
    let editor = Some("Editor");
    let bar = Some("FindBar");
    let replace = Some("FindBar > ReplaceField");
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-f", Find, editor),
        KeyBinding::new("cmd-r", Replace, editor),
        KeyBinding::new("cmd-g", FindNext, editor),
        KeyBinding::new("cmd-shift-g", FindPrevious, editor),
        KeyBinding::new("cmd-f", Find, bar),
        KeyBinding::new("cmd-r", Replace, bar),
        KeyBinding::new("cmd-g", FindNext, bar),
        KeyBinding::new("cmd-shift-g", FindPrevious, bar),
        KeyBinding::new("ctrl-cmd-g", SelectAllMatches, bar),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-f", Find, editor),
        KeyBinding::new("ctrl-r", Replace, editor),
        KeyBinding::new("ctrl-f", Find, bar),
        KeyBinding::new("ctrl-r", Replace, bar),
        KeyBinding::new("ctrl-alt-shift-j", SelectAllMatches, bar),
    ]);
    cx.bind_keys([
        KeyBinding::new("f3", FindNext, editor),
        KeyBinding::new("shift-f3", FindPrevious, editor),
        KeyBinding::new("f3", FindNext, bar),
        KeyBinding::new("shift-f3", FindPrevious, bar),
        KeyBinding::new("enter", FindNext, bar),
        KeyBinding::new("shift-enter", FindPrevious, bar),
        KeyBinding::new("escape", CloseFind, bar),
        KeyBinding::new("alt-c", ToggleMatchCase, bar),
        KeyBinding::new("alt-w", ToggleWholeWords, bar),
        KeyBinding::new("alt-x", ToggleRegex, bar),
        KeyBinding::new("alt-a", ReplaceAll, bar),
        KeyBinding::new("enter", ReplaceMatch, replace),
        KeyBinding::new("up", OlderSearch, bar),
        KeyBinding::new("down", NewerSearch, bar),
        KeyBinding::new("up", OlderReplacement, replace),
        KeyBinding::new("down", NewerReplacement, replace),
    ]);
}

/// The last search in any file, which a new find bar starts with.
#[derive(Default)]
struct LastFind {
    query: String,
    options: FindOptions,
    /// Searches and replacements used, oldest first.
    searches: Vec<String>,
    replacements: Vec<String>,
}

fn remember(list: &mut Vec<String>, text: String) {
    if text.is_empty() {
        return;
    }
    list.retain(|t| *t != text);
    list.push(text);
    if list.len() > HISTORY_LIMIT {
        list.remove(0);
    }
}

impl Global for LastFind {}

pub(crate) struct FindState {
    pub(crate) query: Entity<TextInput>,
    replacement: Entity<TextInput>,
    options: FindOptions,
    /// Whether the bar's showing; matches are only marked while it is.
    pub(crate) open: bool,
    replacing: bool,
    /// Every match, in order, as byte ranges.
    pub(crate) matches: Rc<Vec<Range<usize>>>,
    /// The match the bar is on.
    pub(crate) current: Option<usize>,
    /// What's wrong with the pattern, when it isn't one.
    error: Option<String>,
    /// Where finding as you type starts from: the cursor when the bar opened, or the match moved
    /// to last.
    origin: usize,
    /// In Selection: only matches inside these, which follow edits.
    pub(crate) scope: Option<SelectionSet>,
    /// What was selected when the bar opened, which In Selection searches.
    selected_on_open: Option<SelectionSet>,
    /// Where Up and Down are in the histories: how many back from the newest.
    search_back: usize,
    replacement_back: usize,
    _subscriptions: Vec<Subscription>,
}

impl FindState {
    fn new(window: &mut Window, cx: &mut Context<EditorView>) -> Self {
        let query = cx.new(|cx| TextInput::new("Find", cx));
        let replacement = cx.new(|cx| TextInput::new("Replace", cx));
        let subscriptions =
            vec![
                cx.subscribe_in(&query, window, |this, _, _: &TextInputEvent, _, cx| {
                    this.search(cx);
                }),
            ];
        let last = cx.try_global::<LastFind>();
        Self {
            query,
            replacement,
            options: last.map(|l| l.options.clone()).unwrap_or_default(),
            open: false,
            replacing: false,
            matches: Rc::default(),
            current: None,
            error: None,
            origin: 0,
            scope: None,
            selected_on_open: None,
            search_back: 0,
            replacement_back: 0,
            _subscriptions: subscriptions,
        }
    }
}

impl EditorView {
    /// Opens the find bar, with a replacement field if `replace`. A selection on one line is what
    /// to find; one over several lines is where to find, In Selection; otherwise it's the last
    /// search, ready to type over.
    pub fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A text box, like a commit message, has nothing to search.
        if self.plain {
            return;
        }
        if self.find.is_none() {
            self.find = Some(FindState::new(window, cx));
        }
        let range = self.selections.primary().range();
        let text = self.buffer.read(cx).buffer().slice(range.clone());
        let selected = (!range.is_empty() && !text.contains('\n')).then_some(text);
        let several_lines = self.selections.iter().any(|s| {
            let range = s.range();
            !range.is_empty() && self.buffer.read(cx).buffer().slice(range).contains('\n')
        });
        let origin = range.start;
        let last = cx.try_global::<LastFind>().map(|l| l.query.clone());
        let already_open = self.is_finding();
        let selections = self.selections.clone();
        let find = self.find.as_mut().expect("made above");
        find.open = true;
        find.replacing = replace;
        find.origin = origin;
        find.search_back = 0;
        find.replacement_back = 0;
        // Pressed again with the bar open, the selection is the match: the scope stays.
        if !already_open {
            find.selected_on_open = several_lines.then(|| selections.clone());
            find.scope = find.selected_on_open.clone();
        }
        let query = find.query.clone();
        let current = query.read(cx).text();
        let text = selected.or_else(|| current.is_empty().then_some(last).flatten());
        let focus = query.read(cx).focus_handle(cx);
        query.update(cx, |input, cx| input.set_text(&text.unwrap_or(current), cx));
        window.focus(&focus, cx);
        self.search(cx);
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_search(cx);
        if let Some(find) = &mut self.find {
            find.open = false;
            find.scope = None;
            find.matches = Rc::default();
            find.current = None;
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub fn is_finding(&self) -> bool {
        self.find.as_ref().is_some_and(|f| f.open)
    }

    /// What's found, as `(current, count)`, for tests and the bar.
    pub fn find_matches(&self) -> (Option<usize>, usize) {
        self.find
            .as_ref()
            .map_or((None, 0), |f| (f.current, f.matches.len()))
    }

    /// The query and the options, compiled; none with nothing to find or a bad pattern.
    fn find_regex(&mut self, cx: &App) -> Option<Regex> {
        let find = self.find.as_mut()?;
        let query = find.query.read(cx).text();
        if query.is_empty() {
            find.error = None;
            return None;
        }
        match compile(&query, &find.options) {
            Ok(regex) => {
                find.error = None;
                Some(regex)
            }
            Err(error) => {
                find.error = Some(error.to_string());
                None
            }
        }
    }

    /// Finds every match again, as after typing, and moves to the first from where finding
    /// started.
    fn search(&mut self, cx: &mut Context<Self>) {
        self.refresh_matches(cx);
        let Some(find) = &mut self.find else { return };
        if let Some(query) = Some(find.query.read(cx).text()).filter(|q| !q.is_empty()) {
            let options = find.options.clone();
            let last = cx.default_global::<LastFind>();
            last.query = query;
            last.options = options;
        }
        let origin = find.origin;
        let first = find
            .matches
            .iter()
            .position(|m| m.start >= origin)
            .or((!find.matches.is_empty()).then_some(0));
        self.go_to_match(first, cx);
    }

    /// Finds every match in the text as it is, keeping the bar on the match it was on, or the
    /// next one.
    pub(crate) fn refresh_matches(&mut self, cx: &mut Context<Self>) {
        if !self.is_finding() {
            return;
        }
        let regex = self.find_regex(cx);
        let text = self.buffer.read(cx).buffer().text();
        let matches: Vec<Range<usize>> = regex
            .map(|regex| {
                regex
                    .find_iter(&text)
                    .filter(|m| !m.range().is_empty())
                    .take(MATCH_LIMIT)
                    .map(|m| m.range())
                    .collect()
            })
            .unwrap_or_default();
        let find = self.find.as_mut().expect("finding");
        let matches: Vec<Range<usize>> = match &find.scope {
            Some(scope) => matches
                .into_iter()
                .filter(|m| {
                    scope.iter().any(|s| {
                        let range = s.range();
                        range.start <= m.start && m.end <= range.end
                    })
                })
                .collect(),
            None => matches,
        };
        let was = find
            .current
            .and_then(|i| find.matches.get(i))
            .map(|m| m.start);
        find.current = was.and_then(|start| matches.iter().position(|m| m.start >= start));
        find.matches = Rc::new(matches);
        cx.notify();
    }

    /// Selects a match, and shows it.
    fn go_to_match(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        find.current = index;
        let Some(range) = index.and_then(|i| find.matches.get(i)).cloned() else {
            cx.notify();
            return;
        };
        find.origin = range.start;
        self.selections = SelectionSet::single(Selection::new(range.start, range.end));
        self.did_move(cx);
        cx.notify();
    }

    /// The next or previous match, around the end. With the bar closed, the last search again.
    fn find_next(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_finding() {
            // The last search again from the cursor, with the bar out of the way.
            let last = cx
                .try_global::<LastFind>()
                .and_then(|l| compile(&l.query, &l.options).ok());
            let Some(regex) = last else {
                self.open_find(false, window, cx);
                return;
            };
            let text = self.buffer.read(cx).buffer().text();
            let at = self.selections.primary().range();
            let mut found = regex.find_iter(&text).filter(|m| !m.range().is_empty());
            let found = if forward {
                let all: Vec<_> = found.map(|m| m.range()).collect();
                all.iter()
                    .find(|m| m.start >= at.end && m.start != at.start)
                    .or(all.first())
                    .cloned()
            } else {
                let mut before = None;
                let mut last = None;
                for m in found.by_ref() {
                    if m.end() <= at.start {
                        before = Some(m.range());
                    }
                    last = Some(m.range());
                }
                before.or(last)
            };
            if let Some(range) = found {
                self.selections = SelectionSet::single(Selection::new(range.start, range.end));
                self.did_move(cx);
                cx.notify();
            }
            return;
        }
        self.remember_search(cx);
        let find = self.find.as_ref().expect("finding");
        if find.matches.is_empty() {
            return;
        }
        let count = find.matches.len();
        let next = match find.current {
            Some(i) if forward => (i + 1) % count,
            Some(i) => (i + count - 1) % count,
            None => {
                self.step_from_cursor(forward, cx);
                return;
            }
        };
        self.go_to_match(Some(next), cx);
    }

    /// Moves to the first match after the cursor, or before it; false if there's none.
    fn step_from_cursor(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let range = self.selections.primary().range();
        let Some(find) = &self.find else { return false };
        let matches = &find.matches;
        if matches.is_empty() {
            return false;
        }
        let index = if forward {
            matches
                .iter()
                .position(|m| m.start >= range.end && m.start != range.start)
                .unwrap_or(0)
        } else {
            matches
                .iter()
                .rposition(|m| m.end <= range.start)
                .unwrap_or(matches.len() - 1)
        };
        self.go_to_match(Some(index), cx);
        true
    }

    /// Keeps what's in the fields for Up and Down.
    fn remember_search(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &self.find else { return };
        let (query, with) = (find.query.read(cx).text(), find.replacement.read(cx).text());
        let replacing = find.replacing;
        let last = cx.default_global::<LastFind>();
        remember(&mut last.searches, query);
        if replacing {
            remember(&mut last.replacements, with);
        }
    }

    /// Up and Down in a field: an earlier entry from its history, or a later one.
    fn step_history(&mut self, replacement: bool, older: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        let list = cx.try_global::<LastFind>().map_or_else(Vec::new, |l| {
            if replacement {
                l.replacements.clone()
            } else {
                l.searches.clone()
            }
        });
        let (back, input) = if replacement {
            (&mut find.replacement_back, find.replacement.clone())
        } else {
            (&mut find.search_back, find.query.clone())
        };
        let current = input.read(cx).text();
        // Starting from what's typed: the newest entry unless that's it already.
        let start = list
            .iter()
            .rev()
            .position(|t| *t == current)
            .map_or(0, |i| i + 1);
        let next = if older {
            (*back).max(start) + 1
        } else {
            back.saturating_sub(1)
        };
        if next == 0 || next > list.len() {
            return;
        }
        *back = next;
        let text = list[list.len() - next].clone();
        input.update(cx, |input, cx| input.set_text(&text, cx));
    }

    /// In Selection: finds only in what was selected when the bar opened.
    fn toggle_in_selection(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        find.scope = match find.scope {
            Some(_) => None,
            None => find.selected_on_open.clone(),
        };
        self.search(cx);
    }

    fn toggle_option(&mut self, toggle: impl FnOnce(&mut FindOptions), cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        toggle(&mut find.options);
        self.search(cx);
    }

    /// What a match is replaced with: the replacement, with `$1` and the like filled in from the
    /// match when finding a pattern.
    fn replacement_for(&mut self, range: &Range<usize>, cx: &mut Context<Self>) -> Option<String> {
        let regex = self.find_regex(cx)?;
        let find = self.find.as_ref()?;
        let template = find.replacement.read(cx).text();
        let text = self.buffer.read(cx).buffer().text();
        Some(replacement(
            &regex,
            &text,
            range.clone(),
            &template,
            &find.options,
        ))
    }

    fn replace_match(&mut self, cx: &mut Context<Self>) {
        self.remember_search(cx);
        let Some(find) = &self.find else { return };
        let Some(range) = find.current.and_then(|i| find.matches.get(i)).cloned() else {
            return;
        };
        let Some(with) = self.replacement_for(&range, cx) else {
            return;
        };
        let after = range.start + with.len();
        self.edit(cx, |edit| {
            edit.apply_edits(
                vec![TextEdit::new(range, with)],
                Some(after),
                UndoGrouping::Distinct,
            )
        });
        if let Some(find) = &mut self.find {
            find.origin = after;
            find.current = None;
        }
        self.refresh_matches(cx);
        let next = self.find.as_ref().and_then(|find| {
            find.matches
                .iter()
                .position(|m| m.start >= after)
                .or((!find.matches.is_empty()).then_some(0))
        });
        self.go_to_match(next, cx);
    }

    /// Replaces every match, as one step undo takes back.
    fn replace_all(&mut self, cx: &mut Context<Self>) {
        self.remember_search(cx);
        let Some(find) = &self.find else { return };
        let matches = find.matches.clone();
        let mut edits = Vec::with_capacity(matches.len());
        for range in matches.iter() {
            let Some(with) = self.replacement_for(range, cx) else {
                return;
            };
            edits.push(TextEdit::new(range.clone(), with));
        }
        if edits.is_empty() {
            return;
        }
        self.edit(cx, |edit| {
            edit.apply_edits(edits, None, UndoGrouping::Distinct)
        });
        self.buffer
            .update(cx, |buffer, _| buffer.break_undo_group());
        self.refresh_matches(cx);
        cx.notify();
    }

    fn select_all_matches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = &self.find else { return };
        if find.matches.is_empty() {
            return;
        }
        let primary = find.current.unwrap_or(0);
        self.selections = SelectionSet::from_vec(
            find.matches
                .iter()
                .map(|m| Selection::new(m.start, m.end))
                .collect(),
            primary,
        );
        self.close_find(window, cx);
        self.did_move(cx);
    }

    /// The bar over the editor, when it's open.
    pub(crate) fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let find = self.find.as_ref().filter(|f| f.open)?;
        let theme = cx.global::<Theme>().clone();
        let query_text = find.query.read(cx).text();
        let nothing = !query_text.is_empty() && find.matches.is_empty();
        let count: SharedString = if let Some(error) = &find.error {
            error.clone().into()
        } else if query_text.is_empty() {
            "".into()
        } else if find.matches.is_empty() {
            "0 results".into()
        } else {
            let more = if find.matches.len() == MATCH_LIMIT {
                "+"
            } else {
                ""
            };
            match find.current {
                Some(i) => format!("{}/{}{more}", i + 1, find.matches.len()),
                None => format!("{}{more} results", find.matches.len()),
            }
            .into()
        };
        let field = |input: &Entity<TextInput>, id: &'static str, bad: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex_1()
                .min_w(px(120.))
                .max_w(px(420.))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .border_1()
                .border_color(if bad { theme.error } else { theme.border })
                .bg(theme.editor_background)
                .child(input.clone())
        };
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(5.))
                .py(px(1.))
                .rounded(px(4.))
                .cursor_pointer()
                .font_weight(FontWeight::SEMIBOLD)
                .when(on, |d| d.bg(theme.selection).text_color(theme.text))
                .when(!on, |d| {
                    d.text_color(theme.text_muted).hover(|s| s.bg(theme.hover))
                })
                .child(label)
        };
        let button = |id: &'static str, label: &'static str, enabled: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .text_color(if enabled {
                    theme.text
                } else {
                    theme.text_faint
                })
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme.hover)))
                .child(label)
        };
        let has_matches = !find.matches.is_empty();
        let options = &find.options;
        let find_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .child(field(
                &find.query,
                "find-query",
                nothing || find.error.is_some(),
            ))
            .child(
                toggle("find-case", "Cc", options.case_sensitive).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.toggle_option(|o| o.case_sensitive = !o.case_sensitive, cx)
                    },
                )),
            )
            .child(
                toggle("find-words", "W", options.whole_words).on_click(cx.listener(
                    |this, _, _, cx| this.toggle_option(|o| o.whole_words = !o.whole_words, cx),
                )),
            )
            .child(toggle("find-regex", ".*", options.regex).on_click(
                cx.listener(|this, _, _, cx| this.toggle_option(|o| o.regex = !o.regex, cx)),
            ))
            .when(find.selected_on_open.is_some(), |d| {
                d.child(
                    toggle("find-in-selection", "In Selection", find.scope.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_in_selection(cx))),
                )
            })
            .child(
                div()
                    .min_w(px(70.))
                    .px(px(6.))
                    .text_color(if nothing || find.error.is_some() {
                        theme.error
                    } else {
                        theme.text_muted
                    })
                    .child(count),
            )
            .child(
                button("find-previous", "↑", has_matches)
                    .on_click(cx.listener(|this, _, window, cx| this.find_next(false, window, cx))),
            )
            .child(
                button("find-next", "↓", has_matches)
                    .on_click(cx.listener(|this, _, window, cx| this.find_next(true, window, cx))),
            )
            .child(
                button("find-select-all", "Select All", has_matches).on_click(
                    cx.listener(|this, _, window, cx| this.select_all_matches(window, cx)),
                ),
            )
            .child(div().flex_1())
            .child(
                button("find-close", "✕", true)
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            );
        let replace_row = find.replacing.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .key_context("ReplaceField")
                        .flex_1()
                        .min_w(px(120.))
                        .max_w(px(420.))
                        .flex()
                        .child(field(&find.replacement, "find-replacement", false)),
                )
                .child(
                    button(
                        "find-replace",
                        "Replace",
                        has_matches && find.current.is_some(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.replace_match(cx))),
                )
                .child(
                    button("find-replace-all", "Replace All", has_matches)
                        .on_click(cx.listener(|this, _, _, cx| this.replace_all(cx))),
                )
                .child(
                    toggle("find-preserve-case", "Preserve Case", options.preserve_case).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.toggle_option(|o| o.preserve_case = !o.preserve_case, cx)
                        }),
                    ),
                )
        });
        Some(
            div()
                .key_context("FindBar")
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(8.))
                .py(px(5.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(12.5))
                .text_color(theme.text)
                .on_action(
                    cx.listener(|this, _: &Find, window, cx| this.open_find(false, window, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &Replace, window, cx| this.open_find(true, window, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &FindNext, window, cx| this.find_next(true, window, cx)),
                )
                .on_action(cx.listener(|this, _: &FindPrevious, window, cx| {
                    this.find_next(false, window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &CloseFind, window, cx| this.close_find(window, cx)),
                )
                .on_action(cx.listener(|this, _: &ToggleMatchCase, _, cx| {
                    this.toggle_option(|o| o.case_sensitive = !o.case_sensitive, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleWholeWords, _, cx| {
                    this.toggle_option(|o| o.whole_words = !o.whole_words, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleRegex, _, cx| {
                    this.toggle_option(|o| o.regex = !o.regex, cx)
                }))
                .on_action(cx.listener(|this, _: &ReplaceMatch, _, cx| this.replace_match(cx)))
                .on_action(cx.listener(|this, _: &ReplaceAll, _, cx| this.replace_all(cx)))
                .on_action(cx.listener(|this, _: &SelectAllMatches, window, cx| {
                    this.select_all_matches(window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &OlderSearch, _, cx| this.step_history(false, true, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &NewerSearch, _, cx| this.step_history(false, false, cx)),
                )
                .on_action(cx.listener(|this, _: &OlderReplacement, _, cx| {
                    this.step_history(true, true, cx)
                }))
                .on_action(cx.listener(|this, _: &NewerReplacement, _, cx| {
                    this.step_history(true, false, cx)
                }))
                .child(find_row)
                .children(replace_row),
        )
    }

    /// The editor's own handlers for the find keys, for when the bar isn't focused.
    pub(crate) fn on_find_actions<E: InteractiveElement>(
        &self,
        root: E,
        cx: &mut Context<Self>,
    ) -> E {
        root.on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(false, window, cx)))
            .on_action(
                cx.listener(|this, _: &Replace, window, cx| this.open_find(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FindNext, window, cx| this.find_next(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FindPrevious, window, cx| this.find_next(false, window, cx)),
            )
    }
}
