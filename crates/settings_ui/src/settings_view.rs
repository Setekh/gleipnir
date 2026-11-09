//! Settings, as IntelliJ lays them out: pages on the left, with a search over all of them, and each
//! page's settings on the right. Every change is saved and takes effect at once.

use crate::LanguageSettings;
use gleipnir_core::{Toolchains, settings, update_settings};
use gleipnir_packages::settings::{CodeStyle, Settings};
use gleipnir_ui::{
    ContextMenu, MenuDismissed, TextInput, TextInputEvent, Theme, Themes, themes_dir,
};
use gleipnir_workspace::{
    ModalView, add_user_binding, humanize_action_name, remove_user_binding, shortcuts_by_action,
    user_bindings, user_keymap_path,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, KeyDownEvent, Pixels, Point, SharedString, Subscription, Window, actions, anchored,
    deferred, div, prelude::*, px,
};

actions!(settings, [CloseSettings]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Appearance,
    Editor,
    CodeStyle,
    Keymap,
    Languages,
    RunDebug,
    Packages,
}

impl Page {
    pub const ALL: [Page; 7] = [
        Page::Appearance,
        Page::Editor,
        Page::CodeStyle,
        Page::Keymap,
        Page::Languages,
        Page::RunDebug,
        Page::Packages,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::Editor => "Editor",
            Page::CodeStyle => "Code Style",
            Page::Keymap => "Keymap",
            Page::Languages => "Languages & Tools",
            Page::RunDebug => "Run & Debug",
            Page::Packages => "Packages",
        }
    }
}

/// How one setting is changed.
enum Control {
    Toggle {
        get: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    },
    Number {
        get: fn(&Settings) -> f32,
        set: fn(&mut Settings, f32),
        min: f32,
        max: f32,
        step: f32,
    },
    Choice {
        get: fn(&Settings, &App) -> String,
        options: fn(&App) -> Vec<String>,
        set: fn(&mut Settings, &str),
    },
}

/// A setting on a page: what it's called, what it does, and how it's changed.
struct Row {
    page: Page,
    label: &'static str,
    description: &'static str,
    control: Control,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            page: Page::Appearance,
            label: "Theme",
            description: "The colors of the whole window. Themes are files in the themes folder, which Gleipnir reads again as they change.",
            control: Control::Choice {
                get: |s, cx| {
                    // The older names of the built-in themes.
                    match s.appearance.theme.as_str() {
                        "Dark" => "Gleipnir Dark".into(),
                        "Light" => "Gleipnir Light".into(),
                        name if cx
                            .try_global::<Themes>()
                            .map_or(name.starts_with("Gleipnir "), |t| t.find(name).is_some()) =>
                        {
                            name.into()
                        }
                        name => format!("{name} (missing)"),
                    }
                },
                options: |cx| {
                    cx.try_global::<Themes>()
                        .map(Themes::names)
                        .unwrap_or_else(|| vec!["Gleipnir Dark".into(), "Gleipnir Light".into()])
                },
                set: |s, v| s.appearance.theme = v.to_string(),
            },
        },
        Row {
            page: Page::Appearance,
            label: "Interface font size",
            description: "The size of text outside the editor: panels, menus and tabs.",
            control: Control::Number {
                get: |s| s.appearance.ui_font_size,
                set: |s, v| s.appearance.ui_font_size = v,
                min: 10.,
                max: 20.,
                step: 1.,
            },
        },
        Row {
            page: Page::Editor,
            label: "Font",
            description: "The editor's font. Automatic picks the first good monospace font installed.",
            control: Control::Choice {
                get: |s, _| {
                    s.editor
                        .font_family
                        .clone()
                        .unwrap_or_else(|| AUTOMATIC.into())
                },
                options: monospace_fonts,
                set: |s, v| {
                    s.editor.font_family = (v != AUTOMATIC).then(|| v.to_string());
                },
            },
        },
        Row {
            page: Page::Editor,
            label: "Font size",
            description: "The size of the editor's text, and the console's.",
            control: Control::Number {
                get: |s| s.editor.font_size,
                set: |s, v| s.editor.font_size = v,
                min: 8.,
                max: 32.,
                step: 1.,
            },
        },
        Row {
            page: Page::Editor,
            label: "Line height",
            description: "The height of each line in the editor, in pixels.",
            control: Control::Number {
                get: |s| s.editor.line_height,
                set: |s, v| s.editor.line_height = v,
                min: 12.,
                max: 48.,
                step: 1.,
            },
        },
        Row {
            page: Page::Editor,
            label: "Show completion while typing",
            description: "Suggestions pop up as a word is typed. Off, they show only on Ctrl+Space.",
            control: Control::Toggle {
                get: |s| s.editor.completion_while_typing,
                set: |s, v| s.editor.completion_while_typing = v,
            },
        },
        Row {
            page: Page::Editor,
            label: "Show parameter info while typing",
            description: "A call's parameters show on typing its opening parenthesis.",
            control: Control::Toggle {
                get: |s| s.editor.parameter_info_while_typing,
                set: |s, v| s.editor.parameter_info_while_typing = v,
            },
        },
        Row {
            page: Page::Editor,
            label: "Inlay hints",
            description: "Parameter names and inferred types, shown between the code.",
            control: Control::Toggle {
                get: |s| s.editor.inlay_hints,
                set: |s, v| s.editor.inlay_hints = v,
            },
        },
        Row {
            page: Page::Languages,
            label: "Install missing tools automatically",
            description: "A language server or debugger that isn't installed is installed the first time it's needed.",
            control: Control::Toggle {
                get: |s| s.tools.install_automatically,
                set: |s, v| s.tools.install_automatically = v,
            },
        },
        Row {
            page: Page::RunDebug,
            label: "Show passed tests",
            description: "Test results list the tests that passed, as well as the rest.",
            control: Control::Toggle {
                get: |s| s.tests.show_passed,
                set: |s, v| s.tests.show_passed = v,
            },
        },
    ]
}

