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
        }
    }
}

const FILES: &[(&str, &[u8])] = &[
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
        assert_eq!(file_icon(Path::new("README.md")), FileIcon::Plain);
        assert_eq!(file_icon(Path::new("Makefile")), FileIcon::Plain);
    }
}
