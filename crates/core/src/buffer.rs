use anyhow::{Context as _, Result};
use gleipnir_packages::registry;
use gleipnir_syntax::{Language, Syntax};
use gleipnir_text::ops::{EditConfig, EditContext};
use gleipnir_text::{AppliedEdit, Bias, Buffer, SelectionSet, Snapshot};
use gpui::{AppContext as _, Context, EntityId, EventEmitter, Task};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

/// How the document relates to its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskStatus {
    /// Nothing unusual. The buffer may still have unsaved edits.
    InSync,
    /// The file changed on disk while the buffer had unsaved edits, so neither was overwritten.
    Conflict,
    /// The file no longer exists.
    Deleted,
}

#[derive(Clone, Debug)]
pub enum BufferEvent {
    /// The text changed. `origin` is the view that made the change, which has already updated its
    /// own selections; `None` means it came from outside, like a reload.
    Edited {
        edits: Arc<Vec<AppliedEdit>>,
        origin: Option<EntityId>,
        /// The buffer's version once these edits are applied.
        version: u64,
    },
    Saved,
    DiskStatusChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

/// A problem a language server reported, positioned in this buffer. Ranges follow edits made
/// after it was reported, until the server sends fresh ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: std::ops::Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
}

/// One open document, shared by every view showing it.
pub struct BufferModel {
    buffer: Buffer,
    syntax: Option<Syntax>,
    edit_config: EditConfig,
    path: Option<PathBuf>,
    /// The file's modification time when we last read or wrote it.
    mtime: Option<SystemTime>,
    disk_status: DiskStatus,
    parsing: bool,
    checking_disk: bool,
    diagnostics: Vec<Diagnostic>,
}

impl EventEmitter<BufferEvent> for BufferModel {}

impl BufferModel {
    pub fn new(text: &str, path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        Self::with_mtime(text, path, None, cx)
    }

