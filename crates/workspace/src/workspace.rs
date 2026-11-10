//! The window's root view.

mod decorations;
mod title_bar;
pub use title_bar::{TITLE_BAR_HEIGHT, TitleSide, current_branch, list_branches, title_button};

use crate::actions_list::list_actions;
use crate::dock::{Dock, DockPosition, MIN_CENTER_SIZE, MIN_DOCK_SIZE, Panel};
use crate::image_view::{self, ImageView};
use crate::pane::{Pane, PaneEvent, PaneItem};
use crate::pane_tree::{Axis, PaneTree};
use crate::persistence::{
    self, SavedDock, SavedPane, SavedTab, SavedTree, SavedWorkspace, remember_last_project,
};
use anyhow::Result;
use gleipnir_core::{
    BufferModel, BufferStore, DiskStatus, LanguageServers, Project, ProjectEvent, Toolchains,
};
use gleipnir_editor::{EditorEvent, EditorState, EditorView};
use gleipnir_text::SelectionSet;
use gleipnir_ui::{Icon, Theme};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyBinding, MouseButton, MouseMoveEvent, PathPromptOptions, Pixels,
    Point, PromptLevel, Render, SharedString, Styled as _, Subscription, Task, WeakEntity, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, canvas, div, prelude::*, px, relative,
    size, svg,
};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

actions!(
    workspace,
    [
        Quit,
        OpenFile,
        OpenFolder,
        NewFile,
        SaveAs,
        CloseTab,
        NextTab,
        PreviousTab,
        SplitRight,
        SplitDown,
        FocusNextPane,
        FocusPreviousPane,
        FocusEditor,
        HideActiveDock,
        NavigateBack,
        NavigateForward,
    ]
);

/// How long a message stays in the status bar.
const MESSAGE_LIFETIME: Duration = Duration::from_secs(10);
/// How long after a change to wait before saving the layout, so a drag doesn't write on each move.
const SAVE_DELAY: Duration = Duration::from_millis(400);

pub fn bind_keys(cx: &mut App) {
    image_view::bind_keys(cx);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", NewFile, None),
        KeyBinding::new("cmd-shift-s", SaveAs, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PreviousTab, None),
        KeyBinding::new("cmd-\\", SplitRight, None),
        KeyBinding::new("cmd-shift-\\", SplitDown, None),
        KeyBinding::new("cmd-[", NavigateBack, None),
        KeyBinding::new("cmd-]", NavigateForward, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-shift-s", SaveAs, None),
        KeyBinding::new("ctrl-w", CloseTab, None),
        KeyBinding::new("ctrl-\\", SplitRight, None),
        KeyBinding::new("ctrl-shift-\\", SplitDown, None),
        KeyBinding::new("ctrl-alt-left", NavigateBack, None),
        KeyBinding::new("ctrl-alt-right", NavigateForward, None),
    ]);
    cx.bind_keys([
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("alt-tab", FocusNextPane, None),
        KeyBinding::new("alt-shift-tab", FocusPreviousPane, None),
        KeyBinding::new("shift-escape", HideActiveDock, None),
    ]);
    crate::name_prompt::bind_keys(cx);
}

/// Identifies a tab, for closing one other than the active one.
pub struct CloseTabAt {
    pub pane: Entity<Pane>,
    pub index: usize,
}

#[derive(Clone, Debug)]
pub enum WorkspaceEvent {
    /// The file in the focused tab changed.
    ActiveFileChanged(Option<PathBuf>),
    /// Something asked for a path to be shown, such as a click on a breadcrumb.
    RevealPath(PathBuf),
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

/// A drag in progress on a divider.
#[derive(Clone)]
enum ResizeDrag {
    Dock(DockPosition),
    Split {
        path: Vec<usize>,
        index: usize,
        axis: Axis,
        last: Point<Pixels>,
    },
}

/// Runs on every new workspace, to add panels. Features that depend on this crate register here, so
/// this crate doesn't need to know about them.
#[derive(Clone)]
pub struct WorkspaceInit(pub Rc<WorkspaceInitFn>);

pub type WorkspaceInitFn = dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

impl gpui::Global for WorkspaceInit {}

/// The buffer store shared by all windows.
pub struct GlobalStore(pub Entity<BufferStore>);

impl gpui::Global for GlobalStore {}

type ExtraAction = Rc<dyn Fn(gpui::Div, &mut Context<Workspace>) -> gpui::Div>;
type EditorHook = Rc<
    dyn Fn(
        &mut Workspace,
        &Entity<EditorView>,
        &mut Window,
        &mut Context<Workspace>,
    ) -> Vec<Subscription>,
>;
type StatusItem = Rc<dyn Fn(&mut Workspace, &mut Context<Workspace>) -> Option<AnyElement>>;

/// A place to go back to: a file, and a line and byte column in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavEntry {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
}

/// How many places back navigation remembers.
const NAV_LIMIT: usize = 100;

/// A popup over the workspace, like Search Everywhere. Emits `DismissEvent` to close itself.
pub trait ModalView: gpui::Render + Focusable + EventEmitter<gpui::DismissEvent> {}

struct ActiveModal {
    view: gpui::AnyView,
    type_id: std::any::TypeId,
    name: &'static str,
    /// Where focus goes back to when the popup closes.
    restore_focus: Option<FocusHandle>,
    /// What it pops up beside, like the caret, rather than at the top of the window.
    anchor: Option<Bounds<Pixels>>,
    _subscription: Subscription,
}

/// How much room a popup shown beside something is given, to choose above or below it.
const ANCHORED_WIDTH: f32 = 620.;
const ANCHORED_HEIGHT: f32 = 320.;

