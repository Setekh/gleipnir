//! IntelliJ's Terminal tool window: tabs of terminals at the bottom, each a shell in the project
//! folder unless started for something else, with a + to open another. A tab whose shell exits
//! closes; one running a program says it ended, and keeps its output.

use crate::agents::{AGENTS, Agent, agent_launch};
use crate::session::{Launch, Session, SessionEvent};
use crate::view::{OpenLocation, TerminalView};
use gleipnir_ui::{Icon, Theme};
use gleipnir_workspace::{DockPosition, Panel, Workspace};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, Subscription, WeakEntity, Window, div,
    prelude::*, px, svg,
};
use std::path::PathBuf;

pub const TERMINAL_PANEL: &str = "terminal";
pub const AGENTS_PANEL: &str = "agents";

/// Which tool window: the Terminal at the bottom, with shells, or Agents on the right, with
/// coding agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Terminal,
    Agents,
}

impl PanelKind {
    pub fn id(self) -> &'static str {
        match self {
            PanelKind::Terminal => TERMINAL_PANEL,
            PanelKind::Agents => AGENTS_PANEL,
        }
    }
}

struct Tab {
    view: Entity<TerminalView>,
    /// Whether it's a shell, which closes when it exits, rather than a program.
    shell: bool,
    /// The agent it runs, if it's an agent's tab.
    agent: Option<&'static str>,
    _subscriptions: Vec<Subscription>,
}

pub struct TerminalPanel {
    kind: PanelKind,
    workspace: WeakEntity<Workspace>,
    root: PathBuf,
    tabs: Vec<Tab>,
    active: usize,
    focus_handle: FocusHandle,
    /// Why the last terminal couldn't start, until one does.
    error: Option<String>,
    /// A shell's on its way, opened when the panel first shows with none.
    starting: bool,
}

impl TerminalPanel {
    pub fn new(workspace: &Workspace, kind: PanelKind, cx: &mut Context<Self>) -> Self {
        Self {
            kind,
            workspace: workspace.weak_handle(),
            root: workspace
                .root()
                .map(|r| r.to_path_buf())
                .unwrap_or_else(gleipnir_packages_home),
            tabs: Vec::new(),
            active: 0,
            focus_handle: cx.focus_handle(),
            error: None,
            starting: false,
        }
    }

