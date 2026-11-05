use super::*;
use gleipnir_text::{Buffer, Selection, SelectionSet, TextEdit, UndoGrouping};
use proptest::prelude::*;
use std::time::Instant;

fn parse(language: Language, source: &str) -> (Syntax, Buffer) {
    let buffer = Buffer::new(source);
    let mut syntax = Syntax::new(language);
    let parsed = syntax
        .start_parse(buffer.snapshot())
        .run()
        .expect("parse should succeed");
    syntax.install(parsed);
    (syntax, buffer)
}

/// The highlight of the first occurrence of `needle`.
fn kind_of(language: Language, source: &str, needle: &str) -> Option<HighlightKind> {
    let (syntax, buffer) = parse(language, source);
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in source"));
    let spans = highlights(
        language,
        syntax.tree().unwrap(),
        &buffer.snapshot(),
        0..source.len(),
    );
    spans
        .iter()
        .find(|s| s.range.start <= start && start < s.range.end)
        .map(|s| s.kind)
}

fn sample(language: Language) -> &'static str {
    match language {
        Language::Rust => {
            "struct P { x: i32 }\nfn main() { let p = P { x: 1 }; println!(\"{}\", p.x); }\n"
        }
        Language::Go => {
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n"
        }
        Language::Odin => {
            "package main\n\nimport \"core:fmt\"\n\nmain :: proc() {\n\tfmt.println(\"hi\")\n}\n"
        }
        Language::TypeScript => {
            "interface P { x: number }\nexport const f = (p: P): number => p.x;\n"
        }
        Language::Tsx => "const V = (p: { n: string }) => <div className=\"a\">{p.n}</div>;\n",
        Language::JavaScript => "export function f(a) { return <b>{a}</b>; }\n",
    }
}

#[test]
fn every_language_parses_real_code_without_errors() {
    for language in Language::ALL {
        let (syntax, _) = parse(language, sample(language));
        let root = syntax.tree().unwrap().root_node();
        assert!(!root.has_error(), "{language:?}: {}", root.to_sexp());
    }
}

#[test]
fn every_language_highlights_its_sample() {
    for language in Language::ALL {
        let source = sample(language);
        let (syntax, buffer) = parse(language, source);
        let spans = highlights(
            language,
            syntax.tree().unwrap(),
            &buffer.snapshot(),
            0..source.len(),
        );
        assert!(spans.len() > 3, "{language:?} produced only {spans:?}");
    }
}

#[test]
fn languages_are_detected_from_file_extensions() {
    for (path, expected) in [
        ("a/main.rs", Some(Language::Rust)),
        ("main.go", Some(Language::Go)),
        ("main.odin", Some(Language::Odin)),
        ("a.ts", Some(Language::TypeScript)),
        ("a.mts", Some(Language::TypeScript)),
        ("a.tsx", Some(Language::Tsx)),
        ("a.js", Some(Language::JavaScript)),
        ("a.jsx", Some(Language::JavaScript)),
        ("README.md", None),
        ("Makefile", None),
    ] {
        assert_eq!(Language::from_path(Path::new(path)), expected, "{path}");
    }
}

#[test]
fn go_and_odin_use_tabs_and_others_use_spaces() {
    assert!(Language::Go.edit_config().hard_tabs);
    assert!(Language::Odin.edit_config().hard_tabs);
    assert!(!Language::Rust.edit_config().hard_tabs);
    assert!(!Language::TypeScript.edit_config().hard_tabs);
}

#[test]
fn rust_does_not_auto_close_single_quotes() {
    let has_quote = |l: Language| l.edit_config().pairs.iter().any(|(o, _)| *o == '\'');
    assert!(!has_quote(Language::Rust));
    assert!(has_quote(Language::Go));
}

#[test]
fn rust_highlights_by_role() {
    let s = "// note\nstruct Point;\nfn run(a: i32) -> i32 { let v = \"s\"; helper!(); 7 }";
    let k = |needle| kind_of(Language::Rust, s, needle);
    assert_eq!(k("// note"), Some(HighlightKind::Comment));
    assert_eq!(k("struct"), Some(HighlightKind::Keyword));
    assert_eq!(k("Point"), Some(HighlightKind::Type));
    assert_eq!(k("run"), Some(HighlightKind::Function));
    assert_eq!(k("i32"), Some(HighlightKind::Type));
    assert_eq!(k("\"s\""), Some(HighlightKind::String));
    assert_eq!(k("helper"), Some(HighlightKind::Macro));
    assert_eq!(k("7"), Some(HighlightKind::Number));
}