const AUTOMATIC: &str = "Automatic";

/// Installed fonts that are made for code: well-known ones, and any whose name says so.
fn monospace_fonts(cx: &App) -> Vec<String> {
    const KNOWN: &[&str] = &[
        "JetBrains Mono",
        "Fira Code",
        "SF Mono",
        "Menlo",
        "Monaco",
        "Cascadia Code",
        "Source Code Pro",
        "Hack",
        "Iosevka",
        "IBM Plex Mono",
        "Ubuntu Mono",
        "DejaVu Sans Mono",
        "Inconsolata",
        "Victor Mono",
        "Roboto Mono",
        "Courier New",
    ];
    let installed = cx.text_system().all_font_names();
    let mut fonts: Vec<String> = installed
        .into_iter()
        .filter(|name| {
            !name.starts_with('.')
                && (KNOWN.contains(&name.as_str())
                    || name.contains("Mono")
                    || name.contains("Code"))
        })
        .collect();
    fonts.sort();
    fonts.dedup();
    fonts.insert(0, AUTOMATIC.into());
    fonts
}

/// Whether a search matches some text, ignoring case.
fn matches(query: &str, text: &str) -> bool {
    query.is_empty() || text.to_lowercase().contains(&query.to_lowercase())
}

/// An action listed on the Keymap page.
#[derive(Clone)]
struct KeymapRow {
    name: String,
    title: String,
    /// (keys, context, whether it's the person's own)
    shortcuts: Vec<(String, Option<String>, bool)>,
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    page: Page,
    search: Entity<TextInput>,
    languages: Entity<LanguageSettings>,
    /// The action a shortcut is being recorded for, waiting for its keys.
    recording: Option<String>,
    /// A problem with the last change, like a file that couldn't be saved.
    problem: Option<SharedString>,
    menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for SettingsView {}
impl ModalView for SettingsView {}

/// Opening Settings puts the cursor in the search, as IntelliJ does; the view's own focus is for
/// recording a shortcut.
impl Focusable for SettingsView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }
}

