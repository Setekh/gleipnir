//! Terminals running real programs on a real pseudo-terminal.

use super::*;
use gpui::{Entity, TestAppContext};
use std::time::{Duration, Instant};

fn start(program: &str, args: &[&str], cx: &mut TestAppContext) -> Entity<Session> {
    // The program's output arrives on alacritty's own thread.
    cx.executor().allow_parking();
    cx.update(gleipnir_ui::init_theme);
    let launch = Launch {
        program: Some((
            program.to_string(),
            args.iter().map(|a| a.to_string()).collect(),
        )),
        cwd: std::env::temp_dir(),
        env: [("PS1".to_string(), "$ ".to_string())].into(),
        title: "test".into(),
    };
    cx.update(|cx| Session::spawn(launch, cx)).unwrap()
}

/// Waits, in real time as the program runs on its own, for the screen to show something.
fn wait_for(
    session: &Entity<Session>,
    cx: &mut TestAppContext,
    done: impl Fn(&str) -> bool,
) -> String {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        let text = session.read_with(cx, |s, _| s.screen_lines().join("\n"));
        if done(&text) {
            return text;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "never came:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn a_programs_output_shows_and_its_end_is_heard(cx: &mut TestAppContext) {
    let session = start(
        "/bin/sh",
        &["-c", "printf 'hello \\033[31mred\\033[0m\\n'; exit 3"],
        cx,
    );
    let ended = std::rc::Rc::new(std::cell::Cell::new(None));
    let seen = ended.clone();
    cx.update(|cx| {
        cx.subscribe(&session, move |_, event: &SessionEvent, _| {
            if let SessionEvent::Exited(code) = event {
                seen.set(Some(*code));
            }
        })
        .detach()
    });
    let text = wait_for(&session, cx, |t| t.contains("hello red"));
    assert!(text.starts_with("hello red"), "{text:?}");
    let started = Instant::now();
    while ended.get().is_none() && started.elapsed() < Duration::from_secs(30) {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(ended.get(), Some(Some(3)));
}

#[gpui::test]
fn typing_reaches_the_shell_and_resizing_tells_it(cx: &mut TestAppContext) {
    let session = start("/bin/sh", &[], cx);
    wait_for(&session, cx, |t| t.contains('$'));
    session.update(cx, |s, _| s.type_bytes(b"echo $((6*7))\r".to_vec()));
    wait_for(&session, cx, |t| t.lines().any(|l| l == "42"));
    session.update(cx, |s, _| s.resize(40, 10, 8, 16));
    session.update(cx, |s, _| s.type_bytes(b"stty size\r".to_vec()));
    wait_for(&session, cx, |t| t.lines().any(|l| l == "10 40"));
    // Ctrl+C stops what's running, and the shell carries on.
    session.update(cx, |s, _| s.type_bytes(b"sleep 30\r".to_vec()));
    session.update(cx, |s, _| s.type_bytes(vec![0x03]));
    session.update(cx, |s, _| s.type_bytes(b"echo after\r".to_vec()));
    wait_for(&session, cx, |t| t.lines().any(|l| l == "after"));
}

#[gpui::test]
fn selecting_copies_the_text_across_lines(cx: &mut TestAppContext) {
    let session = start("/bin/sh", &[], cx);
    wait_for(&session, cx, |t| t.contains('$'));
    session.update(cx, |s, _| {
        s.type_bytes(b"echo first; echo second\r".to_vec())
    });
    let text = wait_for(&session, cx, |t| t.lines().any(|l| l == "second"));
    let row = text.lines().position(|l| l == "first").unwrap();
    session.update(cx, |s, _| {
        s.start_selection(
            0,
            row,
            alacritty_terminal::index::Side::Left,
            alacritty_terminal::selection::SelectionType::Simple,
        );
        s.extend_selection(5, row + 1, alacritty_terminal::index::Side::Right);
    });
    let selected = session.read_with(cx, |s, _| s.selected_text());
    assert_eq!(selected.as_deref(), Some("first\nsecond"));
}

#[test]
fn a_bracketed_paste_reaches_the_program_whole() {
    use alacritty_terminal::term::TermMode;
    assert_eq!(
        paste_bytes("ls\n", TermMode::BRACKETED_PASTE),
        b"\x1b[200~ls\n\x1b[201~".to_vec()
    );
}

#[gpui::test]
fn cmd_clicking_a_place_in_a_file_opens_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/x.rs"), "a\nb\nc\n").unwrap();
    cx.executor().allow_parking();
    cx.update(gleipnir_ui::init_theme);
    let launch = Launch {
        program: Some((
            "/bin/sh".into(),
            vec!["-c".into(), "echo 'error at src/x.rs:3:2'; sleep 5".into()],
        )),
        cwd: dir.path().to_path_buf(),
        env: Default::default(),
        title: "test".into(),
    };
    let session = cx.update(|cx| Session::spawn(launch, cx)).unwrap();
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session.clone(), cx));
    let opened = std::rc::Rc::new(std::cell::RefCell::new(None));
    let seen = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &OpenLocation, _| {
            *seen.borrow_mut() = Some((event.0.clone(), event.1, event.2));
        })
        .detach()
    });
    let started = Instant::now();
    while !session.read_with(cx, |s, _| s.screen_lines().join("\n").contains("x.rs:3")) {
        assert!(started.elapsed() < Duration::from_secs(30));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.run_until_parked();
    // On the `x` of the path.
    let at = view.read_with(cx, |v, _| v.cell_center(13, 0));
    let cmd = if cfg!(target_os = "macos") {
        gpui::Modifiers::command()
    } else {
        gpui::Modifiers::control()
    };
    cx.simulate_click(at, cmd);
    let (path, line, column) = opened.borrow().clone().expect("the file opened");
    assert!(path.ends_with("src/x.rs"), "{path:?}");
    assert_eq!((line, column), (2, 1));
}

