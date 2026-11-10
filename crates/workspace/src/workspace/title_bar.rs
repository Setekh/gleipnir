//! IntelliJ's main toolbar, drawn in place of the OS title bar: the project, with the recent ones a
//! click away; the Git branch; and on the right what features add, like the run configuration,
//! Search Everywhere and Settings. On macOS the window's buttons sit over its left end; on Linux
//! it draws them, where the desktop puts them, and the app's menus, as there's no menu bar there.
//! It drags and zooms the window like a title bar.

use super::decorations::{self, WindowButton};
use super::{OpenFolder, StatusItem, Workspace};
use crate::persistence::recent_projects;
use gleipnir_ui::{ContextMenu, Icon, MenuDismissed, Theme};
use gpui::{
    AnyElement, Bounds, Context, Entity, FocusHandle, Focusable as _, MouseButton, OwnedMenuItem,
    Pixels, Point, Subscription, Window, anchored, canvas, deferred, div, point, prelude::*, px,
    svg,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const TITLE_BAR_HEIGHT: f32 = 40.;
/// Room for macOS's close, minimise and zoom buttons at the bar's left end.
const WINDOW_BUTTONS_WIDTH: f32 = 78.;
/// How often the branch is read again, as it changes from outside, like a checkout in a terminal.
const BRANCH_POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleSide {
    Left,
    Right,
}

/// A menu open from the title bar, where it opened, and where focus goes back to when it closes.
pub(super) type OpenMenu = (
    Entity<ContextMenu>,
    Point<Pixels>,
    Subscription,
    Option<FocusHandle>,
);

/// Whether the bar shows the app's menus: where there's no menu bar of the system's to hold them.
fn shows_app_menus() -> bool {
    !cfg!(target_os = "macos")
}

/// Where macOS should draw the window's buttons, centred in the bar.
pub fn window_buttons_position() -> Point<Pixels> {
    point(px(14.), px(TITLE_BAR_HEIGHT / 2. - 6.))
}

/// The branch checked out in a repository, or a short commit when it's detached. Reads the files
/// rather than running git, as it's asked often.
pub fn current_branch(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    // In a worktree or submodule, `.git` is a file pointing at the real folder.
    let git_dir = if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if target.is_absolute() {
            target
        } else {
            root.join(target)
        }
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            Some(
                reference
                    .strip_prefix("refs/heads/")
                    .unwrap_or(reference)
                    .to_string(),
            )
        }
        None => Some(head.chars().take(7).collect()),
    }
}

/// Something a menu item does to the workspace.
type WorkspaceAction = dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

/// How many branches of each kind the branches menu lists.
const MENU_BRANCHES: usize = 15;

/// A repository's local and remote branches, each most recently committed to first.
pub fn list_branches(root: &Path) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let output = std::process::Command::new("git")
        .args([
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| anyhow::anyhow!("couldn't run git: {e}"))?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for reference in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(branch) = reference.strip_prefix("refs/heads/") {
            local.push(branch.to_string());
        } else if let Some(branch) = reference.strip_prefix("refs/remotes/")
            && !branch.ends_with("/HEAD")
        {
            remote.push(branch.to_string());
        }
    }
    Ok((local, remote))
}

/// A path with the home folder written as `~`, as menus show them.
fn home_relative(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display())
        }
        _ => path.display().to_string(),
    }
}

/// Up to two letters standing for a project, like IntelliJ's badge: `fix-partner` is `FP`.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => String::new(),
        [one] => one.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    letters.to_uppercase()
}

impl Workspace {
    /// Adds something to the title bar, on the left after the branch or on the right, placed
    /// among the others on its side by `order`, lowest first.
    pub fn add_title_item(
        &mut self,
        side: TitleSide,
        order: u8,
        item: impl Fn(&mut Workspace, &mut Context<Workspace>) -> Option<AnyElement> + 'static,
    ) {
        let item: StatusItem = Rc::new(item);
        self.title_items.push((side, order, item));
        self.title_items
            .sort_by_key(|(side, order, _)| (*side == TitleSide::Right, *order));
    }

    /// The menu open from the title bar, if there is one.
    pub fn open_menu(&self) -> Option<&Entity<ContextMenu>> {
        self.menu.as_ref().map(|(menu, _, _, _)| menu)
    }