pub enum EmptyHint {
    Action(Box<dyn gpui::Action>),
    Text(&'static str),
}

/// How many recently opened files and used commands to remember.
const RECENT_LIMIT: usize = 50;

pub struct Workspace {
    focus_handle: FocusHandle,
    store: Entity<BufferStore>,
    root: Option<PathBuf>,
    project: Option<Entity<Project>>,
    toolchains: Entity<Toolchains>,
    language_servers: Entity<LanguageServers>,
    center: PaneTree<Entity<Pane>>,
    active_pane: Entity<Pane>,
    docks: [Dock; 3],
    resize: Option<ResizeDrag>,
    /// The screen bounds of each split, by its path in the tree, for turning a drag into a ratio.
    split_bounds: Rc<RefCell<HashMap<Vec<usize>, Bounds<Pixels>>>>,
    /// The size of the window's content, inside any frame the app draws around it.
    window_size: gpui::Size<Pixels>,
    /// Where the content starts in the window: past the frame's shadow, when it has one.
    content_origin: Point<Pixels>,
    message: Option<(SharedString, bool)>,
    message_task: Option<Task<()>>,
    extra_actions: Vec<ExtraAction>,
    restoring: bool,
    save_task: Option<Task<()>>,
    weak_self: WeakEntity<Self>,
    pane_subscriptions: HashMap<gpui::EntityId, Subscription>,
    last_active_file: Option<PathBuf>,
    closing_confirmed: bool,
    modal: Option<ActiveModal>,
    /// Most recent first.
    recent_files: Vec<PathBuf>,
    recent_actions: Vec<String>,
    /// What the empty pane suggests, which features add to: a label, and the action whose shortcut
    /// to show, or fixed text for gestures that aren't key bindings.
    empty_hints: Vec<(&'static str, EmptyHint)>,
    /// Run on every editor as it's created, by features that attach to editors.
    editor_hooks: Vec<EditorHook>,
    /// Extra things on the right of the status bar, before the cursor position.
    status_items: Vec<StatusItem>,
    /// What features add to the title bar, and on which side.
    title_items: Vec<(TitleSide, u8, StatusItem)>,
    /// A menu opened from the title bar.
    menu: Option<title_bar::OpenMenu>,
    /// Which of the app's menus is open from the title bar, where it shows them, as on Linux.
    open_app_menu: Option<usize>,
    /// Which of them last closed, and when: a press on its name that closed it mustn't reopen it.
    app_menu_closed: Option<(usize, std::time::Instant)>,
    /// Where each of those menus' names is in the bar, to open each below its name.
    app_menu_bounds: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    branch: Option<String>,
    branch_task: Option<Task<()>>,
    nav_back: Vec<NavEntry>,
    nav_forward: Vec<NavEntry>,
    /// Buffers whose unsaved text is indexed for search, with the pending refresh.
    indexed_buffers: HashMap<gpui::EntityId, Option<Task<()>>>,
}

/// A plain description of the workspace, for tests and the end-to-end driver.
#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceSnapshot {
    pub root: Option<String>,
    pub panes: Vec<PaneSnapshot>,
    pub active_pane: usize,
    pub docks: Vec<DockSnapshot>,
    pub message: Option<String>,
    pub panels: HashMap<String, serde_json::Value>,
    pub actions: usize,
    /// The open popup's type name, if there is one.
    pub modal: Option<String>,
    /// Files and symbols indexed, and whether indexing is still running.
    pub index: Option<IndexSnapshot>,
    /// Each package's chosen SDK and where its tools were found, by package id.
    pub toolchains: std::collections::BTreeMap<String, ToolchainSnapshot>,
    pub recent_files: Vec<String>,
    /// Each started language server's state, by package id.
    pub language_servers: std::collections::BTreeMap<String, String>,
    /// The theme in use, by name, with its background, to see a change to its file.
    pub theme: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolchainSnapshot {
    pub sdk: Option<String>,
    pub sdk_version: Option<String>,
    /// Tool name to where it was found, or null.
    pub tools: std::collections::BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct IndexSnapshot {
    pub files: usize,
    pub symbols: usize,
    pub busy: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PaneSnapshot {
    pub tabs: Vec<String>,
    pub active: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct DockSnapshot {
    pub position: DockPosition,
    pub visible: bool,
    pub size: f32,
    pub panels: Vec<String>,
    /// The panel the dock shows.
    pub active: Option<String>,
}

impl Workspace {
    pub fn new(
        store: Entity<BufferStore>,
        root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let weak_self = cx.weak_entity();
        let pane = cx.new(|cx| Pane::new(weak_self.clone(), window, cx));
        let root = root.map(|r| std::fs::canonicalize(&r).unwrap_or(r));
        let project = root.clone().map(|root| cx.new(|cx| Project::new(root, cx)));
        let toolchains = cx.new(|cx| Toolchains::new(root.clone(), cx));
        // A changed SDK choice is part of the project's saved state.
        cx.observe(&toolchains, |this, _, cx| this.schedule_save(cx))
            .detach();
        // Servers start lazily, when a file of theirs is registered by whatever shows code insight.
        let language_servers =
            cx.new(|cx| LanguageServers::new(root.clone(), toolchains.clone(), cx));
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            store,
            root,
            project,
            toolchains,
            language_servers,
            center: PaneTree::Pane(pane.clone()),
            active_pane: pane.clone(),
            docks: [
                Dock::new(DockPosition::Left),
                Dock::new(DockPosition::Bottom),
                Dock::new(DockPosition::Right),
            ],
            resize: None,
            split_bounds: Rc::new(RefCell::new(HashMap::new())),
            window_size: window.viewport_size(),
            content_origin: Point::default(),
            message: None,
            message_task: None,
            extra_actions: Vec::new(),
            restoring: false,
            save_task: None,
            weak_self,
            pane_subscriptions: HashMap::new(),
            last_active_file: None,
            closing_confirmed: false,
            modal: None,
            recent_files: Vec::new(),
            recent_actions: Vec::new(),
            empty_hints: Vec::new(),
            editor_hooks: Vec::new(),
            status_items: Vec::new(),
            title_items: Vec::new(),
            menu: None,
            open_app_menu: None,
            app_menu_closed: None,
            app_menu_bounds: Rc::default(),
            branch: None,
            branch_task: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            indexed_buffers: HashMap::new(),
        };
        this.watch_pane(&pane, cx);
        if let Some(root) = &this.root {
            remember_last_project(root).ok();
        }
        this.watch_branch(cx);
        // Prettier configs in JavaScript run with Node, which is slow enough to do ahead of
        // opening files, rather than as each opens.
        if let Some(root) = this.root.clone() {
            let ran = cx
                .background_executor()
                .spawn(async move { gleipnir_packages::project_style::prewarm_prettier(&root) });
            cx.spawn(async move |this, cx| {
                if ran.await {
                    this.update(cx, |this, cx| {
                        this.store
                            .update(cx, |store, cx| store.apply_code_style(cx))
                    })
                    .ok();
                }
            })
            .detach();
        }
        // Language servers hear of files changed outside the editor, like on a branch switch.
        if let Some(project) = this.project.clone() {
            cx.subscribe(&project, |this, _, event, cx| {
                // A commit, checkout or merge changes what open files are compared with.
                if let ProjectEvent::GitStatusChanged = event {
                    this.store
                        .update(cx, |store, cx| store.refresh_committed_texts(cx));
                }
                if let ProjectEvent::FilesChanged(paths) = event {
                    this.language_servers.read(cx).files_changed(paths);
                    // A change to `.editorconfig` or Prettier's config re-indents open files.
                    if paths
                        .iter()
                        .any(|p| gleipnir_packages::project_style::is_style_config(p))
                    {
                        this.store
                            .update(cx, |store, cx| store.apply_code_style(cx));
                    }
                }
            })
            .detach();
        }
        this.update_title(window, cx);

        // Closing the window asks about unsaved work first.
        let handle = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let Some(workspace) = handle.upgrade() else {
                return true;
            };
            workspace.update(cx, |workspace, cx| {
                workspace.window_should_close(window, cx)
            })
        });
        this
    }

    /// Keeps Search Everywhere's symbols for a file current with its unsaved edits, once typing
    /// pauses, and goes back to the file on disk if the edits are thrown away.
    fn index_unsaved_text(&mut self, buffer: &Entity<BufferModel>, cx: &mut Context<Self>) {
        let id = buffer.entity_id();
        if self.project.is_none() || self.indexed_buffers.contains_key(&id) {
            return;
        }
        self.indexed_buffers.insert(id, None);
        cx.subscribe(buffer, move |this, buffer, event, cx| {
            if !matches!(event, gleipnir_core::BufferEvent::Edited { .. }) {
                return;
            }
            let id = buffer.entity_id();
            let buffer = buffer.downgrade();
            let refresh = cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(300))
                    .await;
                this.update(cx, |this, cx| {
                    let Some(buffer) = buffer.upgrade() else {
                        return;
                    };
                    let model = buffer.read(cx);
                    let (Some(path), Some(project)) = (model.path(), this.project.clone()) else {
                        return;
                    };
                    let (path, text) = (path.to_path_buf(), model.buffer().snapshot());
                    project.update(cx, |p, cx| p.index_text(&path, text, cx));
                })
                .ok();
            });
            this.indexed_buffers.insert(id, Some(refresh));
        })
        .detach();
        let path = buffer.read(cx).path().map(Path::to_path_buf);
        cx.observe_release(buffer, move |this, buffer, cx| {
            this.indexed_buffers.remove(&id);
            if buffer.buffer().is_dirty()
                && let (Some(path), Some(project)) = (&path, this.project.clone())
            {
                project.update(cx, |p, cx| p.reindex_from_disk(path, cx));
            }
        })
        .detach();
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// A handle to this workspace that doesn't keep it alive.
    pub fn weak_handle(&self) -> WeakEntity<Self> {
        self.weak_self.clone()
    }

    pub fn store(&self) -> &Entity<BufferStore> {
        &self.store
    }

    pub fn project(&self) -> Option<&Entity<Project>> {
        self.project.as_ref()
    }

    pub fn language_servers(&self) -> &Entity<LanguageServers> {
        &self.language_servers
    }

    /// Swaps in another hub, for tests that use fake servers. Only before any file is open.
    pub fn set_language_servers(&mut self, servers: Entity<LanguageServers>) {
        self.language_servers = servers;
    }

    pub fn toolchains(&self) -> &Entity<Toolchains> {
        &self.toolchains
    }

    /// Recently opened files, most recent first, the current one included.
    pub fn recent_files(&self) -> &[PathBuf] {
        &self.recent_files
    }

    /// Commands recently run from search, most recent first.
    pub fn recent_actions(&self) -> &[String] {
        &self.recent_actions
    }

    pub fn note_action_used(&mut self, name: &str, cx: &mut Context<Self>) {
        self.recent_actions.retain(|a| a != name);
        self.recent_actions.insert(0, name.to_string());
        self.recent_actions.truncate(RECENT_LIMIT);
        self.schedule_save(cx);
    }

