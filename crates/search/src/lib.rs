//! Search Everywhere: double Shift finds any file, type, symbol or command.

mod double_tap;

pub use double_tap::{DOUBLE_TAP_WINDOW, DoubleTap};

use gleipnir_core::{IndexStatus, Project, ProjectEvent};
use gleipnir_index::{
    Cancel, FileHit, Query, SymbolFilter, SymbolHit, fuzzy, search_files, search_symbols,
};
use gleipnir_syntax::SymbolKind;
use gleipnir_ui::{FileIcon, Icon, ListNav, TextInput, TextInputEvent, Theme, file_icon};
use gleipnir_workspace::{ActionInfo, EmptyHint, ModalView, Workspace, list_actions};
use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, HighlightStyle, KeyBinding, ModifiersChangedEvent, Render, SharedString,
    StyledText, Subscription, Task, WeakEntity, Window, actions, div, prelude::*, px, svg,
    uniform_list,
};
use std::cell::RefCell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

actions!(
    search,
    [
        SearchEverywhere,
        GoToFile,
        GoToClass,
        GoToSymbol,
        FindAction,
        RecentFiles,
        SelectNext,
        SelectPrevious,
        PageDown,
        PageUp,
        NextTab,
        PreviousTab,
        Confirm,
        Dismiss,
    ]
);

/// How many of each kind the All tab shows. More are a Tab press away.
const ALL_TAB_PER_SECTION: usize = 6;
const RESULT_LIMIT: usize = 200;
const ROW_HEIGHT: f32 = 28.;
const VISIBLE_ROWS: usize = 14;

pub fn bind_keys(cx: &mut App) {
    let popup = Some("SearchEverywhere");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, popup),
        KeyBinding::new("down", SelectNext, popup),
        KeyBinding::new("pageup", PageUp, popup),
        KeyBinding::new("pagedown", PageDown, popup),
        KeyBinding::new("tab", NextTab, popup),
        KeyBinding::new("shift-tab", PreviousTab, popup),
        KeyBinding::new("enter", Confirm, popup),
        KeyBinding::new("escape", Dismiss, popup),
    ]);
    // IntelliJ's own shortcuts for these.
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-shift-o", GoToFile, None),
        KeyBinding::new("cmd-o", GoToClass, None),
        KeyBinding::new("cmd-alt-o", GoToSymbol, None),
        KeyBinding::new("cmd-shift-a", FindAction, None),
        KeyBinding::new("cmd-e", RecentFiles, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-n", GoToFile, None),
        KeyBinding::new("ctrl-n", GoToClass, None),
        KeyBinding::new("ctrl-alt-shift-n", GoToSymbol, None),
        KeyBinding::new("ctrl-shift-a", FindAction, None),
        KeyBinding::new("ctrl-e", RecentFiles, None),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    All,
    Classes,
    Files,
    Symbols,
    Actions,
    /// Not one of the tabs, but the same popup over recent files only.
    Recent,
}

impl Tab {
    const CYCLE: [Tab; 5] = [
        Tab::All,
        Tab::Classes,
        Tab::Files,
        Tab::Symbols,
        Tab::Actions,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Classes => "Classes",
            Tab::Files => "Files",
            Tab::Symbols => "Symbols",
            Tab::Actions => "Actions",
            Tab::Recent => "Recent Files",
        }
    }
}

