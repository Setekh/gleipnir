//! Languages and syntax trees: parsing, highlighting and structural selection.
//!
//! Parsing is split so it can run off the UI thread. `Syntax` keeps the last tree and records
//! edits made since, `ParseJob` is a self-contained unit of work for a background thread, and
//! `Syntax::install` folds the result back in. Until then, highlighting uses the old tree with
//! the edits applied, which is close enough for a frame or two, so a large file can't stall typing.

mod highlight;
mod symbols;

pub use highlight::{HighlightKind, HighlightSpan, highlights, precompile_queries};

/// Highlights a whole text at once, parsing it here: for text that isn't being edited, like a
/// version of a file shown in a diff.
pub fn highlight_text(language: Language, text: &str) -> Vec<HighlightSpan> {
    let snapshot = gleipnir_text::Buffer::new(text).snapshot();
    let syntax = Syntax::new(language);
    let Some(parsed) = syntax.start_parse(snapshot.clone()).run() else {
        return Vec::new();
    };
    highlights(language, &parsed.tree, &snapshot, 0..snapshot.len())
}
pub use symbols::{Symbol, SymbolKind, extract_symbols};

use gleipnir_text::{AppliedEdit, Point as TextPoint, Snapshot};
use std::ops::Range;
use tree_sitter::{InputEdit, Language as Grammar, Parser, Point, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Go,
    Odin,
    TypeScript,
    Tsx,
    JavaScript,
    CSharp,
    Json,
    Html,
    Css,
    Markdown,
}

impl Language {
    pub const ALL: [Language; 11] = [
        Language::Rust,
        Language::Go,
        Language::Odin,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::CSharp,
        Language::Json,
        Language::Html,
        Language::Css,
        Language::Markdown,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Go => "Go",
            Self::Odin => "Odin",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::JavaScript => "JavaScript",
            Self::CSharp => "C#",
            Self::Json => "JSON",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Markdown => "Markdown",
        }
    }

    pub fn grammar(self) -> Grammar {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Odin => tree_sitter_odin::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Html => tree_sitter_html::LANGUAGE.into(),
            Self::Css => tree_sitter_css::LANGUAGE.into(),
            // Markdown's block structure. The text inside blocks has a grammar of its own, which
            // highlighting runs over each block's text in view.
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
        }
    }
}

/// A unit of parsing work, safe to run on any thread.
pub struct ParseJob {
    language: Language,
    old_tree: Option<Tree>,
    snapshot: Snapshot,
}

/// The output of a `ParseJob`.
pub struct ParsedTree {
    tree: Tree,
    version: u64,
}

impl ParseJob {
    pub fn run(self) -> Option<ParsedTree> {
        let mut parser = Parser::new();
        parser.set_language(&self.language.grammar()).ok()?;
        let snapshot = &self.snapshot;
        let mut read = |byte: usize, _: Point| -> &[u8] {
            let (chunk, start) = snapshot.chunk_at(byte);
            &chunk.as_bytes()[byte.saturating_sub(start).min(chunk.len())..]
        };
        let tree = parser.parse_with_options(&mut read, self.old_tree.as_ref(), None)?;
        Some(ParsedTree {
            tree,
            version: snapshot.version(),
        })
    }
}

/// A buffer's syntax state.
pub struct Syntax {
    language: Language,
    tree: Option<Tree>,
    /// The document version `tree` was parsed from. Not changed by applying edits to the tree.
    tree_version: Option<u64>,
    /// Edits since the job in flight started, tagged with the version each one produced.
    pending: Vec<(u64, InputEdit)>,
    /// Results for document versions before this are for text that has been replaced wholesale.
    min_version: u64,
    /// Bumped whenever the tree changes, including by edits, to invalidate highlight caches.
    generation: u64,
}

