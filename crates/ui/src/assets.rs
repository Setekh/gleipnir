//! Icons, compiled into the binary so there's nothing to install or lose.

use gleipnir_syntax::Language;
use gpui::{AssetSource, Hsla, Result, SharedString, rgb};
use std::borrow::Cow;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    ChevronRight,
    ChevronDown,
    Folder,
    File,
    Close,
    Tree,
    Search,
    Problems,
    Run,
    Debug,
    Stop,
    Resume,
    Pause,
    StepOver,
    StepInto,
    StepOut,
    Rerun,
    Console,
    Plus,
    Minus,
    Breakpoints,
    RunToCursor,
    Mute,
    TestPassed,
    TestFailed,
    TestSkipped,
    TestRunning,
    RerunFailed,
    Settings,
    Branch,
    Commit,
    Terminal,
    Agent,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::Folder => "icons/folder.svg",
            Self::File => "icons/file.svg",
            Self::Close => "icons/close.svg",
            Self::Tree => "icons/tree.svg",
            Self::Search => "icons/search.svg",
            Self::Problems => "icons/problems.svg",
            Self::Run => "icons/run.svg",
            Self::Debug => "icons/debug.svg",
            Self::Stop => "icons/stop.svg",
            Self::Resume => "icons/resume.svg",
            Self::Pause => "icons/pause.svg",
            Self::StepOver => "icons/step-over.svg",
            Self::StepInto => "icons/step-into.svg",
            Self::StepOut => "icons/step-out.svg",
            Self::Rerun => "icons/rerun.svg",
            Self::Console => "icons/console.svg",
            Self::Plus => "icons/plus.svg",
            Self::Minus => "icons/minus.svg",
            Self::Breakpoints => "icons/breakpoints.svg",
            Self::RunToCursor => "icons/run-cursor.svg",
            Self::Mute => "icons/mute.svg",
            Self::TestPassed => "icons/test-passed.svg",
            Self::TestFailed => "icons/test-failed.svg",
            Self::TestSkipped => "icons/test-skipped.svg",
            Self::TestRunning => "icons/test-running.svg",
            Self::RerunFailed => "icons/rerun-failed.svg",
            Self::Settings => "icons/settings.svg",
            Self::Branch => "icons/branch.svg",
            Self::Commit => "icons/commit.svg",
            Self::Terminal => "icons/terminal.svg",
            Self::Agent => "icons/agent.svg",
            Self::WindowMinimize => "icons/window-minimize.svg",
            Self::WindowMaximize => "icons/window-maximize.svg",
            Self::WindowRestore => "icons/window-restore.svg",
            Self::WindowClose => "icons/window-close.svg",
        }
    }
}

