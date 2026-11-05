//! Maps highlight query captures onto a small set of colors' worth of categories.

use crate::Language;
use gleipnir_text::Snapshot;
use regex::Regex;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, QueryPredicateArg, Tree};

/// What the theme needs to know about a piece of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Attribute,
    Comment,
    Constant,
    Function,
    Keyword,
    Label,
    Macro,
    Namespace,
    Number,
    Operator,
    Parameter,
    Property,
    Punctuation,
    String,
    StringEscape,
    Tag,
    Type,
    Variable,
    VariableBuiltin,
}

impl HighlightKind {
    /// Interprets a capture name. Grammars disagree on naming: Odin's query comes from Neovim and
    /// says `@repeat`, where others say `@keyword`. `None` leaves text in the default color.
    fn from_capture(name: &str) -> Option<Self> {
        Some(match name {
            "attribute" | "preproc" => Self::Attribute,
            "comment" | "comment.documentation" => Self::Comment,
            "constant" | "constant.builtin" | "boolean" => Self::Constant,
            "constructor" | "type" | "type.builtin" => Self::Type,
            "escape" | "string.escape" => Self::StringEscape,
            "field" | "property" => Self::Property,
            "float" | "number" => Self::Number,
            "function.macro" => Self::Macro,
            "function" | "function.builtin" | "function.call" | "function.method" => Self::Function,
            "keyword"
            | "keyword.function"
            | "keyword.operator"
            | "keyword.return"
            | "conditional"
            | "conditional.ternary"
            | "include"
            | "repeat"
            | "storageclass" => Self::Keyword,
            "label" => Self::Label,
            "namespace" | "module" => Self::Namespace,
            "operator" => Self::Operator,
            "parameter" | "variable.parameter" => Self::Parameter,
            "punctuation"
            | "punctuation.bracket"
            | "punctuation.delimiter"
            | "punctuation.special" => Self::Punctuation,
            "character" | "string" | "string.special" => Self::String,
            "tag" => Self::Tag,
            "variable" => Self::Variable,
            "variable.builtin" => Self::VariableBuiltin,
            _ => return None,
        })
    }
}

/// A run of text with one highlight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub kind: HighlightKind,
}

/// A compiled query with what's needed to interpret its results.
struct LanguageQuery {
    query: Query,
    kinds: Vec<Option<HighlightKind>>,
    /// Patterns that only apply if a capture matches a regex, per pattern index. These come from
    /// `#lua-match?`, which tree-sitter doesn't evaluate itself.
    extra_predicates: HashMap<usize, Vec<(u32, Regex)>>,
}

