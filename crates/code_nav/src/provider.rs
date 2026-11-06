//! Answers the editor's questions by asking the file's language server.

use crate::convert;
use anyhow::Result;
use gleipnir_core::{BufferModel, LanguageServers};
use gleipnir_editor::{
    CompletionItem, Completions, HoverInfo, SemanticsProvider, SignatureInfo, Triggers,
};
use gleipnir_lsp::lsp_types as lsp;
use gpui::{App, AppContext as _, Entity, Task};

pub struct LspSemantics {
    pub servers: Entity<LanguageServers>,
}

fn first_chars(strings: Option<&Vec<String>>) -> Vec<char> {
    strings
        .into_iter()
        .flatten()
        .filter_map(|s| s.chars().next())
        .collect()
}

/// Where the word ending at `offset` starts.
fn word_start(buffer: &gleipnir_text::Buffer, offset: usize) -> usize {
    let mut start = offset;
    while let Some(c) = buffer
        .char_before(start)
        .filter(|c| c.is_alphanumeric() || *c == '_')
    {
        start -= c.len_utf8();
    }
    start
}

impl SemanticsProvider for LspSemantics {
    fn triggers(&self, buffer: &Entity<BufferModel>, cx: &App) -> Triggers {
        let Some(capabilities) = self.servers.read(cx).capabilities_for(buffer, cx) else {
            return Triggers::default();
        };
        let completion = first_chars(
            capabilities
                .completion_provider
                .as_ref()
                .and_then(|c| c.trigger_characters.as_ref()),
        );
        let signature = capabilities
            .signature_help_provider
            .as_ref()
            .map(|s| {
                let mut chars = first_chars(s.trigger_characters.as_ref());
                chars.extend(first_chars(s.retrigger_characters.as_ref()));
                chars
            })
            .unwrap_or_default();
        Triggers {
            completion,
            signature,
        }
    }

    fn completions(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        trigger: Option<char>,
        cx: &mut App,
    ) -> Task<Result<Completions>> {
        let request = self
            .servers
            .read(cx)
            .completions(buffer, offset, trigger, cx);
        let buffer = buffer.clone();
        cx.spawn(async move |cx| -> Result<Completions> {
            let response = request.await?;
            Ok(cx.update(|cx| {
                let model = buffer.read(cx);
                let text = model.buffer();
                let start = word_start(text, offset.min(text.len()));
                let (items, incomplete) = match response {
                    lsp::CompletionResponse::Array(items) => (items, false),
                    lsp::CompletionResponse::List(list) => (list.items, list.is_incomplete),
                };
                let items = items
                    .into_iter()
                    .map(|item| convert::completion_item(item, text, start))
                    .collect();
                Completions { items, incomplete }
            }))
        })
    }

    fn resolve(
        &self,
        buffer: &Entity<BufferModel>,
        mut item: CompletionItem,
        cx: &mut App,
    ) -> Task<Result<CompletionItem>> {
        let Some(original) = item.data.downcast_ref::<lsp::CompletionItem>().cloned() else {
            return Task::ready(Ok(item));
        };
        let request = self
            .servers
            .read(cx)
            .resolve_completion(buffer, original, cx);
        let buffer = buffer.clone();
        cx.spawn(async move |cx| {
            let Ok(resolved) = request.await else {
                return Ok(item);
            };
            cx.update(|cx| {
                let text = buffer.read(cx).buffer();
                if let Some(edits) = resolved
                    .additional_text_edits
                    .clone()
                    .filter(|e| !e.is_empty())
                {
                    item.additional_edits = convert::text_edits(text, edits);
                }
                if let Some(doc) = convert::documentation(resolved.documentation.clone()) {
                    item.documentation = Some(doc);
                }
            });
            Ok(item)
        })
    }

    fn hover(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Result<Option<HoverInfo>>> {
        let request = self.servers.read(cx).hover(buffer, offset, cx);
        cx.background_spawn(async move {
            Ok(request
                .await?
                .map(|h| HoverInfo {
                    markdown: convert::hover_markdown(h),
                })
                .filter(|h| !h.markdown.trim().is_empty()))
        })
    }

    fn signature_help(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Result<Option<SignatureInfo>>> {
        let request = self.servers.read(cx).signature_help(buffer, offset, cx);
        cx.background_spawn(async move { Ok(request.await?.and_then(convert::signature_info)) })
    }

    fn document_highlights(
        &self,
        buffer: &Entity<BufferModel>,
        offset: usize,
        cx: &mut App,
    ) -> Task<Result<Vec<std::ops::Range<usize>>>> {
        let request = self
            .servers
            .read(cx)
            .document_highlights(buffer, offset, cx);
        let buffer = buffer.clone();
        cx.spawn(async move |cx| {
            let ranges = request.await?;
            Ok(buffer.read_with(cx, |model, _| {
                ranges
                    .into_iter()
                    .map(|r| gleipnir_core::range_to_offsets(model.buffer(), &r))
                    .collect()
            }))
        })
    }

    fn inlay_hints(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &mut App,
    ) -> Task<Result<Vec<gleipnir_editor::InlayHint>>> {
        let request = self.servers.read(cx).inlay_hints(buffer, cx);
        let buffer = buffer.clone();
        cx.spawn(async move |cx| {
            let hints = request.await?;
            Ok(buffer.read_with(cx, |model, _| {
                hints
                    .into_iter()
                    .filter_map(|h| convert::inlay_hint(model.buffer(), h))
                    .collect()
            }))
        })
    }

    fn semantic_tokens(
        &self,
        buffer: &Entity<BufferModel>,
        cx: &mut App,
    ) -> Task<Result<gleipnir_editor::SemanticTokens>> {
        let request = self.servers.read(cx).semantic_tokens(buffer, cx);
        let buffer = buffer.clone();
        cx.spawn(async move |cx| {
            let (types, tokens) = request.await?;
            Ok(buffer.read_with(cx, |model, _| {
                convert::semantic_tokens(model.buffer(), &types, &tokens)
            }))
        })
    }
}
