//! Command tests use a small notation: `|` marks a cursor, and `[` `]` mark a selection with the
//! head at the `]`, or `<` `>` with the head at the `<`. That keeps each test readable as a
//! before and after picture, for any number of cursors.

use super::*;
use proptest::prelude::*;

fn parse(marked: &str) -> (Buffer, SelectionSet) {
    let mut text = String::new();
    let mut items = Vec::new();
    let mut open: Option<(usize, bool)> = None;
    for c in marked.chars() {
        match c {
            '|' => items.push(Selection::cursor(text.len())),
            '[' => open = Some((text.len(), false)),
            '<' => open = Some((text.len(), true)),
            ']' | '>' => {
                let (start, reversed) = open.take().unwrap();
                items.push(if reversed {
                    Selection::new(text.len(), start)
                } else {
                    Selection::new(start, text.len())
                });
            }
            c => text.push(c),
        }
    }
    (Buffer::new(&text), SelectionSet::from_vec(items, 0))
}

fn render(buffer: &Buffer, selections: &SelectionSet) -> String {
    let text = buffer.text();
    let mut marks: Vec<(usize, u8, char)> = Vec::new();
    for s in selections.iter() {
        let range = s.range();
        if s.is_empty() {
            marks.push((s.head, 1, '|'));
        } else if s.is_reversed() {
            marks.push((range.start, 2, '<'));
            marks.push((range.end, 0, '>'));
        } else {
            marks.push((range.start, 2, '['));
            marks.push((range.end, 0, ']'));
        }
    }
    marks.sort();
    let mut out = String::new();
    let mut at = 0;
    for (offset, _, mark) in marks {
        out.push_str(&text[at..offset]);
        out.push(mark);
        at = offset;
    }
    out.push_str(&text[at..]);
    out
}

fn config() -> EditConfig {
    EditConfig {
        line_comment: Some("//".to_string()),
        ..EditConfig::default()
    }
}

/// Runs a command against a marked document and returns the marked result.
fn run(marked: &str, command: impl FnOnce(&mut EditContext)) -> String {
    run_with(&config(), marked, command)
}

fn run_with(config: &EditConfig, marked: &str, command: impl FnOnce(&mut EditContext)) -> String {
    let (mut buffer, mut selections) = parse(marked);
    let mut cx = EditContext {
        buffer: &mut buffer,
        selections: &mut selections,
        config,
        now: Instant::now(),
    };
    command(&mut cx);
    render(&buffer, &selections)
}

fn motion(marked: &str, motion: Motion, extend: bool) -> String {
    let (buffer, mut selections) = parse(marked);
    move_selections(&buffer, &mut selections, motion, extend, 4);
    render(&buffer, &selections)
}

fn type_str(marked: &str, text: &str) -> String {
    run(marked, |cx| {
        for c in text.chars() {
            cx.type_char(c);
        }
    })
}

#[test]
fn the_test_notation_round_trips() {
    for marked in ["a|b|c", "[ab]c", "<ab>c", "|", "a[b] |", "[a][b]"] {
        let (buffer, selections) = parse(marked);
        assert_eq!(render(&buffer, &selections), marked);
    }
}

#[test]
fn typing_replaces_every_selection() {
    assert_eq!(type_str("a|b", "x"), "ax|b");
    assert_eq!(type_str("[ab] [cd]", "x"), "x| x|");
    assert_eq!(type_str("|a |b |c", "x"), "x|a x|b x|c");
}

#[test]
fn brackets_auto_close_in_front_of_whitespace_or_closers() {
    assert_eq!(type_str("|", "("), "(|)");
    assert_eq!(type_str("| x", "["), "[|] x");
    assert_eq!(type_str("(|)", "{"), "({|})");
    assert_eq!(type_str("|,", "("), "(|),");
}

#[test]
fn brackets_do_not_auto_close_in_front_of_code() {
    assert_eq!(type_str("|foo", "("), "(|foo");
}

#[test]
fn typing_a_closer_steps_over_an_existing_one() {
    assert_eq!(type_str("|", "()"), "()|");
    assert_eq!(type_str("|", "foo(bar)"), "foo(bar)|");
    assert_eq!(type_str("|", "\"a\""), "\"a\"|");
}

#[test]
fn quotes_auto_close_except_after_a_word() {
    assert_eq!(type_str("|", "\""), "\"|\"");
    assert_eq!(type_str("don|", "'"), "don'|");
}

#[test]
fn opening_characters_wrap_a_selection_and_keep_it_selected() {
    assert_eq!(type_str("[foo]", "("), "([foo])");
    assert_eq!(type_str("[a] [b]", "\""), "\"[a]\" \"[b]\"");
}

