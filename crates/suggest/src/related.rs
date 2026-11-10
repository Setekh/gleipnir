//! What the model is shown from other open files: from each, the piece that shares the most
//! names with the code around the cursor, so a type or function used here but defined there is
//! in view. Kept small, as every byte is read by the model before it can answer.

use std::collections::HashSet;

/// A file open beside the one being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherFile {
    /// As the model is shown it: the path within the project.
    pub name: String,
    pub text: String,
}

/// How many lines a piece is, and how far apart pieces start.
const PIECE_LINES: usize = 40;
const PIECE_STEP: usize = 20;
/// The most of one piece, and of all of them, in bytes.
const PIECE_BYTES: usize = 2000;
const TOTAL_BYTES: usize = 6000;
/// How many lines around the cursor its names are taken from: before, and after.
const NEAR_BEFORE: usize = 60;
const NEAR_AFTER: usize = 20;

/// Names of three or more characters, as code uses them.
fn names(text: &str) -> HashSet<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|w| w.len() >= 3 && !w.starts_with(|c: char| c.is_ascii_digit()))
        .collect()
}

/// The lines around `offset`, whose names say what's related.
fn near(text: &str, offset: usize) -> &str {
    let offset = offset.min(text.len());
    let start = text[..offset]
        .rmatch_indices('\n')
        .nth(NEAR_BEFORE)
        .map_or(0, |(i, _)| i + 1);
    let end = text[offset..]
        .match_indices('\n')
        .nth(NEAR_AFTER)
        .map_or(text.len(), |(i, _)| offset + i);
    &text[start..end]
}

/// Up to `limit` bytes of `text`, ending at a line's end.
fn whole_lines(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    match text[..end].rfind('\n') {
        Some(at) => &text[..=at],
        None => &text[..end],
    }
}

/// From each file, in the order given, the piece sharing the most names with the code around
/// the cursor, for as many as fit. Files sharing none are left out.
pub fn related(files: &[OtherFile], text: &str, offset: usize) -> Vec<OtherFile> {
    let wanted = names(near(text, offset));
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut chosen = Vec::new();
    let mut total = 0;
    for file in files {
        let starts: Vec<usize> = std::iter::once(0)
            .chain(file.text.match_indices('\n').map(|(i, _)| i + 1))
            .filter(|&i| i < file.text.len())
            .collect();
        let mut best: Option<(usize, &str)> = None;
        for first in (0..starts.len()).step_by(PIECE_STEP) {
            let end = starts
                .get(first + PIECE_LINES)
                .copied()
                .unwrap_or(file.text.len());
            let piece = whole_lines(&file.text[starts[first]..end], PIECE_BYTES);
            let shared = names(piece).intersection(&wanted).count();
            if shared > 0 && best.is_none_or(|(most, _)| shared > most) {
                best = Some((shared, piece));
            }
        }
        let Some((_, piece)) = best else {
            continue;
        };
        if total + piece.len() > TOTAL_BYTES {
            break;
        }
        total += piece.len();
        chosen.push(OtherFile {
            name: file.name.clone(),
            text: piece.to_string(),
        });
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, text: &str) -> OtherFile {
        OtherFile {
            name: name.into(),
            text: text.into(),
        }
    }

    #[test]
    fn the_piece_defining_what_the_cursor_uses_is_chosen() {
        let filler: String = (0..100)
            .map(|i| format!("const unrelated{i} = {i};\n"))
            .collect();
        let address =
            "export const ProvisioningAddress = types.model({\n  street: types.string,\n})\n";
        let other = file("src/address.ts", &format!("{filler}{address}{filler}"));
        let text = "const data = types.model({\n  address: types.maybe(ProvisioningAddress";
        let found = related(&[other], text, text.len());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "src/address.ts");
        assert!(found[0].text.contains(address), "{}", found[0].text);
        assert!(found[0].text.len() <= PIECE_BYTES);
    }

    #[test]
    fn files_sharing_nothing_are_left_out_and_the_total_is_bounded() {
        let unrelated = file("a.ts", "let zebra = 1;\n");
        let big = "shared_name();\n".repeat(400);
        let files: Vec<OtherFile> = (0..10).map(|i| file(&format!("{i}.ts"), &big)).collect();
        let text = "shared_name";
        assert!(related(&[unrelated], text, text.len()).is_empty());
        let found = related(&files, text, text.len());
        let total: usize = found.iter().map(|f| f.text.len()).sum();
        assert!(!found.is_empty() && total <= TOTAL_BYTES, "{total}");
        assert_eq!(found[0].name, "0.ts", "the first given comes first");
    }
}
