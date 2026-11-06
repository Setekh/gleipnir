//! Tests through the real view: keys go through the keymap and actions, and frames really lay out.

use crate::{EditorView, bind_keys};
use gleipnir_core::BufferModel;
use gleipnir_text::{Selection, SelectionSet};
use gpui::{
    AppContext as _, Context, Entity, Focusable, IntoElement, Modifiers, MouseButton,
    ParentElement, Pixels, Point, Render, Styled, TestAppContext, VisualTestContext, Window, div,
    point, px, size,
};
use std::path::PathBuf;

/// `secondary` is Cmd on macOS and Ctrl on Linux, which the keymap uses for most of these. The
/// rest differ by platform, so tests name what they mean rather than which key does it.
struct Keys;
impl Keys {
    const UNDO: &str = if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    };
    const REDO: &str = if cfg!(target_os = "macos") {
        "cmd-shift-z"
    } else {
        "ctrl-shift-z"
    };
    const COPY: &str = if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    };
    const CUT: &str = if cfg!(target_os = "macos") {
        "cmd-x"
    } else {
        "ctrl-x"
    };
    const PASTE: &str = if cfg!(target_os = "macos") {
        "cmd-v"
    } else {
        "ctrl-v"
    };
    const SELECT_ALL: &str = if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    };
    const CURSOR_BELOW: &str = if cfg!(target_os = "macos") {
        "cmd-alt-down"
    } else {
        "ctrl-alt-down"
    };
    const NEXT_OCCURRENCE: &str = if cfg!(target_os = "macos") {
        "ctrl-g"
    } else {
        "alt-j"
    };
    const ALL_OCCURRENCES: &str = if cfg!(target_os = "macos") {
        "ctrl-cmd-g"
    } else {
        "ctrl-alt-shift-j"
    };
    const EXTEND: &str = if cfg!(target_os = "macos") {
        "alt-up"
    } else {
        "ctrl-w"
    };
    const SHRINK: &str = if cfg!(target_os = "macos") {
        "alt-down"
    } else {
        "ctrl-shift-w"
    };
    const DUPLICATE: &str = if cfg!(target_os = "macos") {
        "cmd-d"
    } else {
        "ctrl-d"
    };
    const COMMENT: &str = if cfg!(target_os = "macos") {
        "cmd-/"
    } else {
        "ctrl-/"
    };
    const DOC_END: &str = if cfg!(target_os = "macos") {
        "cmd-down"
    } else {
        "ctrl-end"
    };
}

struct Host {
    children: Vec<Entity<EditorView>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_col().size_full().children(
            self.children
                .iter()
                .map(|c| div().flex_1().child(c.clone())),
        )
    }
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        bind_keys(cx);
    });
}

fn setup<'a>(
    text: &str,
    path: Option<&str>,
    cx: &'a mut TestAppContext,
) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    init(cx);
    let path = path.map(PathBuf::from);
    let (host, cx) = cx.add_window_view(|window, cx| {
        let buffer = cx.new(|cx| BufferModel::new(text, path, cx));
        let editor = cx.new(|cx| EditorView::new(buffer, window, cx));
        Host {
            children: vec![editor],
        }
    });
    let editor = host.read_with(cx, |host, _| host.children[0].clone());
    focus(&editor, cx);
    (editor, cx)
}

fn focus(editor: &Entity<EditorView>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let handle = editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
    cx.run_until_parked();
}