#[test]
fn a_language_can_opt_out_of_a_pair() {
    // Rust lifetimes make auto-closing a single quote more annoying than useful.
    let rust = EditConfig {
        pairs: vec![('(', ')'), ('"', '"')],
        ..config()
    };
    let result = run_with(&rust, "|", |cx| {
        cx.type_char('\'');
    });
    assert_eq!(result, "'|");
}

#[test]
fn every_cursor_auto_closes_independently() {
    assert_eq!(type_str("|\n|foo", "("), "(|)\n(|foo");
}

#[test]
fn newline_keeps_indentation() {
    assert_eq!(run("    a|", |cx| drop(cx.newline())), "    a\n    |");
}

#[test]
fn newline_after_an_opener_adds_a_level() {
    assert_eq!(run("if x {|", |cx| drop(cx.newline())), "if x {\n    |");
}

#[test]
fn newline_between_a_pair_puts_the_closer_on_its_own_line() {
    assert_eq!(run("  f(|)", |cx| drop(cx.newline())), "  f(\n      |\n  )");
}

#[test]
fn newline_uses_tabs_when_the_language_does() {
    let go = EditConfig {
        hard_tabs: true,
        ..config()
    };
    assert_eq!(
        run_with(&go, "\tif {|}", |cx| drop(cx.newline())),
        "\tif {\n\t\t|\n\t}"
    );
}

#[test]
fn newline_on_a_whitespace_only_line_leaves_no_trailing_whitespace() {
    assert_eq!(run("    |", |cx| drop(cx.newline())), "\n    |");
}

#[test]
fn newline_applies_to_every_cursor() {
    assert_eq!(run("a|\nb|", |cx| drop(cx.newline())), "a\n|\nb\n|");
}

#[test]
fn newline_does_not_split_empty_quotes_onto_lines() {
    assert_eq!(run("\"|\"", |cx| drop(cx.newline())), "\"\n|\"");
}

#[test]
fn backspace_deletes_selections_and_characters() {
    assert_eq!(run("ab|", |cx| drop(cx.backspace())), "a|");
    assert_eq!(run("[ab]c", |cx| drop(cx.backspace())), "|c");
    assert_eq!(run("a|b|c", |cx| drop(cx.backspace())), "|c");
}

#[test]
fn backspace_at_the_start_of_the_document_does_nothing() {
    let (mut buffer, mut selections) = parse("|a");
    let config = config();
    let mut cx = EditContext {
        buffer: &mut buffer,
        selections: &mut selections,
        config: &config,
        now: Instant::now(),
    };
    assert!(cx.backspace().is_empty());
    assert!(!buffer.can_undo());
}

#[test]
fn backspace_joins_lines() {
    assert_eq!(run("a\n|b", |cx| drop(cx.backspace())), "a|b");
}

#[test]
fn backspace_removes_an_empty_pair() {
    assert_eq!(run("(|)", |cx| drop(cx.backspace())), "|");
    assert_eq!(run("\"|\"", |cx| drop(cx.backspace())), "|");
}

#[test]
fn backspace_in_leading_spaces_goes_back_a_tab_stop() {
    assert_eq!(run("        |x", |cx| drop(cx.backspace())), "    |x");
    assert_eq!(run("      |x", |cx| drop(cx.backspace())), "    |x");
    assert_eq!(run("  a   |x", |cx| drop(cx.backspace())), "  a  |x");
}

#[test]
fn backspace_deletes_a_whole_grapheme() {
    assert_eq!(run("e\u{301}|", |cx| drop(cx.backspace())), "|");
    assert_eq!(run("😀|", |cx| drop(cx.backspace())), "|");
}

#[test]
fn delete_forward_and_word_deletes() {
    assert_eq!(run("|ab", |cx| drop(cx.delete_forward())), "|b");
    assert_eq!(run("foo bar|", |cx| drop(cx.delete_word_left())), "foo |");
    assert_eq!(run("|foo bar", |cx| drop(cx.delete_word_right())), "| bar");
}

#[test]
fn cursors_deleting_the_same_word_merge_rather_than_overlap() {
    assert_eq!(run("fo|o| bar", |cx| drop(cx.delete_word_left())), "| bar");
}

#[test]
fn delete_lines_removes_touched_lines() {
    assert_eq!(run("a\nb|\nc", |cx| drop(cx.delete_lines())), "a\n|c");
    assert_eq!(run("a\n[b\nc]\nd", |cx| drop(cx.delete_lines())), "a\n|d");
}

#[test]
fn delete_lines_handles_the_last_line_having_no_newline() {
    assert_eq!(run("a\nb|", |cx| drop(cx.delete_lines())), "a|");
    assert_eq!(run("only|", |cx| drop(cx.delete_lines())), "|");
}

