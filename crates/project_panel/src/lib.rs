//! The project tree: the panel on the left.

use anyhow::Result;
use gleipnir_core::{BufferStore, Project, ProjectEvent};
use gleipnir_index::{
    DirEntry, EntryKind, FileTree, GitState, GitStatus, Row, fs_ops, read_directory,
};
use gleipnir_ui::{
    ContextMenu, FileIcon, Icon, ListNav, MenuDismissed, TextInput, Theme, file_icon,
};
use gleipnir_workspace::{DockPosition, FocusEditor, OpenFolder, Panel, Workspace, WorkspaceEvent};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, FocusHandle,
    Focusable, KeyBinding, KeyContext, MouseButton, MouseDownEvent, Pixels, Point, PromptLevel,
    Render, SharedString, Subscription, WeakEntity, Window, actions, anchored, deferred, div,
    prelude::*, px, svg, uniform_list,
};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

actions!(
    project_panel,
    [
        Toggle,
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        ExpandOrDescend,
        CollapseOrAscend,
        Open,
        NewFile,
        NewFolder,
        Rename,
        Delete,
        CopyPath,
        RevealInSystem,
        RevealActiveFile,
        Confirm,
        Cancel,
    ]
);

const PANEL_ID: &str = "project";
const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 14.;

pub fn bind_keys(cx: &mut App) {
    let browsing = Some("ProjectPanel && !editing");
    let editing = Some("ProjectPanel && editing");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, browsing),
        KeyBinding::new("down", SelectNext, browsing),
        KeyBinding::new("home", SelectFirst, browsing),
        KeyBinding::new("end", SelectLast, browsing),
        KeyBinding::new("right", ExpandOrDescend, browsing),
        KeyBinding::new("left", CollapseOrAscend, browsing),
        KeyBinding::new("enter", Open, browsing),
        KeyBinding::new("shift-f6", Rename, browsing),
        KeyBinding::new("delete", Delete, browsing),
        KeyBinding::new("escape", FocusEditor, browsing),
        KeyBinding::new("enter", Confirm, editing),
        KeyBinding::new("escape", Cancel, editing),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-1", Toggle, None),
        KeyBinding::new("alt-f1", RevealActiveFile, None),
        KeyBinding::new("cmd-backspace", Delete, browsing),
        KeyBinding::new("cmd-n", NewFile, browsing),
        KeyBinding::new("cmd-shift-n", NewFolder, browsing),
        KeyBinding::new("cmd-shift-c", CopyPath, browsing),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("alt-1", Toggle, None),
        KeyBinding::new("alt-f1", RevealActiveFile, None),
        KeyBinding::new("alt-insert", NewFile, browsing),
        KeyBinding::new("ctrl-alt-insert", NewFolder, browsing),
        KeyBinding::new("ctrl-shift-c", CopyPath, browsing),
    ]);
}

/// Adds the panel to a workspace.
pub fn add_to_workspace(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let handle = cx.weak_entity();
    let project = workspace.project().cloned();
    let store = workspace.store().clone();
    let workspace_entity = cx.entity();
    let panel =
        cx.new(|cx| ProjectPanel::new(handle, workspace_entity, store, project, window, cx));
    workspace.add_panel(panel.clone(), cx);
    workspace.register_action(|workspace, _: &Toggle, window, cx| {
        workspace.toggle_panel(PANEL_ID, window, cx);
    });
    let panel_for_reveal = panel.downgrade();
    workspace.register_action(move |workspace, _: &RevealActiveFile, window, cx| {
        let Some(path) = workspace.active_file(cx) else {
            return;
        };
        workspace.show_panel(PANEL_ID, cx);
        if let Some(panel) = panel_for_reveal.upgrade() {
            panel.update(cx, |panel, cx| {
                panel.reveal(&path, cx);
                window.focus(&panel.focus_handle, cx);
            });
        }
    });
}

/// What's being typed into the tree.
enum EditKind {
    NewFile { dir: PathBuf },
    NewFolder { dir: PathBuf },
    Rename { path: PathBuf },
}

