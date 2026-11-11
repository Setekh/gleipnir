//! Code insight from language servers: completion and hover in the editor, go to declaration,
//! implementations and usages, the Problems panel, rename, quick fixes and reformatting.

mod apply;
mod convert;
mod find_in_files;
mod panels;
mod picker;
mod provider;
mod rename;

pub use apply::apply_workspace_edit;
pub use find_in_files::FindInFiles;
pub use panels::{PROBLEMS_PANEL, ProblemsPanel, USAGES_PANEL, Usage, UsagesPanel, previews};
pub use picker::{Picker, PickerItem};
pub use provider::LspSemantics;
pub use rename::RenameModal;

use gleipnir_core::{GotoKind, LanguageServerEvent, LanguageServers, Location, ServerStatus};
use gleipnir_editor::{EditorEvent, EditorView};
use gleipnir_lsp::lsp_types as lsp;
use gleipnir_text::Utf16Point;
use gleipnir_ui::Theme;
use gleipnir_workspace::Workspace;
use gpui::{
    App, AppContext as _, Context, Entity, KeyBinding, Subscription, Window, actions, div,
    prelude::*, px,
};
use std::path::PathBuf;
use std::rc::Rc;

actions!(
    code_nav,
    [
        GoToDeclarationOrUsages,
        GoToImplementation,
        GoToTypeDeclaration,
        FindUsages,
        ShowUsages,
        FileStructure,
        Rename,
        ShowIntentions,
        Reformat,
        ShowProblems,
        RestartLanguageServer,
        FindInPath,
        ReplaceInPath,
    ]
);

pub fn bind_keys(cx: &mut App) {
    picker::bind_keys(cx);
    find_in_files::bind_keys(cx);
    panels::bind_keys(cx);
    rename::bind_keys(cx);
    let editor = Some("Editor");
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-b", GoToDeclarationOrUsages, None),
        KeyBinding::new("cmd-alt-b", GoToImplementation, None),
        KeyBinding::new("cmd-shift-b", GoToTypeDeclaration, None),
        KeyBinding::new("alt-f7", FindUsages, None),
        KeyBinding::new("cmd-alt-f7", ShowUsages, None),
        KeyBinding::new("cmd-f12", FileStructure, None),
        KeyBinding::new("cmd-6", ShowProblems, None),
        KeyBinding::new("cmd-shift-f", FindInPath, None),
        KeyBinding::new("cmd-shift-r", ReplaceInPath, None),
        KeyBinding::new("shift-f6", Rename, editor),
        KeyBinding::new("alt-enter", ShowIntentions, editor),
        KeyBinding::new("cmd-alt-l", Reformat, editor),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-b", GoToDeclarationOrUsages, None),
        KeyBinding::new("ctrl-alt-b", GoToImplementation, None),
        KeyBinding::new("ctrl-shift-b", GoToTypeDeclaration, None),
        KeyBinding::new("alt-f7", FindUsages, None),
        KeyBinding::new("ctrl-alt-f7", ShowUsages, None),
        KeyBinding::new("ctrl-f12", FileStructure, None),
        KeyBinding::new("alt-6", ShowProblems, None),
        KeyBinding::new("ctrl-shift-f", FindInPath, None),
        KeyBinding::new("ctrl-shift-r", ReplaceInPath, None),
        KeyBinding::new("shift-f6", Rename, editor),
        KeyBinding::new("alt-enter", ShowIntentions, editor),
        KeyBinding::new("ctrl-alt-l", Reformat, editor),
    ]);
}