#[test]
fn rust_all_caps_identifiers_are_constants_not_types() {
    // The grammar's own pattern for this has a typo. Ours has to win over its `^[A-Z]` type rule.
    let s = "fn f() { let a = MAX_SIZE; let b = Mixed; }";
    assert_eq!(
        kind_of(Language::Rust, s, "MAX_SIZE"),
        Some(HighlightKind::Constant)
    );
    assert_eq!(
        kind_of(Language::Rust, s, "Mixed"),
        Some(HighlightKind::Type)
    );
}

#[test]
fn go_highlights_by_role() {
    let s = "package main\n// c\nfunc Run(n int) string { return fmt.Sprint(n) }";
    let k = |needle| kind_of(Language::Go, s, needle);
    assert_eq!(k("package"), Some(HighlightKind::Keyword));
    assert_eq!(k("// c"), Some(HighlightKind::Comment));
    assert_eq!(k("Run"), Some(HighlightKind::Function));
    assert_eq!(k("int"), Some(HighlightKind::Type));
    assert_eq!(k("Sprint"), Some(HighlightKind::Function));
}

#[test]
fn go_specific_patterns_beat_its_trailing_identifier_catch_all() {
    let s = "package main\nfunc Run() { value := 1; helper(value) }";
    let k = |needle| kind_of(Language::Go, s, needle);
    assert_eq!(k("Run"), Some(HighlightKind::Function));
    assert_eq!(k("helper"), Some(HighlightKind::Function));
    assert_eq!(k("value"), Some(HighlightKind::Variable));
}

#[test]
fn odin_highlights_by_role() {
    let s = "package main\n// c\nmain :: proc() {\n\tcount := 1\n\tfor true { }\n}\n";
    let k = |needle| kind_of(Language::Odin, s, needle);
    assert_eq!(k("package"), Some(HighlightKind::Keyword));
    assert_eq!(k("// c"), Some(HighlightKind::Comment));
    assert_eq!(k("main ::"), Some(HighlightKind::Function));
    assert_eq!(k("proc"), Some(HighlightKind::Keyword));
    assert_eq!(k("for"), Some(HighlightKind::Keyword));
    assert_eq!(k("1"), Some(HighlightKind::Number));
}

#[test]
fn odin_lua_match_predicates_are_enforced() {
    // Without evaluating #lua-match?, these patterns apply to every identifier, painting ordinary
    // variables as types and constants.
    let s = "main :: proc() {\n\tcount := 1\n\tTOTAL := 2\n\t_ = count\n}\n";
    let k = |needle| kind_of(Language::Odin, s, needle);
    assert_eq!(k("count"), Some(HighlightKind::Variable));
    assert_eq!(k("TOTAL"), Some(HighlightKind::Constant));
}

#[test]
fn typescript_inherits_javascript_highlights_and_adds_its_own() {
    let s = "interface Foo { x: number }\nconst a = \"s\"; function run() { return 1; }";
    let k = |needle| kind_of(Language::TypeScript, s, needle);
    assert_eq!(k("interface"), Some(HighlightKind::Keyword));
    assert_eq!(k("number"), Some(HighlightKind::Type));
    assert_eq!(k("const"), Some(HighlightKind::Keyword));
    assert_eq!(k("\"s\""), Some(HighlightKind::String));
    assert_eq!(k("run"), Some(HighlightKind::Function));
}

#[test]
fn jsx_tags_are_highlighted() {
    let k = |l, s: &str, n| kind_of(l, s, n);
    assert_eq!(
        k(Language::JavaScript, "const v = <div></div>;", "div"),
        Some(HighlightKind::Tag)
    );
    assert_eq!(
        k(Language::Tsx, "const v = <div></div>;", "div"),
        Some(HighlightKind::Tag)
    );
}