struct InlineEdit {
    kind: EditKind,
    input: Entity<TextInput>,
    _blur: Subscription,
}

/// A line in the list: a tree row, or the input for a new entry, which has no row of its own yet.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DisplayRow {
    Entry(usize),
    NewEntry { depth: usize },
}

/// The payload while dragging a row, to move it into another folder.
#[derive(Clone)]
struct DraggedEntry {
    path: PathBuf,
    name: SharedString,
}

struct DragPreview {
    name: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(4.))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .text_color(theme.text)
            .text_size(px(theme.ui_font_size))
            .child(self.name.clone())
    }
}

#[derive(Serialize, Deserialize, Default)]
struct SavedPanel {
    /// Relative to the root, so a project can move without losing this.
    expanded: Vec<PathBuf>,
    selected: Option<PathBuf>,
}

/// Moves a path to the Trash. Replaceable so tests don't fill the real one.
type TrashFn = Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>;

pub struct ProjectPanel {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    store: Entity<BufferStore>,
    tree: Option<FileTree>,
    rows: Vec<Row>,
    display: Vec<DisplayRow>,
    nav: ListNav,
    selected: Option<PathBuf>,
    /// A path to select once the folders leading to it have loaded.
    pending_reveal: Option<PathBuf>,
    project: Option<Entity<Project>>,
    edit: Option<InlineEdit>,
    menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    trash: TrashFn,
    _subscriptions: Vec<Subscription>,
}