pub fn add_to_workspace(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let servers = workspace.language_servers().clone();
    let root = workspace.root().map(PathBuf::from);
    let weak = cx.weak_entity();
    let usages = cx.new(|cx| UsagesPanel::new(weak.clone(), root.clone(), cx));
    let problems = cx.new(|cx| ProblemsPanel::new(weak.clone(), root.clone(), &servers, cx));
    workspace.add_panel(usages, cx);
    workspace.add_panel(problems, cx);

    workspace.add_editor_hook(attach);

    workspace.register_action(|w, _: &GoToDeclarationOrUsages, window, cx| {
        go_to(w, GotoKind::Definition, true, window, cx)
    });
    workspace.register_action(|w, _: &GoToImplementation, window, cx| {
        go_to(w, GotoKind::Implementation, false, window, cx)
    });
    workspace.register_action(|w, _: &GoToTypeDeclaration, window, cx| {
        go_to(w, GotoKind::TypeDefinition, false, window, cx)
    });
    workspace.register_action(|w, _: &FindUsages, window, cx| find_usages(w, true, window, cx));
    workspace.register_action(|w, _: &ShowUsages, window, cx| find_usages(w, false, window, cx));
    workspace.register_action(|w, _: &FileStructure, window, cx| file_structure(w, window, cx));
    workspace.register_action(|w, _: &Rename, window, cx| rename::start(w, window, cx));
    workspace.register_action(|w, _: &ShowIntentions, window, cx| show_intentions(w, window, cx));
    workspace.register_action(|w, _: &Reformat, window, cx| reformat(w, window, cx));
    workspace
        .register_action(|w, _: &FindInPath, window, cx| find_in_files::open(w, false, window, cx));
    workspace.register_action(|w, _: &ReplaceInPath, window, cx| {
        find_in_files::open(w, true, window, cx)
    });
    workspace.register_action(|w, _: &ShowProblems, window, cx| {
        w.toggle_panel(PROBLEMS_PANEL, window, cx)
    });
    workspace.register_action(|w, _: &RestartLanguageServer, _, cx| {
        let Some(buffer) = active_buffer(w, cx) else {
            return;
        };
        if let Some((package, _)) = LanguageServers::server_name_for(&buffer, cx) {
            w.language_servers()
                .clone()
                .update(cx, |servers, cx| servers.restart(&package, cx));
        }
    });

    // Edits a server asks for while running a command, like a fix that's computed on demand.
    cx.subscribe_in(
        &servers,
        window,
        |workspace, servers, event, _, cx| match event {
            LanguageServerEvent::ApplyEditRequested {
                package_id,
                request_id,
                edit,
            } => {
                let apply = apply_workspace_edit(workspace, edit.clone(), cx);
                let (servers, package_id, request_id) =
                    (servers.clone(), package_id.clone(), request_id.clone());
                cx.spawn(async move |workspace, cx| {
                    let result = apply.await;
                    servers.read_with(cx, |servers, _| {
                        servers.respond_to_apply_edit(&package_id, request_id, result.is_ok())
                    });
                    if let Err(error) = result {
                        workspace
                            .update(cx, |w, cx| w.show_error(format!("{error:#}"), cx))
                            .ok();
                    }
                })
                .detach();
            }
            LanguageServerEvent::StatusChanged(_) => cx.notify(),
            LanguageServerEvent::DiagnosticsChanged(_) => {}
        },
    )
    .detach();

    workspace.add_status_item(status_item);
}

