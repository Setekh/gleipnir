//! Applies a server's `WorkspaceEdit`, as renames and fixes produce: text edits in any number of
//! files, and files created, renamed and deleted.

use crate::convert;
use anyhow::{Context as _, Result};
use gleipnir_core::LanguageServers;
use gleipnir_lsp::lsp_types as lsp;
use gleipnir_lsp::uri_to_path;
use gleipnir_workspace::Workspace;
use gpui::{AsyncApp, Context, Entity, Task};
use std::path::PathBuf;

enum Op {
    Edit(PathBuf, Vec<lsp::TextEdit>),
    Create {
        path: PathBuf,
        overwrite: bool,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        overwrite: bool,
    },
    Delete {
        path: PathBuf,
        recursive: bool,
    },
}

fn path(uri: &lsp::Uri) -> Result<PathBuf> {
    uri_to_path(uri).with_context(|| format!("not a file: {}", uri.as_str()))
}

fn edits_of(edit: lsp::TextDocumentEdit) -> Result<Op> {
    let edits = edit
        .edits
        .into_iter()
        .map(|e| match e {
            lsp::OneOf::Left(e) => e,
            lsp::OneOf::Right(annotated) => annotated.text_edit,
        })
        .collect();
    Ok(Op::Edit(path(&edit.text_document.uri)?, edits))
}

fn operations(edit: lsp::WorkspaceEdit) -> Result<Vec<Op>> {
    let mut ops = Vec::new();
    match edit.document_changes {
        Some(lsp::DocumentChanges::Edits(edits)) => {
            for edit in edits {
                ops.push(edits_of(edit)?);
            }
        }
        Some(lsp::DocumentChanges::Operations(operations)) => {
            for operation in operations {
                ops.push(match operation {
                    lsp::DocumentChangeOperation::Edit(edit) => edits_of(edit)?,
                    lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Create(c)) => Op::Create {
                        path: path(&c.uri)?,
                        overwrite: c.options.and_then(|o| o.overwrite).unwrap_or(false),
                    },
                    lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Rename(r)) => Op::Rename {
                        from: path(&r.old_uri)?,
                        to: path(&r.new_uri)?,
                        overwrite: r.options.and_then(|o| o.overwrite).unwrap_or(false),
                    },
                    lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Delete(d)) => Op::Delete {
                        path: path(&d.uri)?,
                        recursive: d.options.and_then(|o| o.recursive).unwrap_or(false),
                    },
                });
            }
        }
        None => {
            let mut changes: Vec<_> = edit.changes.unwrap_or_default().into_iter().collect();
            changes.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            for (uri, edits) in changes {
                ops.push(Op::Edit(path(&uri)?, edits));
            }
        }
    }
    Ok(ops)
}

/// Applies the edit, returning how many files changed. Files open in the editor are edited there and
/// left for the user to save, as with their own typing. Files that weren't open are saved, as
/// nothing would show they'd changed otherwise.
pub fn apply_workspace_edit(
    workspace: &mut Workspace,
    edit: lsp::WorkspaceEdit,
    cx: &mut Context<Workspace>,
) -> Task<Result<usize>> {
    let store = workspace.store().clone();
    let servers = workspace.language_servers().clone();
    let ops = match operations(edit) {
        Ok(ops) => ops,
        Err(error) => return Task::ready(Err(error)),
    };
    cx.spawn(async move |_, cx| {
        let mut changed = std::collections::BTreeSet::new();
        let mut edited = Vec::new();
        for op in ops {
            match op {
                Op::Edit(path, edits) => {
                    edited.push(edit_file(&store, &servers, &path, edits, cx).await?);
                    changed.insert(path);
                }
                Op::Create { path, overwrite } => {
                    if overwrite || !path.exists() {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&path, "")?;
                    }
                    changed.insert(path);
                }
                Op::Rename {
                    from,
                    to,
                    overwrite,
                } => {
                    if to.exists() && !overwrite {
                        anyhow::bail!("{} already exists", to.display());
                    }
                    std::fs::rename(&from, &to)?;
                    store.update(cx, |store, cx| store.paths_moved(&from, &to, cx));
                    changed.insert(to);
                }
                Op::Delete { path, recursive } => {
                    if path.is_dir() && recursive {
                        std::fs::remove_dir_all(&path)?;
                    } else if path.is_dir() {
                        std::fs::remove_dir(&path)?;
                    } else {
                        std::fs::remove_file(&path)?;
                    }
                    changed.insert(path);
                }
            }
        }
        // One undo, from any of the files, takes back the whole change.
        cx.update(|cx| gleipnir_core::BufferModel::link_last_edits(edited, cx));
        Ok(changed.len())
    })
}

async fn edit_file(
    store: &Entity<gleipnir_core::BufferStore>,
    servers: &Entity<LanguageServers>,
    path: &std::path::Path,
    edits: Vec<lsp::TextEdit>,
    cx: &mut AsyncApp,
) -> Result<(Entity<gleipnir_core::BufferModel>, bool)> {
    let open = store.read_with(cx, |store, _| store.get(path));
    let was_open = open.is_some();
    let buffer = match open {
        Some(buffer) => buffer,
        None => store.update(cx, |store, cx| store.open(path, cx)).await?,
    };
    // The server hears about the change like any other, so it isn't working from stale text.
    servers.update(cx, |servers, cx| servers.register_buffer(&buffer, cx));
    buffer.update(cx, |buffer, cx| {
        let edits = convert::text_edits(buffer.buffer(), edits);
        buffer.apply_external_edits(edits, cx);
    });
    if !was_open {
        buffer.update(cx, |buffer, cx| buffer.save(cx)).await?;
    }
    Ok((buffer, !was_open))
}