    /// Opens a shell in the project folder, named as IntelliJ names them: Local, Local (2)…
    pub fn new_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let taken: Vec<String> = self
            .tabs
            .iter()
            .map(|t| t.view.read(cx).session().read(cx).launch().title.clone())
            .collect();
        let title = (1..)
            .map(|n| match n {
                1 => "Local".to_string(),
                n => format!("Local ({n})"),
            })
            .find(|name| !taken.contains(name))
            .unwrap_or_default();
        let launch = Launch {
            program: None,
            cwd: self.root.clone(),
            env: Default::default(),
            title,
        };
        self.open(launch, true, window, cx);
    }

    /// Shows an agent's tab, opening one if it has none, as Claude Code's IntelliJ plugin does.
    /// Started as `launch` says, if given; else as the workspace says, which mustn't be busy.
    pub fn open_agent(
        &mut self,
        agent: &Agent,
        launch: Option<Launch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|t| {
            t.agent == Some(agent.id) && t.view.read(cx).session().read(cx).exited().is_none()
        }) {
            self.activate(index, window, cx);
            return;
        }
        let launch = match launch {
            Some(launch) => launch,
            None => {
                let Some(workspace) = self.workspace.upgrade() else {
                    return;
                };
                agent_launch(agent, workspace.read(cx), cx)
            }
        };
        if self.open(launch, false, window, cx).is_some()
            && let Some(tab) = self.tabs.last_mut()
        {
            tab.agent = Some(agent.id);
        }
    }

    /// Types `reference` and a space into the agent tab used last, or Claude Code's, opened with
    /// `launch` if there's none, and shows it. Nothing's sent until Enter is pressed there.
    pub fn send_to_agent(
        &mut self,
        reference: &str,
        launch: Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let running =
            |t: &Tab| t.agent.is_some() && t.view.read(cx).session().read(cx).exited().is_none();
        let index = match self.tabs.get(self.active).filter(|t| running(t)) {
            Some(_) => Some(self.active),
            None => self.tabs.iter().rposition(running),
        };
        let view = match index {
            Some(index) => {
                self.activate(index, window, cx);
                self.tabs[index].view.clone()
            }
            None => {
                self.open_agent(&crate::agents::CLAUDE_CODE, Some(launch), window, cx);
                let Some(tab) = self.tabs.last() else {
                    return;
                };
                tab.view.clone()
            }
        };
        let text = format!("{reference} ");
        view.update(cx, |view, cx| {
            view.session().update(cx, |session, _| session.paste(&text))
        });
    }

    /// What + opens: a shell in the Terminal, Claude Code among the Agents.
    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.kind {
            PanelKind::Terminal => self.new_shell(window, cx),
            PanelKind::Agents => self.open_agent(&crate::agents::CLAUDE_CODE, None, window, cx),
        }
    }

    /// The Agents' ▾ menu: which agent to start.
    fn show_new_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        let menu = cx.new(|cx| {
            let mut menu = gleipnir_ui::ContextMenu::new(cx);
            for agent in AGENTS {
                let panel = panel.clone();
                menu = menu.item(agent.name, None, move |window, cx| {
                    panel
                        .update(cx, |p, cx| p.open_agent(&agent, None, window, cx))
                        .ok();
                });
            }
            menu
        });
        self.workspace
            .update(cx, |w, cx| w.show_menu(menu, position, window, cx))
            .ok();
    }

    /// Opens a tab running `launch`, and shows it.
    pub fn open(
        &mut self,
        launch: Launch,
        shell: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        let session = match Session::spawn(launch, cx) {
            Ok(session) => session,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return None;
            }
        };
        self.error = None;
        let view = cx.new(|cx| TerminalView::new(session, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &view,
                window,
                |this, view, event: &SessionEvent, window, cx| match event {
                    SessionEvent::Exited(_) => {
                        let shell = this.tabs.iter().any(|t| &t.view == view && t.shell);
                        if shell {
                            this.close(view, window, cx);
                        }
                    }
                    SessionEvent::TitleChanged => cx.notify(),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &OpenLocation, window, cx| {
                    let OpenLocation(path, line, column) = event;
                    let (path, position) = (path.clone(), Some((*line, *column)));
                    this.workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .open_path_at(path, position, window, cx)
                                .detach_and_log_err(cx)
                        })
                        .ok();
                },
            ),
        ];
        self.tabs.push(Tab {
            view: view.clone(),
            shell,
            agent: None,
            _subscriptions: subscriptions,
        });
        self.active = self.tabs.len() - 1;
        window.focus(&view.focus_handle(cx), cx);
        cx.notify();
        Some(view)
    }

    fn close(&mut self, view: &Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|t| &t.view == view) else {
            return;
        };
        self.tabs.remove(index);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        match self.tabs.get(self.active) {
            Some(tab) => window.focus(&tab.view.focus_handle(cx), cx),
            // The last one gone: the tool window hides, as IntelliJ's does. Afterwards, as this
            // can be the workspace's own doing, from Cmd+W.
            None => {
                let workspace = self.workspace.clone();
                let panel = cx.entity().downgrade();
                let id = self.kind.id();
                window.defer(cx, move |window, cx| {
                    let empty = panel.read_with(cx, |p, _| p.tabs.is_empty());
                    if empty.unwrap_or(false) {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.click_panel_button(id, window, cx);
                                workspace.focus_center(window, cx);
                            })
                            .ok();
                    }
                });
            }
        }
        cx.notify();
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            let view = tab.view.clone();
            self.close(&view, window, cx);
        }
    }

    /// Makes sure there's a terminal to show, and focuses it.
    pub fn ensure_one(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active) {
            Some(tab) => window.focus(&tab.view.focus_handle(cx), cx),
            None => self.new_shell(window, cx),
        }
    }

    pub fn active_view(&self) -> Option<&Entity<TerminalView>> {
        self.tabs.get(self.active).map(|t| &t.view)
    }

    pub fn views(&self) -> impl Iterator<Item = &Entity<TerminalView>> {
        self.tabs.iter().map(|t| &t.view)
    }

    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(index) {
            self.active = index;
            window.focus(&tab.view.focus_handle(cx), cx);
            cx.notify();
        }
    }
}

fn gleipnir_packages_home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.tabs.get(self.active) {
            Some(tab) => tab.view.focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Panel for TerminalPanel {
    fn id(&self) -> &'static str {
        TERMINAL_PANEL
    }
    fn title(&self) -> &'static str {
        "Terminal"
    }
    fn icon(&self) -> Icon {
        Icon::Terminal
    }
    fn position(&self) -> DockPosition {
        DockPosition::Bottom
    }
    fn debug_state(&self, cx: &App) -> serde_json::Value {
        self.state(cx)
    }
}

/// The Agents tool window on the right: the same tabs, holding coding agents.
pub struct AgentsPanel {
    inner: Entity<TerminalPanel>,
}

impl AgentsPanel {
    pub fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        Self {
            inner: cx.new(|cx| TerminalPanel::new(workspace, PanelKind::Agents, cx)),
        }
    }

    pub fn tabs(&self) -> &Entity<TerminalPanel> {
        &self.inner
    }
}

impl Focusable for AgentsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.inner.focus_handle(cx)
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.inner.clone())
    }
}