    /// The branch shown in the title bar.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// Reads the branch again now, rather than at the next look.
    pub fn watch_branch_now(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = &self.root {
            self.branch = current_branch(root);
            cx.notify();
        }
    }

    /// Reads the branch now, and again every so often while the window's open.
    pub(super) fn watch_branch(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        self.branch = current_branch(&root);
        self.branch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BRANCH_POLL).await;
                let read = root.clone();
                let branch = cx
                    .background_executor()
                    .spawn(async move { current_branch(&read) })
                    .await;
                let changed = this.update(cx, |this, cx| {
                    if this.branch != branch {
                        this.branch = branch;
                        cx.notify();
                    }
                });
                if changed.is_err() {
                    return;
                }
            }
        }));
    }

    /// Shows a menu at a place in the window, such as below the title bar item that opened it.
    pub fn show_menu(
        &mut self,
        menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Going from one menu straight to another, focus still goes back to where it was before
        // the first, not to the menu that's closing.
        let restore = match &self.menu {
            Some((_, _, _, restore)) => restore.clone(),
            None => window.focused(cx),
        };
        window.focus(&menu.read(cx).focus_handle(cx), cx);
        let return_to = restore.clone();
        let subscription = cx.subscribe_in(
            &menu,
            window,
            move |this, _, _: &MenuDismissed, window, cx| {
                this.menu = None;
                if let Some(index) = this.open_app_menu.take() {
                    this.app_menu_closed = Some((index, Instant::now()));
                }
                match &return_to {
                    Some(focus) => window.focus(focus, cx),
                    None => this.focus_center(window, cx),
                }
                cx.notify();
            },
        );
        self.open_app_menu = None;
        self.menu = Some((menu, position, subscription, restore));
        cx.notify();
    }

    /// Closes whatever menu is open from the bar, sending focus back to where it was.
    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, _, _, restore)) = self.menu.take() else {
            return;
        };
        if let Some(index) = self.open_app_menu.take() {
            self.app_menu_closed = Some((index, Instant::now()));
        }
        match restore {
            Some(focus) => window.focus(&focus, cx),
            None => self.focus_center(window, cx),
        }
        cx.notify();
    }

    /// Opens one of the app's menus below its name in the bar. Its items do what they do from
    /// the macOS menu bar: dispatch their action to wherever focus was.
    pub fn open_app_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = cx
            .get_menus()
            .and_then(|menus| menus.into_iter().nth(index))
        else {
            return;
        };
        let at = self
            .app_menu_bounds
            .borrow()
            .get(index)
            .map_or(point(px(8.), px(TITLE_BAR_HEIGHT)), |b| {
                point(b.left(), b.bottom() + px(2.))
            });
        // Shortcuts as they are where focus is, since that's where the action goes.
        let focus = match &self.menu {
            Some((_, _, _, restore)) => restore.clone(),
            None => window.focused(cx),
        };
        let mut entries = Vec::new();
        flatten_menu(&menu.items, "", &mut entries);
        let hints: Vec<Option<gpui::SharedString>> = entries
            .iter()
            .map(|entry| {
                let (_, action) = entry.as_ref()?;
                let label = match &focus {
                    Some(focus) => shortcut_where(action.as_ref(), focus, window, cx),
                    None => super::shortcut_label(action.as_ref(), window, cx),
                };
                (!label.is_empty()).then(|| label.into())
            })
            .collect();
        let context_menu = cx.new(|cx| {
            entries
                .into_iter()
                .zip(hints)
                .fold(ContextMenu::new(cx), |menu, (entry, hint)| match entry {
                    None => menu.separator(),
                    Some((label, action)) => menu.item(label, hint, move |window, cx| {
                        window.dispatch_action(action.boxed_clone(), cx)
                    }),
                })
        });
        self.show_menu(context_menu, at, window, cx);
        self.open_app_menu = Some(index);
    }

    /// The app's menus' names, for the bar, each opening its menu.
    fn render_app_menus(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let names: Vec<gpui::SharedString> = cx
            .get_menus()
            .unwrap_or_default()
            .into_iter()
            .map(|menu| menu.name)
            .collect();
        {
            let mut bounds = self.app_menu_bounds.borrow_mut();
            bounds.resize(names.len(), Bounds::default());
        }
        names
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let open = self.open_app_menu == Some(index);
                let store = self.app_menu_bounds.clone();
                div()
                    .id(("app-menu", index))
                    .debug_selector(move || format!("app-menu-{index}"))
                    .relative()
                    .px(px(7.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .when(open, |d| d.bg(theme.hover))
                    .hover(|d| d.bg(theme.hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            // Pressing an open menu's name closes it, whether the press reaches
                            // the name first or the menu, which closes on presses outside it.
                            let just_closed = this.app_menu_closed.is_some_and(|(i, at)| {
                                i == index && at.elapsed() < Duration::from_millis(250)
                            });
                            if this.open_app_menu == Some(index) {
                                this.close_menu(window, cx);
                            } else if !just_closed {
                                this.open_app_menu(index, window, cx);
                            }
                        }),
                    )
                    // With one menu open, moving across the names opens each, as in a menu bar.
                    .on_mouse_move(cx.listener(move |this, _, window, cx| {
                        if this.open_app_menu.is_some_and(|open| open != index) {
                            this.open_app_menu(index, window, cx);
                        }
                    }))
                    .child(name)
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                if let Some(slot) = store.borrow_mut().get_mut(index) {
                                    *slot = bounds;
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The projects menu: those opened before, then opening another.
    fn open_project_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.root.clone();
        let recent: Vec<PathBuf> = recent_projects()
            .into_iter()
            .filter(|p| Some(p) != current.as_ref())
            .collect();
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(cx);
            for path in recent {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let shown = home_relative(&path);
                menu = menu.item(name, Some(shown.into()), move |_, cx| {
                    super::open_project_window(Some(path.clone()), false, cx);
                });
            }
            menu.separator().item("Open…", None, |window, cx| {
                window.dispatch_action(Box::new(OpenFolder), cx)
            })
        });
        self.show_menu(menu, position, window, cx);
    }

    /// The branches menu, as IntelliJ's: a new branch, then local branches, most recently
    /// committed to first, then remote ones. Choosing a branch offers what can be done with it.
    fn open_branch_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let current = self.branch.clone();
        let listed = cx.background_executor().spawn({
            let root = root.clone();
            async move { list_branches(&root) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let listed = listed.await;
            this.update_in(cx, |this, window, cx| {
                let (local, remote) = match listed {
                    Ok(branches) => branches,
                    Err(error) => {
                        this.show_error(format!("{error:#}"), cx);
                        return;
                    }
                };
                let handle = cx.weak_entity();
                let menu = cx.new(|cx| {
                    let on = |action: Box<WorkspaceAction>| {
                        let handle = handle.clone();
                        move |window: &mut Window, cx: &mut gpui::App| {
                            if let Some(workspace) = handle.upgrade() {
                                workspace.update(cx, |w, cx| action(w, window, cx));
                            }
                        }
                    };
                    // What IntelliJ's branch popup starts with, run by name, as Git's actions
                    // are a feature's of their own.
                    let named = |name: &'static str| {
                        move |window: &mut Window, cx: &mut gpui::App| {
                            if let Ok(action) = cx.build_action(name, None) {
                                window.dispatch_action(action, cx);
                            }
                        }
                    };
                    let mut menu = ContextMenu::new(cx);
                    if cx.build_action("git::Commit", None).is_ok() {
                        menu = menu
                            .item("Update Project…", None, named("git::UpdateProject"))
                            .item("Commit…", None, named("git::Commit"))
                            .item("Push…", None, named("git::Push"))
                            .separator();
                    }
                    let mut menu = menu.item(
                        "New Branch…",
                        None,
                        on(Box::new(|w, window, cx| w.new_branch(None, window, cx))),
                    );
                    menu = menu.separator();
                    for branch in local.iter().take(MENU_BRANCHES) {
                        let hint = (Some(branch) == current.as_ref()).then(|| "current".into());
                        let name = branch.clone();
                        menu = menu.item(
                            branch.clone(),
                            hint,
                            on(Box::new(move |w, window, cx| {
                                w.open_branch_actions(name.clone(), false, position, window, cx)
                            })),
                        );
                    }
                    let remote: Vec<&String> = remote
                        .iter()
                        // A remote branch that's checked out locally is listed as the local one.
                        .filter(|r| {
                            r.split_once('/')
                                .is_none_or(|(_, b)| !local.contains(&b.to_string()))
                        })
                        .take(MENU_BRANCHES)
                        .collect();
                    if !remote.is_empty() {
                        menu = menu.separator();
                    }
                    for branch in remote {
                        let name = branch.clone();
                        menu = menu.item(
                            branch.clone(),
                            Some("remote".into()),
                            on(Box::new(move |w, window, cx| {
                                w.open_branch_actions(name.clone(), true, position, window, cx)
                            })),
                        );
                    }
                    menu
                });
                this.show_menu(menu, position, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// What can be done with a branch: check it out, branch from it, merge it into the current
    /// branch or rebase the current one onto it, and delete it.
    fn open_branch_actions(
        &mut self,
        branch: String,
        remote: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let current = self.branch.clone().unwrap_or_default();
        let is_current = !remote && branch == current;
        let handle = cx.weak_entity();
        let git = |args: Vec<String>, done: String| {
            let handle = handle.clone();
            let root = root.clone();
            move |_: &mut Window, cx: &mut gpui::App| {
                if let Some(workspace) = handle.upgrade() {
                    workspace.update(cx, |w, cx| w.run_git(&root, args.clone(), done.clone(), cx));
                }
            }
        };
        let menu = cx.new(|cx| {
            let mut menu = ContextMenu::new(cx);
            if !is_current {
                let args = if remote {
                    vec!["switch".into(), "--track".into(), branch.clone()]
                } else {
                    vec!["switch".into(), branch.clone()]
                };
                menu = menu.item("Checkout", None, git(args, "Switched to {branch}".into()));
            }
            let from = branch.clone();
            let handle = handle.clone();
            menu = menu.item(
                format!("New Branch from '{branch}'…"),
                None,
                move |window: &mut Window, cx: &mut gpui::App| {
                    if let Some(workspace) = handle.upgrade() {
                        let from = from.clone();
                        workspace.update(cx, |w, cx| w.new_branch(Some(from), window, cx));
                    }
                },
            );
            if !is_current && !current.is_empty() {
                menu = menu
                    .separator()
                    .item(
                        format!("Merge '{branch}' into '{current}'"),
                        None,
                        git(
                            vec!["merge".into(), "--no-edit".into(), branch.clone()],
                            format!("Merged {branch} into {current}"),
                        ),
                    )
                    .item(
                        format!("Rebase '{current}' onto '{branch}'"),
                        None,
                        git(
                            vec!["rebase".into(), branch.clone()],
                            format!("Rebased {current} onto {branch}"),
                        ),
                    );
            }
            if !remote && !is_current {
                // `-d` refuses a branch that isn't merged, so nothing is lost by a slip.
                menu = menu.separator().item(
                    "Delete",
                    None,
                    git(
                        vec!["branch".into(), "-d".into(), branch.clone()],
                        format!("Deleted {branch}"),
                    ),
                );
            }
            menu
        });
        self.show_menu(menu, position, window, cx);
    }

    /// Asks for a name and makes a branch with it, from the current one or `from`, and switches
    /// to it.
    pub fn new_branch(
        &mut self,
        from: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let title = match &from {
            Some(from) => format!("New branch from {from}"),
            None => "New branch".to_string(),
        };
        self.ask_for_name(
            title,
            "Branch name",
            move |name, workspace, cx| {
                let mut args = vec!["switch".to_string(), "-c".to_string(), name.clone()];
                args.extend(from);
                workspace.run_git(&root, args, format!("Created {name}"), cx);
            },
            window,
            cx,
        );
    }

    /// Runs `git` in a repository off the UI thread, saying how it went: `done` when it worked,
    /// or git's own message when it didn't, as when it refuses to lose uncommitted work or stops
    /// on a conflict. The branch shown is read again either way.
    pub fn run_git(
        &mut self,
        root: &Path,
        args: Vec<String>,
        done: String,
        cx: &mut Context<Self>,
    ) {
        let root = root.to_path_buf();
        let run = cx.background_executor().spawn({
            let root = root.clone();
            async move {
                std::process::Command::new("git")
                    .args(&args)
                    .current_dir(&root)
                    .output()
            }
        });
        cx.spawn(async move |this, cx| {
            let output = run.await;
            this.update(cx, |this, cx| {
                this.branch = current_branch(&root);
                match output {
                    Ok(output) if output.status.success() => {
                        let branch = this.branch.clone().unwrap_or_default();
                        this.show_message(done.replace("{branch}", &branch), cx);
                    }
                    Ok(output) => {
                        let mut error = String::from_utf8_lossy(&output.stderr).into_owned();
                        if error.trim().is_empty() {
                            error = String::from_utf8_lossy(&output.stdout).into_owned();
                        }
                        let error = error.trim().lines().collect::<Vec<_>>().join(" ");
                        this.show_error(error, cx);
                    }
                    Err(error) => this.show_error(format!("couldn't run git: {error}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_menu(&self) -> Option<AnyElement> {
        let (menu, position, _, _) = self.menu.as_ref()?;
        Some(
            deferred(anchored().position(*position).child(menu.clone()))
                .with_priority(2)
                .into_any_element(),
        )
    }

    pub(super) fn render_title_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.global::<Theme>().clone();
        let items: Vec<(TitleSide, AnyElement)> = self
            .title_items
            .clone()
            .iter()
            .filter_map(|(side, _, item)| Some((*side, item(self, cx)?)))
            .collect();
        let (left_items, right_items): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|(side, _)| *side == TitleSide::Left);
        let project = self
            .root
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned());
        let fullscreen = window.is_fullscreen();
        let leading = if cfg!(target_os = "macos") && !fullscreen {
            WINDOW_BUTTONS_WIDTH
        } else {
            8.
        };
        // Elsewhere the bar draws the window's buttons itself, at whichever ends the desktop puts
        // them.
        let (left_buttons, right_buttons) = decorations::buttons(window, cx);
        let workspace = cx.weak_entity();
        let render_buttons = |buttons: Vec<WindowButton>, window: &Window| {
            let workspace = workspace.clone();
            let close = Rc::new(move |window: &mut Window, cx: &mut gpui::App| {
                workspace
                    .update(cx, |this, cx| {
                        if this.window_should_close(window, cx) {
                            window.remove_window();
                        }
                    })
                    .ok();
            });
            buttons
                .into_iter()
                .map(|b| {
                    let close = close.clone();
                    decorations::button(b, &theme, window, move |w, cx| close(w, cx))
                })
                .collect::<Vec<_>>()
        };
        let left_buttons = render_buttons(left_buttons, window);
        let right_buttons = render_buttons(right_buttons, window);
        let can_maximize = window.window_controls().maximize;
        let has_window_menu =
            decorations::draws_buttons(window) && window.window_controls().window_menu;
        let app_menus = if shows_app_menus() {
            self.render_app_menus(&theme, cx)
        } else {
            Vec::new()
        };

        let project_widget = div()
            .id("title-project")
            .debug_selector(|| "title-project".into())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .px(px(6.))
            .py(px(3.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|s| s.bg(theme.hover))
            .when_some(project.clone(), |d, name| {
                d.child(
                    div()
                        .size(px(20.))
                        .rounded(px(4.))
                        .bg(theme.accent)
                        .text_color(theme.background)
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(initials(&name)),
                )
            })
            .child(
                div()
                    .text_color(theme.text)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(project.unwrap_or_else(|| "No project".into())),
            )
            .child(chevron(&theme))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                let at = point(event.position().x - px(40.), px(TITLE_BAR_HEIGHT));
                this.open_project_menu(at, window, cx);
            }));

        let branch_widget = self.branch.clone().map(|branch| {
            div()
                .id("title-branch")
                .debug_selector(|| "title-branch".into())
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .px(px(6.))
                .py(px(3.))
                .rounded(px(6.))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.bg(theme.hover))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                    let at = point(event.position().x - px(40.), px(TITLE_BAR_HEIGHT));
                    this.open_branch_menu(at, window, cx);
                }))
                .child(
                    svg()
                        .path(Icon::Branch.path())
                        .size(px(14.))
                        .text_color(theme.text_muted),
                )
                .child(branch)
                .child(chevron(&theme))
        });

        div()
            .id("title-bar")
            .debug_selector(|| "title-bar".into())
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .pl(px(leading))
            .pr(px(8.))
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(13.))
            // Empty parts of the bar move the window, a double click zooms it, and a right click
            // opens the desktop's window menu, as the title bar it replaces did.
            .on_mouse_down(MouseButton::Left, move |event, window, _| {
                if event.click_count < 2 {
                    window.start_window_move();
                } else if cfg!(target_os = "macos") {
                    window.titlebar_double_click();
                } else if can_maximize {
                    window.zoom_window();
                }
            })
            .when(has_window_menu, |bar| {
                bar.on_mouse_down(MouseButton::Right, |event, window, _| {
                    window.show_window_menu(event.position);
                })
            })
            .children(left_buttons)
            .children(app_menus)
            .child(project_widget)
            .children(branch_widget)
            .children(left_items.into_iter().map(|(_, item)| item))
            .child(div().flex_1().h_full())
            .children(right_items.into_iter().map(|(_, item)| {
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(item)
            }))
            .when(!right_buttons.is_empty(), |bar| {
                bar.child(div().w(px(4.))).children(right_buttons)
            })
            .into_any_element()
    }
}

/// A menu's items as a flat list, `None` for a separator. A submenu's items follow with its name
/// before theirs, as the popup has no submenus.
fn flatten_menu(
    items: &[OwnedMenuItem],
    prefix: &str,
    out: &mut Vec<Option<(String, Box<dyn gpui::Action>)>>,
) {
    for item in items {
        match item {
            OwnedMenuItem::Separator => {
                if out.last().is_some_and(Option::is_some) {
                    out.push(None);
                }
            }
            OwnedMenuItem::Action { name, action, .. } => {
                out.push(Some((format!("{prefix}{name}"), action.boxed_clone())));
            }
            OwnedMenuItem::Submenu(menu) => {
                flatten_menu(&menu.items, &format!("{prefix}{} › ", menu.name), out)
            }
            OwnedMenuItem::SystemMenu(_) => {}
        }
    }
    if out.last().is_some_and(Option::is_none) {
        out.pop();
    }
}

/// The keys bound to an action in the context of what has focus, which a menu's own focus would
/// otherwise hide.
fn shortcut_where(
    action: &dyn gpui::Action,
    focus: &FocusHandle,
    window: &Window,
    cx: &gpui::App,
) -> String {
    window
        .highest_precedence_binding_for_action_in(action, focus)
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|k| k.unparse(cx.platform_info().os))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| super::shortcut_label(action, window, cx))
}