/// Wires up an editor: answers its questions from the servers, and keeps its file in sync with them.
fn attach(
    workspace: &mut Workspace,
    editor: &Entity<EditorView>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Vec<Subscription> {
    let servers = workspace.language_servers().clone();
    let buffer = editor.read(cx).buffer().clone();
    editor.update(cx, |editor, _| {
        editor.set_semantics(Rc::new(LspSemantics {
            servers: servers.clone(),
        }))
    });
    servers.update(cx, |servers, cx| servers.register_buffer(&buffer, cx));
    editor.update(cx, |editor, cx| editor.refresh_annotations(cx));
    // Hints and semantic colors are asked for again when the file's server changes state, as
    // a server answers emptily until it's loaded the project.
    let weak_editor = editor.downgrade();
    let watched = buffer.clone();
    let status = cx.subscribe(&servers, move |_, _, event, cx| {
        let LanguageServerEvent::StatusChanged(package) = event else {
            return;
        };
        let ours =
            LanguageServers::server_name_for(&watched, cx).is_some_and(|(p, _)| p == *package);
        if ours && let Some(editor) = weak_editor.upgrade() {
            editor.update(cx, |editor, cx| editor.refresh_annotations(cx));
        }
    });
    vec![
        status,
        // An untitled file that's been saved now has a language and a path.
        cx.observe(&buffer, move |_, buffer, cx| {
            servers.update(cx, |servers, cx| servers.register_buffer(&buffer, cx));
        }),
        cx.subscribe_in(editor, window, |workspace, _, event, window, cx| {
            if let EditorEvent::GoToDefinition { .. } = event {
                go_to(workspace, GotoKind::Definition, true, window, cx);
            }
        }),
    ]
}

/// While a server's still loading, an empty answer may only mean it doesn't know yet.
fn still_busy(
    workspace: &Workspace,
    buffer: &Entity<gleipnir_core::BufferModel>,
    cx: &App,
) -> Option<String> {
    let (package, server) = LanguageServers::server_name_for(buffer, cx)?;
    match workspace.language_servers().read(cx).status(&package)? {
        ServerStatus::Running { busy: Some(task) } => Some(format!(
            "{server} is still busy ({task}); try again when it's done"
        )),
        ServerStatus::Starting => {
            Some(format!("{server} is still starting; try again in a moment"))
        }
        ServerStatus::Installing => Some(format!(
            "{server} is being installed; try again when it's done"
        )),
        _ => None,
    }
}

fn active_buffer(workspace: &Workspace, cx: &App) -> Option<Entity<gleipnir_core::BufferModel>> {
    Some(workspace.active_editor(cx)?.read(cx).buffer().clone())
}

/// The active editor's buffer and cursor.
fn cursor(
    workspace: &Workspace,
    cx: &App,
) -> Option<(
    Entity<EditorView>,
    Entity<gleipnir_core::BufferModel>,
    usize,
)> {
    let editor = workspace.active_editor(cx)?;
    let view = editor.read(cx);
    let buffer = view.buffer().clone();
    let head = view.selections().primary().head;
    Some((editor, buffer, head))
}

/// Opens a file at a position in the server's terms, remembering where we were for Navigate Back.
pub fn open_location(
    workspace: &mut Workspace,
    path: PathBuf,
    start: Utf16Point,
    record: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if record {
        workspace.record_position(cx);
    }
    open_location_then(workspace, path, start, window, cx).detach();
}

/// Opens a location and resolves once the cursor is there, for callers that do something after,
/// like handing focus back to a list being previewed from.
pub fn open_location_then(
    workspace: &mut Workspace,
    path: PathBuf,
    start: Utf16Point,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::Task<()> {
    let open = workspace.open_path(path, window, cx);
    cx.spawn(async move |_, cx| {
        let Ok(editor) = open.await else { return };
        editor.update(cx, |editor, cx| {
            let point = {
                let buffer = editor.buffer().read(cx).buffer();
                buffer.offset_to_point(buffer.utf16_to_offset(start))
            };
            editor.jump_to(point.line, point.col, cx);
        });
    })
}

fn word_at(buffer: &Entity<gleipnir_core::BufferModel>, offset: usize, cx: &App) -> Option<String> {
    let text = buffer.read(cx).buffer();
    text.word_at(offset).map(|range| text.slice(range))
}

fn supports(
    workspace: &Workspace,
    buffer: &Entity<gleipnir_core::BufferModel>,
    kind: GotoKind,
    cx: &App,
) -> bool {
    let Some(capabilities) = workspace
        .language_servers()
        .read(cx)
        .capabilities_for(buffer, cx)
    else {
        // Not started yet: let the request say what's wrong.
        return true;
    };
    match kind {
        GotoKind::Definition => capabilities.definition_provider.is_some(),
        GotoKind::Implementation => capabilities.implementation_provider.is_some(),
        GotoKind::TypeDefinition => capabilities.type_definition_provider.is_some(),
    }
}

/// Cmd+B: go to where the name under the cursor is declared, or, on the declaration itself, show
/// where it's used.
fn go_to(
    workspace: &mut Workspace,
    kind: GotoKind,
    usages_if_here: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some((_, buffer, offset)) = cursor(workspace, cx) else {
        return;
    };
    if !supports(workspace, &buffer, kind, cx) {
        let what = match kind {
            GotoKind::Implementation => "implementations",
            GotoKind::TypeDefinition => "type declarations",
            GotoKind::Definition => "declarations",
        };
        workspace.show_message(format!("This language's server can't find {what}"), cx);
        return;
    }
    let here = {
        let model = buffer.read(cx);
        model
            .path()
            .map(|p| (p.to_path_buf(), model.buffer().offset_to_utf16(offset)))
    };
    let request = workspace
        .language_servers()
        .read(cx)
        .goto(&buffer, offset, kind, cx);
    cx.spawn_in(window, async move |this, cx| {
        let result = request.await;
        this.update_in(cx, |workspace, window, cx| match result {
            Err(error) => workspace.show_error(format!("{error:#}"), cx),
            Ok(locations) if locations.is_empty() => {
                let message = match kind {
                    _ if let Some(busy) = still_busy(workspace, &buffer, cx) => busy,
                    GotoKind::Implementation => "No implementations found".into(),
                    _ => "Cannot find declaration to go to".into(),
                };
                workspace.show_message(message, cx);
            }
            Ok(locations) => {
                let on_declaration = here.as_ref().is_some_and(|(path, point)| {
                    locations
                        .iter()
                        .any(|l| &l.path == path && l.start <= *point && *point <= l.end)
                });
                if usages_if_here && on_declaration {
                    find_usages(workspace, false, window, cx);
                } else {
                    let title = match kind {
                        GotoKind::Implementation => "Choose Implementation",
                        GotoKind::TypeDefinition => "Choose Type Declaration",
                        GotoKind::Definition => "Choose Declaration",
                    };
                    choose_location(workspace, title, locations, window, cx);
                }
            }
        })
        .ok();
    })
    .detach();
}

/// Goes straight to a single place, or offers a choice of several.
fn choose_location(
    workspace: &mut Workspace,
    title: &'static str,
    locations: Vec<Location>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let [location] = locations.as_slice() {
        open_location(
            workspace,
            location.path.clone(),
            location.start,
            true,
            window,
            cx,
        );
        return;
    }
    let previews = previews(workspace, locations, cx);
    let root = workspace.root().map(PathBuf::from);
    cx.spawn_in(window, async move |this, cx| {
        let usages = previews.await;
        this.update_in(cx, |workspace, window, cx| {
            show_usage_picker(workspace, title, root, usages, window, cx)
        })
        .ok();
    })
    .detach();
}

fn show_usage_picker(
    workspace: &mut Workspace,
    title: &str,
    root: Option<PathBuf>,
    usages: Vec<Usage>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let items = usages
        .iter()
        .map(|u| PickerItem {
            label: u.text.clone(),
            detail: format!(
                "{}:{}",
                panels::display_path(root.as_deref(), &u.path),
                u.line + 1
            ),
            indent: 0,
        })
        .collect();
    let handle = cx.weak_entity();
    let title = title.to_string();
    // Beside the caret, as IntelliJ's choosers are, with the hover out of the way.
    let anchor = workspace.active_editor(cx).and_then(|editor| {
        editor.update(cx, |editor, cx| {
            editor.dismiss_popups(cx);
            editor.caret_bounds(cx)
        })
    });
    workspace.toggle_modal_at(anchor, window, cx, move |_, cx| {
        Picker::new(
            title,
            items,
            0,
            move |index, window, cx| {
                let usage = usages[index].clone();
                handle
                    .update(cx, |workspace, cx| {
                        open_location(workspace, usage.path, usage.start, true, window, cx)
                    })
                    .ok();
            },
            cx,
        )
    });
}

/// Alt+F7 lists usages in the Find panel; Cmd+Alt+F7 in a popup, going straight there if there's
/// only one.
fn find_usages(
    workspace: &mut Workspace,
    in_panel: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some((_, buffer, offset)) = cursor(workspace, cx) else {
        return;
    };
    let name = word_at(&buffer, offset, cx).unwrap_or_default();
    let request = workspace
        .language_servers()
        .read(cx)
        .references(&buffer, offset, cx);
    let root = workspace.root().map(PathBuf::from);
    let panel = workspace.panel::<UsagesPanel>(cx);
    if in_panel && let Some(panel) = &panel {
        panel.update(cx, |panel, cx| {
            panel.start_search(format!("Usages of {name}"), cx)
        });
        workspace.show_panel(USAGES_PANEL, cx);
    }
    cx.spawn_in(window, async move |this, cx| {
        let locations = match request.await {
            Ok(locations) => locations,
            Err(error) => {
                this.update(cx, |w, cx| w.show_error(format!("{error:#}"), cx))
                    .ok();
                return;
            }
        };
        let Ok(previews) = this.update(cx, |w, cx| previews(w, locations, cx)) else {
            return;
        };
        let usages = previews.await;
        this.update_in(cx, |workspace, window, cx| {
            if in_panel && let Some(panel) = panel {
                panel.update(cx, |panel, cx| panel.set_usages(usages, cx));
            } else if usages.is_empty() {
                let message = still_busy(workspace, &buffer, cx)
                    .unwrap_or_else(|| format!("No usages of {name} found"));
                workspace.show_message(message, cx);
            } else if let [usage] = usages.as_slice() {
                open_location(workspace, usage.path.clone(), usage.start, true, window, cx);
            } else {
                let title = format!("Usages of {name} ({})", usages.len());
                show_usage_picker(workspace, &title, root, usages, window, cx);
            }
        })
        .ok();
    })
    .detach();
}

type StructureEntry = (PickerItem, lsp::Position, lsp::Range);

fn flatten_symbols(symbols: Vec<lsp::DocumentSymbol>, depth: usize, out: &mut Vec<StructureEntry>) {
    for symbol in symbols {
        out.push((
            PickerItem {
                label: symbol.name,
                detail: symbol
                    .detail
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                indent: depth,
            },
            symbol.selection_range.start,
            symbol.range,
        ));
        flatten_symbols(symbol.children.unwrap_or_default(), depth + 1, out);
    }
}

/// Cmd+F12: the file's structure, to jump to anything in it.
fn file_structure(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some((editor, buffer, offset)) = cursor(workspace, cx) else {
        return;
    };
    let request = workspace
        .language_servers()
        .read(cx)
        .document_symbols(&buffer, cx);
    cx.spawn_in(window, async move |this, cx| {
        let response = request.await;
        this.update_in(cx, |workspace, window, cx| {
            let mut entries: Vec<StructureEntry> = Vec::new();
            match response {
                Ok(Some(lsp::DocumentSymbolResponse::Nested(symbols))) => {
                    flatten_symbols(symbols, 0, &mut entries)
                }
                Ok(Some(lsp::DocumentSymbolResponse::Flat(symbols))) => {
                    for symbol in symbols {
                        entries.push((
                            PickerItem {
                                label: symbol.name,
                                detail: symbol.container_name.unwrap_or_default(),
                                indent: 0,
                            },
                            symbol.location.range.start,
                            symbol.location.range,
                        ));
                    }
                }
                Ok(None) => {}
                Err(error) => return workspace.show_error(format!("{error:#}"), cx),
            }
            if entries.is_empty() {
                return workspace.show_message("Nothing to show in this file's structure", cx);
            }
            // Start on the innermost symbol the cursor is in, or else the last one before it.
            let here = buffer.read(cx).buffer().offset_to_utf16(offset);
            let here = (here.line, here.character);
            let key = |p: lsp::Position| (p.line, p.character);
            let selected = entries
                .iter()
                .enumerate()
                .filter(|(_, (_, _, r))| key(r.start) <= here && here <= key(r.end))
                .max_by_key(|(_, (item, _, _))| item.indent)
                .map(|(i, _)| i)
                .or_else(|| entries.iter().rposition(|(_, p, _)| key(*p) <= here))
                .unwrap_or(0);
            let (items, positions): (Vec<_>, Vec<_>) =
                entries.into_iter().map(|(item, p, _)| (item, p)).unzip();
            let name = buffer
                .read(cx)
                .path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let handle = cx.weak_entity();
            workspace.toggle_modal(window, cx, move |_, cx| {
                Picker::new(
                    name,
                    items,
                    selected,
                    move |index, _, cx| {
                        let position = positions[index];
                        handle
                            .update(cx, |workspace, cx| workspace.record_position(cx))
                            .ok();
                        editor.update(cx, |editor, cx| {
                            let point = {
                                let text = editor.buffer().read(cx).buffer();
                                text.offset_to_point(text.utf16_to_offset(convert::utf16(position)))
                            };
                            editor.jump_to(point.line, point.col, cx);
                        });
                    },
                    cx,
                )
            });
        })
        .ok();
    })
    .detach();
}

/// Alt+Enter: fixes and refactorings the server offers here.
fn show_intentions(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some((editor, buffer, _)) = cursor(workspace, cx) else {
        return;
    };
    let range = editor.read(cx).selections().primary().range();
    let request = workspace
        .language_servers()
        .read(cx)
        .code_actions(&buffer, range, cx);
    cx.spawn_in(window, async move |this, cx| {
        let result = request.await;
        this.update_in(cx, |workspace, window, cx| {
            let actions = match result {
                Ok(actions) if actions.is_empty() => {
                    return workspace.show_message("No intentions here", cx);
                }
                Ok(actions) => actions,
                Err(error) => return workspace.show_error(format!("{error:#}"), cx),
            };
            // Preferred fixes first, as IntelliJ puts quick fixes above other intentions.
            let mut actions = actions;
            actions.sort_by_key(|a| match a {
                lsp::CodeActionOrCommand::CodeAction(a) if a.is_preferred == Some(true) => 0,
                lsp::CodeActionOrCommand::CodeAction(a)
                    if a.kind
                        .as_ref()
                        .is_some_and(|k| k.as_str().starts_with("quickfix")) =>
                {
                    1
                }
                _ => 2,
            });
            let items = actions
                .iter()
                .map(|a| match a {
                    lsp::CodeActionOrCommand::CodeAction(a) => PickerItem {
                        label: a.title.clone(),
                        ..Default::default()
                    },
                    lsp::CodeActionOrCommand::Command(c) => PickerItem {
                        label: c.title.clone(),
                        ..Default::default()
                    },
                })
                .collect();
            let handle = cx.weak_entity();
            workspace.toggle_modal(window, cx, move |_, cx| {
                Picker::new(
                    "Intentions",
                    items,
                    0,
                    move |index, _, cx| {
                        let action = actions[index].clone();
                        handle
                            .update(cx, |workspace, cx| {
                                run_code_action(workspace, buffer, action, cx)
                            })
                            .ok();
                    },
                    cx,
                )
            });
        })
        .ok();
    })
    .detach();
}

fn run_code_action(
    workspace: &mut Workspace,
    buffer: Entity<gleipnir_core::BufferModel>,
    action: lsp::CodeActionOrCommand,
    cx: &mut Context<Workspace>,
) {
    let servers = workspace.language_servers().clone();
    let action = match action {
        lsp::CodeActionOrCommand::Command(command) => {
            let run = servers.read(cx).execute_command(&buffer, command, cx);
            cx.spawn(async move |this, cx| {
                if let Err(error) = run.await {
                    this.update(cx, |w, cx| w.show_error(format!("{error:#}"), cx))
                        .ok();
                }
            })
            .detach();
            return;
        }
        lsp::CodeActionOrCommand::CodeAction(action) => action,
    };
    let resolve = servers.read(cx).resolve_code_action(&buffer, action, cx);
    cx.spawn(async move |this, cx| {
        let result: anyhow::Result<()> = async {
            let action = resolve.await?;
            if let Some(edit) = action.edit {
                this.update(cx, |w, cx| apply_workspace_edit(w, edit, cx))?
                    .await?;
            }
            if let Some(command) = action.command {
                servers
                    .read_with(cx, |servers, cx| {
                        servers.execute_command(&buffer, command, cx)
                    })
                    .await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            this.update(cx, |w, cx| w.show_error(format!("{error:#}"), cx))
                .ok();
        }
    })
    .detach();
}

/// Cmd+Alt+L: formats the file with its language's formatter, as one undo step.
fn reformat(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(buffer) = active_buffer(workspace, cx) else {
        return;
    };
    let request = workspace.language_servers().read(cx).format(&buffer, cx);
    cx.spawn_in(window, async move |this, cx| match request.await {
        Ok(edits) => {
            buffer.update(cx, |buffer, cx| {
                let edits = convert::text_edits(buffer.buffer(), edits);
                if !edits.is_empty() {
                    buffer.apply_external_edits(edits, cx);
                }
            });
        }
        Err(error) => {
            this.update(cx, |w, cx| w.show_error(format!("{error:#}"), cx))
                .ok();
        }
    })
    .detach();
}

/// The active file's server, in the status bar, when it's doing something or isn't working.
fn status_item(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> Option<gpui::AnyElement> {
    let buffer = active_buffer(workspace, cx)?;
    let (package, server) = LanguageServers::server_name_for(&buffer, cx)?;
    let theme = cx.global::<Theme>().clone();
    let (text, color) = match workspace.language_servers().read(cx).status(&package)? {
        ServerStatus::Starting => (format!("{server}: starting…"), theme.text_muted),
        ServerStatus::Installing => (format!("installing {server}…"), theme.text_muted),
        ServerStatus::Running { busy: Some(task) } => {
            (format!("{server}: {task}"), theme.text_muted)
        }
        ServerStatus::Running { busy: None } => (server.clone(), theme.text_faint),
        ServerStatus::Missing(_) => (format!("{server} isn't installed"), theme.warning),
        ServerStatus::Failed(_) => (format!("{server} stopped"), theme.error),
    };
    let tooltip = match workspace.language_servers().read(cx).status(&package)? {
        ServerStatus::Missing(why) | ServerStatus::Failed(why) => Some(why.clone()),
        _ => None,
    };
    Some(
        div()
            .id("language-server-status")
            .debug_selector(|| "language-server-status".into())
            .max_w(px(320.))
            .truncate()
            .text_color(color)
            .child(text)
            .when_some(tooltip, |d, why| {
                d.cursor_pointer()
                    .on_click(cx.listener(move |w, _, _, cx| w.show_error(why.clone(), cx)))
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests;
