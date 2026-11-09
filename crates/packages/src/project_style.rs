//! A project's own say on indentation, from the files the tools it uses read: `.editorconfig`,
//! and for the files Prettier formats, Prettier's config. These win over the language's defaults
//! and the person's settings, as in IntelliJ, so a file is indented the way its project formats it.

use globset::{GlobBuilder, GlobMatcher};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Indentation a project sets for a file, and which file said so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectIndent {
    pub tab_size: Option<usize>,
    pub hard_tabs: Option<bool>,
    /// The config files that said so, nearest last, to show where the indentation comes from.
    pub sources: Vec<PathBuf>,
}

impl ProjectIndent {
    fn take(&mut self, other: ProjectIndent) {
        if other.tab_size.is_some() {
            self.tab_size = other.tab_size;
        }
        if other.hard_tabs.is_some() {
            self.hard_tabs = other.hard_tabs;
        }
        self.sources.extend(other.sources);
    }

    pub fn is_empty(&self) -> bool {
        self.tab_size.is_none() && self.hard_tabs.is_none()
    }
}

/// What the project says about indenting a file: `.editorconfig`, then Prettier's config for
/// the files Prettier formats, which wins as Prettier would.
pub fn project_indent(file: &Path) -> ProjectIndent {
    let mut indent = editorconfig_indent(file);
    if prettier_formats(file) {
        indent.take(prettier_indent(file));
    }
    indent
}

// EditorConfig.

/// `.editorconfig` files from the one marked `root = true`, or the top, down to the file's
/// folder; the nearer a file and the later a section, the more it counts.
pub fn editorconfig_indent(file: &Path) -> ProjectIndent {
    let mut configs = Vec::new();
    for dir in file.ancestors().skip(1) {
        let path = dir.join(".editorconfig");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let root = parse_editorconfig(&text).0;
            configs.push((dir.to_path_buf(), path, text));
            if root {
                break;
            }
        }
    }
    let mut indent = ProjectIndent::default();
    for (dir, path, text) in configs.into_iter().rev() {
        let (_, sections) = parse_editorconfig(&text);
        let Ok(relative) = file.strip_prefix(&dir) else {
            continue;
        };
        let mut found = ProjectIndent::default();
        let mut indent_size: Option<String> = None;
        let mut tab_width: Option<usize> = None;
        for (pattern, pairs) in &sections {
            if !editorconfig_matches(pattern, relative) {
                continue;
            }
            for (key, value) in pairs {
                match key.as_str() {
                    "indent_style" => match value.as_str() {
                        "tab" => found.hard_tabs = Some(true),
                        "space" => found.hard_tabs = Some(false),
                        _ => {}
                    },
                    "indent_size" => indent_size = Some(value.clone()),
                    "tab_width" => tab_width = value.parse().ok(),
                    _ => {}
                }
            }
        }
        // `indent_size = tab` means the tab width; a tab width alone sets the size too.
        found.tab_size = match indent_size.as_deref() {
            Some("tab") => tab_width,
            Some(size) => size.parse().ok().or(tab_width),
            None => tab_width,
        }
        .filter(|&n| n > 0);
        if !found.is_empty() {
            found.sources.push(path);
        }
        indent.take(found);
    }
    indent
}

/// An EditorConfig section: its pattern, and its (key, value) pairs.
type Section = (String, Vec<(String, String)>);

/// Whether the file says it's the top, and its sections as (pattern, [(key, value)]), with keys
/// and values lowercased as the format says.
fn parse_editorconfig(text: &str) -> (bool, Vec<Section>) {
    let mut root = false;
    let mut sections: Vec<Section> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(pattern) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((pattern.to_string(), Vec::new()));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim().to_lowercase(), value.trim().to_lowercase());
        match sections.last_mut() {
            Some((_, pairs)) => pairs.push((key, value)),
            None if key == "root" => root = value == "true",
            None => {}
        }
    }
    (root, sections)
}

