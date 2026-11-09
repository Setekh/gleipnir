use super::*;
use gleipnir_editor::EditorView;
use gpui::{AppContext as _, Focusable as _, TestAppContext, VisualTestContext};

/// The screenshot's case in a real editor: the field's closing parenthesis typed, and the comma
/// every sibling has offered, at 100%.
#[gpui::test]
fn the_missing_comma_is_offered_after_typing_and_taken_with_tab(cx: &mut TestAppContext) {
    gleipnir_test_support::isolate_state();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_editor::bind_keys(cx);
    });
    let text = "export const ProvisioningData = types.model({\n  inputCode: types.maybe(types.string),\n  address: types.maybe(ProvisioningAddress),\n  address2: types.maybe(ProvisioningAddress\n  partyName: types.maybe(types.string),\n})\n";
    let buffer = cx.new(|cx| BufferModel::new(text, Some("/tmp/store.ts".into()), cx));
    let (editor, cx) = cx.add_window_view(|window, cx| {
        let mut editor = EditorView::new(buffer.clone(), window, cx);
        editor.set_suggester(Rc::new(FileSuggester));
        editor
    });
    let offset = text.find("ProvisioningAddress\n").unwrap() + "ProvisioningAddress".len();
    let point = buffer.read_with(cx, |b, _| b.buffer().offset_to_point(offset));
    editor.update(cx, |e, cx| e.jump_to(point.line, point.col, cx));
    cx.update(|window, cx| {
        let handle = editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
    cx.simulate_input(")");
    let suggestion = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        editor.read_with(cx, |e, _| e.suggestion().cloned())
    };
    let shown = suggestion(cx).expect("a suggestion");
    assert_eq!(shown.text, ",");
    assert_eq!(shown.score, Some(1.0));
    cx.simulate_keystrokes("tab");
    let now = buffer.read_with(cx, |b, _| b.buffer().text());
    assert!(
        now.contains("address2: types.maybe(ProvisioningAddress),\n"),
        "{now}"
    );
}