/// Adds search to a workspace: the actions, double Shift, and hints in the empty pane.
pub fn add_to_workspace(workspace: &mut Workspace, _: &mut Window, _: &mut Context<Workspace>) {
    workspace.register_action(|w, _: &SearchEverywhere, window, cx| open(w, Tab::All, window, cx));
    workspace.register_action(|w, _: &GoToFile, window, cx| open(w, Tab::Files, window, cx));
    workspace.register_action(|w, _: &GoToClass, window, cx| open(w, Tab::Classes, window, cx));
    workspace.register_action(|w, _: &GoToSymbol, window, cx| open(w, Tab::Symbols, window, cx));
    workspace.register_action(|w, _: &FindAction, window, cx| open(w, Tab::Actions, window, cx));
    workspace.register_action(|w, _: &RecentFiles, window, cx| open(w, Tab::Recent, window, cx));

    let shift = Rc::new(RefCell::new(DoubleTap::default()));
    let keys = shift.clone();
    workspace.add_root_hook(move |root, cx| {
        let shift = shift.clone();
        let keys = keys.clone();
        root.on_modifiers_changed(cx.listener(
            move |workspace, event: &ModifiersChangedEvent, window, cx| {
                let m = event.modifiers;
                let only_shift = m.shift && !m.control && !m.alt && !m.platform && !m.function;
                if shift
                    .borrow_mut()
                    .modifiers_changed(m.shift, only_shift, Instant::now())
                {
                    open_all_from_double_shift(workspace, window, cx);
                }
            },
        ))
        .capture_key_down(move |_, _, _| keys.borrow_mut().key_pressed())
    });

    workspace.add_empty_hint("Search everywhere", EmptyHint::Text("Double Shift"));
    workspace.add_empty_hint("Go to file", EmptyHint::Action(Box::new(GoToFile)));
    workspace.add_empty_hint("Recent files", EmptyHint::Action(Box::new(RecentFiles)));
}

/// Double Shift opens on All, or switches an open popup back to All, as IntelliJ does.
fn open_all_from_double_shift(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(search) = workspace.active_modal::<SearchPopup>() {
        search.update(cx, |search, cx| search.set_tab(Tab::All, cx));
    } else {
        open(workspace, Tab::All, window, cx);
    }
}

/// Opens the popup on a tab. The same shortcut again closes it; a different one switches tab.
fn open(workspace: &mut Workspace, tab: Tab, window: &mut Window, cx: &mut Context<Workspace>) {
    if let Some(search) = workspace.active_modal::<SearchPopup>() {
        if search.read(cx).tab == tab {
            workspace.dismiss_modal(window, cx);
        } else {
            search.update(cx, |search, cx| search.set_tab(tab, cx));
        }
        return;
    }
    let handle = cx.weak_entity();
    let project = workspace.project().cloned();
    let recent = workspace.recent_files().to_vec();
    let recent_actions = workspace.recent_actions().to_vec();
    // Taken before the popup takes focus, so availability reflects where the user was.
    let actions = available_actions(window, cx);
    workspace.toggle_modal::<SearchPopup>(window, cx, move |window, cx| {
        SearchPopup::new(
            handle,
            project,
            recent,
            recent_actions,
            actions,
            tab,
            window,
            cx,
        )
    });
}

fn item_key(item: &Item) -> String {
    match item {
        Item::Header(label) => format!("header {label}"),
        Item::File { absolute, .. } => format!("file {}", absolute.display()),
        Item::Symbol(hit) => format!(
            "symbol {} {} {}",
            hit.entry.path, hit.entry.symbol.line, hit.entry.symbol.name
        ),
        Item::Action { entry, .. } => format!("action {}", entry.info.name),
    }
}

fn symbol_kind(kind: gleipnir_lsp::lsp_types::SymbolKind) -> Option<SymbolKind> {
    use gleipnir_lsp::lsp_types::SymbolKind as K;
    Some(match kind {
        K::CLASS | K::STRUCT | K::OBJECT | K::TYPE_PARAMETER => SymbolKind::Class,
        K::INTERFACE => SymbolKind::Interface,
        K::ENUM => SymbolKind::Enum,
        K::FUNCTION | K::CONSTRUCTOR | K::OPERATOR => SymbolKind::Function,
        K::METHOD => SymbolKind::Method,
        K::CONSTANT => SymbolKind::Constant,
        K::VARIABLE | K::FIELD | K::PROPERTY => SymbolKind::Variable,
        K::MODULE | K::NAMESPACE | K::PACKAGE => SymbolKind::Module,
        K::ENUM_MEMBER => SymbolKind::EnumMember,
        _ => return None,
    })
}