impl Syntax {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            tree: None,
            tree_version: None,
            pending: Vec::new(),
            min_version: 0,
            generation: 0,
        }
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Records edits, which must be given in the order they were applied. `version_after` is the
    /// buffer's version once the last of them was applied.
    pub fn edit(&mut self, edits: &[AppliedEdit], version_after: u64) {
        let first_version = version_after + 1 - edits.len() as u64;
        for (index, edit) in edits.iter().enumerate() {
            let input = input_edit(edit);
            if let Some(tree) = &mut self.tree {
                tree.edit(&input);
            }
            self.pending.push((first_version + index as u64, input));
        }
        self.generation += 1;
    }

    /// Whether the tree is behind the given document version.
    pub fn needs_parse(&self, version: u64) -> bool {
        self.tree_version != Some(version)
    }

    pub fn start_parse(&self, snapshot: Snapshot) -> ParseJob {
        ParseJob {
            language: self.language,
            old_tree: self.tree.clone(),
            snapshot,
        }
    }

    /// Folds in a finished parse. Edits made while it ran are applied to the new tree so its
    /// positions are current. Results older than what's already installed are dropped.
    pub fn install(&mut self, parsed: ParsedTree) {
        let superseded = self
            .tree_version
            .is_some_and(|current| current > parsed.version);
        if superseded || parsed.version < self.min_version {
            return;
        }
        let mut tree = parsed.tree;
        self.pending
            .retain(|(version, _)| *version > parsed.version);
        for (_, edit) in &self.pending {
            tree.edit(edit);
        }
        self.tree = Some(tree);
        self.tree_version = Some(parsed.version);
        self.generation += 1;
    }

    /// Discards everything, for when a reload replaces the whole document. `version` is the
    /// document's version after the replacement; a parse of anything older is now meaningless.
    pub fn reset(&mut self, version: u64) {
        self.min_version = version;
        self.tree = None;
        self.tree_version = None;
        self.pending.clear();
        self.generation += 1;
    }

    /// The innermost function, method, procedure or closure around `offset`.
    pub fn enclosing_function(&self, offset: usize) -> Option<Range<usize>> {
        const FUNCTIONS: &[&str] = &[
            "function_item",
            "closure_expression",
            "function_declaration",
            "method_declaration",
            "func_literal",
            "procedure_declaration",
            "procedure",
            "method_definition",
            "arrow_function",
            "function_expression",
            "function",
            "generator_function_declaration",
            "constructor_declaration",
            "local_function_statement",
            "lambda_expression",
            "anonymous_method_expression",
        ];
        let tree = self.tree.as_ref()?;
        let mut node = tree.root_node().descendant_for_byte_range(offset, offset)?;
        loop {
            if FUNCTIONS.contains(&node.kind()) {
                return Some(node.byte_range());
            }
            node = node.parent()?;
        }
    }

    /// Every plain name in a range, as written: variables and parameters, not fields after a dot
    /// or names in strings and comments.
    pub fn identifiers(&self, range: Range<usize>) -> Vec<Range<usize>> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut cursor = tree.walk();
        let mut descend = true;
        loop {
            let node = cursor.node();
            let node_range = node.byte_range();
            let overlaps = node_range.start < range.end && node_range.end > range.start;
            if descend && overlaps {
                if matches!(node.kind(), "identifier" | "shorthand_property_identifier") {
                    found.push(node_range);
                } else if cursor.goto_first_child() {
                    continue;
                }
            }
            if cursor.goto_next_sibling() {
                descend = true;
                continue;
            }
            loop {
                if !cursor.goto_parent() {
                    return found;
                }
                if cursor.goto_next_sibling() {
                    descend = true;
                    break;
                }
            }
        }
    }

    /// The next larger syntax node around `range`, which powers Extend Selection.
    pub fn expand_selection(&self, range: Range<usize>) -> Option<Range<usize>> {
        let tree = self.tree.as_ref()?;
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(range.start, range.end)?;
        loop {
            let node_range = node.byte_range();
            if node_range.start <= range.start && range.end <= node_range.end && node_range != range
            {
                return Some(node_range);
            }
            node = node.parent()?;
        }
    }
}

fn input_edit(edit: &AppliedEdit) -> InputEdit {
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: ts_point(edit.start),
        old_end_position: ts_point(edit.old_end),
        new_end_position: ts_point(edit.new_end),
    }
}

fn ts_point(point: TextPoint) -> Point {
    Point {
        row: point.line,
        column: point.col,
    }
}

#[cfg(test)]
mod tests;
