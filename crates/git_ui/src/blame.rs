//! IntelliJ's Annotate with Git Blame: beside each line in the gutter, who last changed it and
//! when, tinted by how recent the change is. Resting on one shows the commit; clicking it chooses
//! the commit in the log.
//!
//! The blame is of the text being edited, unsaved changes included (`git blame --contents -`),
//! asked for again a moment after typing stops and after each save or commit. Meanwhile the
//! editor moves the labels with the lines and blanks the lines that change.

use crate::{LOG_PANEL, LogPanel};
use anyhow::{Context as _, Result};
use gleipnir_core::{BufferEvent, BufferModel, ProjectEvent};
use gleipnir_editor::{EditorEvent, EditorView, GutterLabel, GutterText};
use gleipnir_index::git_ops;
use gleipnir_workspace::Workspace;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, Global, SharedString, Subscription, Task,
    WeakEntity, Window,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// How long after typing stops the blame is asked for again.
const EDIT_DELAY: Duration = Duration::from_millis(800);
/// Past this many commits, their whole messages aren't read; tooltips show the first line.
const MESSAGE_LIMIT: usize = 400;

/// A commit that last changed some lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlameCommit {
    pub hash: String,
    pub author: String,
    pub email: String,
    /// When it was authored, in seconds since 1970.
    pub time: i64,
    /// The author's time zone, as seconds east of UTC.
    pub offset: i64,
    pub summary: String,
    /// The whole message, when it's been read; otherwise the summary.
    pub message: String,
}

impl BlameCommit {
    /// Lines that aren't committed yet have a hash of zeros.
    pub fn is_committed(&self) -> bool {
        self.hash.bytes().any(|b| b != b'0')
    }

    /// What the gutter shows: the day, and the author's first name.
    pub fn label(&self) -> String {
        let name = self.author.split_whitespace().next().unwrap_or("");
        let name: String = name.chars().take(12).collect();
        format!("{} {name}", date(self.time + self.offset))
    }

    /// What resting on the label shows.
    pub fn details(&self) -> String {
        let short = &self.hash[..self.hash.len().min(8)];
        let local = self.time + self.offset;
        let minutes = local.rem_euclid(86_400) / 60;
        format!(
            "{short}  {} <{}>\n{} {:02}:{:02}\n\n{}",
            self.author,
            self.email,
            date(local),
            minutes / 60,
            minutes % 60,
            self.message.trim_end()
        )
    }
}

/// Who last changed each line of a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    /// For each line, an index into `commits`.
    pub lines: Vec<usize>,
}

/// Reads `git blame --porcelain` a line at a time. Each run of lines starts with a header,
/// `<hash> <original line> <final line> [<count>]`; the first time a commit appears, lines
/// about it follow (`author`, `author-time`, `summary` and so on); and each line of the file
/// follows its header, after a tab.
#[derive(Default)]
pub struct BlameParser {
    blame: Blame,
    by_hash: HashMap<String, usize>,
    /// The commit and the final line, zero-based, of the line whose header was read last.
    current: Option<(usize, usize)>,
}

