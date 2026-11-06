//! Turns what language servers send into the editor's terms.

use gleipnir_core::range_to_offsets;
use gleipnir_editor::{CompletionItem, CompletionKind, SignatureInfo};
use gleipnir_lsp::lsp_types as lsp;
use gleipnir_text::{Buffer, TextEdit, Utf16Point};
use std::ops::Range;
use std::rc::Rc;

/// A snippet expanded to plain text, with where its first tab stop is.
#[derive(Debug, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    pub selection: Option<Range<usize>>,
}

/// Expands an LSP snippet. Placeholders keep their default text, choices their first option, and
/// variables their default or nothing. Only the first tab stop is kept, as `$1` or else `$0`.
pub fn parse_snippet(source: &str) -> Snippet {
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut text = String::new();
    let mut stops = Vec::new();
    parse_into(&chars, &mut i, &mut text, &mut stops, false);
    let first = stops
        .iter()
        .filter(|(n, _)| *n > 0)
        .min_by_key(|(n, _)| *n)
        .or_else(|| stops.iter().find(|(n, _)| *n == 0));
    Snippet {
        selection: first.map(|(_, r)| r.clone()),
        text,
    }
}

fn parse_into(
    src: &[char],
    i: &mut usize,
    out: &mut String,
    stops: &mut Vec<(u32, Range<usize>)>,
    nested: bool,
) {
    while *i < src.len() {
        let c = src[*i];
        match c {
            '\\' if src
                .get(*i + 1)
                .is_some_and(|n| matches!(n, '$' | '}' | '\\' | ',' | '|')) =>
            {
                out.push(src[*i + 1]);
                *i += 2;
            }
            '}' if nested => {
                *i += 1;
                return;
            }
            '$' => {
                *i += 1;
                parse_dollar(src, i, out, stops);
            }
            c => {
                out.push(c);
                *i += 1;
            }
        }
    }
}

fn read_number(src: &[char], i: &mut usize) -> Option<u32> {
    let start = *i;
    while src.get(*i).is_some_and(char::is_ascii_digit) {
        *i += 1;
    }
    src[start..*i].iter().collect::<String>().parse().ok()
}

fn read_name(src: &[char], i: &mut usize) -> String {
    let start = *i;
    while src
        .get(*i)
        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
    {
        *i += 1;
    }
    src[start..*i].iter().collect()
}

fn parse_dollar(
    src: &[char],
    i: &mut usize,
    out: &mut String,
    stops: &mut Vec<(u32, Range<usize>)>,
) {
    match src.get(*i) {
        Some(c) if c.is_ascii_digit() => {
            let n = read_number(src, i).unwrap_or(0);
            stops.push((n, out.len()..out.len()));
        }
        Some('{') => {
            *i += 1;
            let start = out.len();
            if src.get(*i).is_some_and(char::is_ascii_digit) {
                let n = read_number(src, i).unwrap_or(0);
                match src.get(*i) {
                    Some(':') => {
                        *i += 1;
                        parse_into(src, i, out, stops, true);
                    }
                    Some('|') => {
                        *i += 1;
                        let mut choice = String::new();
                        let mut first = true;
                        while let Some(&c) = src.get(*i) {
                            *i += 1;
                            match c {
                                '\\' => {
                                    if let Some(&n) = src.get(*i) {
                                        *i += 1;
                                        if first {
                                            choice.push(n);
                                        }
                                    }
                                }
                                ',' => first = false,
                                '|' if src.get(*i) == Some(&'}') => {
                                    *i += 1;
                                    break;
                                }
                                c if first => choice.push(c),
                                _ => {}
                            }
                        }
                        out.push_str(&choice);
                    }
                    Some('}') => *i += 1,
                    _ => skip_to_close(src, i),
                }
                stops.push((n, start..out.len()));
            } else {
                // A variable: its default, or nothing, as there's no selection or clipboard here.
                read_name(src, i);
                match src.get(*i) {
                    Some(':') => {
                        *i += 1;
                        parse_into(src, i, out, stops, true);
                    }
                    Some('}') => *i += 1,
                    _ => skip_to_close(src, i),
                }
            }
        }
        Some(c) if c.is_alphabetic() || *c == '_' => {
            read_name(src, i);
        }
        _ => out.push('$'),
    }
}

