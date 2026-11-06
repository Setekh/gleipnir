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
    /// Where the manifest came from: `builtin/...`, or the folder of an installed package.
    pub origin: String,
}

impl Package {
    pub fn is_builtin(&self) -> bool {
        self.origin.starts_with("builtin/")
    }
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
                Ok(manifest) => registry.add(manifest, origin.to_string()),
                Err(error) => registry.errors.push(format!("{error:#}")),
            }
        }
        registry
    }

    /// The built-in packages, then those installed in `dir`, each in a folder with a
    /// `package.toml`. An installed package with a built-in's id replaces it, which is how a
    /// language's servers or tools are changed.
    pub fn with_installed(dir: &Path) -> Self {
        let mut sources: Vec<(String, String)> = BUILTIN
            .iter()
            .map(|(origin, text)| (origin.to_string(), text.to_string()))
            .collect();
        let mut errors = Vec::new();
        let mut folders: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        folders.sort();
        for folder in folders {
            let manifest = folder.join("package.toml");
            if !manifest.is_file() {
                continue;
            }
            match std::fs::read_to_string(&manifest) {
                Ok(text) => {
                    let origin = folder.display().to_string();
                    // A replacement takes the built-in's place.
                    let id = parse_manifest(&text, &origin).ok().map(|m| m.package.id);
                    if let Some(id) = id {
                        sources.retain(|(o, t)| {
                            !(o.starts_with("builtin/")
                                && parse_manifest(t, o).is_ok_and(|m| m.package.id == id))
                        });
                    }
                    sources.push((origin, text));
                }
                Err(error) => errors.push(format!("{}: {error}", manifest.display())),
            }
        }
        let mut registry =
            Self::from_sources(sources.iter().map(|(o, t)| (o.as_str(), t.as_str())));
        registry.errors.extend(errors);
        registry
    }

    fn add(&mut self, manifest: Manifest, origin: String) {
        if self.packages.iter().any(|p| p.id() == manifest.package.id) {
            self.errors
                .push(format!("two packages are called {:?}", manifest.package.id));
            return;
        }
        for spec in &manifest.languages {
            let Some(grammar) = grammar(&spec.grammar) else {
                // New grammars would need WASM, which isn't built yet.
                self.errors.push(format!(
                    "{}: {} uses the grammar {:?}, which isn't built in (only rust, go, odin, \
                     typescript, tsx and javascript are)",
                    manifest.package.id, spec.name, spec.grammar
                ));
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
        self.packages.push(Package { manifest, origin });
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

/// Where installed packages live: a folder each, with a `package.toml`.
pub fn packages_dir() -> std::path::PathBuf {
    crate::dirs::config_dir().join("packages")
}

/// The application's registry: the built-in packages and those installed. Read once at startup,
/// so installing one takes a restart.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry::with_installed(&packages_dir()))
}

/// Installs a package from a folder holding its `package.toml`, by copying the folder in.
/// Returns the package's name.
pub fn install_package(from: &Path) -> anyhow::Result<String> {
    install_package_into(from, &packages_dir())
}

/// Installs a package into a given packages folder.
pub fn install_package_into(from: &Path, packages: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(from.join("package.toml"))
        .map_err(|_| anyhow::anyhow!("{} has no package.toml", from.display()))?;
    let manifest = parse_manifest(&text, &from.display().to_string())?;
    let to = packages.join(&manifest.package.id);
    if to.exists() {
        std::fs::remove_dir_all(&to)?;
    }
    copy_folder(from, &to)?;
    Ok(manifest.package.name)
}

fn copy_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
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

    #[test]
    fn installed_packages_add_to_and_replace_the_built_in_ones() {
        let dir = tempfile::tempdir().unwrap();
        let write = |folder: &str, text: &str| {
            std::fs::create_dir_all(dir.path().join(folder)).unwrap();
            std::fs::write(dir.path().join(folder).join("package.toml"), text).unwrap();
        };
        // Go with a different server.
        write(
            "my-go",
            "[package]\nid = \"go\"\nname = \"Go, my way\"\nversion = \"1.0.0\"\n\n[[language]]\nname = \"Go\"\ngrammar = \"go\"\nextensions = [\"go\"]\n\n[server]\ntool = \"my-gopls\"\n\n[tool.my-gopls]\nbinary = \"my-gopls\"\n",
        );
        // A language without a built-in grammar.
        write(
            "zig",
            "[package]\nid = \"zig\"\nname = \"Zig\"\nversion = \"0.1.0\"\n\n[[language]]\nname = \"Zig\"\ngrammar = \"zig\"\nextensions = [\"zig\"]\n",
        );
        let registry = Registry::with_installed(dir.path());
        let go = registry.package("go").unwrap();
        assert_eq!(go.name(), "Go, my way");
        assert!(!go.is_builtin());
        assert_eq!(
            registry
                .packages()
                .iter()
                .filter(|p| p.id() == "go")
                .count(),
            1
        );
        assert!(registry.package("rust").unwrap().is_builtin());
        assert!(
            registry.errors().iter().any(|e| e.contains("zig")),
            "{:?}",
            registry.errors()
        );
        assert!(registry.language_for_path(Path::new("a.go")).is_some());
    }

    #[test]
    fn installing_copies_the_folder_in_and_refuses_one_without_a_manifest() {
        let source = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("queries")).unwrap();
        std::fs::write(source.path().join("queries/extra.scm"), "").unwrap();
        std::fs::write(
            source.path().join("package.toml"),
            "[package]\nid = \"mine\"\nname = \"Mine\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(
            install_package_into(source.path(), packages.path()).unwrap(),
            "Mine"
        );
        assert!(packages.path().join("mine/queries/extra.scm").is_file());
        let registry = Registry::with_installed(packages.path());
        assert!(registry.package("mine").is_some());

        let empty = tempfile::tempdir().unwrap();
        assert!(install_package_into(empty.path(), packages.path()).is_err());
    }
}
