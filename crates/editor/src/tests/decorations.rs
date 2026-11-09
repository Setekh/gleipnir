//! A debugger's marks in the gutter and beside the code, from a provider the test controls.

use super::*;
use crate::{BreakpointKind, BreakpointMark, DecorationProvider, EditorEvent};
use gpui::{App, MouseButton};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

#[derive(Default)]
struct Marks {
    breakpoints: Vec<usize>,
    execution: Option<usize>,
    run_lines: Vec<usize>,
    values: Vec<(usize, String)>,
}

struct Fake(Rc<RefCell<Marks>>);

impl DecorationProvider for Fake {
    fn breakpoints(&self, _: &App) -> Vec<BreakpointMark> {
        self.0
            .borrow()
            .breakpoints
            .iter()
            .map(|&line| BreakpointMark {
                line,
                kind: BreakpointKind::Line,
                enabled: true,
                verified: None,
            })
            .collect()
    }

    fn execution_line(&self, _: &App) -> Option<(usize, bool)> {
        self.0.borrow().execution.map(|line| (line, true))
    }

    fn run_lines(&self, _: &App) -> Vec<usize> {
        self.0.borrow().run_lines.clone()
    }

    fn inline_values(&self, lines: Range<usize>, _: &App) -> Vec<(usize, String)> {
        let marks = self.0.borrow();
        marks
            .values
            .iter()
            .filter(|(line, _)| lines.contains(line))
            .cloned()
            .collect()
    }
}

struct Decorated<'a> {
    editor: Entity<EditorView>,
    cx: &'a mut VisualTestContext,
    marks: Rc<RefCell<Marks>>,
    events: Rc<RefCell<Vec<String>>>,
}

fn decorated<'a>(text: &str, cx: &'a mut TestAppContext) -> Decorated<'a> {
    let (editor, cx) = setup(text, Some("/p/main.rs"), cx);
    let marks = Rc::new(RefCell::new(Marks::default()));
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    let provider = Rc::new(Fake(marks.clone()));
    cx.update(|_, cx| {
        editor.update(cx, |e, cx| e.set_decorations(provider, cx));
        cx.subscribe(&editor, move |_, event: &EditorEvent, _| {
            let text = match event {
                EditorEvent::GutterClicked { line } => format!("toggle {line}"),
                EditorEvent::RunClicked { line } => format!("run {line}"),
                EditorEvent::GutterMenu { line, .. } => format!("menu {line}"),
                EditorEvent::GutterTextClicked { line } => format!("text {line}"),
                EditorEvent::GutterTextMenu { line, .. } => format!("text menu {line}"),
                _ => return,
            };
            seen.borrow_mut().push(text);
        })
        .detach();
    });
    cx.run_until_parked();
    Decorated {
        editor,
        cx,
        marks,
        events,
    }
}

impl Decorated<'_> {
    /// A point in the gutter on a line: in the marks column, or the Run column.
    fn gutter(&mut self, line: usize, run: bool) -> gpui::Point<Pixels> {
        self.cx.run_until_parked();
        let geometry = self.editor.read_with(self.cx, |e, _| {
            e.geometry.clone().expect("a frame was drawn")
        });
        let x = if run {
            geometry.text_origin_x - px(8.)
        } else {
            geometry.bounds.left() + px(18.)
        };
        let y = geometry.bounds.top() + geometry.line_height * line as f32 + px(2.);
        point(x, y)
    }

    fn state(&mut self) -> crate::EditorState {
        self.cx.run_until_parked();
        self.editor.read_with(self.cx, |e, cx| e.state(cx))
    }
}

#[gpui::test]
fn clicking_the_gutter_asks_for_a_breakpoint_instead_of_selecting(cx: &mut TestAppContext) {
    let mut d = decorated("one\ntwo\nthree", cx);
    let target = d.gutter(1, false);
    d.cx.simulate_click(target, Modifiers::none());
    assert_eq!(*d.events.borrow(), ["toggle 1"]);
    assert_eq!(heads(&d.editor, d.cx), vec![0], "the cursor didn't move");
    // A click on the numbers' right with no Run icon there still means a breakpoint.
    let target = d.gutter(2, true);
    d.cx.simulate_click(target, Modifiers::none());
    assert_eq!(d.events.borrow().last().unwrap(), "toggle 2");
}

#[gpui::test]
fn a_run_icon_runs_and_a_right_click_asks_for_a_menu(cx: &mut TestAppContext) {
    let mut d = decorated("fn main() {\n}\n", cx);
    d.marks.borrow_mut().run_lines = vec![0];
    d.cx.update(|window, _| window.refresh());
    let target = d.gutter(0, true);
    d.cx.simulate_click(target, Modifiers::none());
    let target = d.gutter(1, false);
    d.cx.simulate_mouse_down(target, MouseButton::Right, Modifiers::none());
    assert_eq!(*d.events.borrow(), ["run 0", "menu 1"]);
}

#[gpui::test]
fn marks_and_values_come_from_the_provider(cx: &mut TestAppContext) {
    let mut d = decorated("let a = 1;\nlet b = a + 1;\nprint(b);", cx);
    {
        let mut marks = d.marks.borrow_mut();
        marks.breakpoints = vec![1];
        marks.execution = Some(1);
        marks.values = vec![(0, "a = 1".into()), (1, "b = 2".into())];
    }
    d.cx.update(|window, _| window.refresh());
    let state = d.state();
    assert_eq!(state.breakpoints, [2]);
    assert_eq!(state.execution_line, Some(2));
    assert_eq!(state.inline_values, ["1: a = 1", "2: b = 2"]);
}

