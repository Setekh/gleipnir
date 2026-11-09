//! Inline suggestions, as Copilot shows them, without anything leaving the machine: from the
//! file itself first, and from a small code model run locally by llama.cpp where there is one.

pub mod llama;
pub mod related;
pub mod rules;

use gleipnir_core::BufferModel;
use gleipnir_editor::{InlineSuggester, InlineSuggestion};
use gleipnir_workspace::Workspace;
use gpui::{App, Context, Entity, Task, WeakEntity, Window};
use std::rc::Rc;

/// Suggests from the file itself.
pub struct FileSuggester;

impl InlineSuggester for FileSuggester {
    fn suggest(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Option<InlineSuggestion>> {
        let settings = gleipnir_core::settings(cx).editor.clone();
        if !settings.inline_suggestions {
            return Task::ready(None);
        }
        let snapshot = buffer.read(cx).buffer().snapshot();
        let min_score = settings.suggestion_min_score;
        cx.background_executor().spawn(async move {
            let text = snapshot.slice(0..snapshot.len());
            let (suggestion, score) = rules::suggest(&text, offset)?;
            (score >= min_score).then(|| InlineSuggestion {
                offset,
                text: suggestion,
                score: Some(score),
                source: "this file".into(),
            })
        })
    }
}

/// How long typing must pause before the model's asked, so it isn't asked on every key.
const MODEL_PAUSE: std::time::Duration = std::time::Duration::from_millis(120);
/// A suggestion from the file this sure needn't wait for the model.
const SURE_ENOUGH: f32 = 0.8;

/// How many other open files, most recently used first, the model may be shown pieces of.
const RELATED_FILES: usize = 6;

/// Suggests from a small model run locally, starting it, and installing it where that's allowed,
/// the first time it's wanted. It's shown the code around the cursor, and related pieces of the
/// files open beside it.
pub struct ModelSuggester {
    workspace: WeakEntity<Workspace>,
}

impl ModelSuggester {
    /// The other files open in the workspace, most recently used first, as they are now.
    fn other_files(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &App,
    ) -> Vec<(String, gleipnir_text::Snapshot)> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        let workspace = workspace.read(cx);
        let this = buffer.read(cx).path().map(|p| p.to_path_buf());
        let root = workspace.root().map(|r| r.to_path_buf());
        let store = workspace.store().read(cx);
        workspace
            .recent_files()
            .iter()
            .filter(|path| Some(*path) != this.as_ref())
            .filter_map(|path| {
                let other = store.get(path)?;
                let name = root
                    .as_deref()
                    .and_then(|r| path.strip_prefix(r).ok())
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned();
                Some((name, other.read(cx).buffer().snapshot()))
            })
            .take(RELATED_FILES)
            .collect()
    }
}

impl InlineSuggester for ModelSuggester {
    fn suggest(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Option<InlineSuggestion>> {
        let settings = gleipnir_core::settings(cx).clone();
        if !settings.editor.inline_suggestions || !settings.editor.local_model {
            return Task::ready(None);
        }
        let model = llama::model_named(&settings.editor.suggestion_model);
        let install = settings.tools.install_automatically
            && std::env::var_os("GLEIPNIR_AUTO_INSTALL").is_none_or(|v| v != "0");
        let Some(port) = llama::ensure(model, install) else {
            return Task::ready(None);
        };
        let snapshot = buffer.read(cx).buffer().snapshot();
        let others = self.other_files(buffer, cx);
        let min_score = settings.editor.suggestion_min_score;
        let executor = cx.background_executor().clone();
        cx.background_executor().spawn(async move {
            // Dropped, as the next key replaces it, before it asks.
            executor.timer(MODEL_PAUSE).await;
            let text = snapshot.slice(0..snapshot.len());
            let others: Vec<related::OtherFile> = others
                .into_iter()
                .map(|(name, other)| related::OtherFile {
                    name,
                    text: other.slice(0..other.len()),
                })
                .collect();
            let related = related::related(&others, &text, offset);
            let (line, score) = match llama::complete(port, &text, offset, &related, min_score) {
                Ok(found) => found?,
                Err(_) => {
                    llama::server_gone();
                    return None;
                }
            };
            (score >= min_score).then(|| InlineSuggestion {
                offset,
                text: line,
                score: Some(score),
                source: model.name.into(),
            })
        })
    }
}

/// The file's suggestion, at once if it's sure, else whichever of it and the model's is likelier.
pub struct Suggesters {
    file: FileSuggester,
    model: ModelSuggester,
}

impl InlineSuggester for Suggesters {
    fn suggest(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Option<InlineSuggestion>> {
        let from_file = self.file.suggest(buffer, offset, cx);
        let from_model = self.model.suggest(buffer, offset, cx);
        cx.background_executor().spawn(async move {
            let file = from_file.await;
            if file
                .as_ref()
                .and_then(|s| s.score)
                .is_some_and(|s| s >= SURE_ENOUGH)
            {
                return file;
            }
            let model = from_model.await;
            match (file, model) {
                (Some(f), Some(m)) if m.score > f.score => Some(m),
                (Some(f), _) => Some(f),
                (None, m) => m,
            }
        })
    }
}

pub fn add_to_workspace(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) {
    let suggester: Rc<dyn InlineSuggester> = Rc::new(Suggesters {
        file: FileSuggester,
        model: ModelSuggester {
            workspace: workspace.weak_handle(),
        },
    });
    workspace.add_editor_hook(move |_, editor, _, cx| {
        let suggester = suggester.clone();
        editor.update(cx, |editor, _| editor.set_suggester(suggester));
        Vec::new()
    });
    // While the model downloads or starts, the status bar says so, kept current.
    workspace.add_status_item(|_, cx| {
        let theme = cx.global::<gleipnir_ui::Theme>().clone();
        let (text, color) = match llama::status() {
            llama::Status::Downloading(what, done) => (
                format!("Suggestions: downloading {what} {:.0}%", done * 100.),
                theme.text_muted,
            ),
            llama::Status::Starting => {
                ("Suggestions: starting the model…".into(), theme.text_muted)
            }
            llama::Status::Failed(why) => (format!("Suggestions: {why}"), theme.warning),
            llama::Status::Idle | llama::Status::Ready => return None,
        };
        use gpui::{IntoElement as _, ParentElement as _, Styled as _};
        Some(
            gpui::div()
                .max_w(gpui::px(360.))
                .truncate()
                .text_color(color)
                .child(text)
                .into_any_element(),
        )
    });
    // A setup that failed is tried again once the settings change, as after fixing them.
    cx.observe_global::<gleipnir_core::AppSettings>(|_, _| llama::reset())
        .detach();
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            let busy = matches!(
                llama::status(),
                llama::Status::Downloading(..) | llama::Status::Starting
            );
            if this
                .update(cx, |_, cx| {
                    if busy {
                        cx.notify()
                    }
                })
                .is_err()
            {
                return;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests;