#[test]
fn inner_highlights_win_over_the_string_around_them() {
    let s = "let a = \"x\\ny\";";
    let (syntax, buffer) = parse(Language::Rust, s);
    let spans = highlights(
        Language::Rust,
        syntax.tree().unwrap(),
        &buffer.snapshot(),
        0..s.len(),
    );
    let escape = s.find("\\n").unwrap();
    let span = spans.iter().find(|sp| sp.range.contains(&escape)).unwrap();
    assert_eq!(span.kind, HighlightKind::StringEscape);
}

#[test]
fn spans_are_sorted_and_stay_within_the_requested_range() {
    let source = sample(Language::Rust);
    let (syntax, buffer) = parse(Language::Rust, source);
    for range in [0..source.len(), 5..20, 20..21, 0..0, 200..300] {
        let spans = highlights(
            Language::Rust,
            syntax.tree().unwrap(),
            &buffer.snapshot(),
            range.clone(),
        );
        for s in &spans {
            assert!(range.start <= s.range.start && s.range.end <= range.end);
        }
        for pair in spans.windows(2) {
            assert!(pair[0].range.end <= pair[1].range.start);
        }
    }
}

#[test]
fn highlighting_a_stale_tree_is_safe_when_text_was_deleted() {
    let source = "fn long_function_name() { let value = 1; }\n";
    let (mut syntax, mut buffer) = parse(Language::Rust, source);
    let selections = SelectionSet::cursor(0);
    let applied = buffer.apply_edits(
        vec![TextEdit::new(0..buffer.len(), "x")],
        &selections,
        &selections,
        UndoGrouping::Distinct,
        Instant::now(),
    );
    syntax.edit(&applied, buffer.version());
    // No reparse yet: the tree is stale, and this must not panic.
    highlights(
        Language::Rust,
        syntax.tree().unwrap(),
        &buffer.snapshot(),
        0..buffer.len(),
    );
}

fn insert(buffer: &mut Buffer, syntax: &mut Syntax, at: usize, text: &str) {
    let selections = SelectionSet::single(Selection::cursor(at));
    let applied = buffer.apply_edits(
        vec![TextEdit::new(at..at, text)],
        &selections,
        &selections,
        UndoGrouping::Distinct,
        Instant::now(),
    );
    syntax.edit(&applied, buffer.version());
}

#[test]
fn edits_mark_the_tree_as_needing_a_parse() {
    let (mut syntax, mut buffer) = parse(Language::Rust, "fn a() {}");
    assert!(!syntax.needs_parse(buffer.version()));
    insert(&mut buffer, &mut syntax, 0, "// x\n");
    assert!(syntax.needs_parse(buffer.version()));
    let parsed = syntax.start_parse(buffer.snapshot()).run().unwrap();
    syntax.install(parsed);
    assert!(!syntax.needs_parse(buffer.version()));
}

#[test]
fn edits_during_a_parse_are_carried_onto_the_new_tree() {
    let (mut syntax, mut buffer) = parse(Language::Rust, "fn a() {}\n");
    insert(&mut buffer, &mut syntax, 0, "// one\n");
    let job = syntax.start_parse(buffer.snapshot());

    // This edit lands while the job is running on another thread.
    insert(&mut buffer, &mut syntax, 0, "// two\n");
    syntax.install(job.run().unwrap());

    // The tree is still behind, so another parse is due, and positions already account for the
    // edit: `fn` has moved past both comments.
    assert!(syntax.needs_parse(buffer.version()));
    let fn_item = syntax.tree().unwrap().root_node().named_child(1).unwrap();
    assert_eq!(fn_item.kind(), "function_item");
    assert_eq!(fn_item.start_byte(), "// two\n// one\n".len());

    let parsed = syntax.start_parse(buffer.snapshot()).run().unwrap();
    syntax.install(parsed);
    assert!(!syntax.needs_parse(buffer.version()));
    assert!(!syntax.tree().unwrap().root_node().has_error());
}

#[test]
fn a_stale_parse_finishing_late_is_ignored() {
    let (mut syntax, mut buffer) = parse(Language::Rust, "fn a() {}\n");
    insert(&mut buffer, &mut syntax, 0, "// one\n");
    let older = syntax.start_parse(buffer.snapshot());
    insert(&mut buffer, &mut syntax, 0, "// two\n");
    let newer = syntax.start_parse(buffer.snapshot());

    syntax.install(newer.run().unwrap());
    assert!(!syntax.needs_parse(buffer.version()));
    syntax.install(older.run().unwrap());
    assert!(!syntax.needs_parse(buffer.version()));
}