/// Skips a transform like `${1/(.*)/${1:/upcase}/}`, minding nested braces.
fn skip_to_close(src: &[char], i: &mut usize) {
    let mut depth = 1;
    while let Some(&c) = src.get(*i) {
        *i += 1;
        match c {
            '\\' => *i += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ => {}
        }
    }
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

pub fn text_edits(
    buffer: &Buffer,
    edits: impl IntoIterator<Item = lsp::TextEdit>,
) -> Vec<TextEdit> {
    let mut edits: Vec<TextEdit> = edits
        .into_iter()
        .map(|e| TextEdit::new(range_to_offsets(buffer, &e.range), normalize(&e.new_text)))
        .collect();
    edits.sort_by_key(|e| e.range.start);
    edits
}

fn kind(kind: Option<lsp::CompletionItemKind>) -> CompletionKind {
    use lsp::CompletionItemKind as K;
    match kind {
        Some(K::FUNCTION | K::CONSTRUCTOR) => CompletionKind::Function,
        Some(K::METHOD) => CompletionKind::Method,
        Some(K::FIELD | K::PROPERTY | K::ENUM_MEMBER) => CompletionKind::Field,
        Some(K::VARIABLE | K::VALUE | K::REFERENCE) => CompletionKind::Variable,
        Some(K::CLASS | K::INTERFACE | K::STRUCT | K::ENUM | K::TYPE_PARAMETER) => {
            CompletionKind::Type
        }
        Some(K::KEYWORD | K::OPERATOR) => CompletionKind::Keyword,
        Some(K::MODULE | K::FILE | K::FOLDER) => CompletionKind::Module,
        Some(K::CONSTANT) => CompletionKind::Constant,
        Some(K::SNIPPET) => CompletionKind::Snippet,
        _ => CompletionKind::Other,
    }
}

pub fn documentation(doc: Option<lsp::Documentation>) -> Option<String> {
    match doc? {
        lsp::Documentation::String(s) => Some(s),
        lsp::Documentation::MarkupContent(m) if m.kind == lsp::MarkupKind::PlainText => {
            Some(escape_markdown(&m.value))
        }
        lsp::Documentation::MarkupContent(m) => Some(m.value),
    }
    .filter(|s| !s.trim().is_empty())
}

fn escape_markdown(text: &str) -> String {
    // Plain text keeps its line breaks as paragraphs.
    text.lines().collect::<Vec<_>>().join("\n\n")
}

/// Converts one item from a completion list. `word_start` is where the typed word starts, for items
/// that don't say what they replace.
pub fn completion_item(
    item: lsp::CompletionItem,
    buffer: &Buffer,
    word_start: usize,
) -> CompletionItem {
    let snippet = item.insert_text_format == Some(lsp::InsertTextFormat::SNIPPET);
    let (range, raw_text) = match &item.text_edit {
        Some(lsp::CompletionTextEdit::Edit(edit)) => (Some(edit.range), edit.new_text.clone()),
        Some(lsp::CompletionTextEdit::InsertAndReplace(edit)) => {
            (Some(edit.insert), edit.new_text.clone())
        }
        None => (
            None,
            item.insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone()),
        ),
    };
    let replace_start = range.map_or(word_start, |r| range_to_offsets(buffer, &r).start);
    let (new_text, selection) = if snippet {
        let parsed = parse_snippet(&normalize(&raw_text));
        (parsed.text, parsed.selection)
    } else {
        (normalize(&raw_text), None)
    };
    let detail = {
        let signature = item.label_details.as_ref().and_then(|d| d.detail.clone());
        let description = item.detail.clone().or_else(|| {
            item.label_details
                .as_ref()
                .and_then(|d| d.description.clone())
        });
        match (signature, description) {
            (Some(a), Some(b)) => Some(format!("{a}  {b}")),
            (a, b) => a.or(b),
        }
        .map(|d| d.lines().next().unwrap_or_default().to_string())
    };
    CompletionItem {
        filter_text: item
            .filter_text
            .clone()
            .unwrap_or_else(|| item.label.clone()),
        sort_text: item.sort_text.clone().unwrap_or_else(|| item.label.clone()),
        label: item.label.clone(),
        detail,
        kind: kind(item.kind),
        replace_start,
        new_text,
        selection,
        additional_edits: text_edits(
            buffer,
            item.additional_text_edits.clone().unwrap_or_default(),
        ),
        documentation: documentation(item.documentation.clone()),
        data: Rc::new(item),
    }
}