/// An EditorConfig section's pattern against a path relative to its file's folder: without a
/// slash it matches the name at any depth; with one, from the folder.
fn editorconfig_matches(pattern: &str, relative: &Path) -> bool {
    let pattern = if pattern.contains('/') {
        pattern.trim_start_matches('/').to_string()
    } else {
        format!("**/{pattern}")
    };
    // EditorConfig's `**` crosses folders wherever it is, as in `lib/**.rs`; globset's only as a
    // whole part of the path, so `**.rs` is written `**/*.rs`.
    let pattern = pattern
        .split("**")
        .enumerate()
        .map(|(i, part)| {
            if i > 0 && !part.is_empty() && !part.starts_with('/') {
                format!("/*{part}")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("**");
    glob(&pattern).is_some_and(|g| g.is_match(relative))
}

fn glob(pattern: &str) -> Option<GlobMatcher> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

// Prettier.

/// The files Prettier formats, by extension, which its config applies to.
fn prettier_formats(file: &Path) -> bool {
    const FORMATTED: &[&str] = &[
        "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "json", "json5", "css", "scss",
        "less", "html", "vue", "md", "mdx", "yaml", "yml", "graphql", "gql",
    ];
    file.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| FORMATTED.contains(&e))
}

/// The names Prettier looks for in each folder, in its order.
const PRETTIER_FILES: &[&str] = &[
    "package.json",
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.json5",
    ".prettierrc.toml",
];

/// Prettier's config for a file: the nearest folder up with one, as Prettier finds it, with its
/// `overrides` that match the file applied over the rest.
pub fn prettier_indent(file: &Path) -> ProjectIndent {
    for dir in file.ancestors().skip(1) {
        for name in PRETTIER_FILES {
            let path = dir.join(name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let config = match *name {
                // Only a package.json with a `prettier` key is a config.
                "package.json" => match serde_json::from_str::<Value>(&text) {
                    Ok(json) => match json.get("prettier") {
                        Some(config @ Value::Object(_)) => config.clone(),
                        _ => continue,
                    },
                    Err(_) => continue,
                },
                ".prettierrc.toml" => match toml::from_str::<toml::Value>(&text)
                    .ok()
                    .and_then(|t| serde_json::to_value(t).ok())
                {
                    Some(config) => config,
                    None => continue,
                },
                ".prettierrc.yaml" | ".prettierrc.yml" => simple_yaml(&text),
                // JSON, or JSON5, or YAML: `.prettierrc` may be either.
                _ => lenient_json(&text).unwrap_or_else(|| simple_yaml(&text)),
            };
            let Ok(relative) = file.strip_prefix(dir) else {
                return ProjectIndent::default();
            };
            let mut indent = prettier_options(&config);
            for over in config["overrides"].as_array().into_iter().flatten() {
                if prettier_override_matches(over, relative) {
                    indent.take(prettier_options(&over["options"]));
                }
            }
            if !indent.is_empty() {
                indent.sources.push(path);
            }
            return indent;
        }
    }
    ProjectIndent::default()
}

fn prettier_options(options: &Value) -> ProjectIndent {
    ProjectIndent {
        tab_size: options["tabWidth"]
            .as_u64()
            .map(|n| n as usize)
            .filter(|&n| n > 0),
        hard_tabs: options["useTabs"].as_bool(),
        sources: Vec::new(),
    }
}

/// Whether one of a Prettier config's `overrides` is for a file: its `files` match, and its
/// `excludeFiles` don't. Patterns without a slash match the name at any depth.
fn prettier_override_matches(over: &Value, relative: &Path) -> bool {
    let patterns = |value: &Value| -> Vec<String> {
        match value {
            Value::String(s) => vec![s.clone()],
            Value::Array(items) => items
                .iter()
                .filter_map(|i| i.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        }
    };
    let matches = |pattern: &String| {
        let pattern = if pattern.contains('/') {
            pattern.trim_start_matches("./").to_string()
        } else {
            format!("**/{pattern}")
        };
        glob(&pattern).is_some_and(|g| g.is_match(relative))
    };
    patterns(&over["files"]).iter().any(matches)
        && !patterns(&over["excludeFiles"]).iter().any(matches)
}

/// JSON, allowing what JSON5 configs commonly add: comments and trailing commas.
fn lenient_json(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return value.is_object().then_some(value);
    }
    let mut cleaned = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            cleaned.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    cleaned.push(next);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                cleaned.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        cleaned.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            _ => cleaned.push(c),
        }
    }
    // Commas before a closing bracket.
    let mut out = String::with_capacity(cleaned.len());
    let chars: Vec<char> = cleaned.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    serde_json::from_str::<Value>(&quote_keys(&out))
        .ok()
        .filter(Value::is_object)
}

/// JSON5's bare keys, like `tabWidth: 2`, quoted as JSON needs them.
fn quote_keys(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut in_string = false;
    let mut expecting_key = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if expecting_key && (c.is_alphabetic() || c == '_' || c == '$') {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let rest = chars[i..].iter().find(|c| !c.is_whitespace());
            if rest == Some(&':') {
                out.push('"');
                out.push_str(&word);
                out.push('"');
            } else {
                out.push_str(&word);
            }
            expecting_key = false;
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | ',' => expecting_key = true,
            c if c.is_whitespace() => {}
            _ => expecting_key = false,
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The top level of a YAML config, as `key: value` lines: enough for Prettier's options, which
/// are numbers, booleans and strings. Nested parts, like `overrides`, are left out.
fn simple_yaml(text: &str) -> Value {
    let mut map = serde_json::Map::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '-']) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.split(" #").next().unwrap_or("").trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        let parsed = match value {
            "" => continue,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => value
                .parse::<u64>()
                .map(Value::from)
                .unwrap_or_else(|_| Value::String(value.to_string())),
        };
        map.insert(key.trim().to_string(), parsed);
    }
    Value::Object(map)
}