#[test]
fn delete_lines_with_a_cursor_on_each_of_several_lines() {
    assert_eq!(run("a|\nb\nc|\nd", |cx| drop(cx.delete_lines())), "|b\n|d");
}

#[test]
fn a_selection_ending_at_column_zero_does_not_include_that_line() {
    assert_eq!(run("[a\n]b", |cx| drop(cx.delete_lines())), "|b");
}

#[test]
fn duplicate_copies_the_line_and_moves_onto_it() {
    assert_eq!(run("ab|c", |cx| drop(cx.duplicate())), "abc\nab|c");
}

#[test]
fn duplicate_copies_a_selection_and_selects_the_copy() {
    assert_eq!(run("[ab]c", |cx| drop(cx.duplicate())), "ab[ab]c");
}

#[test]
fn move_lines_up_and_down() {
    assert_eq!(run("a\nb|\nc", |cx| drop(cx.move_lines(true))), "b|\na\nc");
    assert_eq!(run("a|\nb\nc", |cx| drop(cx.move_lines(false))), "b\na|\nc");
}

#[test]
fn move_lines_does_nothing_at_the_edges() {
    assert_eq!(run("a|\nb", |cx| drop(cx.move_lines(true))), "a|\nb");
    assert_eq!(run("a\nb|", |cx| drop(cx.move_lines(false))), "a\nb|");
}

#[test]
fn move_lines_keeps_a_selection_spanning_lines() {
    assert_eq!(
        run("x\n[a\nb]\ny", |cx| drop(cx.move_lines(false))),
        "x\ny\n[a\nb]"
    );
}

#[test]
fn a_selection_ending_at_column_zero_moves_with_its_line() {
    assert_eq!(
        run("[a\n]b\nc", |cx| drop(cx.move_lines(false))),
        "b\n[a\n]c"
    );
    assert_eq!(
        run("x\n[a\n]b", |cx| drop(cx.move_lines(true))),
        "[a\n]x\nb"
    );
}

#[test]
fn a_selection_ending_at_column_zero_stays_valid_when_moved_to_the_last_line() {
    // Found by the property test: the selection's end was left inside the moved text, in the
    // middle of a multi-byte character.
    assert_eq!(run("[😀\n]😀", |cx| drop(cx.move_lines(false))), "😀\n[😀]");
}

#[test]
fn move_lines_moves_several_blocks_at_once() {
    assert_eq!(
        run("a\nb|\nc\nd\ne|\nf", |cx| drop(cx.move_lines(true))),
        "b|\na\nc\ne|\nd\nf"
    );
}

#[test]
fn move_lines_stops_all_blocks_if_one_is_at_the_edge() {
    assert_eq!(
        run("a|\nb\nc|", |cx| drop(cx.move_lines(true))),
        "a|\nb\nc|"
    );
}

#[test]
fn adjacent_blocks_move_together() {
    assert_eq!(
        run("x\na|\ny\nb|\nz", |cx| drop(cx.move_lines(true))),
        "a|\ny\nb|\nx\nz"
    );
}

#[test]
fn tab_goes_to_the_next_stop_with_spaces() {
    assert_eq!(run("|x", |cx| drop(cx.indent())), "    |x");
    assert_eq!(run("ab|x", |cx| drop(cx.indent())), "ab  |x");
}

#[test]
fn tab_inserts_a_tab_for_hard_tab_languages() {
    let go = EditConfig {
        hard_tabs: true,
        ..config()
    };
    assert_eq!(run_with(&go, "|x", |cx| drop(cx.indent())), "\t|x");
}

#[test]
fn tab_with_a_multi_line_selection_indents_lines_and_skips_blank_ones() {
    assert_eq!(run("[a\n\nb]", |cx| drop(cx.indent())), "    [a\n\n    b]");
}

#[test]
fn outdent_removes_a_level_or_as_much_as_there_is() {
    assert_eq!(run("        a|", |cx| drop(cx.outdent())), "    a|");
    assert_eq!(run("  a|", |cx| drop(cx.outdent())), "a|");
    assert_eq!(run("\t\ta|", |cx| drop(cx.outdent())), "\ta|");
    assert_eq!(run("a|", |cx| drop(cx.outdent())), "a|");
}

#[test]
fn outdent_applies_to_each_line_once_even_with_several_cursors_on_it() {
    assert_eq!(run("        a|b|", |cx| drop(cx.outdent())), "    a|b|");
}

