//! Unity projects. Unity keeps a `.meta` file beside every asset and folder under `Assets`, holding
//! the GUID that scenes and prefabs refer to it by, so a file moved, renamed or deleted without its
//! `.meta` breaks those references: here they go together, as they do in Unity's own editor. The
//! `.meta` files are hidden, as the person never edits them, and so are the folders Unity
//! regenerates, which can be gigabytes, even when the project's `.gitignore` doesn't list them.

use std::path::{Path, PathBuf};

/// Folders at a Unity project's root that Unity makes and remakes, never the person.
const GENERATED: &[&str] = &[
    "Library",
    "Temp",
    "Logs",
    "obj",
    "UserSettings",
    "MemoryCaptures",
];

/// Whether a folder is a Unity project's root: Unity writes its version into `ProjectSettings`.
pub fn is_unity_project(root: &Path) -> bool {
    root.join("ProjectSettings/ProjectVersion.txt").is_file() && root.join("Assets").is_dir()
}

/// The Unity project a path is in, if any: the nearest folder above it that is one.
pub fn unity_project_of(path: &Path) -> Option<&Path> {
    path.ancestors().skip(1).find(|dir| is_unity_project(dir))
}

/// Whether a path in a Unity project is one Unity regenerates: one of its folders at the root,
/// or anything in one.
pub fn is_generated(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|first| first.as_os_str().to_str())
        .is_some_and(|first| GENERATED.contains(&first))
}

/// Whether a file name is a Unity `.meta` file.
pub fn is_meta(name: &str) -> bool {
    name.len() > ".meta".len() && name.ends_with(".meta")
}

/// The `.meta` file that goes with a path in a Unity project, if it has one.
pub fn meta_of(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if is_meta(name) {
        return None;
    }
    unity_project_of(path)?;
    let meta = path.with_file_name(format!("{name}.meta"));
    meta.is_file().then_some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unity_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("ProjectSettings")).unwrap();
        fs::write(
            dir.path().join("ProjectSettings/ProjectVersion.txt"),
            "m_EditorVersion: 6000.0.30f1\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("Assets/Scripts")).unwrap();
        fs::write(
            dir.path().join("Assets/Scripts/Player.cs"),
            "class Player {}",
        )
        .unwrap();
        fs::write(dir.path().join("Assets/Scripts/Player.cs.meta"), "guid: 1").unwrap();
        dir
    }

    #[test]
    fn a_unity_project_is_known_by_its_version_file_and_assets() {
        let dir = unity_project();
        assert!(is_unity_project(dir.path()));
        let script = dir.path().join("Assets/Scripts/Player.cs");
        assert_eq!(unity_project_of(&script), Some(dir.path()));
        let plain = tempfile::tempdir().unwrap();
        assert!(!is_unity_project(plain.path()));
    }

    #[test]
    fn a_file_has_its_meta_and_a_meta_has_none() {
        let dir = unity_project();
        let script = dir.path().join("Assets/Scripts/Player.cs");
        assert_eq!(
            meta_of(&script),
            Some(dir.path().join("Assets/Scripts/Player.cs.meta"))
        );
        assert_eq!(meta_of(&script.with_file_name("Player.cs.meta")), None);
        assert_eq!(meta_of(&script.with_file_name("Missing.cs")), None);
    }

    #[test]
    fn only_unitys_own_folders_at_the_root_are_generated() {
        let root = Path::new("/p");
        assert!(is_generated(root, Path::new("/p/Library")));
        assert!(is_generated(
            root,
            Path::new("/p/Library/ScriptAssemblies/A.dll")
        ));
        assert!(is_generated(root, Path::new("/p/obj/Debug")));
        assert!(!is_generated(root, Path::new("/p/Assets/Library")));
        assert!(!is_generated(root, Path::new("/p/Assets/Player.cs")));
        assert!(!is_meta(".meta"));
        assert!(is_meta("Player.cs.meta"));
    }
}