fn text_of(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> String {
    editor.read_with(cx, |e, cx| e.buffer().read(cx).buffer().text())
}

fn heads(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<usize> {
    editor.read_with(cx, |e, _| e.selections().iter().map(|s| s.head).collect())
}

fn ranges(editor: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<std::ops::Range<usize>> {
    editor.read_with(cx, |e, _| {
        e.selections().iter().map(|s| s.range()).collect()
    })
}

#[gpui::test]
fn typing_goes_through_the_input_handler_and_the_keymap(cx: &mut TestAppContext) {
    let (editor, cx) = setup("", None, cx);
    cx.simulate_input("hello");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("world");
    assert_eq!(text_of(&editor, cx), "hello\nworld");
    cx.simulate_keystrokes("left left backspace");
    assert_eq!(text_of(&editor, cx), "hello\nwold");
}

#[gpui::test]
fn every_platform_keymap_entry_names_a_real_action(_cx: &mut TestAppContext) {
    // `binding` panics on an unknown action name, so building both tables checks them all.
    for macos in [true, false] {
        for (keys, action) in crate::keymap(macos) {
            assert!(!keys.is_empty() && !action.is_empty());
        }
    }
    _cx.update(bind_keys);
}

#[gpui::test]
fn no_keymap_binds_one_keystroke_twice(_cx: &mut TestAppContext) {
    for macos in [true, false] {
        let mut seen = std::collections::HashSet::new();
        for (keys, action) in crate::keymap(macos) {
            assert!(
                seen.insert(keys),
                "{keys} is bound twice, the second to {action}"
            );
        }
    }
}

#[gpui::test]
fn linux_keymap_avoids_the_super_key(_cx: &mut TestAppContext) {
    // Hyprland, and Omarchy's defaults, claim it.
    for (keys, _) in crate::keymap(false) {
        assert!(!keys.contains("super") && !keys.contains("cmd"), "{keys}");
    }
}

#[gpui::test]
fn undo_and_redo_restore_text_and_cursors(cx: &mut TestAppContext) {
    let (editor, cx) = setup("abc", None, cx);
    cx.simulate_keystrokes("right");
    cx.simulate_input("X");
    cx.simulate_keystrokes(Keys::UNDO);
    assert_eq!(text_of(&editor, cx), "abc");
    assert_eq!(heads(&editor, cx), vec![1]);
    cx.simulate_keystrokes(Keys::REDO);
    assert_eq!(text_of(&editor, cx), "aXbc");
    assert_eq!(heads(&editor, cx), vec![2]);
}

#[gpui::test]
fn typing_at_several_cursors_edits_every_line(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one\ntwo\nthree", None, cx);
    cx.simulate_keystrokes(Keys::CURSOR_BELOW);
    cx.simulate_keystrokes(Keys::CURSOR_BELOW);
    assert_eq!(heads(&editor, cx).len(), 3);
    cx.simulate_input("> ");
    assert_eq!(text_of(&editor, cx), "> one\n> two\n> three");
    cx.simulate_keystrokes(Keys::UNDO);
    assert_eq!(text_of(&editor, cx), "one\ntwo\nthree");
    assert_eq!(heads(&editor, cx).len(), 3);
}

#[gpui::test]
fn escape_collapses_cursors_then_the_selection(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one\ntwo", None, cx);
    cx.simulate_keystrokes(Keys::CURSOR_BELOW);
    cx.simulate_keystrokes("shift-right");
    assert_eq!(ranges(&editor, cx).len(), 2);
    cx.simulate_keystrokes("escape");
    assert_eq!(ranges(&editor, cx).len(), 1);
    assert!(!ranges(&editor, cx)[0].is_empty());
    cx.simulate_keystrokes("escape");
    assert!(ranges(&editor, cx)[0].is_empty());
}

#[gpui::test]
fn next_occurrence_then_typing_renames_every_match(cx: &mut TestAppContext) {
    let (editor, cx) = setup("foo foobar foo", None, cx);
    cx.simulate_keystrokes(Keys::NEXT_OCCURRENCE);
    assert_eq!(ranges(&editor, cx), vec![0..3]);
    cx.simulate_keystrokes(Keys::NEXT_OCCURRENCE);
    // Started from a bare cursor, so it skips `foobar`.
    assert_eq!(ranges(&editor, cx), vec![0..3, 11..14]);
    cx.simulate_input("x");
    assert_eq!(text_of(&editor, cx), "x foobar x");
}

#[gpui::test]
fn select_all_occurrences_from_a_cursor_matches_whole_words(cx: &mut TestAppContext) {
    let (editor, cx) = setup("foo foobar foo", None, cx);
    cx.simulate_keystrokes(Keys::ALL_OCCURRENCES);
    assert_eq!(ranges(&editor, cx), vec![0..3, 11..14]);
}

#[gpui::test]
fn copy_and_paste_between_cursors_distributes_lines(cx: &mut TestAppContext) {
    let (editor, cx) = setup("aa\nbb\n\n", None, cx);
    editor.update(cx, |e, cx| {
        e.set_selections(
            SelectionSet::from_vec(vec![Selection::new(0, 2), Selection::new(3, 5)], 0),
            cx,
        )
    });
    cx.simulate_keystrokes(Keys::COPY);
    editor.update(cx, |e, cx| {
        e.set_selections(
            SelectionSet::from_vec(vec![Selection::cursor(6), Selection::cursor(7)], 0),
            cx,
        )
    });
    cx.simulate_keystrokes(Keys::PASTE);
    assert_eq!(text_of(&editor, cx), "aa\nbb\naa\nbb");
}

#[gpui::test]
fn copying_with_a_bare_cursor_pastes_a_whole_line_above(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one\ntwo", None, cx);
    cx.simulate_keystrokes(Keys::COPY);
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes(Keys::PASTE);
    assert_eq!(text_of(&editor, cx), "one\none\ntwo");
}

#[gpui::test]
fn cut_with_a_bare_cursor_removes_the_line(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one\ntwo\nthree", None, cx);
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes(Keys::CUT);
    assert_eq!(text_of(&editor, cx), "one\nthree");
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes(Keys::PASTE);
    assert_eq!(text_of(&editor, cx), "one\ntwo\nthree");
}

#[gpui::test]
fn pasting_windows_line_endings_normalizes_them(cx: &mut TestAppContext) {
    let (editor, cx) = setup("", None, cx);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("a\r\nb".to_string()));
    cx.simulate_keystrokes(Keys::PASTE);
    assert_eq!(text_of(&editor, cx), "a\nb");
}

#[gpui::test]
fn the_language_decides_indentation(cx: &mut TestAppContext) {
    let (editor, cx) = setup("", Some("/tmp/main.go"), cx);
    cx.simulate_input("{");
    cx.simulate_keystrokes("enter");
    assert_eq!(text_of(&editor, cx), "{\n\t\n}");
}

#[gpui::test]
fn comment_and_duplicate_use_the_keymap(cx: &mut TestAppContext) {
    let (editor, cx) = setup("x", Some("/tmp/a.rs"), cx);
    cx.simulate_keystrokes(Keys::COMMENT);
    assert_eq!(text_of(&editor, cx), "// x");
    cx.simulate_keystrokes(Keys::DUPLICATE);
    assert_eq!(text_of(&editor, cx), "// x\n// x");
}

#[gpui::test]
fn select_all_and_replace(cx: &mut TestAppContext) {
    let (editor, cx) = setup("everything", None, cx);
    cx.simulate_keystrokes(Keys::SELECT_ALL);
    cx.simulate_input("z");
    assert_eq!(text_of(&editor, cx), "z");
}

/// Two views on one buffer, as split panes will have.
fn setup_split<'a>(
    text: &str,
    cx: &'a mut TestAppContext,
) -> (
    Entity<EditorView>,
    Entity<EditorView>,
    &'a mut VisualTestContext,
) {
    init(cx);
    let (host, cx) = cx.add_window_view(|window, cx| {
        let buffer = cx.new(|cx| BufferModel::new(text, None, cx));
        let a = cx.new(|cx| EditorView::new(buffer.clone(), window, cx));
        let b = cx.new(|cx| EditorView::new(buffer, window, cx));
        Host {
            children: vec![a, b],
        }
    });
    let (a, b) = host.read_with(cx, |h, _| (h.children[0].clone(), h.children[1].clone()));
    (a, b, cx)
}

#[gpui::test]
fn split_views_share_text_but_not_selections(cx: &mut TestAppContext) {
    let (a, b, cx) = setup_split("one two", cx);
    b.update(cx, |e, cx| e.set_selections(SelectionSet::cursor(4), cx));

    focus(&a, cx);
    cx.simulate_input("X");
    assert_eq!(text_of(&b, cx), "Xone two");
    assert_eq!(heads(&a, cx), vec![1]);
    // The other view's cursor followed its text.
    assert_eq!(heads(&b, cx), vec![5]);
}

#[gpui::test]
fn a_view_does_not_transform_its_own_selections_twice(cx: &mut TestAppContext) {
    let (a, _b, cx) = setup_split("abc", cx);
    focus(&a, cx);
    cx.simulate_input("XY");
    assert_eq!(heads(&a, cx), vec![2]);
}

#[gpui::test]
fn undo_in_one_split_reverts_edits_from_the_other(cx: &mut TestAppContext) {
    let (a, b, cx) = setup_split("abc", cx);
    focus(&a, cx);
    cx.simulate_input("X");
    focus(&b, cx);
    cx.simulate_keystrokes(Keys::UNDO);
    assert_eq!(text_of(&a, cx), "abc");
    assert_eq!(heads(&a, cx), vec![0]);
}

#[gpui::test]
fn deleting_text_under_another_views_cursor_keeps_it_valid(cx: &mut TestAppContext) {
    let (a, b, cx) = setup_split("😀😀😀", cx);
    b.update(cx, |e, cx| e.set_selections(SelectionSet::cursor(8), cx));
    focus(&a, cx);
    cx.simulate_keystrokes(Keys::SELECT_ALL);
    cx.simulate_keystrokes("backspace");
    assert_eq!(heads(&b, cx), vec![0]);
}

fn text_point(
    editor: &Entity<EditorView>,
    line: usize,
    col: usize,
    cx: &mut VisualTestContext,
) -> Point<Pixels> {
    cx.run_until_parked();
    let (geometry, offset) = editor.read_with(cx, |e, cx| {
        let buffer = e.buffer().read(cx).buffer();
        (
            e.geometry.clone().expect("a frame was drawn"),
            buffer.point_to_offset(gleipnir_text::Point::new(line, col)),
        )
    });
    let bounds = editor.read_with(cx, |e, cx| {
        geometry.bounds_for_offset(offset, e.buffer().read(cx).buffer())
    });
    let bounds = bounds.expect("the position is on screen");
    point(bounds.left() + px(1.), bounds.top() + px(2.))
}

#[gpui::test]
fn clicking_places_the_cursor(cx: &mut TestAppContext) {
    let (editor, cx) = setup("hello\nworld", None, cx);
    let target = text_point(&editor, 1, 3, cx);
    cx.simulate_click(target, Modifiers::none());
    assert_eq!(heads(&editor, cx), vec![6 + 3]);
}

#[gpui::test]
fn clicking_the_left_half_of_the_last_character_goes_before_it(cx: &mut TestAppContext) {
    // GPUI's own lookup has no glyph past the last one to compare with, so it always returns the
    // end of the line here.
    let (editor, cx) = setup("abc", None, cx);
    let target = text_point(&editor, 0, 2, cx);
    cx.simulate_click(target, Modifiers::none());
    assert_eq!(heads(&editor, cx), vec![2]);
}

#[gpui::test]
fn clicking_the_right_half_of_the_last_character_goes_after_it(cx: &mut TestAppContext) {
    let (editor, cx) = setup("abc", None, cx);
    let target = text_point(&editor, 0, 2, cx);
    let width = editor.read_with(cx, |e, _| e.geometry.as_ref().unwrap().char_width);
    cx.simulate_click(point(target.x + width * 0.7, target.y), Modifiers::none());
    assert_eq!(heads(&editor, cx), vec![3]);
}

#[gpui::test]
fn clicking_past_the_end_of_a_line_goes_to_its_end(cx: &mut TestAppContext) {
    let (editor, cx) = setup("hi\nthere", None, cx);
    let target = text_point(&editor, 0, 0, cx);
    cx.simulate_click(point(target.x + px(500.), target.y), Modifiers::none());
    assert_eq!(heads(&editor, cx), vec![2]);
}

#[gpui::test]
fn clicking_in_a_tab_aligned_line_maps_back_to_the_source(cx: &mut TestAppContext) {
    let (editor, cx) = setup("\tab\n", Some("/tmp/main.go"), cx);
    let target = text_point(&editor, 0, 2, cx);
    cx.simulate_click(target, Modifiers::none());
    assert_eq!(heads(&editor, cx), vec![2]);
}

#[gpui::test]
fn dragging_selects_and_keeps_going_outside_the_editor(cx: &mut TestAppContext) {
    let (editor, cx) = setup("hello\nworld", None, cx);
    let start = text_point(&editor, 0, 1, cx);
    let end = text_point(&editor, 1, 2, cx);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    assert_eq!(ranges(&editor, cx), vec![1..8]);
    // Far past the bottom of the window: selects to the last line.
    cx.simulate_mouse_move(
        point(end.x, px(5000.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    assert_eq!(heads(&editor, cx), vec![8]);
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(start, None, Modifiers::none());
    assert_eq!(heads(&editor, cx), vec![8], "the drag should have ended");
}

#[gpui::test]
fn alt_shift_click_adds_a_cursor(cx: &mut TestAppContext) {
    let (editor, cx) = setup("hello\nworld", None, cx);
    let target = text_point(&editor, 1, 2, cx);
    cx.simulate_click(
        target,
        Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::none()
        },
    );
    assert_eq!(heads(&editor, cx), vec![0, 8]);
    cx.simulate_input("!");
    assert_eq!(text_of(&editor, cx), "!hello\nwo!rld");
}

#[gpui::test]
fn alt_drag_makes_a_column_selection(cx: &mut TestAppContext) {
    let (editor, cx) = setup("abcd\nefgh\nijkl", None, cx);
    let start = text_point(&editor, 0, 1, cx);
    let end = text_point(&editor, 2, 3, cx);
    let alt = Modifiers {
        alt: true,
        ..Modifiers::none()
    };
    cx.simulate_mouse_down(start, MouseButton::Left, alt);
    cx.simulate_mouse_move(end, MouseButton::Left, alt);
    cx.simulate_mouse_up(end, MouseButton::Left, alt);
    assert_eq!(ranges(&editor, cx), vec![1..3, 6..8, 11..13]);
    cx.simulate_input("X");
    assert_eq!(text_of(&editor, cx), "aXd\neXh\niXl");
}

#[gpui::test]
fn the_extend_selection_keys_grow_and_shrink_through_the_syntax_tree(cx: &mut TestAppContext) {
    let (editor, cx) = setup("fn f() { g(1 + 2); }\n", Some("/tmp/a.rs"), cx);
    wait_for_parse(&editor, cx);
    editor.update(cx, |e, cx| e.set_selections(SelectionSet::cursor(11), cx));
    cx.simulate_keystrokes(Keys::EXTEND);
    assert_eq!(
        ranges(&editor, cx),
        vec![11..12],
        "a bare cursor takes its word first"
    );
    cx.simulate_keystrokes(Keys::EXTEND);
    assert_eq!(ranges(&editor, cx), vec![11..16], "then the expression");
    cx.simulate_keystrokes(Keys::SHRINK);
    assert_eq!(ranges(&editor, cx), vec![11..12]);
    cx.simulate_keystrokes(Keys::SHRINK);
    assert_eq!(ranges(&editor, cx), vec![11..11]);
}

fn wait_for_parse(editor: &Entity<EditorView>, cx: &mut VisualTestContext) {
    cx.executor().allow_parking();
    for _ in 0..200 {
        cx.run_until_parked();
        if !editor.read_with(cx, |e, cx| e.buffer().read(cx).is_parse_pending()) {
            cx.run_until_parked();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the parse never finished");
}

#[gpui::test]
fn only_visible_lines_are_laid_out(cx: &mut TestAppContext) {
    let text = "line\n".repeat(100_000);
    let (editor, cx) = setup(&text, None, cx);
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    let count = editor.read_with(cx, |e, _| e.geometry.as_ref().unwrap().lines.len());
    assert!(count < 50, "{count} lines laid out");
}

#[gpui::test]
fn moving_the_cursor_off_screen_scrolls_to_it(cx: &mut TestAppContext) {
    let text = "line\n".repeat(1000);
    let (editor, cx) = setup(&text, None, cx);
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.simulate_keystrokes(Keys::DOC_END);
    cx.run_until_parked();
    let (first, count) = editor.read_with(cx, |e, _| {
        let g = e.geometry.as_ref().unwrap();
        (g.first_line, g.lines.len())
    });
    assert!(
        first <= 1000 && 1000 < first + count,
        "line 1000 isn't in {first}+{count}"
    );
}

#[gpui::test]
fn scrolling_stops_at_the_end_of_the_document(cx: &mut TestAppContext) {
    let (editor, cx) = setup("a\nb\nc", None, cx);
    cx.run_until_parked();
    editor.update(cx, |e, cx| e.scroll_lines(1000., cx));
    cx.run_until_parked();
    let (scroll, height) = editor.read_with(cx, |e, _| {
        let g = e.geometry.as_ref().unwrap();
        (g.scroll.y, g.line_height)
    });
    assert_eq!(scroll, height * 2.);
}

#[gpui::test]
fn truncated_long_lines_still_edit_correctly(cx: &mut TestAppContext) {
    let long = "x".repeat(100_000);
    let (editor, cx) = setup(&long, None, cx);
    cx.run_until_parked();
    let shown = editor.read_with(cx, |e, _| e.geometry.as_ref().unwrap().lines[0].shown_bytes);
    assert!(shown <= 20_000);
    let end = if cfg!(target_os = "macos") {
        "cmd-right"
    } else {
        "end"
    };
    cx.simulate_keystrokes(end);
    cx.simulate_input("!");
    cx.run_until_parked();
    let text = text_of(&editor, cx);
    assert_eq!(text.len(), 100_001);
    assert!(text.ends_with('!'));
}

#[gpui::test]
fn a_line_with_non_ascii_highlights_without_splitting_characters(cx: &mut TestAppContext) {
    let (editor, cx) = setup(
        "fn é() { let s = \"😀é\"; } // 日本\n",
        Some("/tmp/a.rs"),
        cx,
    );
    wait_for_parse(&editor, cx);
    assert!(editor.read_with(cx, |e, _| e.highlights.as_ref().unwrap().spans.len()) > 3);
}

#[gpui::test]
fn ime_composition_applies_at_the_primary_cursor_and_commits(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let (editor, cx) = setup("ab", None, cx);
    cx.update(|window, cx| {
        editor.update(cx, |e, cx| {
            e.set_selections(SelectionSet::cursor(1), cx);
            e.replace_and_mark_text_in_range(None, "に", None, window, cx);
            assert_eq!(e.marked_text_range(window, cx), Some(1..2));
            e.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
            assert_eq!(e.marked_text_range(window, cx), Some(1..3));
            e.replace_text_in_range(None, "日本", window, cx);
            assert_eq!(e.marked_text_range(window, cx), None);
        });
    });
    assert_eq!(text_of(&editor, cx), "a日本b");
    assert_eq!(heads(&editor, cx), vec![1 + "日本".len()]);
    cx.run_until_parked();
}

#[gpui::test]
fn saving_without_a_path_asks_the_host(cx: &mut TestAppContext) {
    let (editor, cx) = setup("x", None, cx);
    let asked = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = asked.clone();
    cx.update(|_, cx| {
        cx.subscribe(&editor, move |_, event: &crate::EditorEvent, _| {
            if matches!(event, crate::EditorEvent::SaveAsRequested) {
                flag.set(true);
            }
        })
        .detach();
    });
    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.simulate_keystrokes(save);
    assert!(asked.get());
}

#[gpui::test]
fn jumping_to_a_line_far_away_centres_it(cx: &mut TestAppContext) {
    let text = "line\n".repeat(1000);
    let (editor, cx) = setup(&text, None, cx);
    cx.simulate_resize(size(px(800.), px(600.)));
    cx.run_until_parked();
    editor.update(cx, |e, cx| e.jump_to(500, 2, cx));
    cx.run_until_parked();
    let (first, count, head) = editor.read_with(cx, |e, _| {
        let g = e.geometry.as_ref().unwrap();
        (g.first_line, g.lines.len(), e.selections().primary().head)
    });
    assert_eq!(head, 500 * 5 + 2);
    let middle = first + count / 2;
    assert!(
        middle.abs_diff(500) <= 2,
        "line 500 should be centred, the screen shows {first}+{count}"
    );
}

mod decorations;
mod semantics;