#[test]
fn comment_toggle_adds_and_removes() {
    assert_eq!(run("a|", |cx| drop(cx.toggle_line_comment())), "// a|");
    assert_eq!(run("// a|", |cx| drop(cx.toggle_line_comment())), "a|");
    assert_eq!(run("//a|", |cx| drop(cx.toggle_line_comment())), "a|");
}

#[test]
fn comment_toggle_uses_the_shallowest_indent_and_skips_blank_lines() {
    assert_eq!(
        run("[    a\n\n  b]", |cx| drop(cx.toggle_line_comment())),
        "[  //   a\n\n  // b]"
    );
}

#[test]
fn comment_toggle_comments_everything_if_any_line_is_not_commented() {
    assert_eq!(
        run("[// a\nb]", |cx| drop(cx.toggle_line_comment())),
        "// [// a\n// b]"
    );
}

#[test]
fn comment_toggle_does_nothing_without_a_comment_syntax() {
    let plain = EditConfig::default();
    assert_eq!(
        run_with(&plain, "a|", |cx| drop(cx.toggle_line_comment())),
        "a|"
    );
}

#[test]
fn paste_replaces_selections() {
    assert_eq!(run("[a]b", |cx| drop(cx.paste("xy", false))), "xy|b");
}

#[test]
fn paste_gives_each_cursor_its_own_line_when_counts_match() {
    assert_eq!(
        run("|\n|", |cx| drop(cx.paste("one\ntwo", false))),
        "one|\ntwo|"
    );
    assert_eq!(
        run("|\n|", |cx| drop(cx.paste("one\ntwo\n", false))),
        "one|\ntwo|"
    );
}

#[test]
fn paste_gives_every_cursor_everything_when_counts_differ() {
    assert_eq!(
        run("|-|", |cx| drop(cx.paste("x\ny\nz", false))),
        "x\ny\nz|-x\ny\nz|"
    );
}

#[test]
fn linewise_paste_goes_above_the_current_line() {
    assert_eq!(run("a|b", |cx| drop(cx.paste("line\n", true))), "line\na|b");
}

#[test]
fn linewise_paste_with_several_cursors_on_a_line_inserts_once() {
    // Found by the property test: this used to insert per cursor and misplace the cursors.
    assert_eq!(run("a|b|", |cx| drop(cx.paste("😀\n", true))), "😀\na|b|");
    assert_eq!(
        run("a|\nb|", |cx| drop(cx.paste("x\ny\nz\n", true))),
        "x\ny\nz\na|\nx\ny\nz\nb|"
    );
}

#[test]
fn linewise_paste_replaces_a_selection_like_any_other() {
    assert_eq!(
        run("[ab]c", |cx| drop(cx.paste("line\n", true))),
        "line\n|c"
    );
}

#[test]
fn copy_takes_selected_text_or_whole_lines() {
    let (buffer, selections) = parse("[ab]c\nd[ef]");
    assert_eq!(
        copy_text(&buffer, &selections),
        ("ab\nef".to_string(), false)
    );
    let (buffer, selections) = parse("ab|c\ndef|");
    assert_eq!(
        copy_text(&buffer, &selections),
        ("abc\ndef\n".to_string(), true)
    );
}

#[test]
fn copy_ignores_bare_cursors_when_something_is_selected() {
    let (buffer, selections) = parse("[ab]c\nd|ef");
    assert_eq!(copy_text(&buffer, &selections), ("ab".to_string(), false));
}

#[test]
fn copy_with_two_cursors_on_one_line_takes_it_once() {
    let (buffer, selections) = parse("a|b|c");
    assert_eq!(copy_text(&buffer, &selections), ("abc\n".to_string(), true));
}

#[test]
fn cut_removes_what_it_copied() {
    let (mut buffer, mut selections) = parse("a\nb|\nc");
    let config = config();
    let mut cx = EditContext {
        buffer: &mut buffer,
        selections: &mut selections,
        config: &config,
        now: Instant::now(),
    };
    let (text, linewise, _) = cx.cut();
    assert_eq!((text.as_str(), linewise), ("b\n", true));
    assert_eq!(render(&buffer, &selections), "a\n|c");
}

#[test]
fn undo_restores_all_text_and_all_selections() {
    let (mut buffer, mut selections) = parse("a|b\n[cd]");
    let config = config();
    let mut cx = EditContext {
        buffer: &mut buffer,
        selections: &mut selections,
        config: &config,
        now: Instant::now(),
    };
    cx.insert("XY", UndoGrouping::Distinct);
    assert_eq!(render(&buffer, &selections), "aXY|b\nXY|");
    let (_, restored) = buffer.undo().unwrap();
    assert_eq!(render(&buffer, &restored), "a|b\n[cd]");
    let (_, restored) = buffer.redo().unwrap();
    assert_eq!(render(&buffer, &restored), "aXY|b\nXY|");
}

