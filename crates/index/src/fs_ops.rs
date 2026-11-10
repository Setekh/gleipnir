//! File operations behind the project tree. They refuse to overwrite anything: a name collision is
//! an error for the user to resolve, never silent data loss.

use anyhow::{Context as _, Result, bail};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Checks a name typed for a new entry. Slashes are allowed when creating, as a quick way to make
/// nested folders, but `..` isn't, so nothing can land outside the folder it was created in.
pub fn validate_relative_name(name: &str) -> Result<()> {
    let name = name.trim_end_matches('/');
    if name.trim().is_empty() {
        bail!("the name can't be empty");
    }
    for component in Path::new(name).components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("{name:?} isn't a valid name");
        }
    }
    Ok(())
}

/// Checks a name for a rename, which stays in the same folder.
pub fn validate_file_name(name: &str) -> Result<()> {
    validate_relative_name(name)?;
    if name.contains('/') {
        bail!("a name can't contain a slash");
    }
    Ok(())
}

fn ensure_free(path: &Path) -> Result<()> {
    // symlink_metadata so a dangling symlink still counts as being in the way.
    if fs::symlink_metadata(path).is_ok() {
        bail!("{} already exists", path.display());
    }
    Ok(())
}

/// Creates an empty file, plus any folders the name implies, and returns its path.
pub fn create_file(dir: &Path, name: &str) -> Result<PathBuf> {
    validate_relative_name(name)?;
    let path = dir.join(name);
    if name.ends_with('/') {
        return create_dir(dir, name);
    }
    ensure_free(&path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // create_new closes the gap between checking and creating.
    fs::File::create_new(&path).with_context(|| format!("can't create {}", path.display()))?;
    Ok(path)
}

pub fn create_dir(dir: &Path, name: &str) -> Result<PathBuf> {
    validate_relative_name(name)?;
    let path = dir.join(name.trim_end_matches('/'));
    ensure_free(&path)?;
    fs::create_dir_all(&path).with_context(|| format!("can't create {}", path.display()))?;
    Ok(path)
}

/// Whether two paths are the same file, as with a case-only rename on a case-insensitive disk.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Renames within a folder, returning the new path. In a Unity project, its `.meta` is renamed
/// to match.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf> {
    validate_file_name(new_name)?;
    let parent = path.parent().context("can't rename this")?;
    let target = parent.join(new_name);
    if target == path {
        return Ok(target);
    }
    let meta =
        crate::unity::meta_of(path).map(|meta| (meta, parent.join(format!("{new_name}.meta"))));
    if !same_file(path, &target) {
        ensure_free(&target)?;
        if let Some((_, meta_target)) = &meta {
            ensure_free(meta_target)?;
        }
    }
    fs::rename(path, &target).with_context(|| format!("can't rename {}", path.display()))?;
    if let Some((meta, meta_target)) = meta {
        fs::rename(&meta, &meta_target)
            .with_context(|| format!("renamed, but can't rename {}", meta.display()))?;
    }
    Ok(target)
}

/// Moves an entry into a folder, returning the new path.
pub fn move_into(path: &Path, dir: &Path) -> Result<PathBuf> {
    let name = path.file_name().context("can't move this")?;
    if dir.starts_with(path) {
        bail!("can't move a folder into itself");
    }
    let target = dir.join(name);
    if target == path {
        return Ok(target);
    }
    ensure_free(&target)?;
    // A Unity asset's `.meta` goes with it, so Unity still knows it by its GUID.
    let meta = crate::unity::meta_of(path).and_then(|meta| {
        let meta_target = dir.join(meta.file_name()?);
        Some((meta, meta_target))
    });
    if let Some((_, meta_target)) = &meta {
        ensure_free(meta_target)?;
    }
    fs::rename(path, &target).with_context(|| format!("can't move {}", path.display()))?;
    if let Some((meta, meta_target)) = meta {
        fs::rename(&meta, &meta_target)
            .with_context(|| format!("moved, but can't move {}", meta.display()))?;
    }
    Ok(target)
}

