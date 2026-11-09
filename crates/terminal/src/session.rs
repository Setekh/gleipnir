//! One terminal: a program, a shell unless told otherwise, running on a pseudo-terminal, with the
//! screen it draws kept by alacritty_terminal. Output is read on alacritty's own thread; this
//! hears about it, and asks to be drawn, at most once per batch.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{AppContext as _, Context, Entity, EventEmitter, Task};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Lines kept above the screen to scroll back to.
const SCROLLBACK: usize = 10_000;

/// Passes the terminal's events on, from alacritty's thread to the session.
#[derive(Clone)]
pub struct Listener(UnboundedSender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.unbounded_send(event).ok();
    }
}

/// What to run, and where.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// The program and its arguments; none for the user's login shell.
    pub program: Option<(String, Vec<String>)>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    /// What the tab says until the program sets a title.
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The screen changed.
    Output,
    TitleChanged,
    /// The program ended, with its exit code if it has one.
    Exited(Option<i32>),
    Bell,
}

pub struct Session {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    size: WindowSize,
    launch: Launch,
    title: Option<String>,
    exited: Option<Option<i32>>,
    _events: Task<()>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    /// Starts the program at 80 by 24, until the view says how big it is.
    pub fn spawn(launch: Launch, cx: &mut gpui::App) -> Result<Entity<Self>> {
        let size = WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        };
        let mut env = launch.env.clone();
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "gleipnir".into());
        let options = tty::Options {
            shell: launch
                .program
                .clone()
                .map(|(program, args)| tty::Shell::new(program, args)),
            working_directory: Some(launch.cwd.clone()),
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&options, size, 0).context("couldn't start a pseudo-terminal")?;
        let (events_tx, events) = unbounded();
        let listener = Listener(events_tx);
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &TermSize::new(size.num_cols as usize, size.num_lines as usize),
            listener.clone(),
        )));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)
            .context("couldn't read from the terminal")?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(cx.new(|cx| Self::new(term, sender, size, launch, events, cx)))
    }

    fn new(
        term: Arc<FairMutex<Term<Listener>>>,
        sender: EventLoopSender,
        size: WindowSize,
        launch: Launch,
        mut events: futures::channel::mpsc::UnboundedReceiver<Event>,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = cx.spawn(async move |this, cx| {
            while let Some(first) = events.next().await {
                // Everything that's come since, handled together, for one redraw.
                let mut batch = vec![first];
                while let Ok(more) = events.try_recv() {
                    batch.push(more);
                }
                let alive = this.update(cx, |this, cx| {
                    for event in batch {
                        this.handle(event, cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });

        Self {
            term,
            sender,
            size,
            launch,
            title: None,
            exited: None,
            _events: task,
        }
    }

    fn handle(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                cx.emit(SessionEvent::Output)
            }
            Event::Title(title) => {
                self.title = Some(title);
                cx.emit(SessionEvent::TitleChanged);
            }
            Event::ResetTitle => {
                self.title = None;
                cx.emit(SessionEvent::TitleChanged);
            }
            // Answers to the program's questions, like where the cursor is.
            Event::PtyWrite(text) => self.write(text.into_bytes()),
            Event::TextAreaSizeRequest(answer) => self.write(answer(self.size).into_bytes()),
            Event::ColorRequest(index, answer) => {
                let theme = cx.global::<gleipnir_ui::Theme>();
                let rgb = crate::colors::rgb_for_index(index, theme);
                self.write(answer(rgb).into_bytes());
            }
            Event::ClipboardStore(_, text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text))
            }
            // A program reading the clipboard without asking would see whatever was copied
            // last, so it isn't allowed to.
            Event::ClipboardLoad(..) => {}
            Event::Bell => cx.emit(SessionEvent::Bell),
            Event::ChildExit(status) => {
                self.exited = Some(status.code());
                cx.emit(SessionEvent::Exited(status.code()));
            }
            Event::Exit => {
                if self.exited.is_none() {
                    self.exited = Some(None);
                    cx.emit(SessionEvent::Exited(None));
                }
            }
        }
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            self.sender.send(Msg::Input(bytes)).ok();
        }
    }

    /// Sends a key as typed, showing the bottom of the output again.
    pub fn type_bytes(&mut self, bytes: Vec<u8>) {
        {
            let mut term = self.term.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        self.write(bytes);
    }

    pub fn paste(&mut self, text: &str) {
        let bytes = crate::keys::paste_bytes(text, self.mode());
        self.type_bytes(bytes);
    }

    /// Fits the terminal to the space it has, in cells, telling the program.
    pub fn resize(&mut self, columns: u16, lines: u16, cell_width: u16, cell_height: u16) {
        let size = WindowSize {
            num_lines: lines.max(1),
            num_cols: columns.max(2),
            cell_width,
            cell_height,
        };
        if (size.num_lines, size.num_cols) == (self.size.num_lines, self.size.num_cols) {
            return;
        }
        self.size = size;
        self.sender.send(Msg::Resize(size)).ok();
        self.term.lock().resize(TermSize::new(
            size.num_cols as usize,
            size.num_lines as usize,
        ));
    }

    pub fn size(&self) -> (usize, usize) {
        (self.size.num_cols as usize, self.size.num_lines as usize)
    }

    pub fn scroll(&mut self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    pub fn term(&self) -> &Arc<FairMutex<Term<Listener>>> {
        &self.term
    }

    pub fn launch(&self) -> &Launch {
        &self.launch
    }

    /// What the tab says: the program's title, else what it was started as.
    pub fn title(&self) -> &str {
        self.title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&self.launch.title)
    }

    pub fn exited(&self) -> Option<Option<i32>> {
        self.exited
    }

    /// Starts selecting at a cell on screen, counted from the top of what's shown.
    pub fn start_selection(&mut self, column: usize, row: usize, side: Side, kind: SelectionType) {
        let mut term = self.term.lock();
        let point = screen_point(&term, column, row);
        term.selection = Some(Selection::new(kind, point, side));
    }

    pub fn extend_selection(&mut self, column: usize, row: usize, side: Side) {
        let mut term = self.term.lock();
        let point = screen_point(&term, column, row);
        if let Some(selection) = &mut term.selection {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.lock().selection = None;
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    /// The text of the lines on screen, without trailing spaces.
    pub fn screen_lines(&self) -> Vec<String> {
        let term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        (0..term.screen_lines())
            .map(|row| line_text(&term, Line(row as i32 - offset)))
            .collect()
    }

    /// Clears the screen and what's scrolled off it, as Cmd+K does.
    pub fn clear(&mut self) {
        // The shell redraws its prompt on Ctrl+L; the history goes here.
        self.term.lock().grid_mut().clear_history();
        self.write(b"\x0c".to_vec());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing the pseudo-terminal hangs the program up.
        self.sender.send(Msg::Shutdown).ok();
    }
}

/// A cell on screen, counted from the top of what's shown, as a point in the grid.
fn screen_point<T>(term: &Term<T>, column: usize, row: usize) -> Point {
    let offset = term.grid().display_offset() as i32;
    let row = row.min(term.screen_lines().saturating_sub(1)) as i32;
    let column = column.min(term.columns().saturating_sub(1));
    Point::new(Line(row - offset), Column(column))
}

/// A line's text, without trailing spaces or the halves of wide characters.
pub(crate) fn line_text<T>(term: &Term<T>, line: Line) -> String {
    use alacritty_terminal::term::cell::Flags;
    let row = &term.grid()[line];
    let mut text = String::new();
    for column in 0..term.columns() {
        let cell = &row[Column(column)];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        text.push(cell.c);
    }
    text.trim_end().to_string()
}
