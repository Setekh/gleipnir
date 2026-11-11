//! Find in Files: every match of some text or a pattern across the project's files, read in
//! parallel. Open files can be searched as they stand rather than as saved.

use crate::search::Cancel;
use anyhow::{Result, anyhow};
use regex::RegexBuilder;
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Past this many matches the search stops: more isn't something anyone reads through.
pub const MATCH_LIMIT: usize = 5_000;
/// Files larger than this are skipped, as they're generated or data.
const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;
/// Lines are shown cut to this many bytes around the match.
const LINE_CONTEXT: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub regex: bool,
    /// File name patterns to search, like `*.rs`, comma separated. Empty means every file.
    pub file_mask: String,
    /// Replacing keeps the case of what's replaced: all capitals, or a capital first letter or
    /// not.
    pub preserve_case: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatch {
    /// Relative to the project root.
    pub path: Arc<str>,
    /// Zero-based.
    pub line: usize,
    /// The match's byte range in the line.
    pub columns: Range<usize>,
    /// Where it starts in UTF-16 units, as language servers and the Find panel count.
    pub utf16_column: usize,
    /// The line, without its ending, cut down around the match if it's long.
    pub text: String,
    /// Where the match is within `text`.
    pub highlight: Range<usize>,
}

/// The compiled query.
pub struct Matcher {
    regex: Regex,
    masks: Vec<globset::GlobMatcher>,
}

pub use regex::Regex;

/// A query as a regular expression, as the options say: the text itself unless it's a pattern,
/// matching any case unless that matters, and with `^` and `$` at line ends.
pub fn compile(query: &str, options: &FindOptions) -> Result<Regex> {
    if query.is_empty() {
        return Err(anyhow!("nothing to find"));
    }
    let mut pattern = if options.regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    if options.whole_words {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    RegexBuilder::new(&pattern)
        .case_insensitive(!options.case_sensitive)
        .multi_line(true)
        .build()
        .map_err(|e| anyhow!("not a valid pattern: {e}"))
}

/// What the match at `range` in `text` is replaced with: `template`, with `$1`, `${name}` and the
/// like filled in from what a pattern caught, and the match's case kept if the options say.
pub fn replacement(
    regex: &Regex,
    text: &str,
    range: Range<usize>,
    template: &str,
    options: &FindOptions,
) -> String {
    let mut out = String::new();
    match regex
        .captures_at(text, range.start)
        .filter(|c| c.get(0).is_some_and(|m| m.range() == range))
    {
        Some(captures) if options.regex => captures.expand(template, &mut out),
        _ => out.push_str(template),
    }
    if options.preserve_case {
        out = keep_case(&text[range], &out);
    }
    out
}

/// `replacement` in the case of `original`: all capitals if it was, otherwise its first letter
/// capital or not as the original's was.
fn keep_case(original: &str, replacement: &str) -> String {
    let letters: Vec<char> = original.chars().filter(|c| c.is_alphabetic()).collect();
    let Some(first) = letters.first() else {
        return replacement.to_string();
    };
    if letters.len() > 1 && letters.iter().all(|c| !c.is_lowercase()) {
        return replacement.to_uppercase();
    }
    let mut chars = replacement.chars();
    let Some(head) = chars.next() else {
        return String::new();
    };
    let head: String = if first.is_uppercase() {
        head.to_uppercase().collect()
    } else {
        head.to_lowercase().collect()
    };
    head + chars.as_str()
}

impl Matcher {
    /// The compiled query, for working out replacements.
    pub fn regex(&self) -> &Regex {
        &self.regex
    }

    pub fn new(query: &str, options: &FindOptions) -> Result<Self> {
        let regex = compile(query, options)?;
        let masks = options
            .file_mask
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| {
                globset::Glob::new(m)
                    .map(|g| g.compile_matcher())
                    .map_err(|e| anyhow!("not a valid file mask: {e}"))
            })
            .collect::<Result<_>>()?;
        Ok(Self { regex, masks })
    }

    fn wants(&self, relative: &str) -> bool {
        if self.masks.is_empty() {
            return true;
        }
        let name = relative.rsplit('/').next().unwrap_or(relative);
        self.masks
            .iter()
            .any(|m| m.is_match(name) || m.is_match(relative))
    }

    /// Every match in a text, by line.
    pub fn find_in(&self, path: &Arc<str>, text: &str, out: &mut Vec<FileMatch>, limit: usize) {
        let mut line_starts: Option<Vec<usize>> = None;
        for found in self.regex.find_iter(text) {
            if out.len() >= limit {
                return;
            }
            if found.start() == found.end() {
                continue;
            }
            let starts = line_starts.get_or_insert_with(|| {
                std::iter::once(0)
                    .chain(text.match_indices('\n').map(|(i, _)| i + 1))
                    .collect()
            });
            let line = starts.partition_point(|&s| s <= found.start()) - 1;
            let line_start = starts[line];
            let line_end = text[line_start..]
                .find('\n')
                .map_or(text.len(), |i| line_start + i);
            let line_text = text[line_start..line_end].trim_end_matches('\r');
            let columns = found.start() - line_start..(found.end().min(line_end) - line_start);
            let (shown, highlight) = excerpt(line_text, columns.clone());
            let utf16_column = line_text
                .get(..columns.start)
                .map_or(columns.start, |before| before.encode_utf16().count());
            out.push(FileMatch {
                path: path.clone(),
                line,
                utf16_column,
                columns,
                text: shown,
                highlight,
            });
        }
    }
}