impl BlameParser {
    pub fn push_line(&mut self, line: &str) {
        if line.starts_with('\t') {
            if let Some((commit, final_line)) = self.current.take() {
                let lines = &mut self.blame.lines;
                if lines.len() <= final_line {
                    lines.resize(final_line + 1, commit);
                }
                lines[final_line] = commit;
            }
            return;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if let Some(final_line) = header(key, value) {
            let next = self.blame.commits.len();
            let commit = *self.by_hash.entry(key.to_string()).or_insert(next);
            if commit == next {
                self.blame.commits.push(BlameCommit {
                    hash: key.to_string(),
                    ..Default::default()
                });
            }
            self.current = Some((commit, final_line));
            return;
        }
        let Some((commit, _)) = self.current else {
            return;
        };
        let commit = &mut self.blame.commits[commit];
        match key {
            "author" => commit.author = value.to_string(),
            "author-mail" => {
                commit.email = value.trim_start_matches('<').trim_end_matches('>').into()
            }
            "author-time" => commit.time = value.parse().unwrap_or(0),
            "author-tz" => commit.offset = zone_offset(value),
            "summary" => {
                commit.summary = value.to_string();
                commit.message = value.to_string();
            }
            _ => {}
        }
    }

    pub fn finish(self) -> Blame {
        self.blame
    }
}

/// The final line, zero-based, if this is a header: a hash, then two or three numbers.
fn header(key: &str, value: &str) -> Option<usize> {
    if key.len() < 40 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let numbers: Vec<usize> = value
        .split(' ')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match numbers.as_slice() {
        [_, final_line] | [_, final_line, _] if *final_line > 0 => Some(final_line - 1),
        _ => None,
    }
}

pub fn parse_blame(output: &str) -> Blame {
    let mut parser = BlameParser::default();
    for line in output.lines() {
        parser.push_line(line);
    }
    parser.finish()
}

/// `+0130` as seconds.
fn zone_offset(zone: &str) -> i64 {
    let (sign, digits) = match zone.as_bytes().first() {
        Some(b'-') => (-1, &zone[1..]),
        Some(b'+') => (1, &zone[1..]),
        _ => (1, zone),
    };
    let number: i64 = digits.parse().unwrap_or(0);
    sign * ((number / 100) * 3600 + (number % 100) * 60)
}

/// The day a time falls on, as `2024-03-14`.
fn date(seconds: i64) -> String {
    // Days to a civil date, after Howard Hinnant's `civil_from_days`.
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Blames a file as `text` has it, unsaved edits included, with each commit's whole message.
pub fn blame_text(path: &Path, text: &str) -> Result<Blame> {
    let dir = path.parent().context("the file has no folder")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("the file's name isn't readable")?;
    let output = git_ops::git_with(
        dir,
        &["blame", "--porcelain", "--contents", "-", "--", name],
        &[],
        Some(text),
    )?;
    let mut blame = parse_blame(&output);
    let hashes: Vec<&str> = blame
        .commits
        .iter()
        .filter(|c| c.is_committed())
        .map(|c| c.hash.as_str())
        .collect();
    if !hashes.is_empty() && hashes.len() <= MESSAGE_LIMIT {
        let mut args = vec!["show", "-s", "--format=%H%x1f%B%x1e"];
        args.extend(hashes);
        if let Ok(messages) = git_ops::git(dir, &args) {
            let messages: HashMap<&str, &str> = messages
                .split('\u{1e}')
                .filter_map(|record| record.trim_start().split_once('\u{1f}'))
                .collect();
            for commit in &mut blame.commits {
                if let Some(message) = messages.get(commit.hash.as_str()) {
                    commit.message = message.trim_end().to_string();
                }
            }
        }
    }
    Ok(blame)
}

/// The gutter's labels for a blame: newer commits tinted more strongly, by their order in time.
pub fn gutter_text(blame: &Blame) -> GutterText {
    let mut by_age: Vec<usize> = (0..blame.commits.len())
        .filter(|&i| blame.commits[i].is_committed())
        .collect();
    by_age.sort_by_key(|&i| blame.commits[i].time);
    let mut shade = vec![0.; blame.commits.len()];
    for (rank, &commit) in by_age.iter().enumerate() {
        shade[commit] = if by_age.len() == 1 {
            1.
        } else {
            rank as f32 / (by_age.len() - 1) as f32
        };
    }
    let labels: Vec<SharedString> = blame
        .commits
        .iter()
        .map(|c| SharedString::from(c.label()))
        .collect();
    GutterText {
        lines: blame
            .lines
            .iter()
            .map(|&commit| {
                blame.commits[commit].is_committed().then(|| GutterLabel {
                    text: labels[commit].clone(),
                    group: commit,
                    shade: shade[commit],
                })
            })
            .collect(),
        details: blame
            .commits
            .iter()
            .map(|c| SharedString::from(c.details()))
            .collect(),
    }
}

/// The editors being annotated, each with what keeps its blame current.
#[derive(Default)]
struct Blames(HashMap<EntityId, Entity<BlameSession>>);

impl Global for Blames {}

/// Turns the active editor's annotations on or off.
pub(crate) fn toggle_blame(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(editor) = workspace.active_editor(cx) else {
        workspace.show_message("Open a file to annotate it", cx);
        return;
    };
    if close(&editor, cx) {
        return;
    }
    let buffer = editor.read(cx).buffer().clone();
    if buffer.read(cx).path().is_none() {
        workspace.show_message("Save the file to annotate it", cx);
        return;
    }
    let id = editor.entity_id();
    let project = workspace.project().cloned();
    let handle = workspace.weak_handle();
    let session = cx.new(|cx| {
        let mut subscriptions = vec![
            cx.subscribe(
                &buffer,
                |this: &mut BlameSession, _, event, cx| match event {
                    BufferEvent::Edited { .. } => this.refresh(EDIT_DELAY, cx),
                    BufferEvent::Saved => this.refresh(Duration::ZERO, cx),
                    BufferEvent::DiskStatusChanged => {}
                },
            ),
            cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
                EditorEvent::GutterTextClicked { line } => this.show_in_log(*line, cx),
                EditorEvent::GutterTextMenu { line, position } => {
                    this.open_menu(*line, *position, window, cx)
                }
                _ => {}
            }),
            cx.observe_release(&editor, move |_, _, cx| {
                cx.default_global::<Blames>().0.remove(&id);
            }),
        ];
        if let Some(project) = &project {
            subscriptions.push(cx.subscribe(project, |this, _, event, cx| {
                if matches!(event, ProjectEvent::GitStatusChanged) {
                    this.refresh(Duration::ZERO, cx);
                }
            }));
        }
        let mut session = BlameSession {
            editor: editor.downgrade(),
            buffer,
            workspace: handle,
            blame: None,
            task: None,
            _subscriptions: subscriptions,
        };
        session.refresh(Duration::ZERO, cx);
        session
    });
    cx.default_global::<Blames>().0.insert(id, session);
    workspace.show_message("Annotating…", cx);
}