impl Panel for AgentsPanel {
    fn id(&self) -> &'static str {
        AGENTS_PANEL
    }
    fn title(&self) -> &'static str {
        "Agents"
    }
    fn icon(&self) -> Icon {
        Icon::Agent
    }
    fn position(&self) -> DockPosition {
        DockPosition::Right
    }
    fn debug_state(&self, cx: &App) -> serde_json::Value {
        self.inner.read(cx).state(cx)
    }
}

impl TerminalPanel {
    fn state(&self, cx: &App) -> serde_json::Value {
        let screen = self
            .active_view()
            .map(|view| crate::view::screen_text(view, cx))
            .unwrap_or_default();
        // Up to the last line with something on it.
        let used = screen
            .iter()
            .rposition(|l| !l.is_empty())
            .map_or(0, |i| i + 1);
        serde_json::json!({
            "tabs": self.tabs.iter().map(|t| t.view.read(cx).session().read(cx).launch().title.clone()).collect::<Vec<_>>(),
            "active": self.active,
            "screen": screen[..used].join("\n"),
            "error": self.error,
        })
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        // Shown with nothing in it, as when the window opens with it showing: a shell starts.
        // An agent never starts unasked; the Agents window offers them instead.
        if self.kind == PanelKind::Terminal
            && self.tabs.is_empty()
            && self.error.is_none()
            && !self.starting
        {
            self.starting = true;
            cx.defer_in(window, |this, window, cx| {
                this.starting = false;
                if this.tabs.is_empty() {
                    this.new_tab(window, cx);
                }
            });
        }
        let tabs = self.tabs.iter().enumerate().map(|(index, tab)| {
            let session = tab.view.read(cx).session().read(cx);
            // A shell's own title is usually `user@host:path`; the tab keeps its name. A program,
            // like an agent, may say what it's doing.
            let mut title = if tab.shell {
                session.launch().title.clone()
            } else {
                session.title().to_string()
            };
            if let Some(code) = session.exited() {
                title = match code {
                    Some(code) => format!("{title} (exited {code})"),
                    None => format!("{title} (ended)"),
                };
            }
            let active = index == self.active;
            let view = tab.view.clone();
            div()
                .id(("terminal-tab", index))
                .debug_selector(move || format!("terminal-tab-{index}"))
                .h_full()
                .px(px(10.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .max_w(px(260.))
                .cursor_pointer()
                .text_color(if active { theme.text } else { theme.text_muted })
                .when(active, |d| d.border_b_2().border_color(theme.accent))
                .hover(|s| s.bg(theme.hover))
                .child(div().truncate().child(title))
                .child(
                    div()
                        .id(("terminal-tab-close", index))
                        .rounded(px(3.))
                        .hover(|s| s.bg(theme.hover))
                        .child(
                            svg()
                                .path(Icon::Close.path())
                                .size(px(10.))
                                .text_color(theme.text_faint),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close(&view, window, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.activate(index, window, cx)))
        });
        let body = match (self.tabs.get(self.active), &self.error) {
            (Some(tab), _) => div().flex_1().min_h_0().child(tab.view.clone()),
            (None, Some(error)) => div()
                .flex_1()
                .p(px(12.))
                .text_color(theme.error)
                .child(format!("The terminal couldn't start: {error}")),
            (None, None) if self.kind == PanelKind::Agents => div()
                .flex_1()
                .p(px(12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .text_color(theme.text_muted)
                .child("Start a coding agent: your own installed tool, signed in by you.")
                .children(AGENTS.iter().map(|agent| {
                    let agent = *agent;
                    div()
                        .id(agent.id)
                        .debug_selector(move || format!("start-{}", agent.id))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme.border)
                        .text_color(theme.text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.hover))
                        .child(agent.name)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_agent(&agent, None, window, cx)
                        }))
                })),
            (None, None) => div()
                .flex_1()
                .p(px(12.))
                .text_color(theme.text_faint)
                .child("No terminal open. Click + to open one."),
        };
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_size(px(12.5))
            .text_color(theme.text)
            .child(
                div()
                    .h(px(28.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .children(tabs)
                    .child(
                        div()
                            .id("terminal-new")
                            .debug_selector(|| "terminal-new".into())
                            .mx(px(6.))
                            .p(px(3.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.hover))
                            .child(
                                svg()
                                    .path(Icon::Plus.path())
                                    .size(px(12.))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx))),
                    )
                    .when(self.kind == PanelKind::Agents, |d| {
                        d.child(
                            div()
                                .id("terminal-new-menu")
                                .debug_selector(|| "terminal-new-menu".into())
                                .px(px(4.))
                                .rounded(px(3.))
                                .cursor_pointer()
                                .text_color(theme.text_muted)
                                .hover(|s| s.bg(theme.hover))
                                .child("▾")
                                .on_click(cx.listener(
                                    |this, event: &gpui::ClickEvent, window, cx| {
                                        this.show_new_menu(event.position(), window, cx)
                                    },
                                )),
                        )
                    }),
            )
            .child(body)
    }
}
