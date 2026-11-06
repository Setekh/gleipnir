//! Converting between paths and `file:` URIs, with the percent-encoding servers expect.

use lsp_types::Uri;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

fn needs_escaping(byte: u8) -> bool {
    !(byte.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&byte))
}

pub fn path_to_uri(path: &Path) -> Uri {
    let mut text = String::from("file://");
    for &byte in path.to_string_lossy().as_bytes() {
        if needs_escaping(byte) {
            text.push_str(&format!("%{byte:02X}"));
        } else {
            text.push(byte as char);
        }
    }
    Uri::from_str(&text).expect("an escaped file path is a valid URI")
}

pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let rest = text.strip_prefix("file://")?;
    // A host part, like file://localhost/path, is legal and means the same machine.
    let rest = if rest.starts_with('/') {
        rest
    } else {
        &rest[rest.find('/')?..]
    };
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?, 16)
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(decoded).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip_through_uris() {
        for path in ["/a/b.rs", "/with space/é 😀.go", "/q?#[]%.ts", "/"] {
            let uri = path_to_uri(Path::new(path));
            assert_eq!(
                uri_to_path(&uri).as_deref(),
                Some(Path::new(path)),
                "{}",
                uri.as_str()
            );
        }
    }

    #[test]
    fn special_characters_are_escaped() {
        assert_eq!(
            path_to_uri(Path::new("/a b/c#d")).as_str(),
            "file:///a%20b/c%23d"
        );
    }

    #[test]
    fn servers_that_escape_more_or_name_a_host_still_work() {
        let uri = Uri::from_str("file://localhost/Users/me/%61pp.rs").unwrap();
        assert_eq!(
            uri_to_path(&uri).unwrap(),
            PathBuf::from("/Users/me/app.rs")
        );
    }

    #[test]
    fn other_schemes_are_not_paths() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert!(uri_to_path(&uri).is_none());
    }
}