impl ProjectPanel {
    fn new(
        workspace: WeakEntity<Workspace>,
        workspace_entity: Entity<Workspace>,
        store: Entity<BufferStore>,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = project.as_ref().map(|p| p.read(cx).root().to_path_buf());
        let mut subscriptions = vec![cx.subscribe_in(
            &workspace_entity,
            window,
            |this, workspace, event, window, cx| match event {
                WorkspaceEvent::ActiveFileChanged(Some(path)) => this.reveal(path, cx),
                WorkspaceEvent::ActiveFileChanged(None) => {}
                WorkspaceEvent::RevealPath(path) => {
                    workspace.update(cx, |workspace, cx| workspace.show_panel(PANEL_ID, cx));
                    this.reveal(path, cx);
                    window.focus(&this.focus_handle, cx);
                }
            },
        )];
        if let Some(project) = &project {
            subscriptions.push(cx.subscribe(project, |this, _, event, cx| match event {
                ProjectEvent::FilesChanged(paths) => this.files_changed(paths, cx),
                ProjectEvent::GitStatusChanged => cx.notify(),
                ProjectEvent::FileListChanged | ProjectEvent::SymbolsChanged => {}
            }));
        }
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            workspace,
            store,
            tree: root.clone().map(FileTree::new),
            rows: Vec::new(),
            display: Vec::new(),
            nav: ListNav::new(),
            selected: None,
            pending_reveal: None,
            project,
            edit: None,
            menu: None,
            trash: Arc::new(fs_ops::move_to_trash),
            _subscriptions: subscriptions,
        };
        this.refresh_rows(cx);
        this.load_pending(cx);
        this
    }

    /// Replaces how deleted files are disposed of, so tests don't touch the real Trash.
    pub fn set_trash(&mut self, trash: impl Fn(&Path) -> Result<()> + Send + Sync + 'static) {
        self.trash = Arc::new(trash);
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// The visible entries' names, indented by depth, for tests.
    pub fn visible_names(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.entry.name))
            .collect()
    }

    pub fn is_loading(&self) -> bool {
        self.rows.iter().any(|row| row.loading)
    }

    /// Re-reads the folders affected by a batch of changes. Folders nobody has opened aren't read,
    /// which is what keeps a build churning through `target/` from costing anything.
    fn files_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let Some(tree) = &self.tree else { return };
        let root = tree.root().to_path_buf();
        let mut dirs: Vec<PathBuf> = Vec::new();
        for path in paths {
            let Ok(relative) = path.strip_prefix(&root) else {
                continue;
            };
            if relative.starts_with(".git") {
                continue;
            }
            if tree.is_loaded(path) {
                dirs.push(path.clone());
            }
            if let Some(parent) = path.parent()
                && tree.is_loaded(parent)
            {
                dirs.push(parent.to_path_buf());
            }
        }
        dirs.sort();
        dirs.dedup();
        for dir in dirs {
            self.reload_dir(dir, cx);
        }
    }

    /// Reads whichever expanded folders haven't been read yet, off the UI thread.
    fn load_pending(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else { return };
        for dir in tree.dirs_to_load() {
            self.read_dir_in_background(dir, cx);
        }
    }

    fn reload_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.read_dir_in_background(dir, cx);
    }

    fn read_dir_in_background(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(tree) = &self.tree else { return };
        let root = tree.root().to_path_buf();
        let read_dir = dir.clone();
        let read = cx.background_spawn(async move { read_directory(&root, &read_dir) });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| this.dir_loaded(dir, result.ok(), cx))
                .ok();
        })
        .detach();
    }

    fn dir_loaded(&mut self, dir: PathBuf, entries: Option<Vec<DirEntry>>, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else { return };
        match entries {
            Some(entries) => tree.set_children(&dir, entries),
            None => tree.load_failed(&dir),
        }
        self.refresh_rows(cx);
        // What just loaded may include expanded folders.
        self.load_pending(cx);
    }

    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if let Some(project) = &self.project {
            project.update(cx, |project, cx| project.refresh_git(cx));
        }
    }

    fn git_status(&self, cx: &App) -> GitStatus {
        self.project
            .as_ref()
            .map(|p| p.read(cx).git().clone())
            .unwrap_or_default()
    }

    /// Rebuilds the visible list, keeping the selection on the same path.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        self.rows = self.tree.as_ref().map_or_else(Vec::new, FileTree::rows);

        // A pending reveal completes as soon as its target is visible.
        if let Some(target) = self.pending_reveal.clone()
            && self.rows.iter().any(|row| row.entry.path == target)
        {
            self.selected = Some(target);
            self.pending_reveal = None;
        }
        // If the selected entry went away, its parent is the nearest thing left.
        if let Some(selected) = self.selected.clone()
            && !self.rows.iter().any(|row| row.entry.path == selected)
        {
            self.selected = selected
                .ancestors()
                .skip(1)
                .find(|a| self.rows.iter().any(|row| row.entry.path == *a))
                .map(Path::to_path_buf);
        }
        self.rebuild_display();
        self.sync_nav();
        cx.notify();
    }

    fn rebuild_display(&mut self) {
        let new_in = self.edit.as_ref().and_then(|edit| match &edit.kind {
            EditKind::NewFile { dir } | EditKind::NewFolder { dir } => Some(dir.clone()),
            EditKind::Rename { .. } => None,
        });
        self.display.clear();
        for (index, row) in self.rows.iter().enumerate() {
            self.display.push(DisplayRow::Entry(index));
            if new_in.as_deref() == Some(row.entry.path.as_path()) {
                self.display.push(DisplayRow::NewEntry {
                    depth: row.depth + 1,
                });
            }
        }
        self.nav.set_count(self.display.len());
    }

    fn selected_row_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| &row.entry.path == selected)
    }

    /// Scrolls the selected row into view, and keeps the list's own index in step.
    fn sync_nav(&mut self) {
        let Some(row) = self.selected_row_index() else {
            return;
        };
        if let Some(display) = self
            .display
            .iter()
            .position(|d| d == &DisplayRow::Entry(row))
        {
            self.nav.select(display);
        }
    }

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(index) {
            self.selected = Some(row.entry.path.clone());
            self.pending_reveal = None;
            self.sync_nav();
            cx.notify();
        }
    }

    fn select_relative(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let next = match self.selected_row_index() {
            None => 0,
            Some(index) if forward => (index + 1).min(self.rows.len() - 1),
            Some(index) => index.saturating_sub(1),
        };
        self.select_row(next, cx);
    }

    /// Right: opens a closed folder, or moves into an open one.
    fn expand_or_descend(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_row_index() else {
            self.select_row(0, cx);
            return;
        };
        let row = &self.rows[index];
        if row.entry.kind != EntryKind::Dir {
            return;
        }
        if row.expanded {
            if self
                .rows
                .get(index + 1)
                .is_some_and(|next| next.depth > self.rows[index].depth)
            {
                self.select_row(index + 1, cx);
            }
        } else {
            let path = row.entry.path.clone();
            self.set_expanded(&path, true, cx);
        }
    }

    /// Left: closes an open folder, or moves up to the parent.
    fn collapse_or_ascend(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_row_index() else {
            return;
        };
        let row = &self.rows[index];
        if row.expanded {
            let path = row.entry.path.clone();
            self.set_expanded(&path, false, cx);
        } else if let Some(parent) = row.entry.path.parent().map(Path::to_path_buf)
            && let Some(parent_index) = self.rows.iter().position(|r| r.entry.path == parent)
        {
            self.select_row(parent_index, cx);
        }
    }

    fn set_expanded(&mut self, dir: &Path, expanded: bool, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else { return };
        if expanded {
            tree.expand(dir);
        } else {
            tree.collapse(dir);
        }
        self.refresh_rows(cx);
        self.load_pending(cx);
        self.save_soon(cx);
    }

    /// Shows and selects a path, opening the folders above it. They may need to load first.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else { return };
        if !tree.expand_to(path) {
            return;
        }
        self.pending_reveal = Some(path.to_path_buf());
        self.refresh_rows(cx);
        self.load_pending(cx);
        self.save_soon(cx);
    }

    fn save_soon(&self, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            // Deferred, as this can run while the workspace is mid-update.
            cx.defer(move |cx| workspace.update(cx, |workspace, cx| workspace.schedule_save(cx)));
        }
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_row_index() else {
            return;
        };
        let row = &self.rows[index];
        let path = row.entry.path.clone();
        if row.entry.kind == EntryKind::Dir {
            let expanded = row.expanded;
            self.set_expanded(&path, !expanded, cx);
        } else {
            self.open_file(path, window, cx);
        }
    }

    fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        // Deferred, so this panel isn't still being updated when the workspace reports back to it.
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_path(path, window, cx).detach();
            });
        });
    }

    fn show_error(&self, message: String, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            cx.defer(move |cx| workspace.update(cx, |w, cx| w.show_error(message, cx)));
        }
    }

    /// The folder a new entry goes in: the selection if it's a folder, otherwise the one it's in.
    fn target_dir(&self) -> Option<PathBuf> {
        let root = self.tree.as_ref()?.root().to_path_buf();
        let Some(index) = self.selected_row_index() else {
            return Some(root);
        };
        let entry = &self.rows[index].entry;
        if entry.kind == EntryKind::Dir {
            Some(entry.path.clone())
        } else {
            entry.path.parent().map(Path::to_path_buf).or(Some(root))
        }
    }

    fn start_edit(
        &mut self,
        kind: EditKind,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", cx);
            if !initial.is_empty() {
                input.set_text(initial, cx);
            }
            input
        });
        let input_focus = input.read(cx).focus_handle(cx);
        // Clicking elsewhere abandons the edit, as there's no dialog to hold it open.
        let blur = cx.on_blur(&input_focus, window, |this, window, cx| {
            this.cancel_edit(window, cx, false);
        });
        window.focus(&input_focus, cx);
        self.edit = Some(InlineEdit {
            kind,
            input,
            _blur: blur,
        });
        self.rebuild_display();
        cx.notify();
    }

    fn start_new(&mut self, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.target_dir() else { return };
        // The new entry shows inside the folder, so it has to be open.
        self.set_expanded(&dir, true, cx);
        let kind = if folder {
            EditKind::NewFolder { dir }
        } else {
            EditKind::NewFile { dir }
        };
        self.start_edit(kind, "", window, cx);
    }

    fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_row_index() else {
            return;
        };
        // The root's name is the folder's own, outside what the project manages.
        if index == 0 {
            return;
        }
        let entry = self.rows[index].entry.clone();
        self.start_edit(
            EditKind::Rename { path: entry.path },
            &entry.name,
            window,
            cx,
        );
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>, refocus: bool) {
        if self.edit.take().is_some() {
            self.rebuild_display();
            if refocus {
                window.focus(&self.focus_handle, cx);
            }
            cx.notify();
        }
    }

    fn confirm_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else { return };
        let name = edit.input.read(cx).text();
        let name = name.trim().to_string();
        let result = match &edit.kind {
            EditKind::NewFile { dir } => {
                fs_ops::create_file(dir, &name).map(|path| (path, None, true))
            }
            EditKind::NewFolder { dir } => {
                fs_ops::create_dir(dir, &name).map(|path| (path, None, false))
            }
            EditKind::Rename { path } => {
                let from = path.clone();
                fs_ops::rename(path, &name).map(|new| (new, Some(from), false))
            }
        };
        match result {
            Ok((path, renamed_from, open)) => {
                self.cancel_edit(window, cx, true);
                if let Some(from) = renamed_from
                    && from != path
                {
                    self.store
                        .update(cx, |store, cx| store.paths_moved(&from, &path, cx));
                }
                self.after_change(&path, cx);
                if open {
                    self.open_file(path, window, cx);
                }
            }
            // Leave the edit open, so the name can be corrected rather than retyped.
            Err(error) => self.show_error(format!("{error:#}"), cx),
        }
    }

    /// Updates right away after our own change, rather than waiting for the watcher to report it.
    fn after_change(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            tree.expand_to(path);
        }
        self.pending_reveal = Some(path.to_path_buf());
        let dirs: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|a| self.tree.as_ref().is_some_and(|t| a.starts_with(t.root())))
            .filter(|a| self.tree.as_ref().is_some_and(|t| t.is_loaded(a)))
            .map(Path::to_path_buf)
            .collect();
        for dir in dirs {
            self.reload_dir(dir, cx);
        }
        self.refresh_rows(cx);
        self.load_pending(cx);
        self.refresh_git(cx);
    }

    /// Moves an entry into a folder, as a drag and drop does. Dropping on a file means its folder.
    pub fn move_entry(&mut self, from: &Path, onto: &Path, cx: &mut Context<Self>) {
        let dir = if onto.is_dir() {
            onto
        } else {
            onto.parent().unwrap_or(onto)
        };
        match fs_ops::move_into(from, dir) {
            Ok(to) if to != from => {
                self.store
                    .update(cx, |store, cx| store.paths_moved(from, &to, cx));
                if let Some(parent) = from.parent().map(Path::to_path_buf) {
                    self.reload_dir(parent, cx);
                }
                self.after_change(&to, cx);
            }
            Ok(_) => {}
            Err(error) => self.show_error(format!("{error:#}"), cx),
        }
    }

    /// Asks, then moves the selection to the Trash.
    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_row_index() else {
            return;
        };
        if index == 0 {
            return;
        }
        let entry = self.rows[index].entry.clone();
        let what = if entry.kind == EntryKind::Dir {
            "folder"
        } else {
            "file"
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Move the {what} \"{}\" to the Trash?", entry.name),
            None,
            &["Move to Trash", "Cancel"],
            cx,
        );
        let trash = self.trash.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let path = entry.path.clone();
            let moved = cx.background_spawn(async move { trash(&path) }).await;
            this.update_in(cx, |this, window, cx| match moved {
                Ok(()) => {
                    if let Some(workspace) = this.workspace.upgrade() {
                        let path = entry.path.clone();
                        window.defer(cx, move |window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.close_clean_tabs_under(&path, window, cx);
                            });
                        });
                    }
                    if let Some(parent) = entry.path.parent().map(Path::to_path_buf) {
                        this.reload_dir(parent, cx);
                    }
                    this.refresh_git(cx);
                }
                Err(error) => this.show_error(format!("{error:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn copy_path(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = &self.selected {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
    }

    fn deploy_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.weak_entity();
        // Each item goes through the normal action, so the menu can't do anything the keys can't.
        let item = |action: Box<dyn gpui::Action>, panel: WeakEntity<Self>| {
            move |window: &mut Window, cx: &mut App| {
                let Some(panel) = panel.upgrade() else { return };
                let handle = panel.read(cx).focus_handle.clone();
                window.focus(&handle, cx);
                window.dispatch_action(action.boxed_clone(), cx);
            }
        };
        let is_root = self.selected_row_index() == Some(0);
        // Shortcuts as they'd apply in the tree, which is where the menu's actions run.
        let focus = self.focus_handle.clone();
        let hint = |action: &dyn gpui::Action, window: &Window, cx: &App| -> Option<SharedString> {
            let binding = window.highest_precedence_binding_for_action_in(action, &focus)?;
            let keys: Vec<String> = binding
                .keystrokes()
                .iter()
                .map(|k| k.unparse(cx.platform_info().os))
                .collect();
            Some(keys.join(" ").into())
        };
        let new_file_hint = hint(&NewFile, window, cx);
        let new_folder_hint = hint(&NewFolder, window, cx);
        let rename_hint = hint(&Rename, window, cx);
        let delete_hint = hint(&Delete, window, cx);
        let copy_hint = hint(&CopyPath, window, cx);
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(cx)
                .item(
                    "New File",
                    new_file_hint,
                    item(Box::new(NewFile), panel.clone()),
                )
                .item(
                    "New Folder",
                    new_folder_hint,
                    item(Box::new(NewFolder), panel.clone()),
                );
            if !is_root {
                menu = menu
                    .separator()
                    .item(
                        "Rename…",
                        rename_hint,
                        item(Box::new(Rename), panel.clone()),
                    )
                    .item(
                        "Move to Trash",
                        delete_hint,
                        item(Box::new(Delete), panel.clone()),
                    );
            }
            menu.separator()
                .item(
                    "Copy Path",
                    copy_hint,
                    item(Box::new(CopyPath), panel.clone()),
                )
                .item(
                    if cfg!(target_os = "macos") {
                        "Reveal in Finder"
                    } else {
                        "Show in Folder"
                    },
                    None,
                    item(Box::new(RevealInSystem), panel.clone()),
                )
        });
        let menu_focus = menu.read(cx).focus_handle(cx);
        window.focus(&menu_focus, cx);
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &MenuDismissed, window, cx| {
                this.menu = None;
                window.focus(&this.focus_handle, cx);
                cx.notify();
            });
        self.menu = Some((menu, position, subscription));
        cx.notify();
    }

    fn row_color(row: &Row, git: &GitStatus, theme: &Theme) -> gpui::Hsla {
        if row.entry.ignored {
            return theme.text_faint;
        }
        match git.state(&row.entry.path) {
            Some(GitState::Added | GitState::Untracked) => theme.git_added,
            Some(GitState::Modified | GitState::Renamed) => theme.git_modified,
            Some(GitState::Deleted) => theme.git_deleted,
            Some(GitState::Conflicted) => theme.git_conflicted,
            None => theme.text,
        }
    }

    fn render_row(
        &self,
        display_index: usize,
        focused: bool,
        theme: &Theme,
        git: &GitStatus,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let display = &self.display[display_index];
        let DisplayRow::Entry(row_index) = display else {
            let DisplayRow::NewEntry { depth } = display else {
                unreachable!()
            };
            return self.render_input_row(*depth, theme);
        };
        let row_index = *row_index;
        let row = &self.rows[row_index];
        let renaming = matches!(
            self.edit.as_ref().map(|e| &e.kind),
            Some(EditKind::Rename { path }) if *path == row.entry.path
        );
        if renaming {
            return self.render_input_row(row.depth, theme);
        }

        let selected = self.selected.as_ref() == Some(&row.entry.path);
        let is_dir = row.entry.kind == EntryKind::Dir;
        let is_root = row_index == 0;
        let color = Self::row_color(row, git, theme);
        let path = row.entry.path.clone();
        let name: SharedString = row.entry.name.clone().into();
        let (click_path, menu_path, drag_path, drop_path) =
            (path.clone(), path.clone(), path.clone(), path.clone());
        let dragged = DraggedEntry {
            path: drag_path,
            name: name.clone(),
        };

        let chevron = is_dir.then(|| {
            let icon = if row.expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            svg()
                .path(icon.path())
                .size(px(13.))
                .text_color(theme.text_muted)
        });
        let icon: AnyElement = if is_dir {
            svg()
                .path(Icon::Folder.path())
                .size(px(14.))
                .text_color(if row.entry.ignored {
                    theme.text_faint
                } else {
                    theme.text_muted
                })
                .into_any_element()
        } else {
            match file_icon(&row.entry.path) {
                FileIcon::Badge(text, badge_color) => div()
                    .w(px(14.))
                    .flex()
                    .justify_center()
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(badge_color)
                    .child(text)
                    .into_any_element(),
                FileIcon::Plain => svg()
                    .path(Icon::File.path())
                    .size(px(13.))
                    .text_color(theme.text_muted)
                    .into_any_element(),
            }
        };
        let location = is_root.then(|| {
            let parent = row
                .entry
                .path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let home = std::env::var("HOME").unwrap_or_default();
            let shown = match parent.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => parent,
            };
            div()
                .text_color(theme.text_faint)
                .text_size(px(11.))
                .truncate()
                .child(shown)
        });
        let selection = if focused {
            theme.selection
        } else {
            theme.selection_inactive
        };
        let drop_highlight = theme.selection;

        let row_element = div()
            .id(("row", row_index))
            .debug_selector(|| format!("row-{}", name))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.))
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(6. + INDENT * row.depth as f32))
            .pr(px(8.))
            .text_color(color)
            .when(selected, |r| r.bg(selection))
            .when(!selected, |r| r.hover(|r| r.bg(theme.hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.selected = Some(click_path.clone());
                this.pending_reveal = None;
                window.focus(&this.focus_handle, cx);
                if event.click_count() >= 2 {
                    this.open_selected(window, cx);
                }
                this.sync_nav();
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.selected = Some(menu_path.clone());
                    this.deploy_menu(event.position, window, cx);
                    cx.stop_propagation();
                }),
            )
            .drag_over::<DraggedEntry>(move |style, _, _, _| style.bg(drop_highlight))
            .on_drop(cx.listener(move |this, dragged: &DraggedEntry, _, cx| {
                this.move_entry(&dragged.path, &drop_path, cx);
            }))
            .child(div().w(px(13.)).flex_none().children(chevron))
            .child(div().flex_none().child(icon))
            .child(
                div()
                    .flex_none()
                    .when(is_root, |n| n.font_weight(gpui::FontWeight::SEMIBOLD))
                    .child(name.clone()),
            )
            .children(location);

        // The root can't move, as everything else is relative to it.
        if is_root {
            row_element.into_any_element()
        } else {
            row_element
                .on_drag(dragged, |dragged, _, _, cx| {
                    let name = dragged.name.clone();
                    cx.new(|_| DragPreview { name })
                })
                .into_any_element()
        }
    }

    fn render_input_row(&self, depth: usize, theme: &Theme) -> AnyElement {
        let Some(edit) = &self.edit else {
            return div().into_any_element();
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(6. + INDENT * depth as f32 + 18.))
            .pr(px(8.))
            .child(
                div()
                    .flex_1()
                    .px(px(4.))
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.accent)
                    .rounded(px(3.))
                    .line_height(px(18.))
                    .child(edit.input.clone()),
            )
            .into_any_element()
    }
}