    pub(crate) fn with_mtime(
        text: &str,
        path: Option<PathBuf>,
        mtime: Option<SystemTime>,
        cx: &mut Context<Self>,
    ) -> Self {
        let entry = path
            .as_deref()
            .and_then(|p| registry().language_for_path(p));
        let mut this = Self {
            buffer: Buffer::new(text),
            syntax: entry.map(|e| Syntax::new(e.grammar)),
            edit_config: entry.map(|e| e.edit_config.clone()).unwrap_or_default(),
            path,
            mtime,
            disk_status: DiskStatus::InSync,
            parsing: false,
            checking_disk: false,
            diagnostics: Vec::new(),
        };
        this.schedule_parse(cx);
        this
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn syntax(&self) -> Option<&Syntax> {
        self.syntax.as_ref()
    }

    pub fn language(&self) -> Option<Language> {
        self.syntax.as_ref().map(Syntax::language)
    }

    pub fn edit_config(&self) -> &EditConfig {
        &self.edit_config
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn disk_status(&self) -> DiskStatus {
        self.disk_status
    }

    pub fn snapshot(&self) -> Snapshot {
        self.buffer.snapshot()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Replaces the diagnostics, sorted by position, most severe first where they start together.
    pub fn set_diagnostics(&mut self, mut diagnostics: Vec<Diagnostic>, cx: &mut Context<Self>) {
        diagnostics.sort_by_key(|d| (d.range.start, d.severity, d.range.end));
        self.diagnostics = diagnostics;
        cx.notify();
    }

    /// Applies edits that don't come from a view, like a rename in another file, as one undo step.
    pub fn apply_external_edits(
        &mut self,
        edits: Vec<gleipnir_text::TextEdit>,
        cx: &mut Context<Self>,
    ) {
        let mut selections = gleipnir_text::SelectionSet::cursor(0);
        let applied = {
            let mut edit_cx = EditContext {
                buffer: &mut self.buffer,
                selections: &mut selections,
                config: &self.edit_config,
                now: Instant::now(),
            };
            edit_cx.apply_edits(edits, None, gleipnir_text::UndoGrouping::Distinct)
        };
        self.did_edit(applied, None, cx);
    }

    pub fn break_undo_group(&mut self) {
        self.buffer.break_undo_group();
    }

    /// Runs an editing command for the view `origin`, which owns `selections`.
    pub fn edit<R>(
        &mut self,
        selections: &mut SelectionSet,
        origin: EntityId,
        cx: &mut Context<Self>,
        command: impl FnOnce(&mut EditContext) -> (R, Vec<AppliedEdit>),
    ) -> R {
        let mut edit_cx = EditContext {
            buffer: &mut self.buffer,
            selections,
            config: &self.edit_config,
            now: Instant::now(),
        };
        let (result, applied) = command(&mut edit_cx);
        self.did_edit(applied, Some(origin), cx);
        result
    }

    /// Reverts the last change, returning the selections to restore in the view that asked.
    pub fn undo(&mut self, origin: EntityId, cx: &mut Context<Self>) -> Option<SelectionSet> {
        let (applied, selections) = self.buffer.undo()?;
        self.did_edit(applied, Some(origin), cx);
        Some(selections)
    }

    pub fn redo(&mut self, origin: EntityId, cx: &mut Context<Self>) -> Option<SelectionSet> {
        let (applied, selections) = self.buffer.redo()?;
        self.did_edit(applied, Some(origin), cx);
        Some(selections)
    }

    fn did_edit(
        &mut self,
        applied: Vec<AppliedEdit>,
        origin: Option<EntityId>,
        cx: &mut Context<Self>,
    ) {
        if applied.is_empty() {
            return;
        }
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(&applied, self.buffer.version());
        }
        for diagnostic in &mut self.diagnostics {
            for edit in &applied {
                let range = &mut diagnostic.range;
                // Text typed at either edge isn't part of the problem: the start moves along with
                // the text after it, and the end stays with the text before it.
                range.start = gleipnir_text::transform_offset(range.start, edit, Bias::Right);
                range.end =
                    gleipnir_text::transform_offset(range.end, edit, Bias::Left).max(range.start);
            }
        }
        self.schedule_parse(cx);
        cx.emit(BufferEvent::Edited {
            edits: Arc::new(applied),
            origin,
            version: self.buffer.version(),
        });
        cx.notify();
    }

    /// Parses on a background thread, at most one job at a time. A new parse starts when one
    /// finishes if edits arrived meanwhile, so a burst of typing collapses into few parses.
    fn schedule_parse(&mut self, cx: &mut Context<Self>) {
        if self.parsing {
            return;
        }
        let version = self.buffer.version();
        let Some(syntax) = self.syntax.as_ref().filter(|s| s.needs_parse(version)) else {
            return;
        };
        self.parsing = true;
        let job = syntax.start_parse(self.buffer.snapshot());
        let parse = cx.background_spawn(async move { job.run() });
        cx.spawn(async move |this, cx| {
            let parsed = parse.await;
            this.update(cx, |this, cx| {
                this.parsing = false;
                if let (Some(syntax), Some(parsed)) = (&mut this.syntax, parsed) {
                    syntax.install(parsed);
                    this.schedule_parse(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether a background parse is running or still needed. Lets tests wait for highlighting.
    pub fn is_parse_pending(&self) -> bool {
        self.parsing
            || self
                .syntax
                .as_ref()
                .is_some_and(|s| s.needs_parse(self.buffer.version()))
    }

    /// Writes the document to its file. On failure the buffer stays dirty and nothing is lost.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        let Some(path) = self.path.clone() else {
            return Task::ready(Err(anyhow::anyhow!("this buffer has no file path")));
        };
        let token = self.buffer.begin_save();
        let text = token.text.clone();
        let write = cx.background_spawn(async move { write_atomically(&path, &text) });
        cx.spawn(async move |this, cx| {
            let mtime = write.await?;
            this.update(cx, |this, cx| {
                this.buffer.finish_save(&token);
                this.mtime = Some(mtime);
                this.set_disk_status(DiskStatus::InSync, cx);
                cx.emit(BufferEvent::Saved);
                cx.notify();
            })
        })
    }

    /// Writes to a new path, which becomes this buffer's file. The language can change with it.
    pub fn save_as(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.set_path(path, cx);
        self.save(cx)
    }

    /// Points the buffer at a file's new location after a rename or move, keeping its contents and
    /// any unsaved edits.
    pub fn file_moved(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.set_path(path, cx);
        cx.emit(BufferEvent::DiskStatusChanged);
        cx.notify();
    }

    fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let entry = registry().language_for_path(&path);
        if entry.map(|e| e.grammar) != self.language() {
            self.syntax = entry.map(|e| Syntax::new(e.grammar));
            self.edit_config = entry.map(|e| e.edit_config.clone()).unwrap_or_default();
            self.schedule_parse(cx);
        }
        self.path = Some(path);
    }

    /// Compares the buffer with its file. A clean buffer picks up changes silently. A dirty one
    /// keeps its edits and is flagged instead, since either side could be the one to keep.
    pub fn check_disk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.checking_disk {
            return;
        }
        self.checking_disk = true;
        let known = self.mtime;
        let read = cx.background_spawn(async move { read_if_changed(&path, known) });
        cx.spawn(async move |this, cx| {
            let change = read.await;
            this.update(cx, |this, cx| {
                this.checking_disk = false;
                this.apply_disk_change(change, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_disk_change(&mut self, change: DiskChange, cx: &mut Context<Self>) {
        match change {
            DiskChange::Unchanged | DiskChange::Unreadable => {}
            DiskChange::Deleted => self.set_disk_status(DiskStatus::Deleted, cx),
            DiskChange::Modified { text, mtime } => {
                let identical = self.buffer.text_for_save() == text;
                self.mtime = Some(mtime);
                if identical {
                    self.set_disk_status(DiskStatus::InSync, cx);
                } else if self.buffer.is_dirty() {
                    self.set_disk_status(DiskStatus::Conflict, cx);
                } else {
                    self.reload(&text, cx);
                }
            }
        }
    }

    /// Replaces the document with what's on disk, discarding unsaved edits. Called directly when
    /// the user resolves a conflict in favour of the file.
    pub fn reload_from_disk(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        let Some(path) = self.path.clone() else {
            return Task::ready(Ok(()));
        };
        let read = cx.background_spawn(async move { read_text_file(&path) });
        cx.spawn(async move |this, cx| {
            let (text, mtime) = read.await?;
            this.update(cx, |this, cx| {
                this.mtime = Some(mtime);
                this.reload(&text, cx);
            })
        })
    }

    fn reload(&mut self, text: &str, cx: &mut Context<Self>) {
        let edit = self.buffer.reset(text);
        if let Some(syntax) = &mut self.syntax {
            syntax.reset(self.buffer.version());
        }
        self.schedule_parse(cx);
        self.set_disk_status(DiskStatus::InSync, cx);
        cx.emit(BufferEvent::Edited {
            edits: Arc::new(vec![edit]),
            origin: None,
            version: self.buffer.version(),
        });
        cx.notify();
    }

    fn set_disk_status(&mut self, status: DiskStatus, cx: &mut Context<Self>) {
        if self.disk_status != status {
            self.disk_status = status;
            cx.emit(BufferEvent::DiskStatusChanged);
            cx.notify();
        }
    }
}

enum DiskChange {
    Unchanged,
    /// A transient failure, or not valid text any more. Leave the buffer alone rather than guess.
    Unreadable,
    Deleted,
    Modified {
        text: String,
        mtime: SystemTime,
    },
}

fn read_if_changed(path: &Path, known: Option<SystemTime>) -> DiskChange {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return DiskChange::Deleted,
        Err(_) => return DiskChange::Unreadable,
    };
    if metadata.modified().ok() == known {
        return DiskChange::Unchanged;
    }
    match read_text_file(path) {
        Ok((text, mtime)) => DiskChange::Modified { text, mtime },
        Err(_) => DiskChange::Unreadable,
    }
}

/// The largest file we'll open. A rope could hold more, but language servers and tree-sitter
/// wouldn't cope, and it's far more often an accidental open of a data file.
const MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;

/// Reads a file as UTF-8. Anything else is an error: loading it lossily and saving would silently
/// corrupt the file.
pub(crate) fn read_text_file(path: &Path) -> Result<(String, SystemTime)> {
    let metadata = fs::metadata(path).with_context(|| format!("can't read {}", path.display()))?;
    if metadata.is_dir() {
        anyhow::bail!("{} is a directory", path.display());
    }
    if metadata.len() > MAX_FILE_SIZE {
        anyhow::bail!(
            "{} is too large to open ({} bytes)",
            path.display(),
            metadata.len()
        );
    }
    let mtime = metadata.modified()?;
    let bytes = fs::read(path).with_context(|| format!("can't read {}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{} is not UTF-8 text", path.display()))?;
    Ok((text, mtime))
}

/// Writes via a temporary file and rename, so a crash or full disk can't leave a half-written file.
/// Writes through symlinks rather than replacing them, and keeps the file's permissions.
fn write_atomically(path: &Path, text: &str) -> Result<SystemTime> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let directory = target
        .parent()
        .context("the path has no parent directory")?;
    let name = target
        .file_name()
        .context("the path has no file name")?
        .to_string_lossy();
    let temporary = directory.join(format!(".{name}.gleipnir-{}.tmp", std::process::id()));

    let result = (|| -> Result<SystemTime> {
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("can't write to {}", directory.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        if let Ok(existing) = fs::metadata(&target) {
            fs::set_permissions(&temporary, existing.permissions())?;
        }
        fs::rename(&temporary, &target)?;
        Ok(fs::metadata(&target)?.modified()?)
    })();
    if result.is_err() {
        fs::remove_file(&temporary).ok();
    }
    result
}