const FILES: &[(&str, &[u8])] = &[
    (
        "icons/window-minimize.svg",
        include_bytes!("../assets/icons/window-minimize.svg"),
    ),
    (
        "icons/window-maximize.svg",
        include_bytes!("../assets/icons/window-maximize.svg"),
    ),
    (
        "icons/window-restore.svg",
        include_bytes!("../assets/icons/window-restore.svg"),
    ),
    (
        "icons/window-close.svg",
        include_bytes!("../assets/icons/window-close.svg"),
    ),
    (
        "icons/agent.svg",
        include_bytes!("../assets/icons/agent.svg"),
    ),
    (
        "icons/terminal.svg",
        include_bytes!("../assets/icons/terminal.svg"),
    ),
    ("icons/run.svg", include_bytes!("../assets/icons/run.svg")),
    (
        "icons/debug.svg",
        include_bytes!("../assets/icons/debug.svg"),
    ),
    ("icons/stop.svg", include_bytes!("../assets/icons/stop.svg")),
    (
        "icons/resume.svg",
        include_bytes!("../assets/icons/resume.svg"),
    ),
    (
        "icons/pause.svg",
        include_bytes!("../assets/icons/pause.svg"),
    ),
    (
        "icons/step-over.svg",
        include_bytes!("../assets/icons/step-over.svg"),
    ),
    (
        "icons/step-into.svg",
        include_bytes!("../assets/icons/step-into.svg"),
    ),
    (
        "icons/step-out.svg",
        include_bytes!("../assets/icons/step-out.svg"),
    ),
    (
        "icons/rerun.svg",
        include_bytes!("../assets/icons/rerun.svg"),
    ),
    (
        "icons/console.svg",
        include_bytes!("../assets/icons/console.svg"),
    ),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    (
        "icons/minus.svg",
        include_bytes!("../assets/icons/minus.svg"),
    ),
    (
        "icons/breakpoints.svg",
        include_bytes!("../assets/icons/breakpoints.svg"),
    ),
    (
        "icons/run-cursor.svg",
        include_bytes!("../assets/icons/run-cursor.svg"),
    ),
    ("icons/mute.svg", include_bytes!("../assets/icons/mute.svg")),
    (
        "icons/test-passed.svg",
        include_bytes!("../assets/icons/test-passed.svg"),
    ),
    (
        "icons/test-failed.svg",
        include_bytes!("../assets/icons/test-failed.svg"),
    ),
    (
        "icons/test-skipped.svg",
        include_bytes!("../assets/icons/test-skipped.svg"),
    ),
    (
        "icons/test-running.svg",
        include_bytes!("../assets/icons/test-running.svg"),
    ),
    (
        "icons/rerun-failed.svg",
        include_bytes!("../assets/icons/rerun-failed.svg"),
    ),
    (
        "icons/settings.svg",
        include_bytes!("../assets/icons/settings.svg"),
    ),
    (
        "icons/branch.svg",
        include_bytes!("../assets/icons/branch.svg"),
    ),
    (
        "icons/commit.svg",
        include_bytes!("../assets/icons/commit.svg"),
    ),
    (
        "icons/run-passed.svg",
        include_bytes!("../assets/icons/run-passed.svg"),
    ),
    (
        "icons/run-failed.svg",
        include_bytes!("../assets/icons/run-failed.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/folder.svg",
        include_bytes!("../assets/icons/folder.svg"),
    ),
    ("icons/file.svg", include_bytes!("../assets/icons/file.svg")),
    (
        "icons/close.svg",
        include_bytes!("../assets/icons/close.svg"),
    ),
    ("icons/tree.svg", include_bytes!("../assets/icons/tree.svg")),
    (
        "icons/search.svg",
        include_bytes!("../assets/icons/search.svg"),
    ),
    (
        "icons/problems.svg",
        include_bytes!("../assets/icons/problems.svg"),
    ),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

/// How a file shows in the tree and on tabs: a small colored mark for the languages we know, a
/// plain page for everything else.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FileIcon {
    Badge(&'static str, Hsla),
    Plain,
}

pub fn file_icon(path: &Path) -> FileIcon {
    let color = |hex: u32| -> Hsla { rgb(hex).into() };
    match gleipnir_packages::registry()
        .language_for_path(path)
        .map(|l| l.grammar)
    {
        Some(Language::Rust) => FileIcon::Badge("Rs", color(0xe3a65d)),
        Some(Language::Go) => FileIcon::Badge("Go", color(0x72d3c2)),
        Some(Language::Odin) => FileIcon::Badge("Od", color(0x82b6ee)),
        Some(Language::TypeScript | Language::Tsx) => FileIcon::Badge("TS", color(0x6fa8e8)),
        Some(Language::JavaScript) => FileIcon::Badge("JS", color(0xe0c65c)),
        Some(Language::CSharp) => FileIcon::Badge("C#", color(0x9b7fe0)),
        Some(Language::Json) => FileIcon::Badge("{}", color(0xd8b45a)),
        Some(Language::Html) => FileIcon::Badge("<>", color(0xe58b5c)),
        Some(Language::Css) => FileIcon::Badge("#", color(0x6aa6e0)),
        Some(Language::Markdown) => FileIcon::Badge("M", color(0x8fb0c8)),
        None => FileIcon::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_embedded_and_is_an_svg() {
        for icon in [
            Icon::ChevronRight,
            Icon::ChevronDown,
            Icon::Folder,
            Icon::File,
            Icon::Close,
            Icon::Tree,
        ] {
            let bytes = Assets
                .load(icon.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{icon:?}"));
            assert!(
                std::str::from_utf8(&bytes).unwrap().contains("<svg"),
                "{icon:?}"
            );
        }
    }

    #[test]
    fn an_unknown_asset_is_none_rather_than_an_error() {
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    }

    #[test]
    fn languages_get_badges_and_other_files_do_not() {
        assert!(matches!(
            file_icon(Path::new("a.rs")),
            FileIcon::Badge("Rs", _)
        ));
        assert!(matches!(
            file_icon(Path::new("a.tsx")),
            FileIcon::Badge("TS", _)
        ));
        assert!(matches!(
            file_icon(Path::new("README.md")),
            FileIcon::Badge("M", _)
        ));
        assert!(matches!(
            file_icon(Path::new("Assets/Player.cs")),
            FileIcon::Badge("C#", _)
        ));
        assert_eq!(file_icon(Path::new("Makefile")), FileIcon::Plain);
        assert_eq!(file_icon(Path::new("notes.txt")), FileIcon::Plain);
    }
}