/// Takes an editor's annotations away; false if it had none.
fn close(editor: &Entity<EditorView>, cx: &mut App) -> bool {
    let had = cx
        .default_global::<Blames>()
        .0
        .remove(&editor.entity_id())
        .is_some();
    if had || editor.read(cx).has_gutter_text() {
        editor.update(cx, |e, cx| e.set_gutter_text(None, cx));
        return true;
    }
    false
}

struct BlameSession {
    editor: WeakEntity<EditorView>,
    buffer: Entity<BufferModel>,
    workspace: WeakEntity<Workspace>,
    /// The latest blame shown, for what each label's commit is.
    blame: Option<Arc<Blame>>,
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl BlameSession {
    /// Blames the text again after `delay`, unless it changes first.
    fn refresh(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let buffer = self.buffer.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Ok((path, text, version)) = this.update(cx, |_, cx| {
                let model = buffer.read(cx);
                (
                    model.path().map(Path::to_path_buf),
                    model.snapshot(),
                    model.buffer().version(),
                )
            }) else {
                return;
            };
            let Some(path) = path else { return };
            let result = cx
                .background_executor()
                .spawn(async move { blame_text(&path, &text.slice(0..text.len())) })
                .await;
            this.update(cx, |this, cx| this.show(result, version, cx))
                .ok();
        }));
    }

    fn show(&mut self, result: Result<Blame>, version: u64, cx: &mut Context<Self>) {
        // A blame of older text would put labels on the wrong lines; the edit since asked again.
        if self.buffer.read(cx).buffer().version() != version {
            return;
        }
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        match result {
            Ok(blame) => {
                let text = gutter_text(&blame);
                self.blame = Some(Arc::new(blame));
                editor.update(cx, |e, cx| e.set_gutter_text(Some(text), cx));
            }
            // Once shown, a failure later, like a file moved meanwhile, keeps what's there.
            Err(error) if self.blame.is_none() => {
                close(&editor, cx);
                self.workspace
                    .update(cx, |w, cx| {
                        w.show_error(format!("Couldn't annotate: {error:#}"), cx)
                    })
                    .ok();
            }
            Err(_) => {}
        }
    }

    /// The commit a line's label is for.
    fn commit_at(&self, line: usize, cx: &App) -> Option<BlameCommit> {
        let group = self.editor.upgrade()?.read(cx).gutter_group(line)?;
        self.blame.as_ref()?.commits.get(group).cloned()
    }

    /// Chooses the line's commit in the Git log, opening it.
    fn show_in_log(&mut self, line: usize, cx: &mut Context<Self>) {
        let Some(commit) = self.commit_at(line, cx) else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| {
                reveal_commit(workspace, &commit.hash, cx)
            })
            .ok();
    }

    fn open_menu(
        &mut self,
        line: usize,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let commit = self.commit_at(line, cx);
        let workspace = self.workspace.clone();
        let editor = self.editor.clone();
        let menu = cx.new(|cx| {
            let mut menu = gleipnir_ui::ContextMenu::new(cx);
            if let Some(commit) = commit {
                let hash = commit.hash.clone();
                let shown = workspace.clone();
                menu = menu
                    .item("Show in Git Log", None, move |_, cx| {
                        shown.update(cx, |w, cx| reveal_commit(w, &hash, cx)).ok();
                    })
                    .item("Copy Hash", None, move |_, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(commit.hash.clone()))
                    })
                    .separator();
            }
            menu.item("Close Annotations", None, move |_, cx| {
                if let Some(editor) = editor.upgrade() {
                    close(&editor, cx);
                }
            })
        });
        self.workspace
            .update(cx, |w, cx| w.show_menu(menu, position, window, cx))
            .ok();
    }
}