/// Adds what language servers found and the index didn't, matched the same way as the rest.
/// Returns whether anything was added.
fn merge_server_symbols(
    found: &mut Found,
    hits: Vec<gleipnir_core::SymbolHit>,
    text: &str,
    root: Option<&Path>,
    tab: Tab,
) -> bool {
    let pattern = fuzzy::Pattern::new(text);
    let mut added = false;
    for hit in hits {
        let Some(kind) = symbol_kind(hit.kind) else {
            continue;
        };
        let Some(matched) = fuzzy::fuzzy_match(&pattern, &hit.name) else {
            continue;
        };
        let path: Arc<str> = root
            .and_then(|r| hit.location.path.strip_prefix(r).ok())
            .unwrap_or(&hit.location.path)
            .to_string_lossy()
            .into();
        let line = hit.location.start.line;
        let list = if kind.is_type() && tab != Tab::Symbols {
            &mut found.classes
        } else {
            &mut found.symbols
        };
        if list.len() >= RESULT_LIMIT
            || found
                .classes
                .iter()
                .chain(found.symbols.iter())
                .any(|h| h.entry.path == path && h.entry.symbol.line == line)
        {
            continue;
        }
        let list = if kind.is_type() && tab != Tab::Symbols {
            &mut found.classes
        } else {
            &mut found.symbols
        };
        list.push(SymbolHit {
            entry: gleipnir_index::SymbolEntry {
                path,
                symbol: gleipnir_syntax::Symbol {
                    name: hit.name,
                    kind,
                    container: hit.container,
                    line,
                    // Servers count UTF-16 units; for the identifiers this is used on, that's bytes.
                    column: hit.location.start.character,
                },
            },
            score: matched.score,
            positions: matched.positions,
        });
        added = true;
    }
    added
}

/// Where `name` is on a line, taking the occurrence nearest `column` when it's there more than
/// once.
fn name_column(line: &str, name: &str, column: usize) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    line.match_indices(name)
        .map(|(at, _)| at)
        .filter(|&at| {
            let before = line[..at].chars().next_back();
            let after = line[at + name.len()..].chars().next();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            !word(before) && !word(after)
        })
        .min_by_key(|&at| at.abs_diff(column))
}

#[derive(Clone, Debug)]
struct ActionEntry {
    info: ActionInfo,
    /// Whether anything where focus was can handle it. Unavailable ones show dimmed and last.
    available: bool,
}

fn available_actions(window: &Window, cx: &App) -> Vec<ActionEntry> {
    let focused = window.focused(cx);
    list_actions(window, cx)
        .into_iter()
        .map(|info| {
            let available = match (&focused, cx.build_action(&info.name, None)) {
                (Some(focus), Ok(action)) => window.is_action_available_in(action.as_ref(), focus),
                (None, Ok(action)) => window.is_action_available(action.as_ref(), cx),
                _ => false,
            };
            ActionEntry { info, available }
        })
        .collect()
}

#[derive(Clone, Debug)]
enum Item {
    Header(&'static str),
    File {
        relative: Arc<str>,
        absolute: PathBuf,
        positions: Vec<usize>,
    },
    Symbol(SymbolHit),
    Action {
        entry: ActionEntry,
        positions: Vec<usize>,
    },
}

impl Item {
    fn is_selectable(&self) -> bool {
        !matches!(self, Item::Header(_))
    }
}

/// Results from the background part of a search.
#[derive(Clone)]
struct Found {
    files: Vec<FileHit>,
    classes: Vec<SymbolHit>,
    symbols: Vec<SymbolHit>,
}

pub struct SearchPopup {
    focus_handle: FocusHandle,
    input: Entity<TextInput>,
    tab: Tab,
    workspace: WeakEntity<Workspace>,
    project: Option<Entity<Project>>,
    recent: Vec<PathBuf>,
    recent_actions: Vec<String>,
    actions: Vec<ActionEntry>,
    items: Vec<Item>,
    nav: ListNav,
    query: Query,
    generation: u64,
    searching: bool,
    cancel: Cancel,
    _search: Option<Task<()>>,
    _refresh: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for SearchPopup {}
impl ModalView for SearchPopup {}

impl Focusable for SearchPopup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Typing goes straight into the field.
        self.input.read(cx).focus_handle(cx)
    }
}

