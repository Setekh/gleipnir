use super::*;
use gleipnir_core::BufferStore;
use gleipnir_test_support::{isolate_state, project, resolve, run_until};
use gpui::{Modifiers, TestAppContext, VisualTestContext, size};
use std::path::Path;

const MOD: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "ctrl"
};

struct Setup<'a> {
    workspace: Entity<Workspace>,
    cx: &'a mut VisualTestContext,
    root: PathBuf,
}

fn setup<'a>(files: &[(&str, &str)], cx: &'a mut TestAppContext) -> (tempfile::TempDir, Setup<'a>) {
    isolate_state();
    cx.executor().allow_parking();
    cx.update(|cx| {
        gleipnir_ui::init_theme(cx);
        gleipnir_ui::bind_text_input_keys(cx);
        gleipnir_editor::bind_keys(cx);
        gleipnir_workspace::bind_keys(cx);
        bind_keys(cx);
    });
    let (dir, root) = project(files);
    let store = cx.new(|_| BufferStore::new());
    let project_root = root.clone();
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(store, Some(project_root), window, cx);
        add_to_workspace(&mut workspace, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1200.), px(800.)));
    let project = cx.update(|_, cx| workspace.read(cx).project().cloned().unwrap());
    run_until(cx, |cx| cx.update(|_, cx| !project.read(cx).is_busy()));
    (
        dir,
        Setup {
            workspace,
            cx,
            root,
        },
    )
}

impl Setup<'_> {
    fn popup(&mut self) -> Option<Entity<SearchPopup>> {
        let workspace = self.workspace.clone();
        self.cx
            .update(|_, cx| workspace.read(cx).active_modal::<SearchPopup>())
    }

    fn double_shift(&mut self) {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::none()
        };
        for _ in 0..2 {
            self.cx.simulate_modifiers_change(shift);
            self.cx.simulate_modifiers_change(Modifiers::none());
        }
        self.cx.run_until_parked();
    }

    /// Types into the popup and waits for its results.
    fn search(&mut self, text: &str) -> Vec<String> {
        self.cx.simulate_input(text);
        self.results()
    }

    fn results(&mut self) -> Vec<String> {
        let popup = self.popup().expect("the popup is open");
        run_until(self.cx, |cx| {
            cx.update(|_, cx| !popup.read(cx).is_searching())
        });
        self.cx.update(|_, cx| popup.read(cx).result_labels())
    }

    fn selected(&mut self) -> Option<String> {
        let popup = self.popup()?;
        self.cx.update(|_, cx| popup.read(cx).selected_label())
    }

    fn open(&mut self, relative: &str) {
        let path = self.root.join(relative);
        let workspace = self.workspace.clone();
        let task = self
            .cx
            .update(|window, cx| workspace.update(cx, |w, cx| w.open_path(path, window, cx)));
        resolve(self.cx, task).unwrap();
    }

    fn editor_state(&mut self) -> gleipnir_editor::EditorState {
        let workspace = self.workspace.clone();
        let wait_for = workspace.clone();
        run_until(self.cx, |cx| {
            cx.update(|_, cx| wait_for.read(cx).editor_state(cx).is_some())
        });
        self.cx
            .update(|_, cx| workspace.read(cx).editor_state(cx).unwrap())
    }
}

const FILES: &[(&str, &str)] = &[
    (
        "src/user_controller.rs",
        "pub struct UserController;\nimpl UserController {\n    pub fn handle_request(&self) {}\n}\n",
    ),
    ("src/main.rs", "fn main() {}\n"),
    (
        "web/api.ts",
        "export interface ApiClient { get(): void }\nexport function createClient() {}\n",
    ),
    (
        "server.go",
        "package main\ntype Server struct{}\nfunc (s *Server) Listen() {}\n",
    ),
    ("README.md", "# readme\n"),
];

#[gpui::test]
fn double_shift_opens_the_popup_on_all(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.double_shift();
    let popup = s.popup().expect("double shift opens search");
    assert_eq!(s.cx.update(|_, cx| popup.read(cx).tab()), Tab::All);
}

#[gpui::test]
fn shift_then_a_key_then_shift_does_not_open_it(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.open("src/main.rs");
    let shift = Modifiers {
        shift: true,
        ..Modifiers::none()
    };
    s.cx.simulate_modifiers_change(shift);
    s.cx.simulate_modifiers_change(Modifiers::none());
    s.cx.simulate_keystrokes("shift-a");
    s.cx.simulate_modifiers_change(shift);
    s.cx.simulate_modifiers_change(Modifiers::none());
    assert!(s.popup().is_none());
}

#[gpui::test]
fn the_all_tab_groups_classes_files_and_symbols(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.double_shift();
    let results = s.search("usercontr");
    assert_eq!(
        results,
        [
            "# Classes",
            "class UserController",
            "# Files",
            "src/user_controller.rs"
        ]
    );
    assert_eq!(s.selected().as_deref(), Some("class UserController"));
}

#[gpui::test]
fn camel_humps_find_types(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.cx.simulate_keystrokes(&format!("{MOD}-o"));
    let results = s.search("UsCo");
    assert_eq!(
        results.first().map(String::as_str),
        Some("class UserController")
    );
}

#[gpui::test]
fn the_classes_tab_only_has_types(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.cx.simulate_keystrokes(&format!("{MOD}-o"));
    let results = s.search("s");
    assert!(
        results
            .iter()
            .all(|r| r.starts_with("class") || r.starts_with("interface")),
        "{results:?}"
    );
    assert!(results.contains(&"class Server".to_string()));
}

