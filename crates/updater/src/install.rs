//! Putting a downloaded installer in place of the installed program. The running program keeps
//! working from the files it started with; the new ones run after a restart.

use anyhow::{Context as _, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How this Gleipnir was installed, which says what to download and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// macOS: the app, like `/Applications/Gleipnir.app`, replaced from the disk image.
    MacApp(PathBuf),
    /// Linux: the program, like `~/.local/bin/gleipnir`, replaced from the archive.
    LinuxBinary(PathBuf),
}

impl Install {
    /// The install the program at `exe` belongs to, or why it can't update itself: it was built
    /// from source, or someone else installed it where this user can't write.
    pub fn of(exe: &Path) -> Result<Install> {
        let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
        if let Some(app) = app_bundle(&exe) {
            let folder = app.parent().context("the app has no folder")?;
            if !writable(folder) {
                bail!(
                    "Gleipnir can't update itself in {}: move it to Applications, or install the new version by hand",
                    folder.display()
                );
            }
            return Ok(Install::MacApp(app));
        }
        let built_here = exe.components().any(|part| part.as_os_str() == "target");
        if cfg!(target_os = "macos") || built_here {
            bail!(
                "This Gleipnir was built from source ({}), so it doesn't update itself",
                exe.display()
            );
        }
        let folder = exe.parent().context("the program has no folder")?;
        if !writable(folder) {
            bail!(
                "Gleipnir can't update itself in {}: install the new version as it was installed there",
                folder.display()
            );
        }
        Ok(Install::LinuxBinary(exe))
    }

    /// How this platform's installer's name ends, as `script/bundle` names them.
    pub fn suffix(&self) -> String {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            other => other,
        };
        match self {
            Install::MacApp(_) => format!("-macos-{arch}.dmg"),
            Install::LinuxBinary(_) => format!("-linux-{arch}.tar.gz"),
        }
    }

    /// Replaces the installed program with the one in `installer`.
    pub fn apply(&self, installer: &Path) -> Result<()> {
        match self {
            Install::MacApp(app) => install_app(installer, app),
            Install::LinuxBinary(exe) => install_binary(installer, exe),
        }
    }
}

/// `…/Gleipnir.app` for `…/Gleipnir.app/Contents/MacOS/gleipnir`.
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then(|| app.to_path_buf())
}

fn writable(folder: &Path) -> bool {
    let probe = folder.join(format!(".gleipnir-update-{}", std::process::id()));
    let made = fs::File::create(&probe).is_ok();
    fs::remove_file(&probe).ok();
    made
}

/// A hidden name beside `path`, on the same disk, so that moving to and from it is instant.
fn beside(path: &Path, what: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{what}"))
}

fn remove(path: &Path) {
    if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path).ok();
    } else {
        fs::remove_file(path).ok();
    }
}

/// Moves `staged` to where `installed` is. If that fails, the old one is put back.
fn swap(staged: &Path, installed: &Path) -> Result<()> {
    let old = beside(installed, "old");
    remove(&old);
    fs::rename(installed, &old)
        .with_context(|| format!("can't move {} aside", installed.display()))?;
    if let Err(error) = fs::rename(staged, installed) {
        fs::rename(&old, installed).ok();
        remove(staged);
        return Err(error)
            .with_context(|| format!("can't put the update at {}", installed.display()));
    }
    remove(&old);
    Ok(())
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("couldn't {what}"))?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        bail!("couldn't {what}: {}", why.trim());
    }
    Ok(())
}

/// Opens the disk image, copies its app beside the installed one, checks its signature is
/// intact, and swaps the two.
fn install_app(image: &Path, app: &Path) -> Result<()> {
    let mount = beside(image, "mount");
    fs::create_dir_all(&mount)?;
    run(
        Command::new("/usr/bin/hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-noautoopen"])
            .arg("-mountpoint")
            .arg(&mount)
            .arg(image),
        "open the update's disk image",
    )?;
    let result = (|| {
        let source = fs::read_dir(&mount)?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .find(|path| path.extension().is_some_and(|e| e == "app"))
            .context("the update's disk image has no app in it")?;
        let staged = beside(app, "new");
        remove(&staged);
        run(
            Command::new("/usr/bin/ditto").arg(&source).arg(&staged),
            "copy the update",
        )?;
        let checked = run(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--strict"])
                .arg(&staged),
            "check the update's signature",
        );
        if let Err(error) = checked {
            remove(&staged);
            return Err(error);
        }
        swap(&staged, app)
    })();
    Command::new("/usr/bin/hdiutil")
        .args(["detach", "-force"])
        .arg(&mount)
        .output()
        .ok();
    fs::remove_dir(&mount).ok();
    result
}

