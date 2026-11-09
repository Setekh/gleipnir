//! Places in source files mentioned in output, a console's or a terminal's, like a panic's
//! `src/lib.rs:11:42` or Odin's `a_test.odin(14:2)`, so they can be clicked.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// A mention of a place in a file: where in the line it is, the path as written, and the line and
/// column, from 1 as tools print them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub range: Range<usize>,
    pub path: String,
    pub line: u32,
    pub column: Option<u32>,
}

/// Extensions of source files worth linking, so that a time like `10:45:56` isn't taken for one.
const SOURCES: &[&str] = &[
    "rs", "go", "odin", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "c", "h", "cc",
    "cpp", "hpp", "zig", "py", "toml", "json",
];

fn is_source(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCES.contains(&e))
}

fn number(text: &str) -> (Option<u32>, usize) {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    (text[..digits].parse().ok(), digits)
}

/// Every place mentioned in a line of output.
pub fn find_links(text: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        // A path is a run of characters with no spaces, quotes or brackets in it.
        let boundary = |b: u8| b.is_ascii_whitespace() || b"'\"`()[]<>,".contains(&b);
        if boundary(bytes[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && !boundary(bytes[end]) && bytes[end] != b':' {
            end += 1;
        }
        let path = &text[start..end];
        let rest = &text[end..];
        if is_source(path) {
            // `path:line:column` or `path:line`.
            if let Some(after) = rest.strip_prefix(':') {
                let (line, digits) = number(after);
                if let Some(line) = line {
                    let mut stop = end + 1 + digits;
                    let mut column = None;
                    if let Some(more) = text[stop..].strip_prefix(':') {
                        let (col, digits) = number(more);
                        if col.is_some() {
                            column = col;
                            stop += 1 + digits;
                        }
                    }
                    links.push(Link {
                        range: start..stop,
                        path: path.to_string(),
                        line,
                        column,
                    });
                    start = stop;
                    continue;
                }
            }
            // Odin's `path(line:column)`.
            if let Some(after) = rest.strip_prefix('(') {
                let (line, digits) = number(after);
                let more = &after[digits..];
                if let Some(line) = line
                    && let Some(more) = more.strip_prefix(':')
                {
                    let (column, digits2) = number(more);
                    if column.is_some() && more[digits2..].starts_with(')') {
                        let stop = end + 1 + digits + 1 + digits2 + 1;
                        links.push(Link {
                            range: start..stop,
                            path: path.to_string(),
                            line,
                            column,
                        });
                        start = stop;
                        continue;
                    }
                }
            }
        }
        start = end.max(start + 1);
    }
    links
}

/// The file a mentioned path means: as written if absolute, else under the first folder it's in.
pub fn resolve(path: &str, folders: &[PathBuf]) -> Option<PathBuf> {
    let path = path.strip_prefix("file://").unwrap_or(path);
    let written = Path::new(path);
    if written.is_absolute() {
        return written.is_file().then(|| written.to_path_buf());
    }
    folders
        .iter()
        .map(|folder| folder.join(written))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(&str, u32, Option<u32>)> {
        find_links(text)
            .into_iter()
            .map(|l| (&text[l.range.clone()], l.line, l.column))
            .collect()
    }

    #[test]
    fn places_are_found_in_each_tools_style() {
        assert_eq!(
            found("thread 'x' panicked at ws_a/src/lib.rs:11:42:"),
            [("ws_a/src/lib.rs:11:42", 11, Some(42))]
        );
        assert_eq!(found("    a_test.go:8: bad 1"), [("a_test.go:8", 8, None)]);
        assert_eq!(
            found("/p/pkg/a_test.odin(14:2):fails() expected"),
            [("/p/pkg/a_test.odin(14:2)", 14, Some(2))]
        );
        assert_eq!(
            found("    at main (/app/index.js:3:9)"),
            [("/app/index.js:3:9", 3, Some(9))]
        );
        assert_eq!(
            found("error[E0425]: --> src/main.rs:4:5"),
            [("src/main.rs:4:5", 4, Some(5))]
        );
    }

    #[test]
    fn times_and_urls_are_not_places() {
        assert!(found("[INFO ] --- [2026-11-06 10:45:56] Starting").is_empty());
        assert!(found("listening on http://localhost:8080").is_empty());
        assert!(found("finished in 0.25s").is_empty());
    }

    #[test]
    fn relative_paths_resolve_under_the_first_folder_holding_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg/a_test.go"), "").unwrap();
        let folders = [dir.path().to_path_buf(), dir.path().join("pkg")];
        assert_eq!(
            resolve("a_test.go", &folders),
            Some(dir.path().join("pkg/a_test.go"))
        );
        assert_eq!(resolve("missing.go", &folders), None);
    }
}
