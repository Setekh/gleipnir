//! Searching the project's files and symbols, in parallel, keeping only the best results.

use crate::fuzzy::{FuzzyMatch, Pattern, fuzzy_match};
use gleipnir_syntax::{Symbol, SymbolKind};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// What was typed, split into the search text and an optional `:line:column` suffix, which jumps
/// straight to that place once a file is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl Query {
    pub fn parse(input: &str) -> Self {
        let input = input.trim();
        let mut parts = input.rsplitn(3, ':');
        let last = parts.next().unwrap_or("");
        let middle = parts.next();
        let first = parts.next();
        let number = |s: &str| s.parse::<u32>().ok().filter(|n| *n > 0);
        match (first, middle, last) {
            (Some(text), Some(line), column)
                if number(line).is_some() && number(column).is_some() =>
            {
                Self {
                    text: text.to_string(),
                    line: number(line),
                    column: number(column),
                }
            }
            (_, Some(_), line) if number(line).is_some() => {
                let text = input[..input.len() - line.len() - 1].to_string();
                Self {
                    text,
                    line: number(line),
                    column: None,
                }
            }
            _ => Self {
                text: input.to_string(),
                line: None,
                column: None,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHit {
    pub path: Arc<str>,
    pub score: i32,
    /// Matched byte offsets in `path`.
    pub positions: Vec<usize>,
}

/// A symbol and the file it's in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolEntry {
    pub path: Arc<str>,
    pub symbol: Symbol,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolHit {
    pub entry: SymbolEntry,
    pub score: i32,
    /// Matched byte offsets in the symbol's name.
    pub positions: Vec<usize>,
}

/// Lets a newer search stop an older one that's still running.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Runs a scorer over items on every core and keeps the `limit` best. Ties go to the item that
/// comes first, for a stable order as the query grows.
fn top_matches<T: Sync, R: Send>(
    items: &[T],
    limit: usize,
    cancel: &Cancel,
    score: impl Fn(&T) -> Option<(i32, R)> + Sync,
) -> Vec<(i32, usize, R)> {
    if items.is_empty() || limit == 0 {
        return Vec::new();
    }
    let threads = thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let chunk = items.len().div_ceil(threads).max(512);
    let score = &score;
    let partials: Vec<Vec<(i32, usize, R)>> = thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .enumerate()
            .map(|(chunk_index, slice)| {
                scope.spawn(move || {
                    // A min-heap of the best so far, so the worst is cheap to drop.
                    let mut heap: BinaryHeap<(Reverse<i32>, usize, usize)> = BinaryHeap::new();
                    let mut kept: Vec<Option<R>> = Vec::new();
                    for (offset, item) in slice.iter().enumerate() {
                        if offset % 1024 == 0 && cancel.is_cancelled() {
                            return Vec::new();
                        }
                        let Some((s, result)) = score(item) else {
                            continue;
                        };
                        let index = chunk_index * chunk + offset;
                        if heap.len() < limit {
                            kept.push(Some(result));
                            heap.push((Reverse(s), index, kept.len() - 1));
                        } else if let Some(&(Reverse(worst), worst_index, slot)) = heap.peek()
                            && (s > worst || (s == worst && index < worst_index))
                        {
                            heap.pop();
                            kept[slot] = Some(result);
                            heap.push((Reverse(s), index, slot));
                        }
                    }
                    heap.into_iter()
                        .map(|(Reverse(s), index, slot)| {
                            (s, index, kept[slot].take().expect("each slot is used once"))
                        })
                        .collect()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a search thread"))
            .collect()
    });
    let mut all: Vec<(i32, usize, R)> = partials.into_iter().flatten().collect();
    all.sort_by_key(|(s, index, _)| (Reverse(*s), *index));
    all.truncate(limit);
    all
}

/// How much a file match is worth on top of the raw fuzzy score.
fn file_score(path: &str, pattern: &Pattern, query_tail: &str) -> Option<(i32, Vec<usize>)> {
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    let name = &path[name_start..];
    if !pattern.contains_separator() {
        // A query without a slash is about the file's name, as in IntelliJ.
        let FuzzyMatch { score, positions } = fuzzy_match(pattern, name)?;
        let stem = name.split('.').next().unwrap_or(name);
        let exact = stem.eq_ignore_ascii_case(query_tail) || name.eq_ignore_ascii_case(query_tail);
        let bonus = if exact { 40 } else { 0 };
        // Shallower files win ties, as they're usually the more central ones.
        let depth = path.matches('/').count() as i32;
        let shifted = positions.into_iter().map(|p| p + name_start).collect();
        return Some((score + bonus - depth, shifted));
    }
    // With a slash, the whole path counts, and the name has to match the last part of the query.
    let FuzzyMatch { score, positions } = fuzzy_match(pattern, path)?;
    let tail = Pattern::new(query_tail);
    let stem = name.split('.').next().unwrap_or(name);
    let tail_bonus =
        if stem.eq_ignore_ascii_case(query_tail) || name.eq_ignore_ascii_case(query_tail) {
            40
        } else {
            fuzzy_match(&tail, name).map_or(-30, |m| m.score / 4)
        };
    Some((score + tail_bonus, positions))
}

pub fn search_files(
    files: &[Arc<str>],
    query: &str,
    limit: usize,
    cancel: &Cancel,
) -> Vec<FileHit> {
    let pattern = Pattern::new(query);
    let tail = query.rsplit('/').next().unwrap_or(query).trim().to_string();
    top_matches(files, limit, cancel, |path| {
        let (score, positions) = file_score(path, &pattern, &tail)?;
        Some((score, (path.clone(), positions)))
    })
    .into_iter()
    .map(|(score, _, (path, positions))| FileHit {
        path,
        score,
        positions,
    })
    .collect()
}

/// Which symbols a search covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolFilter {
    All,
    TypesOnly,
}

pub fn search_symbols(
    symbols: &[SymbolEntry],
    query: &str,
    filter: SymbolFilter,
    limit: usize,
    cancel: &Cancel,
) -> Vec<SymbolHit> {
    let pattern = Pattern::new(query);
    if pattern.is_empty() {
        return Vec::new();
    }
    top_matches(symbols, limit, cancel, |entry| {
        if filter == SymbolFilter::TypesOnly && !entry.symbol.kind.is_type() {
            return None;
        }
        let FuzzyMatch { score, positions } = fuzzy_match(&pattern, &entry.symbol.name)?;
        let exact = entry.symbol.name.eq_ignore_ascii_case(query.trim());
        // Types are what people look for most, then functions.
        let kind_bonus = match entry.symbol.kind {
            kind if kind.is_type() => 6,
            SymbolKind::Function | SymbolKind::Method => 3,
            _ => 0,
        };
        let score = score + if exact { 40 } else { 0 } + kind_bonus;
        Some((score, (entry.clone(), positions)))
    })
    .into_iter()
    .map(|(score, _, (entry, positions))| SymbolHit {
        entry,
        score,
        positions,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<Arc<str>> {
        paths.iter().map(|p| Arc::from(*p)).collect()
    }

    fn top(files: &[Arc<str>], query: &str) -> Vec<String> {
        search_files(files, query, 10, &Cancel::new())
            .into_iter()
            .map(|h| h.path.to_string())
            .collect()
    }

    #[test]
    fn queries_split_off_a_line_and_column() {
        assert_eq!(
            Query::parse("main.rs:12"),
            Query {
                text: "main.rs".into(),
                line: Some(12),
                column: None
            }
        );
        assert_eq!(
            Query::parse("main.rs:12:4"),
            Query {
                text: "main.rs".into(),
                line: Some(12),
                column: Some(4)
            }
        );
        assert_eq!(
            Query::parse("a::b"),
            Query {
                text: "a::b".into(),
                line: None,
                column: None
            }
        );
        assert_eq!(
            Query::parse("plain"),
            Query {
                text: "plain".into(),
                line: None,
                column: None
            }
        );
        assert_eq!(
            Query::parse("x:0"),
            Query {
                text: "x:0".into(),
                line: None,
                column: None
            }
        );
    }

    #[test]
    fn file_search_matches_names_not_folders() {
        let list = files(&["src/editor/view.rs", "editor.rs", "docs/edit.md"]);
        // `edit` isn't looked for in the folder names, so the file in `editor/` doesn't match.
        assert_eq!(top(&list, "edit"), ["docs/edit.md", "editor.rs"]);
    }

    #[test]
    fn an_exact_file_name_comes_first() {
        let list = files(&["src/main_window.rs", "a/b/c/main.rs", "domain.rs"]);
        assert_eq!(top(&list, "main")[0], "a/b/c/main.rs");
    }

    #[test]
    fn a_slash_searches_the_path_and_prefers_the_name_matching_the_end() {
        let list = files(&[
            "crates/editor/src/view.rs",
            "crates/eddy/viewer.rs",
            "media/review.md",
        ]);
        assert_eq!(top(&list, "ed/view")[0], "crates/editor/src/view.rs");
    }

    #[test]
    fn positions_point_into_the_whole_path() {
        let list = files(&["src/lib.rs"]);
        let hit = &search_files(&list, "lib", 1, &Cancel::new())[0];
        assert_eq!(hit.positions, [4, 5, 6]);
    }

    #[test]
    fn only_the_best_few_are_kept_and_in_order() {
        let list: Vec<Arc<str>> = (0..5000)
            .map(|i| Arc::from(format!("f{i}.rs").as_str()))
            .collect();
        let hits = search_files(&list, "f1", 5, &Cancel::new());
        assert_eq!(hits.len(), 5);
        for pair in hits.windows(2) {
            assert!(pair[0].score >= pair[1].score);
        }
    }

    #[test]
    fn a_cancelled_search_stops() {
        let list: Vec<Arc<str>> = (0..50_000)
            .map(|i| Arc::from(format!("f{i}.rs").as_str()))
            .collect();
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(search_files(&list, "f", 10, &cancel).is_empty());
    }

    fn symbol(path: &str, name: &str, kind: SymbolKind) -> SymbolEntry {
        SymbolEntry {
            path: Arc::from(path),
            symbol: Symbol {
                name: name.into(),
                kind,
                container: None,
                line: 0,
                column: 0,
            },
        }
    }

    #[test]
    fn symbol_search_can_be_limited_to_types() {
        let list = vec![
            symbol("a.rs", "UserService", SymbolKind::Class),
            symbol("a.rs", "user_service", SymbolKind::Function),
        ];
        let all = search_symbols(&list, "userserv", SymbolFilter::All, 10, &Cancel::new());
        assert_eq!(all.len(), 2);
        let types = search_symbols(
            &list,
            "userserv",
            SymbolFilter::TypesOnly,
            10,
            &Cancel::new(),
        );
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].entry.symbol.name, "UserService");
    }

    #[test]
    fn an_exact_symbol_name_comes_first() {
        let list = vec![
            symbol("a.rs", "ParserState", SymbolKind::Class),
            symbol("b.rs", "Parser", SymbolKind::Class),
            symbol("c.rs", "parse_row", SymbolKind::Function),
        ];
        let hits = search_symbols(&list, "Parser", SymbolFilter::All, 10, &Cancel::new());
        assert_eq!(hits[0].entry.symbol.name, "Parser");
    }

    #[test]
    fn an_empty_symbol_query_returns_nothing() {
        let list = vec![symbol("a.rs", "A", SymbolKind::Class)];
        assert!(search_symbols(&list, " ", SymbolFilter::All, 10, &Cancel::new()).is_empty());
    }

    /// Run with: cargo test -p gleipnir-index --release -- --ignored --nocapture perf
    #[test]
    #[ignore]
    fn perf_search_over_100k_files() {
        let words = [
            "user",
            "service",
            "controller",
            "model",
            "view",
            "util",
            "http",
            "parser",
        ];
        let list: Vec<Arc<str>> = (0..100_000)
            .map(|i| {
                let a = words[i % words.len()];
                let b = words[(i / 8) % words.len()];
                Arc::from(format!("src/{a}/{b}_{i}/{a}_{b}{}.rs", i % 97).as_str())
            })
            .collect();
        for query in ["u", "usco", "UserService", "view/parser", "zzz"] {
            let started = std::time::Instant::now();
            let hits = search_files(&list, query, 50, &Cancel::new());
            println!("{query:>12}: {:?} ({} hits)", started.elapsed(), hits.len());
        }
    }
}