fn chevron(theme: &Theme) -> impl IntoElement {
    svg()
        .path(Icon::ChevronDown.path())
        .size(px(12.))
        .text_color(theme.text_muted)
}

/// A square icon button for the title bar.
pub fn title_button(id: &'static str, icon: Icon, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(theme.hover))
        .child(
            svg()
                .path(icon.path())
                .size(px(16.))
                .text_color(theme.text_muted),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_flatten_with_submenus_named_and_no_stray_separators() {
        let action = || -> Box<dyn gpui::Action> { Box::new(crate::NewFile) };
        let items = vec![
            OwnedMenuItem::Separator,
            OwnedMenuItem::Action {
                name: "New".into(),
                action: action(),
                os_action: None,
                checked: false,
                disabled: false,
            },
            OwnedMenuItem::Separator,
            OwnedMenuItem::Separator,
            OwnedMenuItem::Submenu(gpui::OwnedMenu {
                name: "Recent".into(),
                items: vec![OwnedMenuItem::Action {
                    name: "a.rs".into(),
                    action: action(),
                    os_action: None,
                    checked: false,
                    disabled: false,
                }],
                disabled: false,
            }),
            OwnedMenuItem::Separator,
        ];
        let mut out = Vec::new();
        flatten_menu(&items, "", &mut out);
        let labels: Vec<Option<String>> = out
            .into_iter()
            .map(|entry| entry.map(|(label, _)| label))
            .collect();
        assert_eq!(
            labels,
            [Some("New".into()), None, Some("Recent › a.rs".into())]
        );
    }

    #[test]
    fn initials_take_a_letter_from_each_of_the_first_two_words() {
        assert_eq!(initials("fix-partner"), "FP");
        assert_eq!(initials("gleipnir"), "GL");
        assert_eq!(initials("lang.additions"), "LA");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn the_branch_is_read_from_head_or_shortened_when_detached() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(current_branch(dir.path()).as_deref(), Some("feature/x"));
        std::fs::write(git.join("HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(current_branch(dir.path()).as_deref(), Some("0123456"));

        // A worktree's `.git` file points at its own folder.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(
            other.path().join(".git"),
            format!("gitdir: {}\n", git.display()),
        )
        .unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(current_branch(other.path()).as_deref(), Some("main"));
        assert_eq!(current_branch(&dir.path().join("nothing")), None);
    }
}