#[gpui::test]
fn without_a_provider_the_gutter_selects_lines_as_before(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one\ntwo\nthree", None, cx);
    cx.run_until_parked();
    let geometry = editor.read_with(cx, |e, _| e.geometry.clone().unwrap());
    let target = point(
        geometry.bounds.left() + px(4.),
        geometry.bounds.top() + geometry.line_height + px(2.),
    );
    cx.simulate_click(target, Modifiers::none());
    assert_eq!(ranges(&editor, cx), vec![4..8]);
    let state = editor.read_with(cx, |e, cx| e.state(cx));
    assert!(state.breakpoints.is_empty());
}

#[gpui::test]
fn tapping_the_modifier_then_holding_it_makes_arrows_add_carets(cx: &mut TestAppContext) {
    let (editor, cx) = setup("one\ntwo\nthree\n", None, cx);
    let held = if cfg!(target_os = "macos") {
        Modifiers::alt()
    } else {
        Modifiers::control()
    };
    let arrow = if cfg!(target_os = "macos") {
        "alt-down"
    } else {
        "ctrl-down"
    };
    // Tap, then press and hold.
    cx.simulate_modifiers_change(held);
    cx.simulate_modifiers_change(Modifiers::none());
    cx.simulate_modifiers_change(held);
    cx.run_until_parked();
    let active = editor.read_with(cx, |e, _| e.caret_gesture.active);
    assert!(active, "the gesture is on");
    cx.simulate_keystrokes(arrow);
    cx.simulate_keystrokes(arrow);
    assert_eq!(heads(&editor, cx).len(), 3);
    cx.simulate_modifiers_change(Modifiers::none());

    // Without the tap, the same keys do what they normally do.
    cx.simulate_keystrokes("escape");
    cx.simulate_modifiers_change(held);
    cx.simulate_keystrokes(arrow);
    assert_eq!(heads(&editor, cx).len(), 1);
}

#[gpui::test]
fn text_beside_lines_widens_the_gutter_shows_details_and_reports_clicks(cx: &mut TestAppContext) {
    use crate::{GutterLabel, GutterText};
    let mut d = decorated("one\ntwo\nthree", cx);
    let geometry = |d: &mut Decorated| {
        d.cx.run_until_parked();
        d.editor
            .read_with(d.cx, |e, _| e.geometry.clone().expect("a frame was drawn"))
    };
    let plain = geometry(&mut d).text_origin_x;
    let label = |text: &str| {
        Some(GutterLabel {
            text: text.to_string().into(),
            group: 0,
            shade: 1.,
        })
    };
    d.editor.update(d.cx, |e, cx| {
        e.set_gutter_text(
            Some(GutterText {
                lines: vec![label("2024-01-02 Ada"), None, label("2024-01-02 Ada")],
                details: vec!["1234abcd  Ada\n\nA message".into()],
            }),
            cx,
        )
    });
    let g = geometry(&mut d);
    assert!(g.text_origin_x > plain, "the gutter makes room");
    assert_eq!(
        d.state().gutter_text,
        ["1: 2024-01-02 Ada", "3: 2024-01-02 Ada"]
    );
    let at = |line: usize| {
        point(
            g.bounds.left() + px(4.),
            g.bounds.top() + g.line_height * line as f32 + px(2.),
        )
    };
    // Resting on a label shows what it's about; a line without one shows nothing.
    d.cx.simulate_mouse_move(at(0), None, Modifiers::none());
    assert!(d.cx.debug_bounds("gutter-text-details").is_some());
    d.cx.simulate_mouse_move(at(1), None, Modifiers::none());
    assert!(d.cx.debug_bounds("gutter-text-details").is_none());
    // Clicks on labels are reported, and a right-click asks for a menu.
    d.cx.simulate_click(at(1), Modifiers::none());
    d.cx.simulate_click(at(2), Modifiers::none());
    d.cx.simulate_mouse_down(at(0), MouseButton::Right, Modifiers::none());
    assert_eq!(*d.events.borrow(), ["text 2", "text menu 0"]);
    // What was in the gutter before still works, moved over.
    let target = point(
        g.text_origin_x - px(8.),
        g.bounds.top() + g.line_height + px(2.),
    );
    d.cx.simulate_click(target, Modifiers::none());
    assert_eq!(d.events.borrow().last().unwrap(), "toggle 1");
    // Typing a line above moves the labels down with their lines.
    d.editor.update(d.cx, |e, cx| {
        let buffer = e.buffer().clone();
        buffer.update(cx, |b, cx| {
            b.apply_external_edits(vec![gleipnir_text::TextEdit::new(0..0, "zero\n")], cx)
        })
    });
    assert_eq!(
        d.state().gutter_text,
        ["2: 2024-01-02 Ada", "4: 2024-01-02 Ada"]
    );
    d.editor.update(d.cx, |e, cx| e.set_gutter_text(None, cx));
    assert_eq!(geometry(&mut d).text_origin_x, plain);
}