#[gpui::test]
fn choosing_a_symbol_opens_its_file_at_it(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.double_shift();
    s.cx.simulate_keystrokes("tab tab tab");
    s.search("handlereq");
    assert_eq!(s.selected().as_deref(), Some("method handle_request"));
    s.cx.simulate_keystrokes("enter");
    let state = s.editor_state();
    assert!(state.path.unwrap().ends_with("user_controller.rs"));
    let text = &state.text;
    let expected = text.find("handle_request").unwrap();
    assert_eq!(state.selections, [(expected, expected)]);
    assert!(s.popup().is_none(), "choosing closes the popup");
}

#[gpui::test]
fn go_to_file_with_a_line_number_jumps_there(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.cx.simulate_keystrokes(&format!("{MOD}-shift-o"));
    s.search("usercon:3:9");
    s.cx.simulate_keystrokes("enter");
    let state = s.editor_state();
    let line_start = state.text.match_indices('\n').nth(1).unwrap().0 + 1;
    assert_eq!(state.selections, [(line_start + 8, line_start + 8)]);
}

#[gpui::test]
fn tab_cycles_through_the_tabs(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.double_shift();
    let popup = s.popup().unwrap();
    let tab = |s: &mut Setup| s.cx.update(|_, cx| popup.read(cx).tab());
    s.cx.simulate_keystrokes("tab");
    assert_eq!(tab(&mut s), Tab::Classes);
    s.cx.simulate_keystrokes("tab tab tab tab");
    assert_eq!(tab(&mut s), Tab::All);
    s.cx.simulate_keystrokes("shift-tab");
    assert_eq!(tab(&mut s), Tab::Actions);
}

#[gpui::test]
fn the_same_shortcut_closes_it_and_another_switches_tab(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.cx.simulate_keystrokes(&format!("{MOD}-shift-o"));
    assert!(s.popup().is_some());
    s.cx.simulate_keystrokes(&format!("{MOD}-o"));
    let popup = s.popup().expect("still open, on another tab");
    assert_eq!(s.cx.update(|_, cx| popup.read(cx).tab()), Tab::Classes);
    s.cx.simulate_keystrokes(&format!("{MOD}-o"));
    assert!(s.popup().is_none());
}

#[gpui::test]
fn escape_closes_and_focus_returns_to_the_editor(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.open("src/main.rs");
    s.double_shift();
    s.cx.simulate_input("xyz");
    s.cx.simulate_keystrokes("escape");
    assert!(s.popup().is_none());
    s.cx.simulate_input("typed ");
    assert!(s.editor_state().text.starts_with("typed "));
}

#[gpui::test]
fn actions_can_be_found_and_run_on_the_editor(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.open("src/main.rs");
    s.cx.simulate_keystrokes(&format!("{MOD}-shift-a"));
    let results = s.search("duplicate line");
    assert_eq!(
        results.first().map(String::as_str),
        Some("> Duplicate Line")
    );
    s.cx.simulate_keystrokes("enter");
    s.cx.run_until_parked();
    assert_eq!(s.editor_state().text, "fn main() {}\nfn main() {}\n");

    // It's remembered, and offered first next time.
    s.cx.simulate_keystrokes(&format!("{MOD}-shift-a"));
    let results = s.results();
    assert_eq!(
        results.first().map(String::as_str),
        Some("> Duplicate Line")
    );
}

#[gpui::test]
fn editor_actions_are_dimmed_and_last_without_an_editor(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.cx.simulate_keystrokes(&format!("{MOD}-shift-a"));
    let results = s.results();
    let split = results.iter().position(|r| r == "> Split Right").unwrap();
    let duplicate = results
        .iter()
        .position(|r| r == "> Duplicate Line")
        .unwrap();
    assert!(split < duplicate, "available actions come first");
}

#[gpui::test]
fn recent_files_starts_on_the_previous_file(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.open("src/main.rs");
    s.open("server.go");
    s.cx.simulate_keystrokes(&format!("{MOD}-e"));
    assert_eq!(s.results(), ["server.go", "src/main.rs"]);
    assert_eq!(s.selected().as_deref(), Some("src/main.rs"));
    s.cx.simulate_keystrokes("enter");
    assert!(s.editor_state().path.unwrap().ends_with("main.rs"));
}

#[gpui::test]
fn the_all_tab_offers_recent_files_before_anything_is_typed(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.open("README.md");
    s.double_shift();
    assert_eq!(s.results(), ["# Recent Files", "README.md"]);
}

#[gpui::test]
fn files_created_later_are_found(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(
        s.root.join("brand_new.odin"),
        "package p\nFreshType :: struct {}\n",
    )
    .unwrap();
    s.double_shift();
    s.cx.simulate_input("freshtype");
    let popup = s.popup().unwrap();
    run_until(s.cx, |cx| {
        cx.update(|_, cx| {
            popup
                .read(cx)
                .result_labels()
                .contains(&"class FreshType".to_string())
        })
    });
}

#[gpui::test]
fn clicking_a_result_opens_it(cx: &mut TestAppContext) {
    let (_dir, mut s) = setup(FILES, cx);
    s.double_shift();
    s.search("readme");
    let row =
        s.cx.debug_bounds("search-row-1")
            .expect("the first result is drawn");
    s.cx.simulate_click(row.center(), Modifiers::none());
    assert!(s.editor_state().path.unwrap().ends_with("README.md"));
    let _ = Path::new("");
}