/// Corrections to the grammar's own query. Its all-caps constant pattern has a stray quote in the
/// regex so it never matches, and it colors number literals as builtin constants.
const RUST_OVERRIDES: &str = r#"
((identifier) @constant (#match? @constant "^[A-Z][A-Z0-9_]+$"))
(integer_literal) @number
(float_literal) @number
"#;

/// On a node captured by several patterns, which one applies. The query files were written for
/// different editors and disagree: Go's ends with a catch-all `(identifier) @variable`, which only
/// works if earlier patterns win, while the others start with theirs and refine it later.
fn first_pattern_wins(language: Language) -> bool {
    language == Language::Go
}

fn query_source(language: Language) -> String {
    let js = tree_sitter_javascript::HIGHLIGHT_QUERY;
    let jsx = tree_sitter_javascript::JSX_HIGHLIGHT_QUERY;
    let ts = tree_sitter_typescript::HIGHLIGHTS_QUERY;
    match language {
        Language::Rust => format!("{}\n{RUST_OVERRIDES}", tree_sitter_rust::HIGHLIGHTS_QUERY),
        Language::Go => tree_sitter_go::HIGHLIGHTS_QUERY.to_string(),
        Language::Odin => tree_sitter_odin::HIGHLIGHTS_QUERY.to_string(),
        // The TypeScript grammars extend JavaScript's and only ship their additions. Later
        // patterns take precedence, so the additions go last.
        Language::TypeScript => format!("{js}\n{ts}"),
        Language::Tsx => format!("{js}\n{jsx}\n{ts}"),
        Language::JavaScript => format!("{js}\n{jsx}"),
    }
}

fn compile(language: Language) -> LanguageQuery {
    let query = Query::new(&language.grammar(), &query_source(language))
        .unwrap_or_else(|e| panic!("{language:?} highlight query: {e}"));
    let kinds = query
        .capture_names()
        .iter()
        .map(|n| HighlightKind::from_capture(n))
        .collect();

    let mut extra_predicates: HashMap<usize, Vec<(u32, Regex)>> = HashMap::new();
    for pattern in 0..query.pattern_count() {
        for predicate in query.general_predicates(pattern) {
            if &*predicate.operator != "lua-match?" {
                continue;
            }
            // The Lua patterns in these queries only use syntax that means the same in a regex.
            if let [
                QueryPredicateArg::Capture(capture),
                QueryPredicateArg::String(pattern_text),
            ] = &*predicate.args
                && let Ok(regex) = Regex::new(pattern_text)
            {
                extra_predicates
                    .entry(pattern)
                    .or_default()
                    .push((*capture, regex));
            }
        }
    }
    LanguageQuery {
        query,
        kinds,
        extra_predicates,
    }
}

fn language_query(language: Language) -> &'static LanguageQuery {
    static QUERIES: OnceLock<HashMap<Language, LanguageQuery>> = OnceLock::new();
    QUERIES
        .get_or_init(|| Language::ALL.into_iter().map(|l| (l, compile(l))).collect())
        .get(&language)
        .expect("every language has a query")
}

/// Compiles every query up front. This takes tens of milliseconds, so run it on a background
/// thread at startup, rather than letting the first frame with highlighting pay for it.
pub fn precompile_queries() {
    language_query(Language::Rust);
}

/// Computes the highlights in a byte range, as sorted, non-overlapping spans.
///
/// Where captures overlap, the innermost node wins. On the same node, see `first_pattern_wins`.
pub fn highlights(
    language: Language,
    tree: &Tree,
    snapshot: &Snapshot,
    range: Range<usize>,
) -> Vec<HighlightSpan> {
    let range = range.start.min(snapshot.len())..range.end.min(snapshot.len());
    if range.is_empty() {
        return Vec::new();
    }
    let lq = language_query(language);
    let text = |node: Node<'_>| {
        let node_range = node.byte_range();
        // A tree with edits applied but not reparsed can briefly point past the text.
        let end = node_range.end.min(snapshot.len());
        let start = node_range.start.min(end);
        std::iter::once(snapshot.slice(start..end).into_bytes())
    };

    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut found: Vec<(Range<usize>, usize, HighlightKind)> = Vec::new();
    let mut captures = cursor.captures(&lq.query, tree.root_node(), text);
    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        let pattern = query_match.pattern_index;
        let Some(kind) = lq.kinds[capture.index as usize] else {
            continue;
        };
        if let Some(predicates) = lq.extra_predicates.get(&pattern) {
            let passes = predicates.iter().all(|(index, regex)| {
                query_match
                    .captures
                    .iter()
                    .filter(|c| c.index == *index)
                    .all(|c| regex.is_match(&snapshot.slice(c.node.byte_range())))
            });
            if !passes {
                continue;
            }
        }
        found.push((capture.node.byte_range(), pattern, kind));
    }
    flatten(found, range, first_pattern_wins(language))
}

/// Paints captures over the range in increasing priority, so the one that should show goes last.
fn flatten(
    mut found: Vec<(Range<usize>, usize, HighlightKind)>,
    range: Range<usize>,
    first_wins: bool,
) -> Vec<HighlightSpan> {
    found.sort_by_key(|(r, pattern, _)| {
        let order = if first_wins {
            usize::MAX - *pattern
        } else {
            *pattern
        };
        (std::cmp::Reverse(r.len()), order)
    });
    let mut painted: Vec<Option<HighlightKind>> = vec![None; range.len()];
    for (r, _, kind) in &found {
        let start = r.start.max(range.start) - range.start;
        let end = r.end.min(range.end).saturating_sub(range.start);
        if start < end {
            painted[start..end].fill(Some(*kind));
        }
    }

    let mut spans: Vec<HighlightSpan> = Vec::new();
    let mut at = 0;
    while at < painted.len() {
        let kind = painted[at];
        let mut end = at + 1;
        while end < painted.len() && painted[end] == kind {
            end += 1;
        }
        if let Some(kind) = kind {
            spans.push(HighlightSpan {
                range: range.start + at..range.start + end,
                kind,
            });
        }
        at = end;
    }
    spans
}
