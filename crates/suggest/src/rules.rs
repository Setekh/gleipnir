//! Suggestions without a model, from the file itself: how the text just typed goes on where it
//! appears elsewhere, and the punctuation the lines around end with. Each is scored by how often
//! the evidence agrees, so a suggestion every similar place backs scores 100%.

/// How far from the cursor to look, in lines, so a long file stays quick.
const LOOK_AROUND: usize = 2000;
/// The longest bit of the typed text to match, in characters, and the shortest worth matching.
const LONGEST_CONTEXT: usize = 40;
const SHORTEST_CONTEXT: usize = 4;

/// A suggestion for the cursor at `offset`: the text, and how often the evidence agrees.
pub fn suggest(text: &str, offset: usize) -> Option<(String, f32)> {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    // At the end of a line only, as there's nothing to write over.
    if !text[offset..line_end].trim().is_empty() {
        return None;
    }
    let typed = &text[line_start..offset];
    if typed.trim().is_empty() {
        return None;
    }
    continuation(text, line_start, typed).or_else(|| line_ending(text, line_start, typed))
}

/// The lines near `line_start`, other than its own, nearest first.
fn nearby_lines(text: &str, line_start: usize) -> Vec<&str> {
    let before: Vec<&str> = text[..line_start].lines().collect();
    let after_start = text[line_start..]
        .find('\n')
        .map_or(text.len(), |i| line_start + i + 1);
    let after: Vec<&str> = text[after_start..].lines().take(LOOK_AROUND).collect();
    let mut lines = Vec::new();
    let mut up = before.iter().rev().take(LOOK_AROUND);
    let mut down = after.iter();
    loop {
        let (a, b) = (up.next(), down.next());
        if a.is_none() && b.is_none() {
            break;
        }
        lines.extend(a.copied());
        lines.extend(b.copied());
    }
    lines
}

/// How the end of what's been typed goes on elsewhere: the longest end of it that appears in
/// other lines, and what most often follows it there, up to the line's end.
fn continuation(text: &str, line_start: usize, typed: &str) -> Option<(String, f32)> {
    let lines = nearby_lines(text, line_start);
    let typed = typed.trim_start();
    let chars: Vec<(usize, char)> = typed.char_indices().collect();
    let longest = chars.len().min(LONGEST_CONTEXT);
    for len in (SHORTEST_CONTEXT..=longest).rev() {
        let context = &typed[chars[chars.len() - len].0..];
        // Starting mid-word would match inside other words.
        if context.starts_with(|c: char| c.is_alphanumeric() || c == '_')
            && (chars.len() - len)
                .checked_sub(1)
                .and_then(|i| chars.get(i))
                .is_some_and(|(_, c)| c.is_alphanumeric() || *c == '_')
        {
            continue;
        }
        let mut found: Vec<&str> = Vec::new();
        for line in &lines {
            let mut from = 0;
            while let Some(at) = line[from..].find(context) {
                let after = &line[from + at + context.len()..];
                found.push(after.trim_end());
                from += at + context.len();
            }
        }
        if found.is_empty() {
            continue;
        }
        let total = found.len();
        let followed: Vec<&str> = found.into_iter().filter(|f| !f.is_empty()).collect();
        // Mostly where it ends a line: nothing to suggest, and shorter matches won't know better.
        if followed.len() * 2 < total {
            return None;
        }
        let (best, count) = most_common(&followed)?;
        return Some((best.to_string(), count as f32 / total as f32));
    }
    None
}

/// The punctuation the lines beside this one end with, like the `,` between object fields,
/// when this one hasn't it and they mostly agree.
fn line_ending(text: &str, line_start: usize, typed: &str) -> Option<(String, f32)> {
    let last = typed.trim_end().chars().last()?;
    if matches!(
        last,
        ',' | ';' | '{' | '(' | '[' | ':' | '=' | '+' | '-' | '.' | '&' | '|'
    ) {
        return None;
    }
    let indent = |line: &str| line.len() - line.trim_start().len();
    let own = indent(typed);
    // Siblings: lines at the same indent, without leaving the block.
    let mut siblings = Vec::new();
    let before: Vec<&str> = text[..line_start].lines().collect();
    for line in before.iter().rev() {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) < own {
            break;
        }
        if indent(line) == own {
            siblings.push(*line);
        }
        if siblings.len() >= 12 {
            break;
        }
    }
    let after_start = text[line_start..]
        .find('\n')
        .map_or(text.len(), |i| line_start + i + 1);
    for line in text[after_start..].lines() {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) < own || siblings.len() >= 24 {
            break;
        }
        if indent(line) == own {
            siblings.push(line);
        }
    }
    if siblings.len() < 2 {
        return None;
    }
    let endings: Vec<&str> = siblings
        .iter()
        .map(|l| match l.trim_end().chars().last() {
            Some(',') => ",",
            Some(';') => ";",
            _ => "",
        })
        .collect();
    let (best, count) = most_common(&endings)?;
    let score = count as f32 / endings.len() as f32;
    (!best.is_empty() && score >= 0.6).then(|| (best.to_string(), score))
}

fn most_common<'a>(items: &[&'a str]) -> Option<(&'a str, usize)> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for item in items {
        match counts.iter_mut().find(|(i, _)| i == item) {
            Some((_, n)) => *n += 1,
            None => counts.push((item, 1)),
        }
    }
    // The most common; between equals, the one seen first, which is the nearest.
    counts.into_iter().rev().max_by_key(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screenshot's case: a new field, among fields that end with commas.
    const STORE: &str = "export const ProvisioningData = types.model({\n  fsiInstanceId: types.maybe(types.string),\n  inputCode: types.maybe(types.string),\n  address: types.maybe(ProvisioningAddress),\n  address2: types.maybe(ProvisioningAddress)\n  partyName: types.maybe(types.string),\n})\n";

    fn at_end_of(text: &str, line: &str) -> usize {
        text.find(line).unwrap() + line.len()
    }

    #[test]
    fn a_missing_comma_among_fields_is_suggested() {
        let offset = at_end_of(STORE, "address2: types.maybe(ProvisioningAddress)");
        assert_eq!(suggest(STORE, offset), Some((",".to_string(), 1.0)));
    }

    #[test]
    fn typed_text_goes_on_as_it_does_elsewhere() {
        let text = "a: types.maybe(types.string),\nb: types.maybe(types.number),\nc: types.maybe(types.string),\nd: types.maybe(ty";
        let (suggestion, score) = suggest(text, text.len()).unwrap();
        assert_eq!(suggestion, "pes.string),");
        assert!((score - 2. / 3.).abs() < 1e-6, "{score}");
    }

    #[test]
    fn nothing_is_suggested_mid_line_on_blank_lines_or_without_evidence() {
        let text = "let a = 1;\nlet b = 2;\n";
        assert_eq!(suggest(text, 4), None, "mid-line");
        assert_eq!(suggest("\n\n", 1), None, "a blank line");
        assert_eq!(suggest("fn unique_name_here", 19), None, "no evidence");
    }

    #[test]
    fn statements_get_the_semicolons_their_neighbours_have() {
        let text = "fn f() {\n    let a = 1;\n    let b = 2;\n    let c = 3\n}\n";
        let offset = at_end_of(text, "let c = 3");
        // `= 3` isn't found elsewhere; the neighbours end with `;`.
        assert_eq!(suggest(text, offset), Some((";".to_string(), 1.0)));
    }
}