#[gpui::test]
fn an_agent_starts_through_the_shell_with_what_the_ide_adds(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gleipnir_ui::init_theme);
    let echo = Agent {
        id: "echo",
        name: "Echo",
        command: "echo",
        install: "",
    };
    let launch = crate::agents::test_launch(&echo, &["--from", "the ide", "*"]);
    let session = cx.update(|cx| Session::spawn(launch, cx)).unwrap();
    // Passed through untouched: not split, not expanded.
    wait_for(&session, cx, |t| t.contains("--from the ide *"));

    let missing = Agent {
        id: "missing",
        name: "Nothing",
        command: "gleipnir-no-such-agent",
        install: "brew install nothing",
    };
    let launch = crate::agents::test_launch(&missing, &[]);
    let session = cx.update(|cx| Session::spawn(launch, cx)).unwrap();
    let text = wait_for(&session, cx, |t| t.contains("brew install nothing"));
    assert!(text.contains("Nothing isn't installed"), "{text}");
}

#[gpui::test]
fn the_wheel_reaches_a_program_listening_to_the_mouse(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gleipnir_ui::init_theme);
    // Asks for the mouse as Claude Code does, full-screen: clicks, drags, all motion, in SGR
    // form. Everything it's sent goes to a file.
    let dir = tempfile::tempdir().unwrap();
    let heard = dir.path().join("heard");
    let script = format!(
        "printf '\\033[?1049h\\033[?1000h\\033[?1002h\\033[?1003h\\033[?1006h'; \
         stty raw -echo; echo ready; exec cat > '{}'",
        heard.display()
    );
    let launch = Launch {
        program: Some(("/bin/sh".into(), vec!["-c".into(), script])),
        cwd: std::env::temp_dir(),
        env: Default::default(),
        title: "test".into(),
    };
    let session = cx.update(|cx| Session::spawn(launch, cx)).unwrap();
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session.clone(), cx));
    let started = Instant::now();
    while !session.read_with(cx, |s, _| s.screen_lines().join("\n").contains("ready")) {
        assert!(started.elapsed() < Duration::from_secs(30));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.run_until_parked();
    let at = view.read_with(cx, |v, _| v.cell_center(2, 3));
    let line = view.read_with(cx, |v, _| v.cell_center(0, 1).y - v.cell_center(0, 0).y);
    // Wheel events go to what's under the pointer.
    cx.simulate_mouse_move(at, None, gpui::Modifiers::none());
    cx.simulate_event(gpui::ScrollEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), line)),
        ..Default::default()
    });
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        let text = std::fs::read_to_string(&heard).unwrap_or_default();
        // The pointer arriving at column 3, row 4, then the wheel, up, there; no arrow keys.
        if text.contains("\x1b[<64;3;4M") {
            assert_eq!(text, "\x1b[<35;3;4M\x1b[<64;3;4M");
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "{text:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