impl SettingsView {
    pub fn new(page: Page, toolchains: Entity<Toolchains>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search settings", cx));
        let languages = cx.new(|cx| LanguageSettings::embedded(toolchains, cx));
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, _: &TextInputEvent, cx| this.searched(cx)),
            cx.observe_global::<gleipnir_core::AppSettings>(|_, cx| cx.notify()),
        ];
        Self {
            focus_handle: cx.focus_handle(),
            page,
            search,
            languages,
            recording: None,
            problem: gleipnir_core::AppSettings::problem(cx).map(Into::into),
            menu: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// The Languages & Tools page's SDKs and tools.
    pub fn languages(&self) -> &Entity<LanguageSettings> {
        &self.languages
    }

    pub fn show_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.recording = None;
        cx.notify();
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).text().trim().to_string()
    }

    /// Whether a page has anything matching the search.
    fn page_matches(&self, page: Page, query: &str, cx: &App) -> bool {
        if query.is_empty() || matches(query, page.title()) {
            return true;
        }
        let in_rows = rows()
            .iter()
            .any(|r| r.page == page && (matches(query, r.label) || matches(query, r.description)));
        in_rows
            || match page {
                Page::CodeStyle => languages().iter().any(|(name, _)| matches(query, name)),
                Page::Keymap => !self.keymap_rows(query, cx).is_empty(),
                Page::Languages => ["sdk", "toolchain", "language server", "debugger"]
                    .iter()
                    .any(|w| matches(query, w)),
                Page::Packages => gleipnir_packages::registry()
                    .packages()
                    .iter()
                    .any(|p| matches(query, p.name())),
                _ => false,
            }
    }

    /// A search moves to the first page with something matching, if this one has nothing.
    fn searched(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if !self.page_matches(self.page, &query, cx)
            && let Some(page) = Page::ALL
                .into_iter()
                .find(|p| self.page_matches(*p, &query, cx))
        {
            self.page = page;
        }
        cx.notify();
    }

    fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        self.problem = update_settings(cx, change)
            .err()
            .map(|e| format!("Couldn't save the settings: {e:#}").into());
        cx.notify();
    }

    /// Changes a setting as if from its control, for tests and the driver.
    pub fn set_value(
        &mut self,
        label: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let row = rows()
            .into_iter()
            .find(|r| r.label == label)
            .ok_or_else(|| anyhow::anyhow!("no setting called {label}"))?;
        match row.control {
            Control::Toggle { set, .. } => {
                let on = value == "true" || value == "on";
                self.change(|s| set(s, on), cx);
            }
            Control::Number { set, min, max, .. } => {
                let number: f32 = value.parse()?;
                self.change(|s| set(s, number.clamp(min, max)), cx);
            }
            Control::Choice { set, .. } => {
                let value = value.to_string();
                self.change(move |s| set(s, &value), cx);
            }
        }
        Ok(())
    }

    fn open_menu(
        &mut self,
        options: Vec<String>,
        set: fn(&mut Settings, &str),
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.weak_entity();
        let menu = cx.new(|cx| {
            options
                .into_iter()
                .fold(ContextMenu::new(cx), |menu, option| {
                    let view = view.clone();
                    let chosen = option.clone();
                    menu.item(option, None, move |_, cx| {
                        let chosen = chosen.clone();
                        view.update(cx, |this, cx| this.change(move |s| set(s, &chosen), cx))
                            .ok();
                    })
                })
        });
        window.focus(&menu.read(cx).focus_handle(cx), cx);
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, _: &MenuDismissed, window, cx| {
                this.menu = None;
                window.focus(&this.focus_handle, cx);
                cx.notify();
            });
        self.menu = Some((menu, position, subscription));
        cx.notify();
    }

    // Keymap.

    fn keymap_rows(&self, query: &str, cx: &App) -> Vec<KeymapRow> {
        let shortcuts = shortcuts_by_action(cx);
        let own = user_bindings(&user_keymap_path());
        let mut rows: Vec<KeymapRow> = cx
            .all_action_names()
            .iter()
            .filter(|name| {
                name.split_once("::").is_some_and(|(space, _)| {
                    !matches!(space, "zed" | "context_menu" | "text_input" | "settings")
                })
            })
            .map(|name| {
                let shortcuts = shortcuts
                    .get(*name)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(keys, context)| {
                        let mine = own
                            .iter()
                            .any(|e| e.action == *name && e.keystrokes == keys);
                        (keys, context, mine)
                    })
                    .collect();
                KeymapRow {
                    name: name.to_string(),
                    title: humanize_action_name(name),
                    shortcuts,
                }
            })
            .filter(|row| {
                matches(query, &row.title)
                    || matches(query, &row.name)
                    || row.shortcuts.iter().any(|(k, _, _)| matches(query, k))
            })
            .collect();
        rows.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.name.cmp(&b.name)));
        rows
    }

    /// Records the next keys pressed as a shortcut for an action.
    pub fn record_shortcut(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(action.to_string());
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.recording.clone() else {
            return;
        };
        cx.stop_propagation();
        let keystroke = &event.keystroke;
        // Modifiers on their own are the start of a shortcut, not one.
        if matches!(
            keystroke.key.as_str(),
            "shift" | "control" | "alt" | "platform" | "function" | "cmd" | "ctrl"
        ) {
            return;
        }
        self.recording = None;
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            cx.notify();
            return;
        }
        let keys = keystroke.unparse(cx.platform_info().os);
        self.problem = add_user_binding(&user_keymap_path(), &keys, &action, cx)
            .err()
            .map(Into::into);
        cx.notify();
    }

    fn remove_shortcut(&mut self, keys: &str, action: &str, cx: &mut Context<Self>) {
        self.problem = remove_user_binding(&user_keymap_path(), keys, action, cx)
            .err()
            .map(Into::into);
        cx.notify();
    }

    // Rendering.

    fn render_control(
        &self,
        index: usize,
        control: &Control,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = settings(cx).clone();
        match control {
            Control::Toggle { get, set } => {
                let on = get(&current);
                let set = *set;
                switch(("setting", index), on, theme)
                    .on_click(cx.listener(move |this, _, _, cx| this.change(|s| set(s, !on), cx)))
                    .into_any_element()
            }
            Control::Number {
                get,
                set,
                min,
                max,
                step,
            } => {
                let value = get(&current);
                let (set, min, max, step) = (*set, *min, *max, *step);
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        small_button(("setting-less", index), "−", theme).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.change(|s| set(s, (value - step).max(min)), cx)
                            },
                        )),
                    )
                    .child(
                        div()
                            .id(("setting-value", index))
                            .debug_selector(move || format!("setting-value-{index}"))
                            .w(px(44.))
                            .text_center()
                            .child(format_number(value)),
                    )
                    .child(
                        small_button(("setting-more", index), "+", theme).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.change(|s| set(s, (value + step).min(max)), cx)
                            },
                        )),
                    )
                    .into_any_element()
            }
            Control::Choice { get, options, set } => {
                let shown = get(&current, cx);
                let (options, set) = (*options, *set);
                div()
                    .id(("setting", index))
                    .debug_selector(move || format!("setting-{index}"))
                    .min_w(px(140.))
                    .px(px(10.))
                    .py(px(4.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .gap(px(8.))
                    .child(shown)
                    .child(div().text_color(theme.text_muted).child("▾"))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        let options = options(cx);
                        this.open_menu(options, set, event.position(), window, cx);
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_rows(&self, query: &str, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.page == self.page)
            .filter(|(_, r)| {
                matches(query, r.label)
                    || matches(query, r.description)
                    || matches(query, r.page.title())
            })
            .map(|(index, row)| {
                setting_row(
                    row.label,
                    row.description,
                    self.render_control(index, &row.control, theme, cx),
                    theme,
                )
            })
            .collect()
    }

    fn render_code_style(
        &self,
        query: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let current = settings(cx).clone();
        languages()
            .into_iter()
            .enumerate()
            .filter(|(_, (name, _))| {
                matches(query, name) || matches(query, "code style indent tab")
            })
            .map(|(index, (name, own))| {
                let style = current.code_style_for(&name, own);
                let changed = current.code_style.contains_key(&name);
                let set = {
                    let name = name.clone();
                    move |style: CodeStyle| {
                        let name = name.clone();
                        move |s: &mut Settings| {
                            if style == own {
                                s.code_style.remove(&name);
                            } else {
                                s.code_style.insert(name.clone(), style);
                            }
                        }
                    }
                };
                let narrower = set(CodeStyle {
                    tab_size: style.tab_size.saturating_sub(1).max(1),
                    ..style
                });
                let wider = set(CodeStyle {
                    tab_size: (style.tab_size + 1).min(16),
                    ..style
                });
                let tabs = set(CodeStyle {
                    hard_tabs: !style.hard_tabs,
                    ..style
                });
                let control = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .child(div().text_color(theme.text_muted).child("Indent"))
                    .child(small_button(("indent-less", index), "−", theme).on_click(
                        cx.listener(move |this, _, _, cx| this.change(narrower.clone(), cx)),
                    ))
                    .child(
                        div()
                            .w(px(20.))
                            .text_center()
                            .child(style.tab_size.to_string()),
                    )
                    .child(small_button(("indent-more", index), "+", theme).on_click(
                        cx.listener(move |this, _, _, cx| this.change(wider.clone(), cx)),
                    ))
                    .child(div().text_color(theme.text_muted).child("Tabs"))
                    .child(switch(("tabs", index), style.hard_tabs, theme).on_click(
                        cx.listener(move |this, _, _, cx| this.change(tabs.clone(), cx)),
                    ));
                let description = if changed {
                    format!(
                        "Changed from the language's {} {}",
                        own.tab_size,
                        if own.hard_tabs { "with tabs" } else { "spaces" }
                    )
                } else {
                    "As the language has it".to_string()
                };
                setting_row_owned(name, description, control.into_any_element(), theme)
            })
            .collect()
    }

    fn render_keymap(&self, query: &str, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let rows = self.keymap_rows(query, cx);
        let recording = self.recording.clone();
        rows.into_iter()
            .take(300)
            .enumerate()
            .map(|(index, row)| {
                let name = row.name.clone();
                let is_recording = recording.as_deref() == Some(row.name.as_str());
                let chips = row
                    .shortcuts
                    .iter()
                    .enumerate()
                    .map(|(n, (keys, context, mine))| {
                        let (keys, action) = (keys.clone(), row.name.clone());
                        let label = match context {
                            Some(context) => format!("{keys}  in {context}"),
                            None => keys.clone(),
                        };
                        div()
                            .id(SharedString::from(format!("shortcut-{index}-{n}")))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(px(11.5))
                            .child(label)
                            .when(*mine, |chip| {
                                chip.child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "remove-shortcut-{index}-{n}"
                                        )))
                                        .cursor_pointer()
                                        .text_color(theme.text_muted)
                                        .hover(|s| s.text_color(theme.error))
                                        .child("×")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_shortcut(&keys, &action, cx)
                                        })),
                                )
                            })
                            .into_any_element()
                    });
                let add = div()
                    .id(SharedString::from(format!("add-shortcut-{index}")))
                    .debug_selector(move || format!("add-shortcut-{index}"))
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(11.5))
                    .text_color(if is_recording {
                        theme.accent
                    } else {
                        theme.text_muted
                    })
                    .hover(|s| s.bg(theme.hover))
                    .child(if is_recording {
                        "Press keys…"
                    } else {
                        "+ Add"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.record_shortcut(&name, window, cx)
                    }));
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(5.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .w(px(230.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(row.title.clone())
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.text_faint)
                                    .child(row.name.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(4.))
                            .children(chips)
                            .child(add),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// Where themes come from, and anything wrong with one there.
    fn render_themes_note(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let dir = themes_dir();
        let mut rows = vec![
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(16.))
                .px(px(16.))
                .py(px(10.))
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(format!(
                            "Themes are TOML files in {}. Its README says how to make one; a theme saved there, or changed, shows at once.",
                            dir.display()
                        )),
                )
                .child(
                    small_text_button("open-themes", "Open Folder", theme).on_click(move |_, _, cx| {
                        cx.reveal_path(&dir.join("README.md"));
                    }),
                )
                .into_any_element(),
        ];
        if let Some(themes) = cx.try_global::<Themes>() {
            rows.extend(themes.problems.iter().map(|(path, problem)| {
                div()
                    .px(px(16.))
                    .py(px(6.))
                    .text_size(px(12.))
                    .text_color(theme.warning)
                    .child(format!(
                        "{}: {problem}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ))
                    .into_any_element()
            }));
        }
        rows
    }

    fn render_packages(&self, query: &str, theme: &Theme) -> Vec<AnyElement> {
        let registry = gleipnir_packages::registry();
        let mut rows: Vec<AnyElement> = registry
            .packages()
            .iter()
            .filter(|p| matches(query, p.name()))
            .map(|p| {
                let origin = if p.is_builtin() {
                    "Built in".to_string()
                } else {
                    format!("Installed from {}", p.origin)
                };
                setting_row_owned(
                    p.name().to_string(),
                    origin,
                    div().into_any_element(),
                    theme,
                )
            })
            .collect();
        rows.extend(registry.errors().iter().map(|error| {
            div()
                .px(px(16.))
                .py(px(6.))
                .text_color(theme.warning)
                .child(error.clone())
                .into_any_element()
        }));
        rows
    }
}

/// Every language, with the indentation it comes with.
fn languages() -> Vec<(String, CodeStyle)> {
    let mut found: Vec<(String, CodeStyle)> = gleipnir_packages::registry()
        .languages()
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                CodeStyle {
                    tab_size: entry.edit_config.tab_size,
                    hard_tabs: entry.edit_config.hard_tabs,
                },
            )
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

fn format_number(value: f32) -> String {
    if value.fract() == 0. {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn setting_row(
    label: &'static str,
    description: &'static str,
    control: AnyElement,
    theme: &Theme,
) -> AnyElement {
    setting_row_owned(label.to_string(), description.to_string(), control, theme)
}

fn setting_row_owned(
    label: String,
    description: String,
    control: AnyElement,
    theme: &Theme,
) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .px(px(16.))
        .py(px(10.))
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .min_w_0()
                .child(div().child(label))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(description),
                ),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// An on/off switch.
fn switch(id: impl Into<gpui::ElementId>, on: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let selector = format!("{id}");
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .w(px(34.))
        .h(px(20.))
        .flex_none()
        .rounded(px(10.))
        .p(px(2.))
        .cursor_pointer()
        .bg(if on { theme.accent } else { theme.border })
        .flex()
        .flex_row()
        .when(on, |d| d.justify_end())
        .child(div().size(px(16.)).rounded(px(8.)).bg(gpui::white()))
}

fn small_text_button(
    id: &'static str,
    label: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .px(px(10.))
        .py(px(4.))
        .rounded(px(5.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .cursor_pointer()
        .hover(|s| s.bg(theme.hover))
        .child(label)
}

fn small_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let selector = format!("{id}");
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .cursor_pointer()
        .hover(|s| s.bg(theme.hover))
        .child(label)
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let query = self.query(cx);
        let nav: Vec<AnyElement> = Page::ALL
            .into_iter()
            .filter(|page| self.page_matches(*page, &query, cx))
            .map(|page| {
                let active = page == self.page;
                div()
                    .id(SharedString::from(format!(
                        "settings-page-{}",
                        page.title()
                    )))
                    .debug_selector(move || format!("settings-page-{}", page.title()))
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme.selection))
                    .when(!active, |d| d.hover(|s| s.bg(theme.hover)))
                    .child(page.title())
                    .on_click(cx.listener(move |this, _, _, cx| this.show_page(page, cx)))
                    .into_any_element()
            })
            .collect();
        let mut body: Vec<AnyElement> = self.render_rows(&query, &theme, cx);
        match self.page {
            Page::Appearance => body.extend(self.render_themes_note(&theme, cx)),
            Page::CodeStyle => body.extend(self.render_code_style(&query, &theme, cx)),
            Page::Keymap => {
                body.push(
                    div()
                        .px(px(16.))
                        .py(px(8.))
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(format!(
                            "Shortcuts you add are kept in {}, and take effect at once.",
                            user_keymap_path().display()
                        ))
                        .into_any_element(),
                );
                body.extend(self.render_keymap(&query, &theme, cx));
            }
            Page::Languages => body.push(self.languages.clone().into_any_element()),
            Page::Packages => {
                body.push(
                    div()
                        .px(px(16.))
                        .py(px(10.))
                        .child(
                            div()
                                .id("settings-install-package")
                                .px(px(10.))
                                .py(px(4.))
                                .rounded(px(5.))
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.surface)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.hover))
                                .child("Install Package…")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(crate::InstallPackage), cx)
                                }),
                        )
                        .into_any_element(),
                );
                body.extend(self.render_packages(&query, &theme));
            }
            _ => {}
        }
        let menu = self.menu.as_ref().map(|(menu, position, _)| {
            deferred(anchored().position(*position).child(menu.clone())).with_priority(3)
        });
        let size = window.viewport_size();
        div()
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(940.).min(size.width - px(40.)))
            .h(px(640.).min(size.height - px(120.)))
            .flex()
            .flex_col()
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(13.))
            .text_color(theme.text)
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(DismissEvent)))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.menu.is_none() {
                    cx.emit(DismissEvent)
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .children(
                        self.problem
                            .clone()
                            .map(|p| div().text_size(px(12.)).text_color(theme.error).child(p)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .w(px(210.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .p(px(8.))
                            .border_r_1()
                            .border_color(theme.border)
                            .bg(theme.surface)
                            .child(
                                div()
                                    .mb(px(6.))
                                    .px(px(8.))
                                    .py(px(4.))
                                    .rounded(px(5.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.background)
                                    .child(self.search.clone()),
                            )
                            .children(nav),
                    )
                    .child(
                        div()
                            .id("settings-body")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .px(px(16.))
                                    .pt(px(14.))
                                    .pb(px(6.))
                                    .text_size(px(15.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(self.page.title()),
                            )
                            .children(body),
                    ),
            )
            .children(menu)
    }
}

impl SettingsView {
    /// What the page shows, as text, for tests and the driver.
    pub fn debug_rows(&self, cx: &App) -> Vec<String> {
        let query = self.query(cx);
        let current = settings(cx).clone();
        let mut lines: Vec<String> = rows()
            .iter()
            .filter(|r| {
                r.page == self.page && (matches(&query, r.label) || matches(&query, r.description))
            })
            .map(|r| {
                let value = match &r.control {
                    Control::Toggle { get, .. } => get(&current).to_string(),
                    Control::Number { get, .. } => format_number(get(&current)),
                    Control::Choice { get, .. } => get(&current, cx),
                };
                format!("{}: {value}", r.label)
            })
            .collect();
        if self.page == Page::Keymap {
            lines.extend(self.keymap_rows(&query, cx).into_iter().map(|row| {
                let keys: Vec<String> = row.shortcuts.iter().map(|(k, _, _)| k.clone()).collect();
                format!("{}: {}", row.title, keys.join(", "))
            }));
        }
        lines
    }

    /// The pages the search leaves, for tests.
    pub fn visible_pages(&self, cx: &App) -> Vec<Page> {
        let query = self.query(cx);
        Page::ALL
            .into_iter()
            .filter(|p| self.page_matches(*p, &query, cx))
            .collect()
    }

    pub fn search_for(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_text(text, cx));
        self.searched(cx);
    }
}