#[test]
fn external_edits_apply_as_one_step_and_selections_follow() {
    let (mut buffer, mut selections) = parse("use a;\nfn f() { x| }");
    let config = config();
    let mut cx = EditContext {
        buffer: &mut buffer,
        selections: &mut selections,
        config: &config,
        now: Instant::now(),
    };
    // A completion's own edit, and an import added at the top, in one go.
    let typed = "use a;\nfn f() { ".len();
    cx.apply_edits(
        vec![
            TextEdit::new(typed..typed + 1, "xyz()"),
            TextEdit::new(0..0, "use b;\n"),
        ],
        None,
        UndoGrouping::Distinct,
    );
    assert_eq!(
        render(&buffer, &selections),
        "use b;\nuse a;\nfn f() { xyz()| }"
    );
    buffer.undo().unwrap();
    assert_eq!(buffer.text(), "use a;\nfn f() { x }");
}

#[test]
fn external_edits_can_place_the_cursor() {
    let (mut buffer, mut selections) = parse("f|");
    let config = config();
    let mut cx = EditContext {
        buffer: &mut buffer,
        selections: &mut selections,
        config: &config,
        now: Instant::now(),
    };
    // A snippet `foo($1)` puts the cursor inside the parentheses.
    cx.apply_edits(
        vec![TextEdit::new(0..1, "foo()")],
        Some(4),
        UndoGrouping::Distinct,
    );
    assert_eq!(render(&buffer, &selections), "foo(|)");
}

#[test]
fn left_and_right_move_by_grapheme_and_cross_lines() {
    assert_eq!(motion("a|b", Motion::Left, false), "|ab");
    assert_eq!(motion("a|b", Motion::Right, false), "ab|");
    assert_eq!(motion("a\n|b", Motion::Left, false), "a|\nb");
    assert_eq!(motion("a|\nb", Motion::Right, false), "a\n|b");
    assert_eq!(motion("|e\u{301}x", Motion::Right, false), "e\u{301}|x");
}

#[test]
fn left_and_right_collapse_a_selection_to_its_edge() {
    assert_eq!(motion("a[bc]d", Motion::Left, false), "a|bcd");
    assert_eq!(motion("a[bc]d", Motion::Right, false), "abc|d");
    assert_eq!(motion("a<bc>d", Motion::Left, false), "a|bcd");
}

#[test]
fn extending_moves_only_the_head() {
    assert_eq!(motion("a|b", Motion::Right, true), "a[b]");
    assert_eq!(motion("a|b", Motion::Left, true), "<a>b");
    assert_eq!(motion("[ab]", Motion::Left, true), "[a]b");
}

#[test]
fn vertical_motion_clamps_at_the_document_edges() {
    assert_eq!(motion("a|b\ncd", Motion::Up(1), false), "a|b\ncd");
    assert_eq!(motion("ab\nc|d", Motion::Down(1), false), "ab\nc|d");
    assert_eq!(motion("a\nb\nc|", Motion::Up(50), false), "a|\nb\nc");
}

#[test]
fn the_goal_column_survives_short_lines() {
    let (buffer, mut selections) = parse("abcd|\nx\nabcd");
    move_selections(&buffer, &mut selections, Motion::Down(1), false, 4);
    assert_eq!(render(&buffer, &selections), "abcd\nx|\nabcd");
    move_selections(&buffer, &mut selections, Motion::Down(1), false, 4);
    assert_eq!(render(&buffer, &selections), "abcd\nx\nabcd|");
}

#[test]
fn horizontal_motion_resets_the_goal_column() {
    let (buffer, mut selections) = parse("abcd|\nx\nabcd");
    move_selections(&buffer, &mut selections, Motion::Down(1), false, 4);
    move_selections(&buffer, &mut selections, Motion::Left, false, 4);
    move_selections(&buffer, &mut selections, Motion::Down(1), false, 4);
    assert_eq!(render(&buffer, &selections), "abcd\nx\n|abcd");
}

#[test]
fn vertical_motion_is_by_screen_column_across_tabs() {
    assert_eq!(motion("    a|\n\tb", Motion::Down(1), false), "    a\n\tb|");
}

#[test]
fn home_toggles_between_first_text_and_column_zero() {
    assert_eq!(motion("    ab|", Motion::LineStart, false), "    |ab");
    assert_eq!(motion("    |ab", Motion::LineStart, false), "|    ab");
    assert_eq!(motion("|    ab", Motion::LineStart, false), "    |ab");
}

