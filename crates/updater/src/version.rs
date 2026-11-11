//! Comparing versions like `0.2.0` and `0.2.0-rc.1`, as semver orders them.

use std::cmp::Ordering;

/// Whether `candidate` is a later version than `current`. A version that can't be read is never
/// newer, so a strange tag doesn't start an update.
pub fn newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(candidate), Some(current)) => compare(&candidate, &current) == Ordering::Greater,
        _ => false,
    }
}

struct Version<'a> {
    numbers: [u64; 3],
    /// What follows a dash, like `rc.1`: such a version comes before the plain one.
    pre: Option<&'a str>,
}

fn parse(text: &str) -> Option<Version<'_>> {
    let text = text.strip_prefix('v').unwrap_or(text);
    // Build details after a plus don't order versions.
    let text = text.split('+').next()?;
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (text, None),
    };
    let mut parts = core.split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        *number = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(Version { numbers, pre })
}

fn compare(a: &Version, b: &Version) -> Ordering {
    a.numbers
        .cmp(&b.numbers)
        .then_with(|| match (a.pre, b.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => compare_pre(a, b),
        })
}

/// Dot-separated parts in turn: numbers by value and before words, words by their letters, and
/// the one with more parts last.
fn compare_pre(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.split('.'), b.split('.'));
    loop {
        let order = match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => a.cmp(b),
            },
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn later_versions_are_newer_and_the_rest_are_not() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn a_pre_release_comes_before_its_release() {
        assert!(newer("0.2.0", "0.2.0-rc.1"));
        assert!(!newer("0.2.0-rc.1", "0.2.0"));
        assert!(newer("0.2.0-rc.2", "0.2.0-rc.1"));
        assert!(newer("0.2.0-rc.10", "0.2.0-rc.9"));
        assert!(newer("0.2.0-rc.1", "0.2.0-beta.3"));
        assert!(newer("0.2.0-rc.1", "0.1.0"));
    }

    #[test]
    fn a_tag_that_is_not_a_version_never_starts_an_update() {
        assert!(!newer("nightly", "0.1.0"));
        assert!(!newer("1.2", "0.1.0"));
        assert!(!newer("1.2.3.4", "0.1.0"));
        assert!(!newer("", "0.1.0"));
    }
}
