//! All known packages, and lookups by file and language.

use crate::manifest::{LanguageSpec, Manifest, parse_manifest};
use gleipnir_syntax::Language;
use gleipnir_text::ops::EditConfig;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// The built-in packages, compiled in.
const BUILTIN: &[(&str, &str)] = &[
    ("builtin/rust.toml", include_str!("../builtin/rust.toml")),
    ("builtin/go.toml", include_str!("../builtin/go.toml")),
    ("builtin/odin.toml", include_str!("../builtin/odin.toml")),
    (
        "builtin/typescript.toml",
        include_str!("../builtin/typescript.toml"),
    ),
];

#[derive(Clone, Debug)]
pub struct Package {
    pub manifest: Manifest,
}

impl Package {
    pub fn id(&self) -> &str {
        &self.manifest.package.id
    }
    pub fn name(&self) -> &str {
        &self.manifest.package.name
    }
}

/// A language, as a package describes it.
#[derive(Clone, Debug)]
pub struct LanguageEntry {
    pub name: String,
    pub grammar: Language,
    pub edit_config: EditConfig,
    pub package_id: String,
}

pub struct Registry {
    packages: Vec<Package>,
    languages: Vec<LanguageEntry>,
    by_extension: HashMap<String, usize>,
    /// Problems with manifests. Built-in ones are checked by tests, so this is for user packages.
    errors: Vec<String>,
}

fn grammar(name: &str) -> Option<Language> {
    Some(match name {
        "rust" => Language::Rust,
        "go" => Language::Go,
        "odin" => Language::Odin,
        "typescript" => Language::TypeScript,
        "tsx" => Language::Tsx,
        "javascript" => Language::JavaScript,
        _ => return None,
    })
}

fn edit_config(spec: &LanguageSpec) -> EditConfig {
    EditConfig {
        tab_size: spec.tab_size.max(1),
        hard_tabs: spec.hard_tabs,
        line_comment: spec.line_comment.clone(),
        pairs: spec
            .pairs
            .iter()
            .filter_map(|pair| {
                let mut chars = pair.chars();
                Some((chars.next()?, chars.next()?))
            })
            .collect(),
    }
}

impl Registry {
    /// Builds a registry from manifest sources, as (origin, text). A broken manifest is skipped and
    /// reported, rather than taking the others down with it.
    pub fn from_sources<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut registry = Self {
            packages: Vec::new(),
            languages: Vec::new(),
            by_extension: HashMap::new(),
            errors: Vec::new(),
        };
        for (origin, source) in sources {
            match parse_manifest(source, origin) {
                Ok(manifest) => registry.add(manifest),
                Err(error) => registry.errors.push(format!("{error:#}")),
            }
        }
        registry
    }

    fn add(&mut self, manifest: Manifest) {
        if self.packages.iter().any(|p| p.id() == manifest.package.id) {
            self.errors
                .push(format!("two packages are called {:?}", manifest.package.id));
            return;
        }
        for spec in &manifest.languages {
            let Some(grammar) = grammar(&spec.grammar) else {
                continue;
            };
            let index = self.languages.len();
            self.languages.push(LanguageEntry {
                name: spec.name.clone(),
                grammar,
                edit_config: edit_config(spec),
                package_id: manifest.package.id.clone(),
            });
            for extension in &spec.extensions {
                let extension = extension.trim_start_matches('.').to_ascii_lowercase();
                if let Some(previous) = self.by_extension.insert(extension.clone(), index) {
                    self.errors.push(format!(
                        "both {} and {} claim .{extension}",
                        self.languages[previous].name, spec.name
                    ));
                }
            }
        }
        self.packages.push(Package { manifest });
    }

    pub fn packages(&self) -> &[Package] {
        &self.packages
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn package(&self, id: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.id() == id)
    }

    pub fn language_for_path(&self, path: &Path) -> Option<&LanguageEntry> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        self.by_extension
            .get(&extension)
            .map(|&index| &self.languages[index])
    }

    pub fn language(&self, grammar: Language) -> Option<&LanguageEntry> {
        self.languages.iter().find(|l| l.grammar == grammar)
    }

    pub fn package_for_language(&self, grammar: Language) -> Option<&Package> {
        self.package(&self.language(grammar)?.package_id)
    }
}

/// The application's registry: the built-in packages.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry::from_sources(BUILTIN.iter().copied()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_manifest_is_valid() {
        let registry = registry();
        assert!(registry.errors().is_empty(), "{:?}", registry.errors());
        let ids: Vec<&str> = registry.packages().iter().map(Package::id).collect();
        assert_eq!(ids, ["rust", "go", "odin", "typescript"]);
    }

    #[test]
    fn languages_are_found_by_extension() {
        let r = registry();
        for (path, expected) in [
            ("a/main.rs", Some(Language::Rust)),
            ("main.go", Some(Language::Go)),
            ("main.odin", Some(Language::Odin)),
            ("a.ts", Some(Language::TypeScript)),
            ("a.mts", Some(Language::TypeScript)),
            ("a.tsx", Some(Language::Tsx)),
            ("a.js", Some(Language::JavaScript)),
            ("a.JSX", Some(Language::JavaScript)),
            ("README.md", None),
            ("Makefile", None),
        ] {
            assert_eq!(
                r.language_for_path(Path::new(path)).map(|l| l.grammar),
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn editing_settings_come_from_the_manifests() {
        let r = registry();
        let go = &r.language(Language::Go).unwrap().edit_config;
        assert!(go.hard_tabs);
        let ts = &r.language(Language::TypeScript).unwrap().edit_config;
        assert_eq!(ts.tab_size, 2);
        let rust = &r.language(Language::Rust).unwrap().edit_config;
        assert!(
            !rust.pairs.iter().any(|(open, _)| *open == '\''),
            "Rust doesn't pair quotes"
        );
        assert!(rust.pairs.contains(&('(', ')')));
        assert_eq!(rust.line_comment.as_deref(), Some("//"));
    }

    #[test]
    fn languages_lead_to_their_package() {
        let r = registry();
        assert_eq!(
            r.package_for_language(Language::Tsx).unwrap().id(),
            "typescript"
        );
        assert_eq!(r.package_for_language(Language::Odin).unwrap().id(), "odin");
    }

    #[test]
    fn a_broken_manifest_is_reported_and_the_rest_still_load() {
        let good = r#"
            [package]
            id = "good"
            name = "Good"
            version = "1"
            [[language]]
            name = "G"
            grammar = "go"
            extensions = ["g"]
        "#;
        let registry =
            Registry::from_sources([("broken.toml", "not = [valid"), ("good.toml", good)]);
        assert_eq!(registry.packages().len(), 1);
        assert_eq!(registry.errors().len(), 1);
        assert!(registry.errors()[0].contains("broken.toml"));
        assert!(registry.language_for_path(Path::new("x.g")).is_some());
    }

    #[test]
    fn two_packages_claiming_an_extension_is_reported() {
        let source = |id: &str| {
            format!(
                "[package]\nid = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\n[[language]]\nname = \"{id}\"\ngrammar = \"go\"\nextensions = [\"same\"]\n"
            )
        };
        let (a, b) = (source("a"), source("b"));
        let registry = Registry::from_sources([("a", a.as_str()), ("b", b.as_str())]);
        assert!(
            registry.errors().iter().any(|e| e.contains(".same")),
            "{:?}",
            registry.errors()
        );
    }
}