#[test]
fn every_cursor_moves_and_collisions_merge() {
    assert_eq!(motion("a|b\nc|d", Motion::Right, false), "ab|\ncd|");
    assert_eq!(motion("|a|b", Motion::DocStart, false), "|ab");
}

#[test]
fn add_cursor_below_and_above_keep_the_column() {
    let (buffer, mut selections) = parse("ab|c\ndef\nghi");
    add_cursor_vertically(&buffer, &mut selections, false, 4);
    assert_eq!(render(&buffer, &selections), "ab|c\nde|f\nghi");
    add_cursor_vertically(&buffer, &mut selections, false, 4);
    assert_eq!(render(&buffer, &selections), "ab|c\nde|f\ngh|i");
    assert_eq!(selections.primary().head, "abc\ndef\ngh".len());
}

#[test]
fn add_cursor_stops_at_the_document_edges() {
    let (buffer, mut selections) = parse("a|");
    add_cursor_vertically(&buffer, &mut selections, true, 4);
    add_cursor_vertically(&buffer, &mut selections, false, 4);
    assert_eq!(selections.len(), 1);
}

#[test]
fn add_cursor_through_a_short_line_returns_to_the_column() {
    let (buffer, mut selections) = parse("abc|\nx\nabc");
    add_cursor_vertically(&buffer, &mut selections, false, 4);
    add_cursor_vertically(&buffer, &mut selections, false, 4);
    assert_eq!(render(&buffer, &selections), "abc|\nx|\nabc|");
}

#[test]
fn next_occurrence_first_selects_the_word_then_finds_more() {
    let (buffer, mut selections) = parse("fo|o bar foo foo");
    add_next_occurrence(&buffer, &mut selections, true);
    assert_eq!(render(&buffer, &selections), "[foo] bar foo foo");
    add_next_occurrence(&buffer, &mut selections, true);
    assert_eq!(render(&buffer, &selections), "[foo] bar [foo] foo");
    add_next_occurrence(&buffer, &mut selections, true);
    assert_eq!(render(&buffer, &selections), "[foo] bar [foo] [foo]");
    add_next_occurrence(&buffer, &mut selections, true);
    assert_eq!(selections.len(), 3);
}

#[test]
fn next_occurrence_wraps_around() {
    let (buffer, mut selections) = parse("foo x [foo]");
    add_next_occurrence(&buffer, &mut selections, false);
    assert_eq!(render(&buffer, &selections), "[foo] x [foo]");
}

#[test]
fn whole_word_matching_skips_longer_identifiers() {
    let (buffer, mut selections) = parse("[foo] foobar foo");
    add_next_occurrence(&buffer, &mut selections, true);
    assert_eq!(render(&buffer, &selections), "[foo] foobar [foo]");
    let (buffer, mut selections) = parse("[foo] foobar foo");
    add_next_occurrence(&buffer, &mut selections, false);
    assert_eq!(render(&buffer, &selections), "[foo] [foo]bar foo");
}

#[test]
fn overlapping_occurrences_are_handled() {
    let (buffer, mut selections) = parse("[aa]aa");
    add_next_occurrence(&buffer, &mut selections, false);
    assert_eq!(render(&buffer, &selections), "[aa][aa]");
}

#[test]
fn removing_the_latest_occurrence_goes_back_one() {
    let (buffer, mut selections) = parse("[foo] foo");
    add_next_occurrence(&buffer, &mut selections, false);
    selections.remove_primary();
    assert_eq!(render(&buffer, &selections), "[foo] foo");
}

#[test]
fn select_all_occurrences_selects_every_match() {
    let (buffer, mut selections) = parse("f|oo bar foo foobar");
    select_all_occurrences(&buffer, &mut selections, true);
    assert_eq!(render(&buffer, &selections), "[foo] bar [foo] foobar");
}

#[test]
fn select_all_occurrences_copes_with_overlaps() {
    let (buffer, mut selections) = parse("[aa]aaa");
    select_all_occurrences(&buffer, &mut selections, false);
    assert_eq!(render(&buffer, &selections), "[aa][aa]a");
}

#[test]
fn typing_with_every_occurrence_selected_renames_them() {
    let (mut buffer, mut selections) = parse("[foo] = foo + foo");
    select_all_occurrences(&buffer, &mut selections, false);
    let config = config();
    let mut cx = EditContext {
        buffer: &mut buffer,
        selections: &mut selections,
        config: &config,
        now: Instant::now(),
    };
    cx.type_char('x');
    assert_eq!(render(&buffer, &selections), "x| = x| + x|");
    buffer.undo().unwrap();
    assert_eq!(buffer.text(), "foo = foo + foo");
}

