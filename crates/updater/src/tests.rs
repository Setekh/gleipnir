//! The whole update against a release served from this machine, installed as on Linux, which
//! needs nothing but files.

use super::*;
use std::cell::RefCell;
use std::io::{BufRead as _, BufReader};
use std::net::TcpListener;

/// Serves `files` by path until the test ends, and returns the address to ask.
fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).ok();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let path = request.split_whitespace().nth(1).unwrap_or_default();
            let found = files.iter().find(|(name, _)| *name == path);
            let (code, body): (&str, &[u8]) = match found {
                Some((_, body)) => ("200 OK", body),
                None => ("404 Not Found", b""),
            };
            let head = format!(
                "HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).ok();
            stream.write_all(body).ok();
        }
    });
    address
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A release of `version` whose archive holds `program`, published with `checksum`.
fn published(version: &str, program: &[u8], checksum: Option<String>) -> String {
    let archive = install::tests::archive(Some(program));
    let suffix = Install::LinuxBinary(PathBuf::new()).suffix();
    let name = format!("gleipnir-{version}{suffix}");
    let checksum = checksum.unwrap_or_else(|| sha256(&archive));
    let release = |address: &str| {
        serde_json::json!({
            "tag_name": format!("v{version}"),
            "assets": [
                {
                    "name": name,
                    "size": archive.len(),
                    "browser_download_url": format!("{address}/{name}"),
                },
                {
                    "name": format!("{name}.sha256"),
                    "browser_download_url": format!("{address}/{name}.sha256"),
                },
            ],
        })
        .to_string()
    };
    // One server for the files, whose address the release, served by another, then names.
    let files = serve(vec![
        (
            Box::leak(format!("/{name}").into_boxed_str()),
            archive.clone(),
        ),
        (
            Box::leak(format!("/{name}.sha256").into_boxed_str()),
            format!("{checksum}  {name}\n").into_bytes(),
        ),
    ]);
    let api = serve(vec![("/latest", release(&files).into_bytes())]);
    format!("{api}/latest")
}

fn installed(dir: &Path, latest: String) -> Source {
    let exe = dir.join("bin/gleipnir");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "old").unwrap();
    Source {
        latest,
        current: "0.1.0".into(),
        install: Install::LinuxBinary(exe),
        downloads: dir.join("updates"),
    }
}

fn program(source: &Source) -> String {
    let Install::LinuxBinary(exe) = &source.install else {
        unreachable!()
    };
    std::fs::read_to_string(exe).unwrap()
}

#[test]
fn a_newer_release_is_downloaded_checked_and_installed() {
    let dir = tempfile::tempdir().unwrap();
    let source = installed(dir.path(), published("0.2.0", b"new", None));
    let steps = RefCell::new(Vec::new());

    let version = update(&source, &|status| steps.borrow_mut().push(status)).unwrap();

    assert_eq!(version.as_deref(), Some("0.2.0"));
    assert_eq!(program(&source), "new");
    let steps = steps.into_inner();
    assert_eq!(steps.first(), Some(&Status::Checking));
    assert!(steps.contains(&Status::Downloading("0.2.0".into(), 1.)));
    assert_eq!(steps.last(), Some(&Status::Installing("0.2.0".into())));
    // The installer isn't kept.
    assert_eq!(std::fs::read_dir(&source.downloads).unwrap().count(), 0);
}

#[test]
fn the_same_or_an_older_release_changes_nothing() {
    for version in ["0.1.0", "0.0.9"] {
        let dir = tempfile::tempdir().unwrap();
        let source = installed(dir.path(), published(version, b"new", None));
        assert_eq!(update(&source, &|_| {}).unwrap(), None);
        assert_eq!(program(&source), "old");
    }
}

#[test]
fn an_installer_that_does_not_match_its_checksum_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let wrong = Some("0".repeat(64));
    let source = installed(dir.path(), published("0.2.0", b"tampered", wrong));

    let error = update(&source, &|_| {}).unwrap_err();

    assert!(
        error.to_string().contains("didn't match its checksum"),
        "{error:#}"
    );
    assert_eq!(program(&source), "old");
    assert_eq!(std::fs::read_dir(&source.downloads).unwrap().count(), 0);
}

#[test]
fn no_releases_yet_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let nothing = serve(Vec::new());
    let source = installed(dir.path(), format!("{nothing}/latest"));
    assert_eq!(update(&source, &|_| {}).unwrap(), None);
    assert_eq!(program(&source), "old");
}

#[test]
fn the_status_bar_offers_the_restart_only_once_the_update_is_in() {
    assert_eq!(label(&Status::Idle), None);
    assert_eq!(
        label(&Status::Downloading("0.2.0".into(), 0.456)),
        Some(("Downloading Gleipnir 0.2.0 46%".into(), false))
    );
    assert_eq!(
        label(&Status::Ready("0.2.0".into())),
        Some(("Restart to update to 0.2.0".into(), true))
    );
}