#[test]
fn a_parse_from_before_a_reload_is_ignored() {
    let (mut syntax, mut buffer) = parse(Language::Rust, "fn a() {}\n");
    insert(&mut buffer, &mut syntax, 0, "// one\n");
    let job = syntax.start_parse(buffer.snapshot());

    buffer.reset("struct Replaced;\n");
    syntax.reset(buffer.version());
    syntax.install(job.run().unwrap());

    assert!(syntax.tree().is_none());
    assert!(syntax.needs_parse(buffer.version()));
    let parsed = syntax.start_parse(buffer.snapshot()).run().unwrap();
    syntax.install(parsed);
    let first = syntax.tree().unwrap().root_node().named_child(0).unwrap();
    assert_eq!(first.kind(), "struct_item");
}

#[test]
fn the_generation_changes_whenever_highlights_could() {
    let (mut syntax, mut buffer) = parse(Language::Rust, "fn a() {}");
    let before = syntax.generation();
    insert(&mut buffer, &mut syntax, 0, "x");
    let edited = syntax.generation();
    assert_ne!(before, edited);
    let parsed = syntax.start_parse(buffer.snapshot()).run().unwrap();
    syntax.install(parsed);
    assert_ne!(edited, syntax.generation());
}

#[test]
fn expand_selection_walks_outward_through_the_tree() {
    let s = "fn f() { let a = g(1 + 2); }";
    let (syntax, _) = parse(Language::Rust, s);
    let start = s.find("1 + 2").unwrap();

    let mut range = start..start + 1;
    let mut seen = vec![];
    while let Some(next) = syntax.expand_selection(range.clone()) {
        seen.push(s[next.clone()].to_string());
        assert!(next.start <= range.start && range.end <= next.end && next != range);
        range = next;
    }
    assert!(seen.contains(&"1 + 2".to_string()), "{seen:?}");
    assert!(seen.contains(&"g(1 + 2)".to_string()), "{seen:?}");
    assert_eq!(seen.last().unwrap(), s);
}

#[test]
fn expanding_the_whole_document_returns_nothing() {
    let s = "fn f() {}";
    let (syntax, _) = parse(Language::Rust, s);
    assert_eq!(syntax.expand_selection(0..s.len()), None);
}

proptest! {
    /// Incremental parsing relies on our edits exactly describing the change. If they're ever
    /// wrong, the tree goes subtly out of step with the text, so compare with a fresh parse. The
    /// edits keep the code valid, because error recovery legitimately depends on parse history.
    #[test]
    fn incremental_parsing_matches_a_fresh_parse(
        statements in prop::collection::vec(
            (0usize..8, prop::sample::select(vec![
                "let a = 1;\n",
                "// é😀 comment\n",
                "call(\"é😀\");\n",
                "if x { y(); }\n",
            ])),
            1..12,
        ),
    ) {
        let source = "fn main() {\n}\n";
        let mut buffer = Buffer::new(source);
        let mut syntax = Syntax::new(Language::Rust);
        syntax.install(syntax.start_parse(buffer.snapshot()).run().unwrap());

        let selections = SelectionSet::cursor(0);
        for (line, text) in statements {
            // Between the first line and the closing brace is always statement position.
            let line = 1 + line % (buffer.line_count() - 2);
            let at = buffer.line_start(line);
            let applied = buffer.apply_edits(
                vec![TextEdit::new(at..at, text)],
                &selections, &selections, UndoGrouping::Distinct, Instant::now(),
            );
            syntax.edit(&applied, buffer.version());
            syntax.install(syntax.start_parse(buffer.snapshot()).run().unwrap());
        }

        let mut fresh = Syntax::new(Language::Rust);
        fresh.install(fresh.start_parse(buffer.snapshot()).run().unwrap());
        let fresh_root = fresh.tree().unwrap().root_node();
        prop_assert!(!fresh_root.has_error());
        prop_assert_eq!(syntax.tree().unwrap().root_node().to_sexp(), fresh_root.to_sexp());
    }

    /// With arbitrary edits the trees may differ, but must never panic or go out of bounds.
    #[test]
    fn arbitrary_edits_never_break_parsing_or_highlighting(
        edits in prop::collection::vec(
            (0usize..400, 0usize..6, "[a-z(){};\"\n é😀]{0,6}"), 1..12),
    ) {
        let source = "fn main() {\n    let a = \"é😀\";\n    call(a, 1);\n}\n\nstruct S { x: i32 }\n";
        let mut buffer = Buffer::new(source);
        let mut syntax = Syntax::new(Language::Rust);
        syntax.install(syntax.start_parse(buffer.snapshot()).run().unwrap());

        let selections = SelectionSet::cursor(0);
        for (at, len, text) in edits {
            let start = buffer.clip(at);
            let end = buffer.clip(start + len);
            let applied = buffer.apply_edits(
                vec![TextEdit::new(start..end, text)],
                &selections, &selections, UndoGrouping::Distinct, Instant::now(),
            );
            syntax.edit(&applied, buffer.version());
            let snapshot = buffer.snapshot();
            // Before the reparse, the tree is stale.
            highlights(Language::Rust, syntax.tree().unwrap(), &snapshot, 0..buffer.len());
            syntax.install(syntax.start_parse(snapshot.clone()).run().unwrap());
            let spans = highlights(Language::Rust, syntax.tree().unwrap(), &snapshot, 0..buffer.len());
            for span in spans {
                prop_assert!(span.range.end <= buffer.len());
            }
        }
    }
}

