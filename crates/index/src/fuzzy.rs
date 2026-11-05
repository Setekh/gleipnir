//! Fuzzy matching in the IntelliJ style. Query characters must appear in order, and a match scores
//! higher the more of them land on word starts and the more of them are consecutive. So `usrCtl`
//! finds `UserController`, `gtu` finds `getUser`, and an exact name beats everything.
//!
//! Scoring is a dynamic program over (query, candidate) positions, in the spirit of fzf's, which
//! finds the best alignment rather than the first one.

/// What kind of character this is, for spotting word starts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Lower,
    Upper,
    Digit,
    Separator,
    Other,
}

fn class(c: char) -> Class {
    if c.is_lowercase() {
        Class::Lower
    } else if c.is_uppercase() {
        Class::Upper
    } else if c.is_numeric() {
        Class::Digit
    } else if matches!(c, '/' | '\\' | '_' | '-' | '.' | ' ' | ':') {
        Class::Separator
    } else {
        Class::Other
    }
}

const SCORE_MATCH: i32 = 16;
const BONUS_FIRST: i32 = 12;
const BONUS_SEPARATOR: i32 = 10;
const BONUS_CAMEL: i32 = 9;
const BONUS_DIGIT: i32 = 4;
const BONUS_CONSECUTIVE: i32 = 7;
const BONUS_CASE: i32 = 2;
const PENALTY_GAP_START: i32 = 4;
const PENALTY_GAP_EXTEND: i32 = 1;
/// A little preference for matches that start early in the candidate.
const PENALTY_LEADING_MAX: i32 = 8;
/// Longer candidates are cut short. Nothing worth matching is longer, and it bounds the work.
const MAX_CANDIDATE_CHARS: usize = 512;

/// How much a match at this position is worth, beyond the match itself.
fn position_bonus(previous: Option<char>, current: char) -> i32 {
    let Some(previous) = previous else {
        return BONUS_FIRST;
    };
    match (class(previous), class(current)) {
        (Class::Separator, Class::Separator) => 0,
        (Class::Separator, _) => BONUS_SEPARATOR,
        (Class::Lower, Class::Upper) => BONUS_CAMEL,
        (Class::Lower | Class::Upper, Class::Digit) => BONUS_DIGIT,
        (Class::Other, Class::Lower | Class::Upper | Class::Digit) => BONUS_SEPARATOR,
        _ => 0,
    }
}

/// A parsed query: its characters, and lowercase copies for comparing.
#[derive(Clone, Debug)]
pub struct Pattern {
    chars: Vec<char>,
    lower: Vec<char>,
}

