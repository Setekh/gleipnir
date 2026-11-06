//! Just enough Markdown for the documentation language servers send: code blocks, paragraphs and
//! rules, with inline markup removed rather than rendered.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Code(String),
    Text(String),
    Rule,
}

/// Removes inline markup: emphasis, inline code ticks, and links (keeping their text).
pub fn plain_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => {}
            '*' | '_' if chars.peek() == Some(&c) => {
                chars.next();
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '[' => {
                // [text](target) keeps the text; anything else stays as written.
                let rest: String = chars.clone().collect();
                if let Some(close) = rest.find("](")
                    && let Some(end) = rest[close..].find(')')
                {
                    out.push_str(&rest[..close]);
                    for _ in 0..close + end + 1 {
                        chars.next();
                    }
                } else {
                    out.push(c);
                }
            }
            c => out.push(c),
        }
    }
    out
}

pub fn blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut code: Option<Vec<String>> = None;
    let flush = |paragraph: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Text(plain_inline(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            match code.take() {
                Some(lines) => {
                    let text = lines.join("\n");
                    if !text.trim().is_empty() {
                        blocks.push(Block::Code(text));
                    }
                }
                None => {
                    flush(&mut paragraph, &mut blocks);
                    code = Some(Vec::new());
                }
            }
            continue;
        }
        if let Some(lines) = &mut code {
            lines.push(line.to_string());
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut paragraph, &mut blocks);
        } else if trimmed.chars().all(|c| c == '-' || c == '*' || c == '_') && trimmed.len() >= 3 {
            flush(&mut paragraph, &mut blocks);
            if !matches!(blocks.last(), Some(Block::Rule) | None) {
                blocks.push(Block::Rule);
            }
        } else {
            let heading = trimmed.trim_start_matches('#').trim_start();
            if heading.len() != trimmed.len() || trimmed.starts_with("- ") {
                flush(&mut paragraph, &mut blocks);
                paragraph.push(trimmed.trim_start_matches('#').trim().to_string());
                flush(&mut paragraph, &mut blocks);
            } else {
                paragraph.push(trimmed.to_string());
            }
        }
    }
    if let Some(lines) = code {
        blocks.push(Block::Code(lines.join("\n")));
    }
    flush(&mut paragraph, &mut blocks);
    while matches!(blocks.last(), Some(Block::Rule)) {
        blocks.pop();
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_blocks_and_paragraphs_are_separated() {
        let markdown =
            "```rust\nfn area(&self) -> f64\n```\n\n---\n\nComputes the **area**,\nin `units`.\n";
        assert_eq!(
            blocks(markdown),
            [
                Block::Code("fn area(&self) -> f64".into()),
                Block::Rule,
                Block::Text("Computes the area, in units.".into()),
            ]
        );
    }

    #[test]
    fn links_keep_their_text() {
        assert_eq!(
            plain_inline("See [the docs](https://x) and [this]."),
            "See the docs and [this]."
        );
    }

    #[test]
    fn headings_and_list_items_become_their_own_lines() {
        assert_eq!(
            blocks("# Title\nbody\n- one\n- two"),
            [
                Block::Text("Title".into()),
                Block::Text("body".into()),
                Block::Text("- one".into()),
                Block::Text("- two".into())
            ]
        );
    }

    #[test]
    fn an_unclosed_fence_still_shows_its_code_and_trailing_rules_go() {
        assert_eq!(
            blocks("text\n---\n```\nlet x"),
            [
                Block::Text("text".into()),
                Block::Rule,
                Block::Code("let x".into())
            ]
        );
        assert_eq!(blocks("a\n\n---\n"), [Block::Text("a".into())]);
    }
}