    /// Opens a popup, replacing any that's open. If one of the same type is open, closes it
    /// instead, so the key that opens a popup also closes it.
    pub fn toggle_modal<V: ModalView>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) -> Option<Entity<V>> {
        self.toggle_modal_at(None, window, cx, build)
    }

    /// Like `toggle_modal`, but pops up just below `anchor`, or above it if there's no room below,
    /// as IntelliJ's choosers appear beside the caret.
    pub fn toggle_modal_at<V: ModalView>(
        &mut self,
        anchor: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) -> Option<Entity<V>> {
        let same_kind = self
            .modal
            .as_ref()
            .is_some_and(|modal| modal.type_id == std::any::TypeId::of::<V>());
        let restore_focus = match self.modal.take() {
            Some(previous) => previous.restore_focus,
            None => window.focused(cx),
        };
        if same_kind {
            if let Some(focus) = restore_focus {
                window.focus(&focus, cx);
            }
            cx.notify();
            return None;
        }
        let view = cx.new(|cx| build(window, cx));
        let focus = view.read(cx).focus_handle(cx);
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, _: &gpui::DismissEvent, window, cx| {
                this.dismiss_modal(window, cx);
            },
        );
        window.focus(&focus, cx);
        let name = std::any::type_name::<V>();
        self.modal = Some(ActiveModal {
            view: view.clone().into(),
            type_id: std::any::TypeId::of::<V>(),
            name: name.rsplit("::").next().unwrap_or(name),
            restore_focus,
            anchor,
            _subscription: subscription,
        });
        cx.notify();
        Some(view)
    }

    pub fn active_modal<V: ModalView>(&self) -> Option<Entity<V>> {
        self.modal.as_ref()?.view.clone().downcast::<V>().ok()
    }

    /// Closes the popup and puts focus back where it was.
    pub fn dismiss_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(modal) = self.modal.take() {
            match modal.restore_focus {
                Some(focus) => window.focus(&focus, cx),
                None => self.focus_center(window, cx),
            }
            cx.notify();
        }
    }

    pub fn has_modal(&self) -> bool {
        self.modal.is_some()
    }

    /// Opens a file and puts the cursor at a zero-based line and byte column.
    pub fn open_path_at(
        &mut self,
        path: PathBuf,
        position: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<EditorView>>> {
        self.record_position(cx);
        let open = self.open_path(path, window, cx);
        cx.spawn(async move |_, cx| {
            let editor = open.await?;
            if let Some((line, column)) = position {
                editor.update(cx, |editor, cx| editor.jump_to(line, column, cx));
            }
            Ok(editor)
        })
    }

    /// Remembers where the cursor is, so Navigate Back can return to it after a jump.
    pub fn record_position(&mut self, cx: &App) {
        let Some(entry) = self.current_position(cx) else {
            return;
        };
        if self.nav_back.last() != Some(&entry) {
            self.nav_back.push(entry);
            if self.nav_back.len() > NAV_LIMIT {
                self.nav_back.remove(0);
            }
        }
        self.nav_forward.clear();
    }

    fn current_position(&self, cx: &App) -> Option<NavEntry> {
        let editor = self.active_editor(cx)?;
        let view = editor.read(cx);
        let model = view.buffer().read(cx);
        let point = model
            .buffer()
            .offset_to_point(view.selections().primary().head);
        Some(NavEntry {
            path: model.path()?.to_path_buf(),
            line: point.line,
            column: point.col,
        })
    }

    pub fn navigation_history(&self) -> (&[NavEntry], &[NavEntry]) {
        (&self.nav_back, &self.nav_forward)
    }

    fn navigate(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.current_position(cx);
        let (from, to) = if back {
            (&mut self.nav_back, &mut self.nav_forward)
        } else {
            (&mut self.nav_forward, &mut self.nav_back)
        };
        // Skip entries that are where we already are.
        let target = loop {
            match from.pop() {
                Some(entry) if Some(&entry) == here.as_ref() => continue,
                other => break other,
            }
        };
        let Some(target) = target else { return };
        to.extend(here);
        let open = self.open_path(target.path.clone(), window, cx);
        cx.spawn(async move |_, cx| {
            if let Ok(editor) = open.await {
                editor.update(cx, |editor, cx| {
                    editor.jump_to(target.line, target.column, cx)
                });
            }
        })
        .detach();
    }

    /// Runs on every editor the workspace creates, now and later. The hook's subscriptions live as
    /// long as the editor's tab.
    pub fn add_editor_hook(
        &mut self,
        hook: impl Fn(
            &mut Workspace,
            &Entity<EditorView>,
            &mut Window,
            &mut Context<Workspace>,
        ) -> Vec<Subscription>
        + 'static,
    ) {
        self.editor_hooks.push(Rc::new(hook));
    }

    /// Adds something to the status bar, such as a language server's state.
    pub fn add_status_item(
        &mut self,
        item: impl Fn(&mut Workspace, &mut Context<Workspace>) -> Option<AnyElement> + 'static,
    ) {
        self.status_items.push(Rc::new(item));
    }

    /// Adds a suggestion to the empty pane, shown with the action's current shortcut.
    pub fn add_empty_hint(&mut self, label: &'static str, hint: EmptyHint) {
        self.empty_hints.push((label, hint));
    }

    /// Adds listeners to the workspace's root element, for features that need to see every key or
    /// modifier change in the window, like the double-shift gesture.
    pub fn add_root_hook(
        &mut self,
        hook: impl Fn(gpui::Div, &mut Context<Workspace>) -> gpui::Div + 'static,
    ) {
        self.extra_actions.push(Rc::new(hook));
    }

    pub fn panes(&self) -> Vec<Entity<Pane>> {
        self.center.panes()
    }

    pub fn active_pane(&self) -> &Entity<Pane> {
        &self.active_pane
    }

    pub fn active_editor(&self, cx: &App) -> Option<Entity<EditorView>> {
        self.active_pane.read(cx).active_editor().cloned()
    }

    /// The active tab's file, whether it's in an editor or an image's preview.
    pub fn active_file(&self, cx: &App) -> Option<PathBuf> {
        self.active_pane.read(cx).active_item()?.path(cx)
    }

    pub fn editor_state(&self, cx: &App) -> Option<EditorState> {
        self.active_editor(cx)
            .map(|editor| editor.read(cx).state(cx))
    }

    /// Adds a panel to its dock. It's saved state, if any, is applied by `restore_layout`.
    pub fn add_panel<T: Panel>(&mut self, panel: Entity<T>, cx: &mut Context<Self>) {
        let position = panel.read(cx).position();
        let dock = self.dock_mut(position);
        dock.panels.push(Box::new(panel));
        cx.notify();
    }

    /// Finds a panel by its type, wherever it's docked.
    pub fn panel<T: Panel>(&self, _: &App) -> Option<Entity<T>> {
        self.docks
            .iter()
            .flat_map(|dock| dock.panels.iter())
            .find_map(|panel| panel.view().downcast::<T>().ok())
    }

    /// Registers an action handled at the window level, for features that live in other crates.
    pub fn register_action<A: gpui::Action>(
        &mut self,
        handler: impl Fn(&mut Workspace, &A, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        let handler = Rc::new(handler);
        self.extra_actions.push(Rc::new(move |div, cx| {
            let handler = handler.clone();
            div.on_action(cx.listener(move |this, action: &A, window, cx| {
                handler(this, action, window, cx);
            }))
        }));
    }

    fn dock_index(position: DockPosition) -> usize {
        match position {
            DockPosition::Left => 0,
            DockPosition::Bottom => 1,
            DockPosition::Right => 2,
        }
    }

    fn dock(&self, position: DockPosition) -> &Dock {
        &self.docks[Self::dock_index(position)]
    }

    fn dock_mut(&mut self, position: DockPosition) -> &mut Dock {
        &mut self.docks[Self::dock_index(position)]
    }

    pub fn is_dock_visible(&self, position: DockPosition) -> bool {
        self.dock(position).visible
    }

    pub fn dock_size(&self, position: DockPosition) -> Pixels {
        self.dock(position).size
    }

    /// Shows a panel if it's hidden, or hides it if it's showing and focused. Otherwise just focuses
    /// it, so the same key can reach a panel that's visible but not in use.
    pub fn toggle_panel(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel_when(id, true, window, cx);
    }

    /// What clicking a panel's button on the rail does: hides it if it's showing, focused or not,
    /// and otherwise shows and focuses it.
    pub fn click_panel_button(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel_when(id, false, window, cx);
    }

    fn toggle_panel_when(
        &mut self,
        id: &str,
        only_if_focused: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self.docks.iter().find_map(|dock| {
            let index = dock.panels.iter().position(|p| p.id(cx) == id)?;
            Some((dock.position, index))
        });
        let Some((position, index)) = found else {
            return;
        };

        let (visible, active, focus) = {
            let dock = self.dock(position);
            (
                dock.visible,
                dock.active == index,
                dock.panels[index].focus_handle(cx),
            )
        };
        let focused = focus.contains_focused(window, cx);
        if visible && active && (focused || !only_if_focused) {
            self.dock_mut(position).visible = false;
            if focused {
                self.focus_center(window, cx);
            }
        } else {
            let dock = self.dock_mut(position);
            dock.visible = true;
            dock.active = index;
            window.focus(&focus, cx);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Shows a panel without changing focus.
    pub fn show_panel(&mut self, id: &str, cx: &mut Context<Self>) {
        for dock in &mut self.docks {
            if let Some(index) = dock.panels.iter().position(|p| p.id(cx) == id) {
                dock.visible = true;
                dock.active = index;
            }
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn hide_active_dock(
        &mut self,
        _: &HideActiveDock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.docks.iter().position(|dock| {
            dock.visible
                && dock
                    .active_panel()
                    .is_some_and(|p| p.focus_handle(cx).contains_focused(window, cx))
        });
        // With focus in the editor, hide whichever is open, starting from the left.
        let target = focused.or_else(|| self.docks.iter().position(|d| d.visible));
        if let Some(index) = target {
            self.docks[index].visible = false;
            self.focus_center(window, cx);
            self.schedule_save(cx);
            cx.notify();
        }
    }

    pub fn focus_center(&self, window: &mut Window, cx: &mut App) {
        focus_pane(&self.active_pane, window, cx);
    }

    fn new_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        let weak = self.weak_self.clone();
        let pane = cx.new(|cx| Pane::new(weak, window, cx));
        self.watch_pane(&pane, cx);
        pane
    }

    fn watch_pane(&mut self, pane: &Entity<Pane>, cx: &mut Context<Self>) {
        let subscription = cx.subscribe(pane, |this, pane, event, cx| match event {
            PaneEvent::Focused => this.set_active_pane(pane, cx),
            PaneEvent::ItemsChanged => {
                this.active_item_changed(cx);
                this.schedule_save(cx);
            }
        });
        self.pane_subscriptions
            .insert(pane.entity_id(), subscription);
    }

    fn set_active_pane(&mut self, pane: Entity<Pane>, cx: &mut Context<Self>) {
        if self.active_pane != pane {
            self.active_pane = pane;
            self.active_item_changed(cx);
            self.schedule_save(cx);
            cx.notify();
        }
    }

    fn active_item_changed(&mut self, cx: &mut Context<Self>) {
        let file = self.active_file(cx);
        if file != self.last_active_file {
            self.last_active_file = file.clone();
            if let Some(path) = &file {
                self.recent_files.retain(|p| p != path);
                self.recent_files.insert(0, path.clone());
                self.recent_files.truncate(RECENT_LIMIT);
            }
            cx.emit(WorkspaceEvent::ActiveFileChanged(file));
        }
        cx.notify();
    }

    fn update_title(&self, window: &mut Window, cx: &App) {
        let project = self
            .root
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned());
        let file = self.active_file(cx).map(|path| {
            let shown = self
                .root
                .as_deref()
                .and_then(|r| path.strip_prefix(r).ok())
                .unwrap_or(&path);
            shown.display().to_string()
        });
        let title = match (project, file) {
            (Some(project), Some(file)) => format!("{project} — {file}"),
            (Some(project), None) => project,
            (None, Some(file)) => file,
            (None, None) => "Gleipnir".to_string(),
        };
        window.set_window_title(&title);
        let dirty = self
            .active_editor(cx)
            .is_some_and(|e| e.read(cx).buffer().read(cx).buffer().is_dirty());
        window.set_window_edited(dirty);
    }

    /// Builds a view on a buffer, wired up for the events a host has to handle.
    fn build_item(
        &mut self,
        buffer: Entity<BufferModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<EditorView>, Vec<Subscription>) {
        let editor = cx.new(|cx| EditorView::new(buffer.clone(), window, cx));
        self.index_unsaved_text(&buffer, cx);
        let mut subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this, editor, event, window, cx| match event {
                    EditorEvent::SaveAsRequested => this.save_as(editor.clone(), window, cx),
                    EditorEvent::SaveConflict => {
                        this.resolve_save_conflict(editor.clone(), window, cx)
                    }
                    EditorEvent::SaveFailed(message) => this.show_error(message.clone(), cx),
                    // Features that know where things are defined, or debug them, subscribe to
                    // these themselves.
                    EditorEvent::GoToDefinition { .. }
                    | EditorEvent::GutterClicked { .. }
                    | EditorEvent::RunClicked { .. }
                    | EditorEvent::GutterMenu { .. }
                    | EditorEvent::GutterTextClicked { .. }
                    | EditorEvent::GutterTextMenu { .. } => {}
                },
            ),
            cx.observe_in(&buffer, window, |this, _, window, cx| {
                this.update_title(window, cx);
                cx.notify();
            }),
            // The status bar shows the cursor position, which only the view knows about.
            cx.observe(&editor, |_, _, cx| cx.notify()),
        ];
        for hook in self.editor_hooks.clone() {
            subscriptions.extend(hook(self, &editor, window, cx));
        }
        (editor, subscriptions)
    }

    /// Opens a file in the active pane, or switches to it if it's already a tab there. An image
    /// opens in a preview instead, and as there's no editor then, the task fails with
    /// [`ShownAsImage`], which callers that only wanted the file shown can ignore.
    pub fn open_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<EditorView>>> {
        if image_view::is_image(&path) {
            self.show_image(path.clone(), window, cx);
            return Task::ready(Err(ShownAsImage(path).into()));
        }
        self.open_as_text(path, window, cx)
    }

    /// Opens a file in an editor, even one that would otherwise be shown as an image, like an SVG.
    pub fn open_as_text(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<EditorView>>> {
        let open = self.store.update(cx, |store, cx| store.open(&path, cx));
        cx.spawn_in(window, async move |this, cx| {
            let buffer = match open.await {
                Ok(buffer) => buffer,
                Err(error) => {
                    this.update(cx, |this, cx| this.show_error(format!("{error:#}"), cx))
                        .ok();
                    return Err(error);
                }
            };
            this.update_in(cx, |this, window, cx| this.show_buffer(buffer, window, cx))
        })
    }

    /// Shows an image in the active pane, reusing a tab already there.
    pub fn show_image(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ImageView> {
        self.record_position(cx);
        let pane = self.active_pane.clone();
        let existing = pane
            .read(cx)
            .items()
            .iter()
            .position(|item| item.image().is_some_and(|i| i.read(cx).path() == path));
        let image = match existing {
            Some(index) => {
                pane.update(cx, |pane, cx| pane.activate(index, cx));
                pane.read(cx).items()[index]
                    .image()
                    .expect("found as an image")
                    .clone()
            }
            None => {
                let (image, subscriptions) = self.build_image(path, cx);
                pane.update(cx, |pane, cx| {
                    pane.add_item(image.clone(), subscriptions, cx)
                });
                image
            }
        };
        focus_pane(&pane, window, cx);
        self.active_item_changed(cx);
        self.update_title(window, cx);
        self.schedule_save(cx);
        image
    }

    fn build_image(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> (Entity<ImageView>, Vec<Subscription>) {
        let weak = self.weak_self.clone();
        let image = cx.new(|cx| ImageView::new(path, weak, cx));
        (image, Vec::new())
    }

    /// Shows a buffer in the active pane, reusing a tab already there.
    pub fn show_buffer(
        &mut self,
        buffer: Entity<BufferModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EditorView> {
        let pane = self.active_pane.clone();
        let existing = pane
            .read(cx)
            .items()
            .iter()
            .position(|item| item.buffer(cx) == Some(&buffer));
        let editor = match existing {
            Some(index) => {
                pane.update(cx, |pane, cx| pane.activate(index, cx));
                pane.read(cx).items()[index]
                    .editor()
                    .expect("found by its buffer")
                    .clone()
            }
            None => {
                let (editor, subscriptions) = self.build_item(buffer, window, cx);
                pane.update(cx, |pane, cx| {
                    pane.add_item(editor.clone(), subscriptions, cx)
                });
                editor
            }
        };
        focus_pane(&pane, window, cx);
        self.active_item_changed(cx);
        self.update_title(window, cx);
        editor
    }

    /// Opens a file in a tab of the active pane without switching to it or moving focus, unless
    /// a tab somewhere already shows it, for changes made from outside, like an AI agent's, that
    /// should stay unsaved and in view for the person to save or undo. Resolves to its buffer.
    pub fn open_in_background(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<BufferModel>>> {
        let open = self.store.update(cx, |store, cx| store.open(&path, cx));
        cx.spawn_in(window, async move |this, cx| {
            let buffer = open.await?;
            this.update_in(cx, |this, window, cx| {
                let shown = this.center.panes().iter().any(|pane| {
                    pane.read(cx)
                        .editors()
                        .any(|editor| editor.read(cx).buffer() == &buffer)
                });
                if !shown {
                    let (editor, subscriptions) = this.build_item(buffer.clone(), window, cx);
                    let pane = this.active_pane.clone();
                    pane.update(cx, |pane, cx| {
                        pane.add_background_item(editor, subscriptions, cx)
                    });
                    this.update_title(window, cx);
                }
                buffer
            })
        })
    }

    pub fn new_untitled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = cx.new(|cx| BufferModel::new("", None, cx));
        self.show_buffer(buffer, window, cx);
    }

    /// Splits the active pane, showing the same file in the new one at the same position.
    pub fn split_active(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.active_pane.clone();
        let new_pane = self.new_pane(window, cx);
        let active_image = source
            .read(cx)
            .active_item()
            .and_then(PaneItem::image)
            .cloned();
        if let Some(image) = active_image {
            let path = image.read(cx).path().to_path_buf();
            let (copy, subscriptions) = self.build_image(path, cx);
            new_pane.update(cx, |pane, cx| pane.add_item(copy, subscriptions, cx));
        } else if let Some(editor) = self.active_editor(cx) {
            let buffer = editor.read(cx).buffer().clone();
            let (copy, subscriptions) = self.build_item(buffer, window, cx);
            let view_source = editor.clone();
            copy.update(cx, |copy, cx| {
                let source = view_source.read(cx);
                let (selections, scroll): (SelectionSet, f32) =
                    (source.selections().clone(), source.scroll_y());
                copy.set_selections(selections, cx);
                let primary = copy.selections().primary().head;
                copy.restore_position(primary, scroll, cx);
            });
            new_pane.update(cx, |pane, cx| pane.add_item(copy, subscriptions, cx));
        }
        self.center.split(&source, new_pane.clone(), axis);
        self.active_pane = new_pane.clone();
        focus_pane(&new_pane, window, cx);
        self.active_item_changed(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn focus_neighbour_pane(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(next) = self.center.neighbour(&self.active_pane, forward) {
            self.set_active_pane(next.clone(), cx);
            focus_pane(&next, window, cx);
        }
    }

    /// How many open views show a buffer. Unsaved edits only need protecting when closing the last.
    fn view_count(&self, buffer: &Entity<BufferModel>, cx: &App) -> usize {
        self.center
            .panes()
            .iter()
            .flat_map(|pane| pane.read(cx).editors().cloned().collect::<Vec<_>>())
            .filter(|item| item.read(cx).buffer() == buffer)
            .count()
    }

    pub fn close_tab_at(
        &mut self,
        target: CloseTabAt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CloseTabAt { pane, index } = target;
        let Some(item) = pane.read(cx).items().get(index).cloned() else {
            return;
        };
        let Some(editor) = item.editor().cloned() else {
            self.remove_tab(&pane, &item, window, cx);
            return;
        };
        let buffer = editor.read(cx).buffer().clone();
        let dirty = buffer.read(cx).buffer().is_dirty();
        if !dirty || self.view_count(&buffer, cx) > 1 {
            self.remove_tab(&pane, &item, window, cx);
            return;
        }

        let name = buffer
            .read(cx)
            .path()
            .and_then(Path::file_name)
            .map_or_else(
                || "untitled".to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Save changes to {name}?"),
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            match answer.await {
                Ok(0) => {
                    let save =
                        this.update_in(cx, |this, window, cx| this.save_item(&editor, window, cx));
                    let Ok(save) = save else { return };
                    // If the save failed or was cancelled, keep the tab rather than lose the work.
                    if save.await {
                        this.update_in(cx, |this, window, cx| {
                            this.remove_tab(&pane, &item, window, cx);
                        })
                        .ok();
                    }
                }
                Ok(1) => {
                    this.update_in(cx, |this, window, cx| {
                        this.remove_tab(&pane, &item, window, cx);
                    })
                    .ok();
                }
                _ => {}
            }
        })
        .detach();
    }

    /// Saves an item, asking for a path if it needs one. Resolves to whether it was saved.
    fn save_item(
        &mut self,
        item: &Entity<EditorView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let buffer = item.read(cx).buffer().clone();
        if buffer.read(cx).path().is_some() {
            let save = buffer.update(cx, |buffer, cx| buffer.save(cx));
            return cx.spawn(async move |this, cx| match save.await {
                Ok(()) => true,
                Err(error) => {
                    this.update(cx, |this, cx| this.show_error(format!("{error:#}"), cx))
                        .ok();
                    false
                }
            });
        }
        let directory = self.default_directory();
        let prompt = cx.prompt_for_new_path(&directory, None);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return false;
            };
            let save = buffer.update(cx, |buffer, cx| buffer.save_as(path, cx));
            match save.await {
                Ok(()) => true,
                Err(error) => {
                    this.update(cx, |this, cx| this.show_error(format!("{error:#}"), cx))
                        .ok();
                    false
                }
            }
        })
    }

    fn default_directory(&self) -> PathBuf {
        self.root
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn remove_tab(
        &mut self,
        pane: &Entity<Pane>,
        item: &PaneItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let had_focus = pane.read(cx).focus_handle(cx).contains_focused(window, cx);
        pane.update(cx, |pane, cx| pane.remove_item(item, cx));

        // An emptied pane goes away, unless it's the last one.
        if pane.read(cx).is_empty() && self.center.panes().len() > 1 {
            let next = self.center.neighbour(pane, false);
            self.center.remove(pane);
            self.pane_subscriptions.remove(&pane.entity_id());
            if &self.active_pane == pane
                && let Some(next) = next
            {
                self.active_pane = next;
            }
        }
        if had_focus {
            self.focus_center(window, cx);
        }
        self.active_item_changed(cx);
        self.update_title(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Moves a tab to a position in a pane, the same one or another. A tab moved into a pane that
    /// already shows its file goes to that file's tab instead, so a pane never has a file twice.
    pub fn move_tab(
        &mut self,
        from: &Entity<Pane>,
        index: usize,
        to: &Entity<Pane>,
        to_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = from.read(cx).items().get(index).cloned() else {
            return;
        };
        if from == to {
            // Positions count the tab itself, which leaves before it's put back.
            let target = if to_index > index {
                to_index - 1
            } else {
                to_index
            };
            if target != index
                && let Some((item, subscriptions)) =
                    from.update(cx, |pane, cx| pane.take_item(index, cx))
            {
                from.update(cx, |pane, cx| {
                    pane.insert_item_at(target, item, subscriptions, cx)
                });
            }
        } else {
            let existing = to
                .read(cx)
                .items()
                .iter()
                .position(|i| i.same_content(&item, cx));
            match existing {
                Some(existing) => {
                    to.update(cx, |pane, cx| pane.activate(existing, cx));
                    self.remove_tab(from, &item, window, cx);
                }
                None => {
                    let Some((item, subscriptions)) =
                        from.update(cx, |pane, cx| pane.take_item(index, cx))
                    else {
                        return;
                    };
                    to.update(cx, |pane, cx| {
                        pane.insert_item_at(to_index, item, subscriptions, cx)
                    });
                    // An emptied pane goes, as when its last tab closes.
                    if from.read(cx).is_empty() && self.center.panes().len() > 1 {
                        self.center.remove(from);
                        self.pane_subscriptions.remove(&from.entity_id());
                    }
                }
            }
            self.active_pane = to.clone();
        }
        let target = to.read(cx).focus_target(cx);
        window.focus(&target, cx);
        self.active_item_changed(cx);
        self.update_title(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn save_as(&mut self, editor: Entity<EditorView>, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = editor.read(cx).buffer().clone();
        let directory = buffer
            .read(cx)
            .path()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| self.default_directory());
        let prompt = cx.prompt_for_new_path(&directory, None);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let save = buffer.update(cx, |buffer, cx| buffer.save_as(path, cx));
            let result = save.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => this.update_title(window, cx),
                    Err(error) => this.show_error(format!("{error:#}"), cx),
                }
                this.active_item_changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The file changed on disk while there are unsaved edits. Either side might be the one to keep,
    /// so this is the user's call.
    fn resolve_save_conflict(
        &mut self,
        editor: Entity<EditorView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = editor.read(cx).buffer().clone();
        let name = buffer
            .read(cx)
            .path()
            .and_then(Path::file_name)
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("{name} changed on disk"),
            Some("Saving will overwrite those changes. Reloading will discard yours."),
            &["Overwrite", "Reload from Disk", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let task = match answer.await {
                Ok(0) => buffer.update(cx, |buffer, cx| buffer.save(cx)),
                Ok(1) => buffer.update(cx, |buffer, cx| buffer.reload_from_disk(cx)),
                _ => return,
            };
            if let Err(error) = task.await {
                this.update(cx, |this, cx| this.show_error(format!("{error:#}"), cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Closes tabs for files at or under a path that's been deleted. Tabs with unsaved edits stay
    /// open, flagged as deleted, as closing them would lose the only copy of that work.
    pub fn close_clean_tabs_under(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for pane in self.center.panes() {
            let doomed: Vec<PaneItem> = pane
                .read(cx)
                .items()
                .iter()
                .filter(|item| {
                    !item.is_dirty(cx) && item.path(cx).is_some_and(|p| p.starts_with(path))
                })
                .cloned()
                .collect();
            for item in doomed {
                self.remove_tab(&pane, &item, window, cx);
            }
        }
    }

    /// Buffers with unsaved edits, each once however many tabs show it.
    fn dirty_items(&self, cx: &App) -> Vec<Entity<EditorView>> {
        let mut seen = Vec::new();
        let mut items = Vec::new();
        for pane in self.center.panes() {
            for item in pane.read(cx).editors() {
                let buffer = item.read(cx).buffer().clone();
                if buffer.read(cx).buffer().is_dirty() && !seen.contains(&buffer) {
                    seen.push(buffer);
                    items.push(item.clone());
                }
            }
        }
        items
    }

    /// Asks about unsaved work before the window goes away. Resolves to whether closing may go ahead.
    pub fn prepare_to_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let dirty = self.dirty_items(cx);
        if dirty.is_empty() {
            return Task::ready(true);
        }
        let message = match dirty.len() {
            1 => "You have an unsaved file.".to_string(),
            count => format!("You have {count} unsaved files."),
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Your changes will be lost if you don't save them."),
            &["Save All", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| match answer.await {
            Ok(0) => {
                for item in dirty {
                    let save =
                        this.update_in(cx, |this, window, cx| this.save_item(&item, window, cx));
                    let Ok(save) = save else { return false };
                    if !save.await {
                        return false;
                    }
                }
                true
            }
            Ok(1) => true,
            _ => false,
        })
    }

    fn window_should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.save_now(cx);
        if self.closing_confirmed || self.dirty_items(cx).is_empty() {
            return true;
        }
        let decision = self.prepare_to_close(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if decision.await {
                this.update_in(cx, |this, window, _| {
                    this.closing_confirmed = true;
                    window.remove_window();
                })
                .ok();
            }
        })
        .detach();
        false
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.save_now(cx);
        let decision = self.prepare_to_close(window, cx);
        cx.spawn(async move |_, cx| {
            if decision.await {
                cx.update(|cx| cx.quit());
            }
        })
        .detach();
    }

    fn open_file_prompt(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            for path in paths {
                let open = this.update_in(cx, |this, window, cx| this.open_path(path, window, cx));
                if let Ok(open) = open {
                    open.await.ok();
                }
            }
        })
        .detach();
    }

    fn open_folder_prompt(&mut self, _: &OpenFolder, _: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| {
                    open_project_window(Some(path), false, cx);
                });
            }
        })
        .detach();
    }

    pub fn show_message(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.set_message(message.into(), false, cx);
    }

    pub fn show_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.set_message(message.into(), true, cx);
    }

    fn set_message(&mut self, message: SharedString, error: bool, cx: &mut Context<Self>) {
        self.message = Some((message, error));
        self.message_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MESSAGE_LIFETIME).await;
            this.update(cx, |this, cx| {
                this.message = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Saves the layout soon, coalescing a burst of changes.
    pub fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.restoring || self.root.is_none() {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            this.update(cx, |this, cx| this.save_now(cx)).ok();
        }));
    }

    pub fn save_now(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.restoring {
            return;
        }
        let state = self.to_saved(cx);
        persistence::save(&root, &state).ok();
    }

    pub fn to_saved(&self, cx: &App) -> SavedWorkspace {
        fn tree(node: &PaneTree<Entity<Pane>>, cx: &App) -> SavedTree {
            match node {
                PaneTree::Pane(pane) => {
                    let pane = pane.read(cx);
                    // Untitled tabs have nothing to reopen from. Their position has to account for
                    // that, or the wrong tab would come back active.
                    let mut tabs = Vec::new();
                    let mut active = 0;
                    for (index, item) in pane.items().iter().enumerate() {
                        let saved = match item {
                            PaneItem::Editor(editor) => {
                                let view = editor.read(cx);
                                view.buffer().read(cx).path().map(|path| SavedTab {
                                    path: path.to_path_buf(),
                                    cursor: view.selections().primary().head,
                                    scroll_y: view.scroll_y(),
                                })
                            }
                            PaneItem::Image(image) => Some(SavedTab {
                                path: image.read(cx).path().to_path_buf(),
                                cursor: 0,
                                scroll_y: 0.,
                            }),
                        };
                        if let Some(saved) = saved {
                            if index == pane.active_index() {
                                active = tabs.len();
                            }
                            tabs.push(saved);
                        }
                    }
                    SavedTree::Pane(SavedPane { tabs, active })
                }
                PaneTree::Split {
                    axis,
                    children,
                    ratios,
                } => SavedTree::Split {
                    axis: *axis,
                    children: children.iter().map(|c| tree(c, cx)).collect(),
                    ratios: ratios.clone(),
                },
            }
        }

        let mut panels = HashMap::new();
        for dock in &self.docks {
            for panel in &dock.panels {
                panels.insert(panel.id(cx).to_string(), panel.save_state(cx));
            }
        }
        SavedWorkspace {
            center: tree(&self.center, cx),
            active_pane: self
                .center
                .panes()
                .iter()
                .position(|p| p == &self.active_pane)
                .unwrap_or(0),
            docks: self
                .docks
                .iter()
                .map(|dock| SavedDock {
                    position: dock.position,
                    visible: dock.visible
                        && dock.active_panel().is_none_or(|panel| panel.reopens(cx)),
                    size: dock.size.into(),
                    active_panel: dock.active_panel().map(|p| p.id(cx).to_string()),
                })
                .collect(),
            panels,
            recent_files: self.recent_files.clone(),
            recent_actions: self.recent_actions.clone(),
            sdks: self.toolchains.read(cx).choices().clone(),
        }
    }

    /// Puts back the project's saved layout, if it has one. Files that no longer exist are skipped.
    /// Returns false if there was nothing to restore.
    pub fn restore_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(root) = self.root.clone() else {
            return false;
        };
        let Some(saved) = persistence::load(&root) else {
            return false;
        };
        // Only files that still exist are worth offering.
        self.recent_files = saved
            .recent_files
            .iter()
            .filter(|p| p.is_file())
            .cloned()
            .collect();
        self.recent_actions = saved.recent_actions.clone();
        let sdks = saved.sdks.clone();
        self.toolchains.update(cx, |t, cx| t.set_choices(sdks, cx));

        for saved_dock in &saved.docks {
            let dock = self.dock_mut(saved_dock.position);
            dock.visible = saved_dock.visible;
            dock.size = px(saved_dock.size.max(MIN_DOCK_SIZE));
            let active = saved_dock.active_panel.clone();
            if let Some(active) = active {
                let index = self
                    .dock(saved_dock.position)
                    .panels
                    .iter()
                    .position(|p| p.id(cx) == active);
                if let Some(index) = index {
                    self.dock_mut(saved_dock.position).active = index;
                }
            }
        }
        for (id, state) in saved.panels {
            for dock in &self.docks {
                if let Some(panel) = dock.panels.iter().find(|p| p.id(cx) == id) {
                    panel.restore_state(state.clone(), window, cx);
                }
            }
        }

        // Build the shape first, keeping only tabs whose files still exist, and only panes with any.
        let weak = self.weak_self.clone();
        let saved_tree = saved.center.to_tree();
        let mut built: Vec<(Entity<Pane>, SavedPane)> = Vec::new();
        let new_tree = saved_tree.filter_map(&mut |saved_pane: &SavedPane| {
            let tabs: Vec<SavedTab> = saved_pane
                .tabs
                .iter()
                .filter(|t| t.path.is_file())
                .cloned()
                .collect();
            if tabs.is_empty() {
                return None;
            }
            let weak = weak.clone();
            let pane = cx.new(|cx| Pane::new(weak, window, cx));
            built.push((
                pane.clone(),
                SavedPane {
                    tabs,
                    active: saved_pane.active,
                },
            ));
            Some(pane)
        });
        let Some(new_tree) = new_tree else {
            cx.notify();
            return true;
        };

        for (pane, _) in &built {
            self.watch_pane(pane, cx);
        }
        self.pane_subscriptions
            .remove(&self.active_pane.entity_id());
        self.center = new_tree;
        let panes = self.center.panes();
        self.active_pane = panes
            .get(saved.active_pane)
            .or_else(|| panes.first())
            .expect("the tree has at least one pane")
            .clone();

        self.restoring = true;
        let active_pane = self.active_pane.clone();
        let total: usize = built.iter().map(|(_, saved)| saved.tabs.len()).sum();
        let finished = Rc::new(std::cell::Cell::new(0usize));
        for (pane, saved_pane) in built {
            let tabs_len = saved_pane.tabs.len();
            for (index, tab) in saved_pane.tabs.into_iter().enumerate() {
                let active_index = saved_pane.active.min(tabs_len - 1);
                if image_view::is_image(&tab.path) {
                    let (image, subscriptions) = self.build_image(tab.path.clone(), cx);
                    pane.update(cx, |pane, cx| {
                        pane.insert_restored(index, image, subscriptions, active_index, cx);
                    });
                    finished.set(finished.get() + 1);
                    if finished.get() == total {
                        self.restoring = false;
                        focus_pane(&active_pane, window, cx);
                        self.active_item_changed(cx);
                        self.update_title(window, cx);
                    }
                    continue;
                }
                let open = self.store.update(cx, |store, cx| store.open(&tab.path, cx));
                let pane = pane.clone();
                let active_pane = active_pane.clone();
                let finished = finished.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let buffer = open.await;
                    this.update_in(cx, |this, window, cx| {
                        if let Ok(buffer) = buffer {
                            let (item, subscriptions) = this.build_item(buffer, window, cx);
                            item.update(cx, |item, cx| {
                                item.restore_position(tab.cursor, tab.scroll_y, cx);
                            });
                            pane.update(cx, |pane, cx| {
                                // Files can finish loading in any order, so keep their saved order.
                                pane.insert_restored(index, item, subscriptions, active_index, cx);
                            });
                        }
                        finished.set(finished.get() + 1);
                        if finished.get() == total {
                            this.restoring = false;
                            focus_pane(&active_pane, window, cx);
                            this.active_item_changed(cx);
                            this.update_title(window, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
        true
    }

    /// Whether a saved layout is still being put back.
    pub fn is_restoring(&self) -> bool {
        self.restoring
    }

    pub fn snapshot(&self, window: &Window, cx: &App) -> WorkspaceSnapshot {
        let panes = self.center.panes();
        WorkspaceSnapshot {
            root: self.root.as_ref().map(|r| r.display().to_string()),
            active_pane: panes
                .iter()
                .position(|p| p == &self.active_pane)
                .unwrap_or(0),
            panes: panes
                .iter()
                .map(|pane| {
                    let pane = pane.read(cx);
                    PaneSnapshot {
                        active: pane.active_index(),
                        tabs: pane
                            .items()
                            .iter()
                            .map(|item| {
                                item.path(cx)
                                    .as_deref()
                                    .and_then(Path::file_name)
                                    .map_or_else(
                                        || "untitled".to_string(),
                                        |n| n.to_string_lossy().into_owned(),
                                    )
                            })
                            .collect(),
                    }
                })
                .collect(),
            docks: self
                .docks
                .iter()
                .map(|dock| DockSnapshot {
                    position: dock.position,
                    visible: dock.visible,
                    size: dock.size.into(),
                    panels: dock.panels.iter().map(|p| p.id(cx).to_string()).collect(),
                    active: dock.active_panel().map(|p| p.id(cx).to_string()),
                })
                .collect(),
            message: self.message.as_ref().map(|(m, _)| m.to_string()),
            panels: self
                .docks
                .iter()
                .flat_map(|dock| dock.panels.iter())
                .map(|p| (p.id(cx).to_string(), p.debug_state(cx)))
                .collect(),
            actions: list_actions(window, cx).len(),
            modal: self.modal.as_ref().map(|modal| modal.name.to_string()),
            index: self.project.as_ref().map(|project| {
                let project = project.read(cx);
                let status = project.status();
                IndexSnapshot {
                    files: status.files,
                    symbols: status.symbols,
                    busy: project.is_busy(),
                }
            }),
            toolchains: self
                .toolchains
                .read(cx)
                .packages()
                .iter()
                .map(|p| {
                    let snapshot = ToolchainSnapshot {
                        sdk: p.chosen.as_ref().map(|s| s.root.display().to_string()),
                        sdk_version: p.chosen.as_ref().and_then(|s| s.version.clone()),
                        tools: p
                            .tools
                            .iter()
                            .map(|t| {
                                (
                                    t.name.clone(),
                                    t.resolution.path().map(|p| p.display().to_string()),
                                )
                            })
                            .collect(),
                    };
                    (p.package_id.clone(), snapshot)
                })
                .collect(),
            recent_files: self
                .recent_files
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            language_servers: self
                .language_servers
                .read(cx)
                .statuses()
                .map(|(id, status)| {
                    let status = match status {
                        gleipnir_core::ServerStatus::Starting => "starting".to_string(),
                        gleipnir_core::ServerStatus::Installing => "installing".to_string(),
                        gleipnir_core::ServerStatus::Running { busy: None } => {
                            "running".to_string()
                        }
                        gleipnir_core::ServerStatus::Running { busy: Some(task) } => {
                            format!("busy: {task}")
                        }
                        gleipnir_core::ServerStatus::Missing(why) => format!("missing: {why}"),
                        gleipnir_core::ServerStatus::Failed(why) => format!("failed: {why}"),
                    };
                    (id.to_string(), status)
                })
                .collect(),
            theme: {
                let theme = cx.global::<Theme>();
                let background = gpui::Rgba::from(theme.background);
                format!(
                    "{} (background #{:02x}{:02x}{:02x})",
                    theme.name,
                    (background.r * 255.).round() as u8,
                    (background.g * 255.).round() as u8,
                    (background.b * 255.).round() as u8,
                )
            },
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(resize) = self.resize.clone() else {
            return;
        };
        if !event.dragging() {
            self.resize = None;
            return;
        }
        let window_size = self.window_size;
        // In the content's coordinates, as the sizes are.
        let event = &MouseMoveEvent {
            position: event.position - self.content_origin,
            ..event.clone()
        };
        match resize {
            ResizeDrag::Dock(position) => {
                let rail = px(RAIL_WIDTH);
                let opposite = |pos: DockPosition, this: &Self| {
                    let dock = this.dock(pos);
                    if dock.visible { dock.size } else { px(0.) }
                };
                let requested = match position {
                    DockPosition::Left => event.position.x - rail,
                    DockPosition::Right => window_size.width - event.position.x - rail,
                    DockPosition::Bottom => {
                        window_size.height - event.position.y - px(STATUS_BAR_HEIGHT)
                    }
                };
                let available = match position {
                    DockPosition::Left => {
                        window_size.width - rail - rail - opposite(DockPosition::Right, self)
                    }
                    DockPosition::Right => {
                        window_size.width - rail - rail - opposite(DockPosition::Left, self)
                    }
                    DockPosition::Bottom => {
                        window_size.height - px(STATUS_BAR_HEIGHT) - px(TITLE_BAR_HEIGHT)
                    }
                };
                let max = (available - px(MIN_CENTER_SIZE)).max(px(MIN_DOCK_SIZE));
                self.dock_mut(position).size = requested.clamp(px(MIN_DOCK_SIZE), max);
            }
            ResizeDrag::Split {
                path,
                index,
                axis,
                last,
            } => {
                let Some(bounds) = self.split_bounds.borrow().get(&path).copied() else {
                    return;
                };
                let delta = match axis {
                    Axis::Horizontal => (event.position.x - last.x) / bounds.size.width,
                    Axis::Vertical => (event.position.y - last.y) / bounds.size.height,
                };
                self.center.resize(&path, index, delta);
                self.resize = Some(ResizeDrag::Split {
                    path,
                    index,
                    axis,
                    last: event.position,
                });
            }
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn end_resize(&mut self, cx: &mut Context<Self>) {
        if self.resize.take().is_some() {
            self.schedule_save(cx);
            cx.notify();
        }
    }

    /// What an empty pane shows: the first things someone might want to do.
    pub(crate) fn render_empty_hint(&self, window: &Window, cx: &App) -> AnyElement {
        let theme = cx.global::<Theme>().clone();
        let shortcut = |action: &dyn gpui::Action| shortcut_label(action, window, cx);
        let hint = |label: &'static str, keys: String| {
            div()
                .flex()
                .flex_row()
                .justify_between()
                .gap(px(32.))
                .child(div().text_color(theme.text_muted).child(label))
                .child(div().text_color(theme.text_faint).child(keys))
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .w(px(280.))
                    .children(self.empty_hints.iter().map(|(label, keys)| {
                        let keys = match keys {
                            EmptyHint::Action(action) => shortcut(action.as_ref()),
                            EmptyHint::Text(text) => text.to_string(),
                        };
                        hint(label, keys)
                    }))
                    .child(hint("Open folder", shortcut(&OpenFolder))),
            )
            .into_any_element()
    }

    fn render_split(
        &self,
        node: &PaneTree<Entity<Pane>>,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            PaneTree::Pane(pane) => div()
                .size_full()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(pane.clone())
                .into_any_element(),
            PaneTree::Split {
                axis,
                children,
                ratios,
            } => {
                let theme = cx.global::<Theme>().clone();
                let horizontal = *axis == Axis::Horizontal;
                let bounds_store = self.split_bounds.clone();
                let bounds_key = path.clone();
                let mut container = div()
                    .relative()
                    .flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .when(horizontal, |d| d.flex_row())
                    .when(!horizontal, |d| d.flex_col())
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                bounds_store.borrow_mut().insert(bounds_key, bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    );
                let count = children.len();
                for (index, (child, ratio)) in children.iter().zip(ratios).enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(index);
                    let element = self.render_split(child, child_path, cx);
                    let cell = div()
                        .relative()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .when(horizontal, |c| c.h_full().w(relative(*ratio)))
                        .when(!horizontal, |c| c.w_full().h(relative(*ratio)))
                        .when(index > 0, |c| {
                            if horizontal {
                                c.border_l_1().border_color(theme.border)
                            } else {
                                c.border_t_1().border_color(theme.border)
                            }
                        })
                        .child(element);
                    container = container.child(cell);
                    if index + 1 < count {
                        // A wider invisible target over the one-pixel line, placed by the sum of
                        // the ratios up to here.
                        let offset: f32 = ratios[..=index].iter().sum();
                        let handle_path = path.clone();
                        let handle = div()
                            .id(gpui::SharedString::from(format!(
                                "split-handle-{path:?}-{index}"
                            )))
                            .debug_selector(|| format!("split-handle-{index}"))
                            .absolute()
                            .when(horizontal, |h| {
                                h.top_0()
                                    .h_full()
                                    .w(px(7.))
                                    .left(relative(offset))
                                    .ml(px(-3.))
                                    .cursor_col_resize()
                            })
                            .when(!horizontal, |h| {
                                h.left_0()
                                    .w_full()
                                    .h(px(7.))
                                    .top(relative(offset))
                                    .mt(px(-3.))
                                    .cursor_row_resize()
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                    this.resize = Some(ResizeDrag::Split {
                                        path: handle_path.clone(),
                                        index,
                                        axis: if horizontal {
                                            Axis::Horizontal
                                        } else {
                                            Axis::Vertical
                                        },
                                        last: event.position,
                                    });
                                    cx.stop_propagation();
                                }),
                            );
                        container = container.child(deferred_handle(handle));
                    }
                }
                container.into_any_element()
            }
        }
    }

    fn render_dock(&self, position: DockPosition, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dock = self.dock(position);
        if !dock.visible {
            return None;
        }
        let panel = dock.active_panel()?;
        let theme = cx.global::<Theme>().clone();
        let size = dock.size;
        let handle = div()
            .id(gpui::SharedString::from(format!(
                "dock-handle-{position:?}"
            )))
            .debug_selector(|| format!("dock-handle-{position:?}"))
            .absolute()
            .map(|h| match position {
                DockPosition::Left => h
                    .top_0()
                    .bottom_0()
                    .right(px(-3.))
                    .w(px(7.))
                    .cursor_col_resize(),
                DockPosition::Right => h
                    .top_0()
                    .bottom_0()
                    .left(px(-3.))
                    .w(px(7.))
                    .cursor_col_resize(),
                DockPosition::Bottom => h
                    .left_0()
                    .right_0()
                    .top(px(-3.))
                    .h(px(7.))
                    .cursor_row_resize(),
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.resize = Some(ResizeDrag::Dock(position));
                    cx.stop_propagation();
                }),
            );
        let base = div()
            .relative()
            .flex_none()
            .min_w_0()
            .min_h_0()
            .bg(theme.surface)
            .border_color(theme.border)
            .map(|d| match position {
                DockPosition::Left => d.w(size).h_full().border_r_1(),
                DockPosition::Right => d.w(size).h_full().border_l_1(),
                DockPosition::Bottom => d.h(size).w_full().border_t_1(),
            })
            .child(panel.view())
            .child(handle);
        Some(base.into_any_element())
    }

    fn render_rail(
        &self,
        side: DockPosition,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.global::<Theme>().clone();
        // The bottom dock's panels show lower on the left rail, as they're a separate group.
        let upper = self.rail_buttons(side, &theme, window, cx);
        let lower = if side == DockPosition::Left {
            self.rail_buttons(DockPosition::Bottom, &theme, window, cx)
        } else {
            Vec::new()
        };
        if upper.is_empty() && lower.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .justify_between()
                .flex_none()
                .w(px(RAIL_WIDTH))
                .h_full()
                .py(px(6.))
                .bg(theme.surface)
                .border_color(theme.border)
                .when(side == DockPosition::Left, |r| r.border_r_1())
                .when(side == DockPosition::Right, |r| r.border_l_1())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(4.))
                        .children(upper),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(4.))
                        .children(lower),
                )
                .into_any_element(),
        )
    }

    fn rail_buttons(
        &self,
        position: DockPosition,
        theme: &Theme,
        _: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let dock = self.dock(position);
        dock.panels
            .iter()
            .enumerate()
            .map(|(index, panel)| {
                let id = panel.id(cx);
                let active = dock.visible && dock.active == index;
                div()
                    .id(gpui::SharedString::from(format!("rail-{id}")))
                    .debug_selector(|| format!("rail-{id}"))
                    .size(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .when(active, |b| b.bg(theme.selection_inactive))
                    .hover(|b| b.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.click_panel_button(id, window, cx);
                    }))
                    .child(
                        svg()
                            .path(panel.icon(cx).path())
                            .size(px(18.))
                            .text_color(if active {
                                theme.accent
                            } else {
                                theme.text_muted
                            }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn render_status_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.global::<Theme>().clone();
        let items: Vec<AnyElement> = self
            .status_items
            .clone()
            .iter()
            .filter_map(|item| item(self, cx))
            .collect();
        let editor = self.active_editor(cx);
        let right = editor.as_ref().map(|editor| {
            let view = editor.read(cx);
            let model = view.buffer().read(cx);
            let buffer = model.buffer();
            let point = buffer.offset_to_point(view.selections().primary().head);
            let cursors = match view.selections().len() {
                1 => String::new(),
                n => format!("{n} cursors   "),
            };
            let config = model.edit_config();
            let indent = if config.hard_tabs {
                "Tab".to_string()
            } else {
                format!("{} spaces", config.tab_size)
            };
            let disk = match model.disk_status() {
                DiskStatus::InSync => "",
                DiskStatus::Conflict => "changed on disk   ",
                DiskStatus::Deleted => "deleted on disk   ",
            };
            format!(
                "{disk}{cursors}{}:{}   {}   {}   UTF-8   {indent}",
                point.line + 1,
                point.col + 1,
                model.language().map_or("Plain Text", |l| l.name()),
                if buffer.is_crlf() { "CRLF" } else { "LF" },
            )
        });

        let left: AnyElement = match &self.message {
            Some((message, is_error)) => div()
                .truncate()
                .text_color(if *is_error {
                    theme.error
                } else {
                    theme.text_muted
                })
                .child(message.clone())
                .into_any_element(),
            None => self.render_breadcrumbs(&theme, cx),
        };

        div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .gap(px(16.))
            .h(px(STATUS_BAR_HEIGHT))
            .flex_none()
            .px(px(10.))
            .bg(theme.surface)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .child(div().flex_1().min_w_0().overflow_hidden().child(left))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(16.))
                    .text_color(theme.text_muted)
                    .children(items)
                    .children(right),
            )
            .into_any_element()
    }

    /// The active file's path as clickable segments, each revealing that folder in the project tree.
    fn render_breadcrumbs(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(path) = self.active_file(cx) else {
            return div().into_any_element();
        };
        let segments = breadcrumb_segments(self.root.as_deref(), &path);
        let count = segments.len();
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .whitespace_nowrap();
        for (index, (label, target)) in segments.into_iter().enumerate() {
            if index > 0 {
                row = row.child(
                    svg()
                        .path(Icon::ChevronRight.path())
                        .size(px(11.))
                        .text_color(theme.text_faint),
                );
            }
            let is_last = index + 1 == count;
            let color = if is_last {
                theme.text
            } else {
                theme.text_muted
            };
            row = row.child(
                div()
                    .id(("crumb", index))
                    .debug_selector(|| format!("crumb-{index}"))
                    .px(px(3.))
                    .rounded(px(3.))
                    .text_color(color)
                    .hover(|c| c.bg(theme.hover))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(WorkspaceEvent::RevealPath(target.clone()));
                    }))
                    .child(label),
            );
        }
        row.into_any_element()
    }
}