/// Whether a changed file is one of those that decide a project's indentation, so open files
/// should look again.
pub fn is_style_config(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| {
            name == ".editorconfig" || name == "package.json" || name.starts_with(".prettierrc")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        dir
    }

    fn indent(dir: &tempfile::TempDir, file: &str) -> (Option<usize>, Option<bool>) {
        let found = project_indent(&dir.path().join(file));
        (found.tab_size, found.hard_tabs)
    }

    #[test]
    fn editorconfig_sections_apply_by_pattern_nearest_last() {
        let dir = project(&[
            (
                ".editorconfig",
                "root = true\n\n[*]\nindent_style = space\nindent_size = 4\n\n[*.{go,mk}]\nindent_style = tab\n\n[Makefile]\nindent_style = tab\n\n[lib/**.rs]\nindent_size = 2\n",
            ),
            ("app/.editorconfig", "[*.rs]\nindent_size = 3\n"),
            // Above the root, so never read.
            ("../.editorconfig", ""),
        ]);
        assert_eq!(indent(&dir, "main.rs"), (Some(4), Some(false)));
        assert_eq!(indent(&dir, "cmd/main.go"), (Some(4), Some(true)));
        assert_eq!(indent(&dir, "build/Makefile"), (Some(4), Some(true)));
        assert_eq!(indent(&dir, "lib/deep/a.rs"), (Some(2), Some(false)));
        assert_eq!(indent(&dir, "app/src/a.rs"), (Some(3), Some(false)));
    }

    #[test]
    fn editorconfig_tab_width_stands_in_for_the_indent_size() {
        let dir = project(&[(
            ".editorconfig",
            "[*.c]\nindent_style = tab\nindent_size = tab\ntab_width = 8\n[*.h]\ntab_width = 2\n",
        )]);
        assert_eq!(indent(&dir, "a.c"), (Some(8), Some(true)));
        assert_eq!(indent(&dir, "a.h"), (Some(2), None));
        assert_eq!(indent(&dir, "a.rs"), (None, None));
    }

    #[test]
    fn prettier_wins_for_what_it_formats_with_its_overrides() {
        let dir = project(&[
            (".editorconfig", "root = true\n[*]\nindent_size = 4\n"),
            (
                ".prettierrc",
                "{\n  \"tabWidth\": 2,\n  \"useTabs\": false,\n  \"overrides\": [\n    { \"files\": \"*.json\", \"options\": { \"tabWidth\": 3 } },\n    { \"files\": [\"legacy/**\"], \"excludeFiles\": \"legacy/new/**\", \"options\": { \"useTabs\": true } }\n  ]\n}\n",
            ),
        ]);
        let found = project_indent(&dir.path().join("src/app.ts"));
        assert_eq!((found.tab_size, found.hard_tabs), (Some(2), Some(false)));
        assert_eq!(
            found.sources,
            [
                dir.path().join(".editorconfig"),
                dir.path().join(".prettierrc")
            ]
        );
        assert_eq!(indent(&dir, "config/a.json"), (Some(3), Some(false)));
        assert_eq!(indent(&dir, "legacy/old/a.js"), (Some(2), Some(true)));
        assert_eq!(indent(&dir, "legacy/new/a.js"), (Some(2), Some(false)));
        // Rust isn't Prettier's, so EditorConfig decides.
        assert_eq!(indent(&dir, "src/main.rs"), (Some(4), None));
    }

    #[test]
    fn prettier_configs_are_read_in_each_format_nearest_first() {
        let yaml = project(&[(
            ".prettierrc.yaml",
            "# style\ntabWidth: 8\nuseTabs: true\nsemi: false\n",
        )]);
        assert_eq!(indent(&yaml, "a.ts"), (Some(8), Some(true)));
        let bare_yaml = project(&[(".prettierrc", "tabWidth: 3\n")]);
        assert_eq!(indent(&bare_yaml, "a.ts"), (Some(3), None));
        let json5 = project(&[(
            ".prettierrc.json5",
            "{\n  // two, as everywhere\n  tabWidth: 2,\n}\n",
        )]);
        assert_eq!(indent(&json5, "a.ts"), (Some(2), None));
        let json5_quoted = project(&[(
            ".prettierrc.json5",
            "{\n  /* house style */\n  \"tabWidth\": 6,\n}\n",
        )]);
        assert_eq!(indent(&json5_quoted, "a.ts"), (Some(6), None));
        let toml = project(&[(".prettierrc.toml", "tabWidth = 5\nuseTabs = false\n")]);
        assert_eq!(indent(&toml, "a.ts"), (Some(5), Some(false)));
        let package = project(&[
            (
                "package.json",
                "{\"name\": \"x\", \"prettier\": {\"tabWidth\": 7}}",
            ),
            // A nearer package.json without a config doesn't stop the search.
            ("app/package.json", "{\"name\": \"app\"}"),
        ]);
        assert_eq!(indent(&package, "app/src/a.tsx"), (Some(7), None));
        let nearest = project(&[
            (".prettierrc", "{\"tabWidth\": 2}"),
            ("app/.prettierrc.json", "{\"tabWidth\": 4}"),
        ]);
        assert_eq!(indent(&nearest, "app/a.ts"), (Some(4), None));
        assert_eq!(indent(&nearest, "b.ts"), (Some(2), None));
    }

    #[test]
    fn style_configs_are_known_by_name() {
        assert!(is_style_config(Path::new("/p/.editorconfig")));
        assert!(is_style_config(Path::new("/p/.prettierrc.yaml")));
        assert!(is_style_config(Path::new("/p/package.json")));
        assert!(!is_style_config(Path::new("/p/src/main.rs")));
    }
}