#[test]
fn column_selection_selects_a_rectangle() {
    let (buffer, _) = parse("abcd\nefgh\nijkl");
    let selections = column_selection(&buffer, (0, 1), (2, 3), 4);
    assert_eq!(render(&buffer, &selections), "a[bc]d\ne[fg]h\ni[jk]l");
}

#[test]
fn column_selection_gives_short_lines_a_cursor_at_their_end() {
    let (buffer, _) = parse("abcd\ne\nijkl");
    let selections = column_selection(&buffer, (0, 2), (2, 4), 4);
    assert_eq!(render(&buffer, &selections), "ab[cd]\ne|\nij[kl]");
}

#[test]
fn column_selection_can_be_dragged_up_and_left() {
    let (buffer, _) = parse("abc\ndef");
    let selections = column_selection(&buffer, (1, 2), (0, 1), 4);
    assert_eq!(render(&buffer, &selections), "a<b>c\nd<e>f");
    assert_eq!(selections.primary().head, 1);
}

#[test]
fn column_selection_uses_screen_columns_across_tabs() {
    let (buffer, _) = parse("\tab\n    cd");
    let selections = column_selection(&buffer, (0, 4), (1, 5), 4);
    assert_eq!(render(&buffer, &selections), "\t[a]b\n    [c]d");
}

#[test]
fn line_range_includes_the_newline_except_at_the_end() {
    let b = Buffer::new("ab\ncd");
    assert_eq!(line_range_at(&b, 1), 0..3);
    assert_eq!(line_range_at(&b, 4), 3..5);
}

#[derive(Clone, Debug)]
enum Op {
    Type(char),
    Newline,
    Backspace,
    DeleteForward,
    DeleteWordLeft,
    DeleteLines,
    Duplicate,
    MoveLines(bool),
    Indent,
    Outdent,
    Comment,
    Paste(String, bool),
    Cut,
    Motion(Motion, bool),
    AddCursor(bool),
    NextOccurrence,
    AllOccurrences,
    Collapse,
    Undo,
    Redo,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let motion = prop_oneof![
        Just(Motion::Left),
        Just(Motion::Right),
        Just(Motion::Up(1)),
        Just(Motion::Down(1)),
        Just(Motion::WordLeft),
        Just(Motion::WordRight),
        Just(Motion::LineStart),
        Just(Motion::LineEnd),
        Just(Motion::DocStart),
        Just(Motion::DocEnd),
    ];
    prop_oneof![
        4 => prop::sample::select(vec!['a', 'é', '😀', ' ', '(', ')', '"', '{', '}', '\t'])
            .prop_map(Op::Type),
        1 => Just(Op::Newline),
        2 => Just(Op::Backspace),
        1 => Just(Op::DeleteForward),
        1 => Just(Op::DeleteWordLeft),
        1 => Just(Op::DeleteLines),
        1 => Just(Op::Duplicate),
        1 => any::<bool>().prop_map(Op::MoveLines),
        1 => Just(Op::Indent),
        1 => Just(Op::Outdent),
        1 => Just(Op::Comment),
        1 => ("[aé😀 \n]{0,6}", any::<bool>()).prop_map(|(t, l)| Op::Paste(t, l)),
        1 => Just(Op::Cut),
        4 => (motion, any::<bool>()).prop_map(|(m, e)| Op::Motion(m, e)),
        1 => any::<bool>().prop_map(Op::AddCursor),
        1 => Just(Op::NextOccurrence),
        1 => Just(Op::AllOccurrences),
        1 => Just(Op::Collapse),
        1 => Just(Op::Undo),
        1 => Just(Op::Redo),
    ]
}

fn assert_invariants(buffer: &Buffer, selections: &SelectionSet) {
    let items = selections.as_slice();
    assert!(!items.is_empty());
    assert!(selections.primary_index() < items.len());
    for s in items {
        for offset in [s.anchor, s.head] {
            assert!(offset <= buffer.len(), "{s:?} past end {}", buffer.len());
            assert_eq!(buffer.clip(offset), offset, "{s:?} not on a char boundary");
        }
    }
    for pair in items.windows(2) {
        assert!(
            pair[0].range().end <= pair[1].range().start,
            "{pair:?} overlap"
        );
    }
}