pub fn hover_markdown(hover: lsp::Hover) -> String {
    fn marked(s: lsp::MarkedString) -> String {
        match s {
            lsp::MarkedString::String(s) => s,
            lsp::MarkedString::LanguageString(l) => format!("```{}\n{}\n```", l.language, l.value),
        }
    }
    match hover.contents {
        lsp::HoverContents::Scalar(s) => marked(s),
        lsp::HoverContents::Array(parts) => parts
            .into_iter()
            .map(marked)
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
        lsp::HoverContents::Markup(m) if m.kind == lsp::MarkupKind::PlainText => {
            escape_markdown(&m.value)
        }
        lsp::HoverContents::Markup(m) => m.value,
    }
}

/// UTF-16 offsets into a label, as servers send for parameters, to byte offsets.
fn utf16_range(label: &str, start: u32, end: u32) -> Option<Range<usize>> {
    let mut units = 0;
    let (mut from, mut to) = (None, None);
    for (byte, c) in label
        .char_indices()
        .chain(std::iter::once((label.len(), '\0')))
    {
        if units == start as usize {
            from = Some(byte);
        }
        if units == end as usize {
            to = Some(byte);
            break;
        }
        units += c.len_utf16();
    }
    Some(from?..to?)
}

pub fn signature_info(help: lsp::SignatureHelp) -> Option<SignatureInfo> {
    let index = help.active_signature.unwrap_or(0) as usize;
    let signature = help
        .signatures
        .get(index)
        .or(help.signatures.first())?
        .clone();
    let active = signature
        .active_parameter
        .or(help.active_parameter)
        .unwrap_or(0) as usize;
    let active_parameter = signature
        .parameters
        .as_ref()
        .and_then(|p| p.get(active))
        .and_then(|p| match &p.label {
            lsp::ParameterLabel::LabelOffsets([start, end]) => {
                utf16_range(&signature.label, *start, *end)
            }
            lsp::ParameterLabel::Simple(name) => {
                // The name's first whole-word match after the opening parenthesis.
                let open = signature.label.find('(').map_or(0, |i| i + 1);
                signature.label[open..]
                    .find(name.as_str())
                    .map(|i| open + i..open + i + name.len())
            }
        });
    Some(SignatureInfo {
        label: signature.label.clone(),
        active_parameter,
        documentation: documentation(signature.documentation.clone()),
    })
}