fn focus_pane(pane: &Entity<Pane>, window: &mut Window, cx: &mut App) {
    let handle = pane.read(cx).focus_target(cx);
    window.focus(&handle, cx);
}

/// A divider's drag target is painted after the panes either side of it, so it's on top of both.
fn deferred_handle(handle: impl IntoElement) -> impl IntoElement {
    gpui::deferred(handle).with_priority(1)
}

/// Splits a file's path into (label, path to reveal) pairs, from the project root down, or just the
/// file name for a file outside the project.
pub fn breadcrumb_segments(root: Option<&Path>, path: &Path) -> Vec<(String, PathBuf)> {
    let name_of = |p: &Path| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    let Some((root, relative)) = root.and_then(|r| Some((r, path.strip_prefix(r).ok()?))) else {
        return vec![(name_of(path), path.to_path_buf())];
    };
    let mut segments = vec![(name_of(root), root.to_path_buf())];
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        segments.push((
            component.as_os_str().to_string_lossy().into_owned(),
            current.clone(),
        ));
    }
    segments
}

pub(crate) const RAIL_WIDTH: f32 = 40.;
pub(crate) const STATUS_BAR_HEIGHT: f32 = 24.;

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let insets = decorations::content_insets(window);
        let viewport = window.viewport_size();
        self.window_size = size(
            viewport.width - insets.left - insets.right,
            viewport.height - insets.top - insets.bottom,
        );
        self.content_origin = Point::new(insets.left, insets.top);
        self.update_title(window, cx);
        // With nothing focused, key bindings still need a path to reach the window's actions.
        if window.focused(cx).is_none() {
            self.focus_center(window, cx);
        }

        let center = self.render_split(&self.center.clone(), Vec::new(), cx);
        let extra_actions = self.extra_actions.clone();
        let resizing = self.resize.clone();

        let root = div()
            .relative()
            .track_focus(&self.focus_handle)
            .key_context("Workspace")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(theme.ui_font)
            .text_size(px(theme.ui_font_size))
            .when_some(resizing, |root, resize| match resize {
                ResizeDrag::Dock(DockPosition::Bottom) => root.cursor_row_resize(),
                ResizeDrag::Split {
                    axis: Axis::Vertical,
                    ..
                } => root.cursor_row_resize(),
                _ => root.cursor_col_resize(),
            })
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::open_file_prompt))
            .on_action(cx.listener(Self::open_folder_prompt))
            .on_action(cx.listener(Self::hide_active_dock))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| this.new_untitled(window, cx)))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| {
                if let Some(editor) = this.active_editor(cx) {
                    this.save_as(editor, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let pane = this.active_pane.clone();
                let index = pane.read(cx).active_index();
                this.close_tab_at(CloseTabAt { pane, index }, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                let pane = this.active_pane.clone();
                pane.update(cx, |pane, cx| pane.activate_relative(true, cx));
                focus_pane(&pane, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                let pane = this.active_pane.clone();
                pane.update(cx, |pane, cx| pane.activate_relative(false, cx));
                focus_pane(&pane, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split_active(Axis::Horizontal, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split_active(Axis::Vertical, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNextPane, window, cx| {
                this.focus_neighbour_pane(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousPane, window, cx| {
                this.focus_neighbour_pane(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusEditor, window, cx| {
                this.focus_center(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &NavigateBack, window, cx| this.navigate(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NavigateForward, window, cx| {
                    this.navigate(false, window, cx)
                }),
            )
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_resize(cx)),
            );
        let root = extra_actions.iter().fold(root, |root, add| add(root, cx));

        let left_rail = self.render_rail(DockPosition::Left, window, cx);
        let right_rail = self.render_rail(DockPosition::Right, window, cx);
        let left = self.render_dock(DockPosition::Left, cx);
        let right = self.render_dock(DockPosition::Right, cx);
        let bottom = self.render_dock(DockPosition::Bottom, cx);
        let status = self.render_status_bar(cx);
        let title_bar = self.render_title_bar(window, cx);
        let menu = self.render_menu();
        let viewport = window.viewport_size();
        let modal = self.modal.as_ref().map(|modal| match modal.anchor {
            Some(anchor) => {
                let left = anchor
                    .left()
                    .min(viewport.width - px(ANCHORED_WIDTH + 8.))
                    .max(px(8.));
                let room_below = viewport.height - anchor.bottom();
                let placed = div().absolute().left(left);
                if room_below >= px(ANCHORED_HEIGHT) || room_below >= anchor.top() {
                    placed.top(anchor.bottom() + px(2.))
                } else {
                    placed.bottom(viewport.height - anchor.top() + px(2.))
                }
                .child(modal.view.clone())
            }
            None => div()
                .absolute()
                .top(px(72.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(modal.view.clone()),
        });

        let root = root
            .child(title_bar)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .children(left_rail)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_1()
                                    .min_h_0()
                                    .children(left)
                                    .child(div().flex_1().min_w_0().min_h_0().child(center))
                                    .children(right),
                            )
                            .children(bottom),
                    )
                    .children(right_rail),
            )
            .child(status)
            .children(modal)
            .children(menu);
        decorations::frame(root, &theme, window)
    }
}

/// The keys that do an action where focus is now, as menus show them, or nothing if there are none.
pub(crate) fn shortcut_label(action: &dyn gpui::Action, window: &Window, cx: &App) -> String {
    window
        .highest_precedence_binding_for_action(action)
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|k| k.unparse(cx.platform_info().os))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// What [`Workspace::open_path`] fails with when the file was shown as an image, so there's no
/// editor to give back.
#[derive(Debug)]
pub struct ShownAsImage(pub PathBuf);

impl std::fmt::Display for ShownAsImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is shown as an image, not in an editor",
            self.0.display()
        )
    }
}

impl std::error::Error for ShownAsImage {}

/// Opens a window on a project. With no root, it comes up empty. Restores the project's saved layout
/// unless `fresh` is set.
pub fn open_project_window(
    root: Option<PathBuf>,
    fresh: bool,
    cx: &mut App,
) -> WindowHandle<Workspace> {
    let store = cx.global::<GlobalStore>().0.clone();
    let init = cx.try_global::<WorkspaceInit>().cloned();
    let bounds = Bounds::centered(None, size(px(1240.), px(800.)), cx);
    let scripted = std::env::var_os("GLEIPNIR_SCRIPT").is_some();
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // Lets window manager rules, such as Hyprland's, target this app.
            app_id: Some("gleipnir".to_string()),
            // The title bar is drawn by the workspace, as IntelliJ's main toolbar.
            titlebar: Some(gpui::TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(title_bar::window_buttons_position()),
            }),
            app_owns_titlebar_drag: true,
            // On Linux the window's buttons are drawn in the title bar too, as on macOS.
            window_decorations: decorations::requested(),
            // A scripted run shouldn't take focus from whatever else is going on.
            focus: !scripted,
            show: true,
            ..Default::default()
        },
        move |window, cx| {
            cx.new(|cx| {
                let mut workspace = Workspace::new(store, root, window, cx);
                if let Some(init) = init {
                    (init.0)(&mut workspace, window, cx);
                }
                if fresh || !workspace.restore_layout(window, cx) {
                    // A first visit to a project starts with its tree showing.
                    if workspace.root().is_some() {
                        for dock in &mut workspace.docks {
                            if dock.position == DockPosition::Left && !dock.panels.is_empty() {
                                dock.visible = true;
                            }
                        }
                    }
                }
                workspace
            })
        },
    )
    .expect("failed to open a window")
}