impl Focusable for ProjectPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ProjectPanel {
    fn id(&self) -> &'static str {
        PANEL_ID
    }

    fn title(&self) -> &'static str {
        "Project"
    }

    fn icon(&self) -> Icon {
        Icon::Tree
    }

    fn position(&self) -> DockPosition {
        DockPosition::Left
    }

    fn save_state(&self, _: &App) -> serde_json::Value {
        let Some(tree) = &self.tree else {
            return serde_json::Value::Null;
        };
        let root = tree.root();
        let relative = |path: &Path| path.strip_prefix(root).ok().map(Path::to_path_buf);
        let mut expanded: Vec<PathBuf> = tree
            .expanded_dirs()
            .filter_map(|path| relative(path))
            .collect();
        expanded.sort();
        serde_json::to_value(SavedPanel {
            expanded,
            selected: self.selected.as_deref().and_then(relative),
        })
        .unwrap_or_default()
    }

    fn restore_state(&mut self, state: serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        let Ok(saved) = serde_json::from_value::<SavedPanel>(state) else {
            return;
        };
        let Some(tree) = &mut self.tree else { return };
        let root = tree.root().to_path_buf();
        for dir in saved.expanded {
            tree.expand(&root.join(dir));
        }
        self.pending_reveal = saved.selected.map(|path| root.join(path));
        self.refresh_rows(cx);
        self.load_pending(cx);
    }

    fn debug_state(&self, cx: &App) -> serde_json::Value {
        let mut state = self.save_state(cx);
        let git = self.git_status(cx);
        if let Some(object) = state.as_object_mut() {
            object.insert("rows".to_string(), self.visible_names().into());
            object.insert("editing".to_string(), self.edit.is_some().into());
            object.insert(
                "git".to_string(),
                self.rows
                    .iter()
                    .filter_map(|row| {
                        let state = git.state(&row.entry.path)?;
                        Some((row.entry.name.clone(), format!("{state:?}").into()))
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into(),
            );
        }
        state
    }
}

impl Render for ProjectPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();

        let mut key_context = KeyContext::new_with_defaults(cx.platform_info().os);
        key_context.add("ProjectPanel");
        if self.edit.is_some() {
            key_context.add("editing");
        }

        let body: AnyElement = if self.tree.is_some() {
            let rows = cx.processor(move |this, range: Range<usize>, window, cx| {
                let theme = cx.global::<Theme>().clone();
                let focused = this.focus_handle.contains_focused(window, cx);
                let git = this.git_status(cx);
                range
                    .map(|index| this.render_row(index, focused, &theme, &git, cx))
                    .collect::<Vec<_>>()
            });
            uniform_list("project-rows", self.display.len(), rows)
                .track_scroll(self.nav.scroll_handle())
                .flex_1()
                .size_full()
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .pt(px(24.))
                .text_color(theme.text_muted)
                .child("No folder open")
                .child(
                    div()
                        .id("open-folder")
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(4.))
                        .bg(theme.surface_raised)
                        .text_color(theme.text)
                        .hover(|b| b.bg(theme.selection))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx))
                        .child("Open Folder…"),
                )
                .into_any_element()
        };

        let menu = self.menu.as_ref().map(|(menu, position, _)| {
            deferred(
                anchored()
                    .position(*position)
                    .snap_to_window()
                    .child(menu.clone()),
            )
            .with_priority(2)
        });

        div()
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_relative(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.select_relative(false, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select_row(0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.select_row(this.rows.len().saturating_sub(1), cx);
            }))
            .on_action(cx.listener(|this, _: &ExpandOrDescend, _, cx| this.expand_or_descend(cx)))
            .on_action(cx.listener(|this, _: &CollapseOrAscend, _, cx| this.collapse_or_ascend(cx)))
            .on_action(cx.listener(|this, _: &Open, window, cx| this.open_selected(window, cx)))
            .on_action(
                cx.listener(|this, _: &NewFile, window, cx| this.start_new(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NewFolder, window, cx| this.start_new(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &Rename, window, cx| this.start_rename(window, cx)))
            .on_action(cx.listener(|this, _: &Delete, window, cx| this.delete_selected(window, cx)))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(cx)))
            .on_action(cx.listener(|this, _: &RevealInSystem, _, cx| {
                if let Some(path) = &this.selected {
                    cx.reveal_path(path);
                }
            }))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm_edit(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                this.cancel_edit(window, cx, true);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // In the empty space below the rows: acts on the project root.
                    this.selected = this.tree.as_ref().map(|t| t.root().to_path_buf());
                    this.deploy_menu(event.position, window, cx);
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .h(px(32.))
                    .px(px(10.))
                    .border_b_1()
                    .border_color(theme.border)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("Project"),
            )
            .child(body)
            .children(menu)
    }
}

#[cfg(test)]
mod tests;