/// Takes `bin/gleipnir` out of the archive and puts it where the program is.
fn install_binary(archive: &Path, exe: &Path) -> Result<()> {
    let file = fs::File::open(archive)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let is_program =
            entry.header().entry_type().is_file() && entry.path()?.ends_with("bin/gleipnir");
        if !is_program {
            continue;
        }
        let staged = beside(exe, "new");
        let mut out = fs::File::create(&staged)?;
        std::io::copy(&mut entry, &mut out)?;
        drop(out);
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
        }
        return swap(&staged, exe);
    }
    bail!("the update's archive has no bin/gleipnir in it")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An archive laid out as `script/bundle` lays out the Linux one.
    pub(crate) fn archive(program: Option<&[u8]>) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut add = |path: &str, bytes: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, bytes).unwrap();
        };
        add("gleipnir-0.2.0-linux-x86_64/LICENSE", b"MIT");
        if let Some(program) = program {
            add("gleipnir-0.2.0-linux-x86_64/bin/gleipnir", program);
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn the_install_is_found_from_where_the_program_runs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let macos = root.join("Gleipnir.app/Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(macos.join("gleipnir"), "").unwrap();
        assert_eq!(
            Install::of(&macos.join("gleipnir")).unwrap(),
            Install::MacApp(root.join("Gleipnir.app"))
        );

        let built = root.join("target/release");
        fs::create_dir_all(&built).unwrap();
        fs::write(built.join("gleipnir"), "").unwrap();
        let error = Install::of(&built.join("gleipnir")).unwrap_err();
        assert!(error.to_string().contains("built from source"), "{error}");
    }

    #[test]
    fn the_program_is_replaced_from_the_archive_and_stays_runnable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("bin/gleipnir");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "old").unwrap();
        let archive_path = dir.path().join("update.tar.gz");
        fs::write(&archive_path, archive(Some(b"new"))).unwrap();

        Install::LinuxBinary(exe.clone())
            .apply(&archive_path)
            .unwrap();

        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // Nothing is left beside it.
        let left: Vec<_> = fs::read_dir(exe.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["gleipnir"]);
    }

    #[test]
    fn an_archive_without_the_program_leaves_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("gleipnir");
        fs::write(&exe, "old").unwrap();
        let archive_path = dir.path().join("update.tar.gz");
        fs::write(&archive_path, archive(None)).unwrap();

        let error = Install::LinuxBinary(exe.clone())
            .apply(&archive_path)
            .unwrap_err();

        assert!(error.to_string().contains("no bin/gleipnir"), "{error}");
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
    }

    /// The macOS path with a real disk image from `script/bundle`:
    /// `GLEIPNIR_TEST_DMG=target/dist/Gleipnir-….dmg cargo test -p gleipnir-updater -- --ignored`
    #[test]
    #[ignore = "needs a disk image built by script/bundle"]
    fn a_real_disk_image_replaces_the_app() {
        let image =
            PathBuf::from(std::env::var_os("GLEIPNIR_TEST_DMG").expect("GLEIPNIR_TEST_DMG"));
        let dir = tempfile::tempdir().unwrap();
        // The image is opened beside itself, so it's copied out of the build folder.
        let copy = dir.path().join("update.dmg");
        fs::copy(&image, &copy).unwrap();
        let app = dir.path().join("Gleipnir.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/gleipnir"), "old").unwrap();

        Install::MacApp(app.clone()).apply(&copy).unwrap();

        let program = fs::metadata(app.join("Contents/MacOS/gleipnir")).unwrap();
        assert!(program.len() > 1_000_000, "the real program is in place");
        assert!(app.join("Contents/Info.plist").exists());
        let left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            left.len(),
            2,
            "only the app and the image are left: {left:?}"
        );
    }

    #[test]
    fn a_swap_that_fails_puts_the_old_one_back() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Gleipnir.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(app.join("Contents/version"), "old").unwrap();

        let missing = dir.path().join("nothing-staged");
        assert!(swap(&missing, &app).is_err());
        assert_eq!(
            fs::read_to_string(app.join("Contents/version")).unwrap(),
            "old"
        );

        let staged = dir.path().join("staged.app");
        fs::create_dir_all(staged.join("Contents")).unwrap();
        fs::write(staged.join("Contents/version"), "new").unwrap();
        swap(&staged, &app).unwrap();
        assert_eq!(
            fs::read_to_string(app.join("Contents/version")).unwrap(),
            "new"
        );
        assert!(!beside(&app, "old").exists());
    }
}