/// Moves to the system Trash, so a mistake can be recovered. A Unity asset's `.meta` goes too,
/// so putting both back restores it as it was.
pub fn move_to_trash(path: &Path) -> Result<()> {
    let meta = crate::unity::meta_of(path);
    trash::delete(path).with_context(|| format!("can't move {} to the Trash", path.display()))?;
    if let Some(meta) = meta {
        trash::delete(&meta)
            .with_context(|| format!("moved to the Trash, but can't move {}", meta.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unity_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("ProjectSettings")).unwrap();
        fs::write(dir.path().join("ProjectSettings/ProjectVersion.txt"), "").unwrap();
        fs::create_dir_all(dir.path().join("Assets/Scripts")).unwrap();
        fs::create_dir_all(dir.path().join("Assets/Other")).unwrap();
        fs::write(
            dir.path().join("Assets/Scripts/Player.cs"),
            "class Player {}",
        )
        .unwrap();
        fs::write(dir.path().join("Assets/Scripts/Player.cs.meta"), "guid: 1").unwrap();
        dir
    }

    #[test]
    fn a_unity_assets_meta_is_renamed_and_moved_with_it() {
        let dir = unity_project();
        let scripts = dir.path().join("Assets/Scripts");
        let renamed = rename(&scripts.join("Player.cs"), "Hero.cs").unwrap();
        assert!(renamed.is_file());
        assert_eq!(
            fs::read_to_string(scripts.join("Hero.cs.meta")).unwrap(),
            "guid: 1"
        );
        assert!(!scripts.join("Player.cs.meta").exists());

        let other = dir.path().join("Assets/Other");
        move_into(&renamed, &other).unwrap();
        assert!(other.join("Hero.cs").is_file());
        assert!(other.join("Hero.cs.meta").is_file());
        assert!(!scripts.join("Hero.cs.meta").exists());
    }

    #[test]
    fn nothing_moves_if_the_metas_new_place_is_taken() {
        let dir = unity_project();
        let scripts = dir.path().join("Assets/Scripts");
        fs::write(scripts.join("Hero.cs.meta"), "someone else's").unwrap();
        assert!(rename(&scripts.join("Player.cs"), "Hero.cs").is_err());
        assert!(scripts.join("Player.cs").is_file());
        assert!(scripts.join("Player.cs.meta").is_file());
        assert_eq!(
            fs::read_to_string(scripts.join("Hero.cs.meta")).unwrap(),
            "someone else's"
        );
    }

    #[test]
    fn creating_a_file_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = create_file(dir.path(), "a.txt").unwrap();
        fs::write(&path, "precious").unwrap();
        assert!(create_file(dir.path(), "a.txt").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "precious");
    }

    #[test]
    fn creating_a_file_can_make_nested_folders() {
        let dir = tempfile::tempdir().unwrap();
        let path = create_file(dir.path(), "a/b/c.txt").unwrap();
        assert!(path.is_file());
        assert_eq!(path, dir.path().join("a/b/c.txt"));
    }

    #[test]
    fn a_trailing_slash_makes_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(create_file(dir.path(), "sub/").unwrap().is_dir());
    }

    #[test]
    fn names_cannot_escape_their_folder() {
        for bad in ["", "   ", "..", "../x", "a/../../x", "/etc/passwd", "."] {
            assert!(
                validate_relative_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        for good in ["a", "a.b", "a/b", ".env", "with space", "é😀"] {
            assert!(
                validate_relative_name(good).is_ok(),
                "{good:?} should be accepted"
            );
        }
    }

    #[test]
    fn creating_a_folder_refuses_an_existing_name() {
        let dir = tempfile::tempdir().unwrap();
        create_dir(dir.path(), "a").unwrap();
        assert!(create_dir(dir.path(), "a").is_err());
    }

    #[test]
    fn renaming_moves_the_file_and_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let a = create_file(dir.path(), "a").unwrap();
        let b = create_file(dir.path(), "b").unwrap();
        fs::write(&b, "keep").unwrap();
        assert!(rename(&a, "b").is_err());
        assert_eq!(fs::read_to_string(&b).unwrap(), "keep");
        let c = rename(&a, "c").unwrap();
        assert!(c.exists() && !a.exists());
    }

    #[test]
    fn renaming_to_the_same_name_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = create_file(dir.path(), "a").unwrap();
        assert_eq!(rename(&a, "a").unwrap(), a);
        assert!(a.exists());
    }

    #[test]
    fn renaming_cannot_change_folder() {
        let dir = tempfile::tempdir().unwrap();
        let a = create_file(dir.path(), "a").unwrap();
        assert!(rename(&a, "sub/a").is_err());
        assert!(rename(&a, "../a").is_err());
    }

    #[test]
    fn a_case_only_rename_works_on_any_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let a = create_file(dir.path(), "name").unwrap();
        let renamed = rename(&a, "NAME").unwrap();
        let listed: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(listed, ["NAME"]);
        assert!(renamed.exists());
    }

    #[test]
    fn moving_into_a_folder_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let sub = create_dir(dir.path(), "sub").unwrap();
        let a = create_file(dir.path(), "a").unwrap();
        let existing = create_file(&sub, "a").unwrap();
        fs::write(&existing, "keep").unwrap();
        assert!(move_into(&a, &sub).is_err());
        assert_eq!(fs::read_to_string(&existing).unwrap(), "keep");
        assert!(a.exists());
    }

    #[test]
    fn moving_works_and_keeps_contents() {
        let dir = tempfile::tempdir().unwrap();
        let sub = create_dir(dir.path(), "sub").unwrap();
        let a = create_file(dir.path(), "a").unwrap();
        fs::write(&a, "data").unwrap();
        let moved = move_into(&a, &sub).unwrap();
        assert_eq!(moved, sub.join("a"));
        assert_eq!(fs::read_to_string(&moved).unwrap(), "data");
    }

    #[test]
    fn a_folder_cannot_move_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let sub = create_dir(dir.path(), "sub").unwrap();
        let inner = create_dir(&sub, "inner").unwrap();
        assert!(move_into(&sub, &sub).is_err());
        assert!(move_into(&sub, &inner).is_err());
        assert!(inner.exists());
    }

    #[test]
    fn moving_to_where_it_already_is_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = create_file(dir.path(), "a").unwrap();
        assert_eq!(move_into(&a, dir.path()).unwrap(), a);
        assert!(a.exists());
    }

    #[test]
    fn a_dangling_symlink_counts_as_taken() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("link")).unwrap();
        assert!(create_file(dir.path(), "link").is_err());
    }
}