proptest! {
    /// The main invariant of the whole model: no sequence of commands can panic, corrupt a
    /// selection, or lose text in a way undo can't restore.
    #[test]
    fn any_command_sequence_keeps_invariants_and_undoes_to_the_original(
        original in "[ab é😀\n\t(){}\"]{0,40}",
        ops in prop::collection::vec(op_strategy(), 0..30),
    ) {
        let config = config();
        let mut buffer = Buffer::new(&original);
        // Normalization may adjust the input, so compare against what the buffer really holds.
        let start = buffer.text();
        let mut selections = SelectionSet::cursor(0);
        let now = Instant::now();

        for op in ops {
            let mut cx = EditContext {
                buffer: &mut buffer,
                selections: &mut selections,
                config: &config,
                now,
            };
            match op {
                Op::Type(c) => drop(cx.type_char(c)),
                Op::Newline => drop(cx.newline()),
                Op::Backspace => drop(cx.backspace()),
                Op::DeleteForward => drop(cx.delete_forward()),
                Op::DeleteWordLeft => drop(cx.delete_word_left()),
                Op::DeleteLines => drop(cx.delete_lines()),
                Op::Duplicate => drop(cx.duplicate()),
                Op::MoveLines(up) => drop(cx.move_lines(up)),
                Op::Indent => drop(cx.indent()),
                Op::Outdent => drop(cx.outdent()),
                Op::Comment => drop(cx.toggle_line_comment()),
                Op::Paste(text, linewise) => drop(cx.paste(&text, linewise)),
                Op::Cut => drop(cx.cut()),
                Op::Motion(motion, extend) => {
                    move_selections(&buffer, &mut selections, motion, extend, 4);
                    buffer.break_undo_group();
                }
                Op::AddCursor(up) => add_cursor_vertically(&buffer, &mut selections, up, 4),
                Op::NextOccurrence => add_next_occurrence(&buffer, &mut selections, false),
                Op::AllOccurrences => select_all_occurrences(&buffer, &mut selections, false),
                Op::Collapse => selections.collapse_to_primary(),
                Op::Undo => {
                    if let Some((_, restored)) = buffer.undo() {
                        selections = restored;
                    }
                }
                Op::Redo => {
                    if let Some((_, restored)) = buffer.redo() {
                        selections = restored;
                    }
                }
            }
            assert_invariants(&buffer, &selections);
        }

        while let Some((_, restored)) = buffer.undo() {
            assert_invariants(&buffer, &restored);
        }
        prop_assert_eq!(buffer.text(), start);
    }

    /// Consumers replay `AppliedEdit`s to stay in sync: LSP servers, and tree-sitter. If the
    /// replay ever diverges, every one of them silently desynchronizes.
    #[test]
    fn replaying_applied_edits_reproduces_the_document(
        original in "[ab é😀\n]{0,40}",
        typed in "[ab é😀\n]{1,4}",
        cursors in 1usize..4,
    ) {
        let config = config();
        let mut buffer = Buffer::new(&original);
        let mut selections = SelectionSet::cursor(0);
        for _ in 1..cursors {
            add_cursor_vertically(&buffer, &mut selections, false, 4);
        }
        let mut replica = buffer.text();
        let mut cx = EditContext {
            buffer: &mut buffer,
            selections: &mut selections,
            config: &config,
            now: Instant::now(),
        };
        let mut applied = cx.insert(&typed, UndoGrouping::Distinct);
        applied.extend(cx.backspace());
        for edit in &applied {
            replica.replace_range(edit.start_byte..edit.old_end_byte, &edit.new_text);
        }
        prop_assert_eq!(&replica, &buffer.text());

        // The same must hold through undo.
        let (undo_edits, _) = buffer.undo().unwrap();
        for edit in &undo_edits {
            replica.replace_range(edit.start_byte..edit.old_end_byte, &edit.new_text);
        }
        prop_assert_eq!(&replica, &buffer.text());
    }

    #[test]
    fn utf16_conversion_round_trips_on_character_boundaries(text in "[ab é😀\n]{0,30}") {
        let buffer = Buffer::new(&text);
        let mut offset = 0;
        loop {
            prop_assert_eq!(buffer.utf16_to_offset(buffer.offset_to_utf16(offset)), offset);
            match buffer.char_at(offset) {
                Some(c) => offset += c.len_utf8(),
                None => break,
            }
        }
    }

    #[test]
    fn another_views_selections_stay_valid_through_edits(
        original in "[ab é😀\n]{1,30}",
        position in 0usize..200,
        typed in "[ab é😀\n]{0,4}",
    ) {
        let config = config();
        let mut buffer = Buffer::new(&original);
        let mut other = SelectionSet::cursor(buffer.clip(position));
        let mut mine = SelectionSet::cursor(buffer.clip(position / 2));
        let mut cx = EditContext {
            buffer: &mut buffer,
            selections: &mut mine,
            config: &config,
            now: Instant::now(),
        };
        let applied = cx.insert(&typed, UndoGrouping::Distinct);
        other.transform(&applied);
        assert_invariants(&buffer, &other);
    }
}