impl SearchPopup {
    #[allow(clippy::too_many_arguments)]
    fn new(
        workspace: WeakEntity<Workspace>,
        project: Option<Entity<Project>>,
        recent: Vec<PathBuf>,
        recent_actions: Vec<String>,
        actions: Vec<ActionEntry>,
        tab: Tab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new("Search files, classes, symbols and actions", cx));
        let mut subscriptions = vec![cx.subscribe(&input, |this, _, _: &TextInputEvent, cx| {
            this.search(cx);
        })];
        if let Some(project) = &project {
            // Results fill in as indexing gets further.
            subscriptions.push(cx.subscribe(project, |this, _, event, cx| {
                if matches!(
                    event,
                    ProjectEvent::SymbolsChanged | ProjectEvent::FileListChanged
                ) {
                    this.search(cx);
                }
            }));
        }
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            input,
            tab,
            workspace,
            project,
            recent,
            recent_actions,
            actions,
            items: Vec::new(),
            nav: ListNav::new(),
            query: Query::parse(""),
            generation: 0,
            searching: false,
            cancel: Cancel::new(),
            _search: None,
            _refresh: None,
            _subscriptions: subscriptions,
        };
        this.search(cx);
        this.keep_progress_current(cx);
        this
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.search(cx);
        }
    }

    fn cycle_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let index = Tab::CYCLE.iter().position(|t| *t == self.tab).unwrap_or(0);
        let count = Tab::CYCLE.len();
        let next = if forward {
            index + 1
        } else {
            index + count - 1
        };
        self.set_tab(Tab::CYCLE[next % count], cx);
    }

    /// Repaints while indexing, so the progress count moves.
    fn keep_progress_current(&mut self, cx: &mut Context<Self>) {
        self._refresh = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let indexing = this.update(cx, |this, cx| {
                    cx.notify();
                    this.status(cx)
                        .is_some_and(|s| s.indexing.is_some() || s.scanning)
                });
                if !matches!(indexing, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn status(&self, cx: &App) -> Option<IndexStatus> {
        Some(self.project.as_ref()?.read(cx).status())
    }

    fn root(&self, cx: &App) -> Option<PathBuf> {
        Some(self.project.as_ref()?.read(cx).root().to_path_buf())
    }

    /// Starts a search for the current text and tab. Files and symbols are searched off the UI
    /// thread; a newer search cancels an older one still running.
    fn search(&mut self, cx: &mut Context<Self>) {
        self.query = Query::parse(&self.input.read(cx).text());
        self.generation += 1;
        self.cancel.cancel();
        self.cancel = Cancel::new();
        let generation = self.generation;
        let text = self.query.text.clone();
        let tab = self.tab;

        if text.trim().is_empty() {
            self.show(self.empty_query_items(cx), cx);
            return;
        }
        if tab == Tab::Actions || tab == Tab::Recent {
            self.show(self.instant_items(&text, cx), cx);
            return;
        }

        let (files, symbols) = match &self.project {
            Some(project) => project.update(cx, |p, _| (p.files(), p.symbols())),
            None => (Arc::default(), Arc::default()),
        };
        let cancel = self.cancel.clone();
        let wants_files = matches!(tab, Tab::All | Tab::Files);
        let wants_classes = matches!(tab, Tab::All | Tab::Classes);
        let wants_symbols = matches!(tab, Tab::All | Tab::Symbols);
        let search_text = text.clone();
        self.searching = true;
        let work = cx.background_spawn(async move {
            let files = if wants_files {
                search_files(&files, &search_text, RESULT_LIMIT, &cancel)
            } else {
                Vec::new()
            };
            let classes = if wants_classes {
                search_symbols(
                    &symbols,
                    &search_text,
                    SymbolFilter::TypesOnly,
                    RESULT_LIMIT,
                    &cancel,
                )
            } else {
                Vec::new()
            };
            let symbols = if wants_symbols {
                let mut all = search_symbols(
                    &symbols,
                    &search_text,
                    SymbolFilter::All,
                    RESULT_LIMIT,
                    &cancel,
                );
                // On the All tab, types already have their own section.
                if tab == Tab::All {
                    all.retain(|hit| !hit.entry.symbol.kind.is_type());
                }
                all
            } else {
                Vec::new()
            };
            Found {
                files,
                classes,
                symbols,
            }
        });
        // Language servers know about more than the index, like dependencies, but answer slower:
        // their results are added when they come.
        let from_servers = (wants_classes || wants_symbols)
            .then(|| self.workspace.upgrade())
            .flatten()
            .map(|w| {
                w.read(cx)
                    .language_servers()
                    .read(cx)
                    .workspace_symbols(&text, cx)
            });
        let root = self.root(cx);
        self._search = Some(cx.spawn(async move |this, cx| {
            let mut found = work.await;
            let shown = this.update(cx, |this, cx| {
                if this.generation == generation {
                    let items = this.build_items(found.clone(), &text, cx);
                    this.show(items, cx);
                    true
                } else {
                    false
                }
            });
            let (Ok(true), Some(from_servers)) = (shown, from_servers) else {
                return;
            };
            let hits = from_servers.await;
            if !merge_server_symbols(&mut found, hits, &text, root.as_deref(), tab) {
                return;
            }
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    let selected = this
                        .nav
                        .selected()
                        .and_then(|i| this.items.get(i))
                        .map(item_key);
                    let items = this.build_items(found, &text, cx);
                    this.show(items, cx);
                    // Keep the selection on what it was, as results arriving shouldn't move it.
                    if let Some(index) =
                        selected.and_then(|key| this.items.iter().position(|i| item_key(i) == key))
                    {
                        this.nav.select(index);
                    }
                }
            })
            .ok();
        }));
    }

    fn show(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        self.searching = false;
        self.items = items;
        self.nav.set_count(self.items.len());
        let first = self.items.iter().position(Item::is_selectable);
        // Recent Files starts on the file before the current one, so Enter goes back to it.
        let start = if self.tab == Tab::Recent && self.query.text.is_empty() {
            self.items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.is_selectable())
                .nth(1)
                .map(|(i, _)| i)
                .or(first)
        } else {
            first
        };
        if let Some(index) = start {
            self.nav.select(index);
        }
        cx.notify();
    }

    fn file_item(&self, absolute: &Path, root: Option<&Path>) -> Item {
        let relative: Arc<str> = root
            .and_then(|r| absolute.strip_prefix(r).ok())
            .unwrap_or(absolute)
            .to_string_lossy()
            .into();
        Item::File {
            relative,
            absolute: absolute.to_path_buf(),
            positions: Vec::new(),
        }
    }

    /// What shows before anything's typed: recent files, and for actions, everything.
    fn empty_query_items(&self, cx: &App) -> Vec<Item> {
        let root = self.root(cx);
        match self.tab {
            Tab::All | Tab::Files | Tab::Recent => {
                let mut items: Vec<Item> = Vec::new();
                if self.tab == Tab::All && !self.recent.is_empty() {
                    items.push(Item::Header("Recent Files"));
                }
                items.extend(
                    self.recent
                        .iter()
                        .map(|p| self.file_item(p, root.as_deref())),
                );
                if self.tab == Tab::All {
                    let recent_actions = self.recent_action_items();
                    if !recent_actions.is_empty() {
                        items.push(Item::Header("Recent Actions"));
                        items.extend(recent_actions);
                    }
                }
                items
            }
            Tab::Actions => {
                let mut items = self.recent_action_items();
                let mut rest: Vec<&ActionEntry> = self
                    .actions
                    .iter()
                    .filter(|a| !self.recent_actions.contains(&a.info.name))
                    .collect();
                rest.sort_by_key(|a| !a.available);
                items.extend(rest.into_iter().map(|entry| Item::Action {
                    entry: entry.clone(),
                    positions: Vec::new(),
                }));
                items
            }
            Tab::Classes | Tab::Symbols => Vec::new(),
        }
    }

    fn recent_action_items(&self) -> Vec<Item> {
        self.recent_actions
            .iter()
            .filter_map(|name| self.actions.iter().find(|a| &a.info.name == name))
            .map(|entry| Item::Action {
                entry: entry.clone(),
                positions: Vec::new(),
            })
            .collect()
    }

    fn action_hits(&self, text: &str) -> Vec<Item> {
        let pattern = fuzzy::Pattern::new(text);
        let mut hits: Vec<(i32, Item)> = self
            .actions
            .iter()
            .filter_map(|entry| {
                let found = fuzzy::fuzzy_match(&pattern, &entry.info.title)?;
                let recent_bonus = if self.recent_actions.contains(&entry.info.name) {
                    8
                } else {
                    0
                };
                let unavailable = if entry.available { 0 } else { -50 };
                Some((
                    found.score + recent_bonus + unavailable,
                    Item::Action {
                        entry: entry.clone(),
                        positions: found.positions,
                    },
                ))
            })
            .collect();
        hits.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        hits.into_iter().map(|(_, item)| item).collect()
    }

    /// The tabs that are quick enough to search right here.
    fn instant_items(&self, text: &str, cx: &App) -> Vec<Item> {
        match self.tab {
            Tab::Actions => self.action_hits(text),
            Tab::Recent => {
                let root = self.root(cx);
                let pattern = fuzzy::Pattern::new(text);
                self.recent
                    .iter()
                    .filter_map(|path| {
                        let name = path.file_name()?.to_string_lossy().into_owned();
                        let found = fuzzy::fuzzy_match(&pattern, &name)?;
                        let Item::File {
                            relative, absolute, ..
                        } = self.file_item(path, root.as_deref())
                        else {
                            return None;
                        };
                        let offset = relative.len() - name.len();
                        let positions = found.positions.iter().map(|p| p + offset).collect();
                        Some(Item::File {
                            relative,
                            absolute,
                            positions,
                        })
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn build_items(&self, found: Found, text: &str, cx: &App) -> Vec<Item> {
        let root = self.root(cx).unwrap_or_default();
        let files = found.files.into_iter().map(|hit| Item::File {
            absolute: root.join(&*hit.path),
            relative: hit.path,
            positions: hit.positions,
        });
        let classes = found.classes.into_iter().map(Item::Symbol);
        let symbols = found.symbols.into_iter().map(Item::Symbol);
        match self.tab {
            Tab::Files => files.collect(),
            Tab::Classes => classes.collect(),
            Tab::Symbols => symbols.collect(),
            _ => {
                let mut items = Vec::new();
                let mut section = |label: &'static str, list: Vec<Item>| {
                    if !list.is_empty() {
                        items.push(Item::Header(label));
                        items.extend(list.into_iter().take(ALL_TAB_PER_SECTION));
                    }
                };
                section("Classes", classes.collect());
                section("Files", files.collect());
                section("Symbols", symbols.collect());
                section("Actions", self.action_hits(text));
                items
            }
        }
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        let mut index = self.nav.selected().unwrap_or(0);
        for _ in 0..count {
            index = if forward {
                (index + 1) % count
            } else {
                (index + count - 1) % count
            };
            if self.items[index].is_selectable() {
                self.nav.select(index);
                break;
            }
        }
        cx.notify();
    }

    fn page(&mut self, forward: bool, cx: &mut Context<Self>) {
        let delta = if forward {
            VISIBLE_ROWS as isize
        } else {
            -(VISIBLE_ROWS as isize)
        };
        self.nav.select_by(delta);
        if let Some(index) = self.nav.selected()
            && !self.items[index].is_selectable()
        {
            self.step(forward, cx);
        }
        cx.notify();
    }

    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let query = self.query.clone();
        match item {
            Item::Header(_) => {}
            Item::File { absolute, .. } => {
                // `name:12:4` is one-based, as people count lines.
                let position = query.line.map(|line| {
                    (
                        (line - 1) as usize,
                        query.column.map_or(0, |c| (c - 1) as usize),
                    )
                });
                workspace.update(cx, |w, cx| {
                    w.dismiss_modal(window, cx);
                    w.open_path_at(absolute, position, window, cx).detach();
                });
            }
            Item::Symbol(hit) => {
                let Some(root) = self.root(cx) else { return };
                let path = root.join(&*hit.entry.path);
                let (line, column) = (
                    hit.entry.symbol.line as usize,
                    hit.entry.symbol.column as usize,
                );
                let name = hit.entry.symbol.name.clone();
                workspace.update(cx, |w, cx| {
                    w.dismiss_modal(window, cx);
                    let open = w.open_path_at(path, Some((line, column)), window, cx);
                    // Language servers count columns in UTF-16 units and the index in bytes, which
                    // differ after non-ASCII text, so land on the name itself.
                    cx.spawn(async move |_, cx| {
                        let Ok(editor) = open.await else { return };
                        editor.update(cx, |editor, cx| {
                            let text = editor.buffer().read(cx).buffer().line(line);
                            if let Some(at) = name_column(&text, &name, column) {
                                editor.jump_to(line, at, cx);
                            }
                        });
                    })
                    .detach();
                });
            }
            Item::Action { entry, .. } => {
                let Ok(action) = cx.build_action(&entry.info.name, None) else {
                    return;
                };
                let name = entry.info.name.clone();
                workspace.update(cx, |w, cx| {
                    w.note_action_used(&name, cx);
                    w.dismiss_modal(window, cx);
                });
                // Runs once focus is back where the user was, which is what the action is for.
                window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
            }
        }
    }

    fn highlighted(text: &str, positions: &[usize], offset: usize, theme: &Theme) -> StyledText {
        let style = HighlightStyle {
            color: Some(theme.accent),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let ranges: Vec<(Range<usize>, HighlightStyle)> = positions
            .iter()
            .filter(|p| **p >= offset)
            .map(|p| p - offset)
            .filter_map(|p| {
                let len = text.get(p..)?.chars().next()?.len_utf8();
                Some((p..p + len, style))
            })
            .collect();
        StyledText::new(SharedString::from(text.to_string())).with_highlights(ranges)
    }

    fn render_item(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.nav.selected() == Some(index);
        let item = &self.items[index];
        let row = div()
            .id(("search-row", index))
            .debug_selector(|| format!("search-row-{index}"))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(ROW_HEIGHT))
            .px(px(12.))
            .when(selected, |r| r.bg(theme.selection))
            .when(item.is_selectable(), |r| {
                r.on_click(cx.listener(move |this, _, window, cx| this.confirm(index, window, cx)))
            });

        match item {
            Item::Header(label) => row
                .text_size(px(11.))
                .text_color(theme.text_faint)
                .child(label.to_uppercase())
                .into_any_element(),
            Item::File {
                relative,
                positions,
                ..
            } => {
                let name_start = relative.rfind('/').map_or(0, |i| i + 1);
                let name = &relative[name_start..];
                let folder = &relative[..name_start];
                let icon = match file_icon(Path::new(name)) {
                    FileIcon::Badge(text, color) => div()
                        .w(px(16.))
                        .flex_none()
                        .text_size(px(9.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(color)
                        .child(text)
                        .into_any_element(),
                    FileIcon::Plain => svg()
                        .path(Icon::File.path())
                        .size(px(14.))
                        .text_color(theme.text_muted)
                        .into_any_element(),
                };
                row.child(icon)
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(Self::highlighted(name, positions, name_start, theme)),
                    )
                    .child(
                        div()
                            .text_color(theme.text_faint)
                            .truncate()
                            .child(Self::highlighted(
                                folder.trim_end_matches('/'),
                                positions,
                                0,
                                theme,
                            )),
                    )
                    .into_any_element()
            }
            Item::Symbol(hit) => {
                let symbol = &hit.entry.symbol;
                let (letter, color) = symbol_badge(symbol.kind, theme);
                let detail = match &symbol.container {
                    Some(container) => format!("{container} · {}", hit.entry.path),
                    None => hit.entry.path.to_string(),
                };
                row.child(
                    div()
                        .w(px(16.))
                        .flex_none()
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(color)
                        .child(letter),
                )
                .child(div().text_color(theme.text).child(Self::highlighted(
                    &symbol.name,
                    &hit.positions,
                    0,
                    theme,
                )))
                .child(div().text_color(theme.text_faint).truncate().child(detail))
                .into_any_element()
            }
            Item::Action { entry, positions } => {
                let color = if entry.available {
                    theme.text
                } else {
                    theme.text_faint
                };
                row.justify_between()
                    .child(div().text_color(color).child(Self::highlighted(
                        &entry.info.title,
                        positions,
                        0,
                        theme,
                    )))
                    .children(entry.info.shortcut.clone().map(|keys| {
                        div()
                            .text_size(px(11.))
                            .text_color(theme.text_muted)
                            .child(keys)
                    }))
                    .into_any_element()
            }
        }
    }

    fn status_text(&self, cx: &App) -> String {
        let Some(status) = self.status(cx) else {
            return "No folder open".into();
        };
        if status.scanning {
            return "Scanning files…".into();
        }
        match status.indexing {
            Some((done, total)) => format!("Indexing symbols {done} / {total}…"),
            None => format!("{} files · {} symbols", status.files, status.symbols),
        }
    }
}

fn symbol_badge(kind: SymbolKind, theme: &Theme) -> (&'static str, gpui::Hsla) {
    let s = &theme.syntax;
    match kind {
        SymbolKind::Class => ("C", s.type_),
        SymbolKind::Interface => ("I", s.type_),
        SymbolKind::Enum => ("E", s.type_),
        SymbolKind::EnumMember => ("e", s.constant),
        SymbolKind::Function => ("f", s.function),
        SymbolKind::Method => ("m", s.function),
        SymbolKind::Constant => ("c", s.constant),
        SymbolKind::Variable => ("v", s.variable),
        SymbolKind::Module => ("M", s.namespace),
        SymbolKind::Macro => ("!", s.macro_),
    }
}

impl Render for SearchPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let tabs: Vec<AnyElement> = if self.tab == Tab::Recent {
            vec![
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .text_color(theme.text)
                    .child(Tab::Recent.label())
                    .into_any_element(),
            ]
        } else {
            Tab::CYCLE
                .iter()
                .map(|tab| {
                    let active = *tab == self.tab;
                    let tab = *tab;
                    div()
                        .id(SharedString::from(format!("search-tab-{}", tab.label())))
                        .debug_selector(move || format!("search-tab-{}", tab.label()))
                        .relative()
                        .px(px(10.))
                        .py(px(6.))
                        .text_color(if active { theme.text } else { theme.text_muted })
                        .hover(|t| t.text_color(theme.text))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                        .child(tab.label())
                        .when(active, |t| {
                            t.child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .left(px(8.))
                                    .right(px(8.))
                                    .h(px(2.))
                                    .bg(theme.accent),
                            )
                        })
                        .into_any_element()
                })
                .collect()
        };

        let list_height = px(ROW_HEIGHT * self.items.len().clamp(1, VISIBLE_ROWS) as f32);
        let body: AnyElement = if self.items.is_empty() {
            let message = if self.query.text.trim().is_empty() {
                match self.tab {
                    Tab::Classes | Tab::Symbols => "Type to search",
                    _ => "Nothing recent",
                }
            } else {
                "Nothing found"
            };
            div()
                .h(list_height)
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_faint)
                .child(message)
                .into_any_element()
        } else {
            let rows = cx.processor(|this, range: Range<usize>, _, cx| {
                let theme = cx.global::<Theme>().clone();
                range
                    .map(|index| this.render_item(index, &theme, cx))
                    .collect::<Vec<_>>()
            });
            uniform_list("search-results", self.items.len(), rows)
                .track_scroll(self.nav.scroll_handle())
                .h(list_height)
                .into_any_element()
        };

        div()
            .key_context("SearchEverywhere")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(680.))
            .flex()
            .flex_col()
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(theme.ui_font_size))
            .text_color(theme.text)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(true, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(false, cx)))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.cycle_tab(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| this.cycle_tab(false, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                if let Some(index) = this.nav.selected() {
                    this.confirm(index, window, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(4.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex().flex_row().children(tabs))
                    .child(
                        div()
                            .px(px(10.))
                            .text_size(px(11.))
                            .text_color(theme.text_faint)
                            .child("Tab to switch"),
                    ),
            )
            .child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(theme.border)
                    .line_height(px(20.))
                    .text_size(px(14.))
                    .child(self.input.clone()),
            )
            .child(body)
            .child(
                div()
                    .px(px(12.))
                    .py(px(5.))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(11.))
                    .text_color(theme.text_faint)
                    .child(self.status_text(cx)),
            )
    }
}

impl SearchPopup {
    /// The visible results as text, for tests and the end-to-end driver.
    pub fn result_labels(&self) -> Vec<String> {
        self.items
            .iter()
            .map(|item| match item {
                Item::Header(label) => format!("# {label}"),
                Item::File { relative, .. } => relative.to_string(),
                Item::Symbol(hit) => format!(
                    "{} {}",
                    hit.entry.symbol.kind.label(),
                    hit.entry.symbol.name
                ),
                Item::Action { entry, .. } => format!("> {}", entry.info.title),
            })
            .collect()
    }

    pub fn selected_label(&self) -> Option<String> {
        let index = self.nav.selected()?;
        self.result_labels().get(index).cloned()
    }

    /// Whether a search is still running.
    pub fn is_searching(&self) -> bool {
        self.searching
    }
}

#[cfg(test)]
mod tests;