/// A long line cut down to the part around the match, with the match's place in what's kept.
fn excerpt(line: &str, columns: Range<usize>) -> (String, Range<usize>) {
    let columns = columns.start.min(line.len())..columns.end.min(line.len());
    if line.len() <= LINE_CONTEXT {
        let trimmed = line.trim_start();
        let cut = line.len() - trimmed.len();
        let start = columns.start.saturating_sub(cut);
        return (
            trimmed.to_string(),
            start..columns.end.saturating_sub(cut).max(start),
        );
    }
    let mut from = columns.start.saturating_sub(LINE_CONTEXT / 2);
    while !line.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (columns.end + LINE_CONTEXT / 2).min(line.len());
    while !line.is_char_boundary(to) {
        to += 1;
    }
    (
        line[from..to].to_string(),
        columns.start - from..columns.end - from,
    )
}

/// Searches files under `root`, in parallel, in the order given. `open` holds the text of files
/// open with unsaved edits, by relative path, which is searched instead of the disk. Stops at
/// `MATCH_LIMIT` matches, or when cancelled.
pub fn find_in_files(
    root: &Path,
    files: &[Arc<str>],
    matcher: &Matcher,
    open: &HashMap<Arc<str>, String>,
    cancel: &Cancel,
) -> Vec<FileMatch> {
    let wanted: Vec<&Arc<str>> = files.iter().filter(|f| matcher.wants(f)).collect();
    let found = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    let chunk = wanted.len().div_ceil(threads).max(32);
    let mut results: Vec<(usize, Vec<FileMatch>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = wanted
            .chunks(chunk)
            .enumerate()
            .map(|(index, slice)| {
                let found = &found;
                scope.spawn(move || {
                    let mut out = Vec::new();
                    for relative in slice {
                        if cancel.is_cancelled() || found.load(Ordering::Relaxed) >= MATCH_LIMIT {
                            break;
                        }
                        let before = out.len();
                        match open.get(*relative) {
                            Some(text) => matcher.find_in(relative, text, &mut out, MATCH_LIMIT),
                            None => {
                                let path = root.join(&***relative);
                                let small = std::fs::metadata(&path)
                                    .is_ok_and(|m| m.len() <= MAX_FILE_SIZE);
                                // Binary files don't read as text, and are skipped.
                                if small && let Ok(text) = std::fs::read_to_string(&path) {
                                    matcher.find_in(relative, &text, &mut out, MATCH_LIMIT);
                                }
                            }
                        }
                        found.fetch_add(out.len() - before, Ordering::Relaxed);
                    }
                    (index, out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a search thread"))
            .collect()
    });
    results.sort_by_key(|(index, _)| *index);
    let mut all: Vec<FileMatch> = results.into_iter().flat_map(|(_, m)| m).collect();
    all.truncate(MATCH_LIMIT);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path, list: &[(&str, &str)]) -> Vec<Arc<str>> {
        for (path, text) in list {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        list.iter().map(|(p, _)| Arc::from(*p)).collect()
    }

    fn find(dir: &Path, list: &[Arc<str>], query: &str, options: FindOptions) -> Vec<String> {
        let matcher = Matcher::new(query, &options).unwrap();
        find_in_files(dir, list, &matcher, &HashMap::new(), &Cancel::new())
            .into_iter()
            .map(|m| {
                format!(
                    "{}:{}:{} {}",
                    m.path,
                    m.line + 1,
                    m.columns.start + 1,
                    m.text
                )
            })
            .collect()
    }

    #[test]
    fn finds_text_across_files_with_its_line_and_column() {
        let dir = tempfile::tempdir().unwrap();
        let list = files(
            dir.path(),
            &[
                ("src/a.rs", "fn main() {\n    let Total = 1;\n}\n"),
                ("src/b.go", "total := 2 // total\n"),
                ("notes.md", "nothing here\n"),
            ],
        );
        assert_eq!(
            find(dir.path(), &list, "total", FindOptions::default()),
            [
                "src/a.rs:2:9 let Total = 1;",
                "src/b.go:1:1 total := 2 // total",
                "src/b.go:1:15 total := 2 // total",
            ]
        );
        let case = FindOptions {
            case_sensitive: true,
            ..Default::default()
        };
        assert_eq!(find(dir.path(), &list, "Total", case).len(), 1);
    }

    #[test]
    fn whole_words_regex_and_file_masks() {
        let dir = tempfile::tempdir().unwrap();
        let list = files(
            dir.path(),
            &[
                ("a.rs", "count counter recount\n"),
                ("b.ts", "count = 3;\n"),
            ],
        );
        let words = FindOptions {
            whole_words: true,
            file_mask: "*.rs".into(),
            ..Default::default()
        };
        assert_eq!(
            find(dir.path(), &list, "count", words),
            ["a.rs:1:1 count counter recount"]
        );
        let regex = FindOptions {
            regex: true,
            ..Default::default()
        };
        assert_eq!(
            find(dir.path(), &list, r"count\s*=", regex),
            ["b.ts:1:1 count = 3;"]
        );
        assert!(
            Matcher::new(
                "(",
                &FindOptions {
                    regex: true,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn replacements_fill_in_groups_and_can_keep_case() {
        let options = |regex, preserve_case| FindOptions {
            regex,
            preserve_case,
            ..Default::default()
        };
        let text = "ann@home Count COUNT count";
        let at = |needle: &str| {
            let start = text.find(needle).unwrap();
            start..start + needle.len()
        };
        let pattern = compile(r"(\w+)@(\w+)", &options(true, false)).unwrap();
        assert_eq!(
            replacement(
                &pattern,
                text,
                at("ann@home"),
                "$2:$1",
                &options(true, false)
            ),
            "home:ann"
        );
        // Not a pattern: `$1` is just text.
        let plain = compile("ann@home", &options(false, false)).unwrap();
        assert_eq!(
            replacement(&plain, text, at("ann@home"), "$1", &options(false, false)),
            "$1"
        );
        let word = compile("count", &options(false, true)).unwrap();
        let keep = options(false, true);
        assert_eq!(
            replacement(&word, text, at("Count"), "total", &keep),
            "Total"
        );
        assert_eq!(
            replacement(&word, text, at("COUNT"), "total", &keep),
            "TOTAL"
        );
        assert_eq!(replacement(&word, text, 21..26, "Total", &keep), "total");
        assert_eq!(
            replacement(&word, text, at("Count"), "total", &options(false, false)),
            "total"
        );
    }

    #[test]
    fn unsaved_text_is_searched_instead_of_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let list = files(dir.path(), &[("a.rs", "old\n")]);
        let open = HashMap::from([(Arc::<str>::from("a.rs"), "new text\n".to_string())]);
        let matcher = Matcher::new("new", &FindOptions::default()).unwrap();
        let found = find_in_files(dir.path(), &list, &matcher, &open, &Cancel::new());
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_long_line_is_cut_around_the_match() {
        let line = format!("{}needle{}", "x".repeat(500), "y".repeat(500));
        let (shown, highlight) = excerpt(&line, 500..506);
        assert!(shown.len() < 260);
        assert_eq!(&shown[highlight], "needle");
    }
}