pub fn utf16(position: lsp::Position) -> Utf16Point {
    Utf16Point::new(position.line, position.character)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(s: &str) -> (String, Option<Range<usize>>) {
        let parsed = parse_snippet(s);
        (parsed.text, parsed.selection)
    }

    #[test]
    fn snippets_expand_with_the_first_tab_stop_selected() {
        assert_eq!(snippet("area($0)"), ("area()".into(), Some(5..5)));
        assert_eq!(
            snippet("clamp(${1:lo}, ${2:hi})$0"),
            ("clamp(lo, hi)".into(), Some(6..8))
        );
        assert_eq!(
            snippet("for ${2:x} in ${1:xs} {\n\t$0\n}"),
            ("for x in xs {\n\t\n}".into(), Some(9..11))
        );
        assert_eq!(snippet("plain"), ("plain".into(), None));
    }

    #[test]
    fn snippets_handle_nesting_choices_variables_and_escapes() {
        assert_eq!(
            snippet("${1:outer ${2:inner}}"),
            ("outer inner".into(), Some(0..11))
        );
        assert_eq!(
            snippet("${1|public,private|} x"),
            ("public x".into(), Some(0..6))
        );
        assert_eq!(snippet("${TM_SELECTED_TEXT}${1}"), ("".into(), Some(0..0)));
        assert_eq!(snippet("${name:default}"), ("default".into(), None));
        assert_eq!(snippet("cost \\$5 \\} $"), ("cost $5 } $".into(), None));
        assert_eq!(
            snippet("${1/(.*)/${1:/upcase}/}x"),
            ("x".into(), Some(0..0))
        );
    }

    fn item(json: serde_json::Value) -> lsp::CompletionItem {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_completion_replaces_its_edit_range_and_carries_its_imports() {
        let buffer = Buffer::new("fn main() {\n    let x = ar\n}");
        let word_start = buffer.text().find("ar").unwrap();
        let converted = completion_item(
            item(serde_json::json!({
                "label": "area",
                "kind": 3,
                "labelDetails": { "detail": "(w: f64)" },
                "detail": "fn area(w: f64) -> f64",
                "insertTextFormat": 2,
                "textEdit": { "range": { "start": {"line": 1, "character": 12}, "end": {"line": 1, "character": 14} }, "newText": "area(${1:w})" },
                "additionalTextEdits": [{ "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0} }, "newText": "use geo::area;\r\n" }]
            })),
            &buffer,
            0,
        );
        assert_eq!(converted.replace_start, word_start);
        assert_eq!(converted.new_text, "area(w)");
        assert_eq!(converted.selection, Some(5..6));
        assert_eq!(
            converted.detail.as_deref(),
            Some("(w: f64)  fn area(w: f64) -> f64")
        );
        assert_eq!(converted.kind, CompletionKind::Function);
        assert_eq!(
            converted.additional_edits,
            [TextEdit::new(0..0, "use geo::area;\n")]
        );
        assert!(
            converted
                .data
                .downcast_ref::<lsp::CompletionItem>()
                .is_some()
        );
    }

    #[test]
    fn without_an_edit_a_completion_replaces_the_typed_word() {
        let buffer = Buffer::new("le");
        let converted = completion_item(
            item(serde_json::json!({ "label": "let", "insertText": "let " })),
            &buffer,
            0,
        );
        assert_eq!(
            (
                converted.replace_start,
                converted.new_text.as_str(),
                converted.selection
            ),
            (0, "let ", None)
        );
    }

    #[test]
    fn hover_contents_of_every_shape_become_markdown() {
        let hover = |contents: serde_json::Value| {
            hover_markdown(
                serde_json::from_value(serde_json::json!({ "contents": contents })).unwrap(),
            )
        };
        assert_eq!(
            hover(serde_json::json!({"kind": "markdown", "value": "**x**"})),
            "**x**"
        );
        assert_eq!(
            hover(serde_json::json!({"language": "go", "value": "func F()"})),
            "```go\nfunc F()\n```"
        );
        assert_eq!(
            hover(serde_json::json!(["a", {"language": "ts", "value": "b"}])),
            "a\n\n---\n\n```ts\nb\n```"
        );
        assert_eq!(
            hover(serde_json::json!({"kind": "plaintext", "value": "one\ntwo"})),
            "one\n\ntwo"
        );
    }

    #[test]
    fn the_active_parameter_is_found_by_offsets_or_by_name() {
        let help: lsp::SignatureHelp = serde_json::from_value(serde_json::json!({
            "signatures": [{ "label": "f(é: i32, b: i32)", "parameters": [{ "label": [2, 8] }, { "label": [10, 16] }] }],
            "activeParameter": 1
        }))
        .unwrap();
        let info = signature_info(help).unwrap();
        assert_eq!(&info.label[info.active_parameter.unwrap()], "b: i32");
        let help: lsp::SignatureHelp = serde_json::from_value(serde_json::json!({
            "signatures": [{ "label": "f(a int, b int)", "parameters": [{ "label": "a int" }, { "label": "b int" }], "activeParameter": 0 }]
        }))
        .unwrap();
        let info = signature_info(help).unwrap();
        assert_eq!(&info.label[info.active_parameter.unwrap()], "a int");
    }
}