/// Run with: cargo test -p gleipnir-syntax --release -- --ignored --nocapture perf
#[test]
#[ignore]
fn perf_on_a_100k_line_file() {
    let function = "fn compute_value(input: &str, count: usize) -> Result<String, Error> {\n    let mut output = String::with_capacity(count);\n    for (index, ch) in input.chars().enumerate() {\n        if index % 2 == 0 { output.push(ch); }\n    }\n    Ok(format!(\"{output}: {count}\"))\n}\n\n";
    let lines_per = function.matches('\n').count();
    let source = function.repeat(100_000 / lines_per);
    let mut buffer = Buffer::new(&source);
    let mut syntax = Syntax::new(Language::Rust);

    let t = Instant::now();
    syntax.install(syntax.start_parse(buffer.snapshot()).run().unwrap());
    println!("{} lines, {} bytes", buffer.line_count(), buffer.len());
    println!("initial parse: {:?}", t.elapsed());

    let snapshot = buffer.snapshot();
    let viewport = buffer.line_start(50_000)..buffer.line_start(50_060);
    let t = Instant::now();
    highlights(
        Language::Rust,
        syntax.tree().unwrap(),
        &snapshot,
        viewport.clone(),
    );
    println!(
        "highlight 60 lines, cold (compiles queries): {:?}",
        t.elapsed()
    );
    let t = Instant::now();
    let spans = highlights(
        Language::Rust,
        syntax.tree().unwrap(),
        &snapshot,
        viewport.clone(),
    );
    println!(
        "highlight 60 lines, warm: {:?} ({} spans)",
        t.elapsed(),
        spans.len()
    );

    // A realistic keystroke: extending an identifier inside a function body.
    let in_body = buffer.text()[viewport.clone()].find("output").unwrap() + viewport.start;
    let t = Instant::now();
    insert(&mut buffer, &mut syntax, in_body, "x");
    println!(
        "keystroke, on the UI thread (edit + tree update): {:?}",
        t.elapsed()
    );
    let t = Instant::now();
    syntax.install(syntax.start_parse(buffer.snapshot()).run().unwrap());
    println!(
        "reparse after a keystroke, in the background: {:?}",
        t.elapsed()
    );

    // The worst case: an edit that breaks the structure of the whole file.
    let t = Instant::now();
    insert(
        &mut buffer,
        &mut syntax,
        viewport.start,
        "fn broken( {{{ \"",
    );
    syntax.install(syntax.start_parse(buffer.snapshot()).run().unwrap());
    println!(
        "reparse after a structure-breaking edit, in the background: {:?}",
        t.elapsed()
    );
    let snapshot = buffer.snapshot();
    let t = Instant::now();
    highlights(Language::Rust, syntax.tree().unwrap(), &snapshot, viewport);
    println!("highlight 60 lines in the broken tree: {:?}", t.elapsed());

    let t = Instant::now();
    let _ = buffer.snapshot();
    println!("snapshot: {:?}", t.elapsed());
}