/// Opens the Git log on a commit.
fn reveal_commit(workspace: &mut Workspace, hash: &str, cx: &mut Context<Workspace>) {
    workspace.show_panel(LOG_PANEL, cx);
    let Some(log) = workspace.panel::<LogPanel>(cx) else {
        return;
    };
    if !log.update(cx, |log, cx| log.reveal(hash, cx)) {
        let short = &hash[..hash.len().min(8)];
        workspace.show_message(format!("{short} isn't among the log's latest commits"), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn porcelain() -> String {
        [
            &format!("{A} 1 1 2"),
            "author Ada Lovelace",
            "author-mail <ada@example.com>",
            "author-time 1700000000",
            "author-tz +0100",
            "committer Ada Lovelace",
            "summary First lines",
            "boundary",
            "filename a.txt",
            "\tone",
            &format!("{A} 2 2"),
            "\ttwo",
            &format!("{B} 3 3 1"),
            "author Grace Hopper",
            "author-mail <grace@example.com>",
            "author-time 1710000000",
            "author-tz -0500",
            "summary Changed three",
            &format!("previous {A} a.txt"),
            "filename a.txt",
            "\tthree",
            &format!("{ZERO} 4 4 1"),
            "author Not Committed Yet",
            "author-mail <not.committed.yet>",
            "author-time 1720000000",
            "author-tz +0000",
            "summary Version of a.txt from a.txt",
            "filename a.txt",
            "\tfour",
            // A commit seen before gets only its header.
            &format!("{A} 3 5 1"),
            "\tfive",
        ]
        .join("\n")
    }

    #[test]
    fn porcelain_output_gives_each_line_its_commit() {
        let blame = parse_blame(&porcelain());
        assert_eq!(blame.lines, [0, 0, 1, 2, 0]);
        assert_eq!(blame.commits.len(), 3);
        let ada = &blame.commits[0];
        assert_eq!(
            (
                ada.author.as_str(),
                ada.email.as_str(),
                ada.time,
                ada.offset
            ),
            ("Ada Lovelace", "ada@example.com", 1_700_000_000, 3600)
        );
        assert_eq!(ada.summary, "First lines");
        assert_eq!(blame.commits[1].offset, -5 * 3600);
        assert!(!blame.commits[2].is_committed());
    }

    #[test]
    fn a_line_at_a_time_reads_the_same_as_all_at_once() {
        let mut parser = BlameParser::default();
        for line in porcelain().lines() {
            parser.push_line(line);
        }
        assert_eq!(parser.finish(), parse_blame(&porcelain()));
    }

    #[test]
    fn labels_show_the_day_and_first_name_and_newer_lines_are_tinted_more() {
        let blame = parse_blame(&porcelain());
        // 1700000000 is 2023-11-14 22:13 UTC, so 23:13 at +0100.
        assert_eq!(blame.commits[0].label(), "2023-11-14 Ada");
        assert!(
            blame.commits[0].details().starts_with(
                "11111111  Ada Lovelace <ada@example.com>\n2023-11-14 23:13\n\nFirst lines"
            ),
            "{}",
            blame.commits[0].details()
        );
        let text = gutter_text(&blame);
        let shades: Vec<Option<f32>> = text
            .lines
            .iter()
            .map(|l| l.as_ref().map(|l| l.shade))
            .collect();
        // Uncommitted lines have no label.
        assert_eq!(shades, [Some(0.), Some(0.), Some(1.), None, Some(0.)]);
        assert_eq!(text.lines[2].as_ref().unwrap().text, "2024-03-09 Grace");
        assert_eq!(text.lines[2].as_ref().unwrap().group, 1);
    }

    #[test]
    fn dates_are_right_across_years_and_before_1970() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(-86_400), "1969-12-31");
        assert_eq!(zone_offset("+0530"), 5 * 3600 + 30 * 60);
    }
}