impl Pattern {
    /// Spaces are ignored, as people put them in where a name has none.
    pub fn new(query: &str) -> Self {
        let chars: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
        let lower = chars
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect();
        Self { chars, lower }
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn contains_separator(&self) -> bool {
        self.chars.contains(&'/')
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    /// Byte offsets of the matched characters in the candidate, for highlighting.
    pub positions: Vec<usize>,
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Matches a pattern against a candidate, or returns `None` if its characters aren't all there in
/// order. An empty pattern matches everything with a score of zero.
pub fn fuzzy_match(pattern: &Pattern, candidate: &str) -> Option<FuzzyMatch> {
    if pattern.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }
    // The quick test first: most candidates fail it, and it costs one pass.
    let mut query = pattern.lower.iter();
    let mut wanted = query.next();
    for c in candidate.chars().take(MAX_CANDIDATE_CHARS) {
        if wanted == Some(&lower(c)) {
            wanted = query.next();
        }
    }
    if wanted.is_some() {
        return None;
    }

    let chars: Vec<(usize, char)> = candidate.char_indices().take(MAX_CANDIDATE_CHARS).collect();
    let n = chars.len();
    let m = pattern.chars.len();
    let bonus: Vec<i32> = (0..n)
        .map(|j| position_bonus(j.checked_sub(1).map(|p| chars[p].1), chars[j].1))
        .collect();

    // best[i][j]: the best score with query[..=i] matched and query[i] at candidate[j].
    // came_from[i][j]: where query[i - 1] was matched on that path.
    const NONE: i32 = i32::MIN / 2;
    let mut best = vec![NONE; m * n];
    let mut came_from = vec![usize::MAX; m * n];
    let at = |i: usize, j: usize| i * n + j;

    for i in 0..m {
        // For a match at column j after a gap: the best cell for query[i - 1] at column j - 2 or
        // earlier, with the gap's cost already taken off. Kept up to date as j advances, so each
        // row costs one pass.
        let mut gap_best = NONE;
        let mut gap_from = usize::MAX;
        for j in i..n {
            if i > 0 && j >= 2 {
                gap_best -= PENALTY_GAP_EXTEND;
                let newest = best[at(i - 1, j - 2)];
                if newest > NONE && newest - PENALTY_GAP_START > gap_best {
                    gap_best = newest - PENALTY_GAP_START;
                    gap_from = j - 2;
                }
            }
            let c = chars[j].1;
            if lower(c) != pattern.lower[i] {
                continue;
            }
            let mut gained = SCORE_MATCH + bonus[j];
            if pattern.chars[i] == c && c.is_uppercase() {
                gained += BONUS_CASE;
            }
            if i == 0 {
                // The first character's bonus counts double: where a match starts matters most.
                best[at(i, j)] = gained + bonus[j] - (j as i32).min(PENALTY_LEADING_MAX);
                continue;
            }
            let previous = if j >= 1 { best[at(i - 1, j - 1)] } else { NONE };
            let via_run = if previous > NONE {
                previous + gained + BONUS_CONSECUTIVE.max(bonus[j])
            } else {
                NONE
            };
            let via_gap = if gap_best > NONE {
                gap_best + gained
            } else {
                NONE
            };
            if via_run > NONE && via_run >= via_gap {
                best[at(i, j)] = via_run;
                came_from[at(i, j)] = j - 1;
            } else if via_gap > NONE {
                best[at(i, j)] = via_gap;
                came_from[at(i, j)] = gap_from;
            }
        }
    }

    let (end, score) = (m - 1..n)
        .map(|j| (j, best[at(m - 1, j)]))
        .max_by_key(|(j, score)| (*score, std::cmp::Reverse(*j)))?;
    if score <= NONE / 2 {
        return None;
    }
    let mut positions = vec![0; m];
    let mut j = end;
    for i in (0..m).rev() {
        positions[i] = chars[j].0;
        if i > 0 {
            j = came_from[at(i, j)];
        }
    }
    Some(FuzzyMatch { score, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, candidate: &str) -> Option<i32> {
        fuzzy_match(&Pattern::new(query), candidate).map(|m| m.score)
    }

    fn matched(query: &str, candidate: &str) -> String {
        let m = fuzzy_match(&Pattern::new(query), candidate).expect("should match");
        candidate
            .char_indices()
            .map(|(i, c)| {
                if m.positions.contains(&i) {
                    c.to_ascii_uppercase()
                } else {
                    '.'
                }
            })
            .collect()
    }

    /// Ranks candidates by score, best first.
    fn rank<'a>(query: &str, candidates: &[&'a str]) -> Vec<&'a str> {
        let pattern = Pattern::new(query);
        let mut scored: Vec<(i32, &str)> = candidates
            .iter()
            .filter_map(|c| fuzzy_match(&pattern, c).map(|m| (m.score, *c)))
            .collect();
        scored.sort_by_key(|(score, c)| (std::cmp::Reverse(*score), c.len()));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    #[test]
    fn characters_must_appear_in_order() {
        assert!(score("abc", "aXbXc").is_some());
        assert!(score("abc", "acb").is_none());
        assert!(score("abc", "ab").is_none());
    }

    #[test]
    fn matching_ignores_case_and_spaces() {
        assert!(score("USER", "user").is_some());
        assert!(score("user ctl", "UserController").is_some());
    }

    #[test]
    fn an_empty_query_matches_everything_equally() {
        assert_eq!(score("", "anything"), Some(0));
        assert_eq!(score("  ", "anything"), Some(0));
    }

    #[test]
    fn camel_humps_find_their_words() {
        assert_eq!(matched("usrCtl", "UserController"), "US.RC..T..L...");
        assert_eq!(
            rank("uc", &["unicorn", "UserController"]),
            ["UserController", "unicorn"]
        );
    }

    #[test]
    fn word_starts_are_preferred_over_the_first_occurrence() {
        // The `g` at the start of `Get` beats the earlier one inside `debug`.
        assert_eq!(matched("gu", "debugGetUser"), ".....G..U...");
    }

    #[test]
    fn an_exact_name_beats_a_longer_one_containing_it() {
        assert_eq!(
            rank("main", &["mainframe.rs", "main", "domain.rs"])[0],
            "main"
        );
    }

    #[test]
    fn a_prefix_beats_a_match_in_the_middle() {
        assert_eq!(
            rank("serv", &["observer", "server"]),
            ["server", "observer"]
        );
    }

    #[test]
    fn consecutive_letters_beat_scattered_ones() {
        assert_eq!(
            rank("view", &["v_i_e_w", "view_model"]),
            ["view_model", "v_i_e_w"]
        );
    }

    #[test]
    fn separators_mark_word_starts() {
        assert_eq!(matched("pp", "project_panel"), "P.......P....");
        assert_eq!(
            rank("pp", &["apple", "project_panel"]),
            ["project_panel", "apple"]
        );
    }

    #[test]
    fn path_segments_count_as_words() {
        assert_eq!(matched("ed/vi", "src/editor/view.rs"), "....ED..../VI.....");
    }

    #[test]
    fn uppercase_in_the_query_prefers_the_same_case() {
        let exact = score("Get", "Get").unwrap();
        let other = score("Get", "get").unwrap();
        assert!(exact > other);
    }

    #[test]
    fn positions_are_byte_offsets_for_unicode() {
        let m = fuzzy_match(&Pattern::new("éx"), "aéx").unwrap();
        assert_eq!(m.positions, [1, 3]);
    }

    #[test]
    fn very_long_candidates_are_bounded() {
        let long = "a".repeat(10_000) + "z";
        // The `z` is past the limit, so this doesn't match, and it doesn't take long either.
        assert!(score("az", &long).is_none());
    }
}
