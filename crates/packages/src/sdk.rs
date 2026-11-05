//! Finding toolchains on this machine.

use crate::dirs::expand_home;
use crate::env::{Env, is_executable};
use crate::manifest::SdkSpec;
use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How an SDK was found, which also orders the candidates: what the user's environment points at
/// comes before what merely happens to be installed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum SdkSource {
    /// Added by hand.
    User,
    /// Named by an environment variable, like GOROOT.
    EnvVar(String),
    /// The one on `PATH`.
    Path,
    /// A usual install location.
    Known,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sdk {
    pub kind: String,
    pub root: PathBuf,
    pub version: Option<String>,
    pub source: SdkSource,
}

impl Sdk {
    pub fn label(&self) -> String {
        match &self.version {
            Some(version) => format!("{version} ({})", self.root.display()),
            None => self.root.display().to_string(),
        }
    }

    /// The environment for processes using this SDK, built on `base`.
    pub fn env(&self, spec: &SdkSpec, base: &Env) -> Env {
        let mut env = base.clone();
        let root = self.root.to_string_lossy();
        for (key, template) in &spec.env {
            let existing = base.get(key).unwrap_or("").to_string();
            let value = template
                .replace("{sdk}", &root)
                .replace("{PATH}", &existing);
            // An empty `{PATH}` would leave a stray separator, which some tools read as ".".
            let value = value.trim_end_matches(':').to_string();
            env.set(key, value);
        }
        env
    }
}

/// Whether a folder is an SDK of this kind, judged by its main executable being there.
pub fn is_sdk_root(spec: &SdkSpec, root: &Path) -> bool {
    is_executable(&root.join(&spec.binary))
}

/// The SDK root an executable belongs to, by stripping the spec's relative binary path from its
/// real location. `/opt/homebrew/bin/go` is a link into `.../libexec/bin/go`, whose root is
/// `.../libexec`.
fn root_from_binary(spec: &SdkSpec, binary: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(binary).ok()?;
    let mut root = real.as_path();
    for _ in Path::new(&spec.binary).components() {
        root = root.parent()?;
    }
    Some(root.to_path_buf())
}

fn glob_dirs(pattern: &str) -> Vec<PathBuf> {
    let expanded = expand_home(pattern);
    let text = expanded.to_string_lossy().into_owned();
    let Some(star) = text.find('*') else {
        return vec![expanded];
    };
    // Only one wildcard, in one path component, is supported, which is all the manifests need.
    let dir_end = text[..star].rfind('/').map_or(0, |i| i + 1);
    let parent = PathBuf::from(&text[..dir_end]);
    let component_end = text[star..].find('/').map_or(text.len(), |i| star + i);
    let (prefix, suffix) = (&text[dir_end..star], &text[star + 1..component_end]);
    let rest = &text[component_end..];
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with(prefix)
                && name.ends_with(suffix)
                && name.len() >= prefix.len() + suffix.len()
        })
        .map(|name| {
            PathBuf::from(format!(
                "{}{name}{rest}",
                parent
                    .display()
                    .to_string()
                    .trim_end_matches('/')
                    .to_string()
                    + "/"
            ))
        })
        .collect();
    found.sort();
    // Newest versions first, which is usually what people want offered first.
    found.reverse();
    found
}

/// Reads the version an SDK reports, giving up on one that hangs.
pub fn read_version(spec: &SdkSpec, root: &Path) -> Option<String> {
    let mut child = Command::new(root.join(&spec.binary))
        .args(&spec.version_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if started.elapsed() > Duration::from_secs(3) {
            child.kill().ok();
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    parse_version(&text)
}

/// Picks a version out of a tool's version line: `go version go1.26.4 darwin/arm64` gives
/// `1.26.4`, Odin's `odin version dev-2026-06:47fa8df59` gives `dev-2026-06`.
pub fn parse_version(text: &str) -> Option<String> {
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    static DEV: OnceLock<Regex> = OnceLock::new();
    let number = NUMBER.get_or_init(|| Regex::new(r"\d+\.\d+(?:\.\d+)?").unwrap());
    let dev = DEV.get_or_init(|| Regex::new(r"dev-\d{4}-\d{2}").unwrap());
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    number
        .find(line)
        .or_else(|| dev.find(line))
        .map(|m| m.as_str().to_string())
}

/// Finds every SDK of a kind, by the manifest's detection rules, without relying on `PATH` alone:
/// apps started from a launcher don't have the user's.
pub fn detect_sdks(spec: &SdkSpec, env: &Env) -> Vec<Sdk> {
    let mut found: Vec<Sdk> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut consider = |root: PathBuf, source: SdkSource, found: &mut Vec<Sdk>| {
        if !is_sdk_root(spec, &root) {
            return;
        }
        let key = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if seen.insert(key) {
            found.push(Sdk {
                kind: spec.kind.clone(),
                version: None,
                root,
                source,
            });
        }
    };
    for rule in &spec.detect {
        let Some((kind, value)) = rule.split_once(':') else {
            continue;
        };
        match kind {
            "env" => {
                if let Some(dir) = env.get(value).filter(|v| !v.is_empty()) {
                    consider(
                        PathBuf::from(dir),
                        SdkSource::EnvVar(value.to_string()),
                        &mut found,
                    );
                }
            }
            "path" => {
                if let Some(root) = env.which(value).and_then(|b| root_from_binary(spec, &b)) {
                    consider(root, SdkSource::Path, &mut found);
                }
            }
            "dir" => consider(expand_home(value), SdkSource::Known, &mut found),
            "glob" => {
                for dir in glob_dirs(value) {
                    consider(dir, SdkSource::Known, &mut found);
                }
            }
            _ => {}
        }
    }
    for sdk in &mut found {
        sdk.version = read_version(spec, &sdk.root);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    fn spec(binary: &str, detect: &[&str]) -> SdkSpec {
        SdkSpec {
            kind: "go".into(),
            name: "Go".into(),
            binary: binary.into(),
            version_args: vec!["version".into()],
            detect: detect.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::from([
                ("GOROOT".to_string(), "{sdk}".to_string()),
                ("PATH".to_string(), "{sdk}/bin:{PATH}".to_string()),
            ]),
        }
    }

    /// A fake SDK whose binary prints a version line.
    fn fake_sdk(root: &Path, binary: &str, version_line: &str) {
        let path = root.join(binary);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("#!/bin/sh\necho '{version_line}'\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn versions_are_read_from_common_formats() {
        assert_eq!(
            parse_version("go version go1.26.4 darwin/arm64").as_deref(),
            Some("1.26.4")
        );
        assert_eq!(parse_version("v24.13.0\n").as_deref(), Some("24.13.0"));
        assert_eq!(
            parse_version("cargo 1.99.1 (5f94df478 2026-08-27)").as_deref(),
            Some("1.99.1")
        );
        assert_eq!(
            parse_version("odin version dev-2026-06:47fa8df59").as_deref(),
            Some("dev-2026-06")
        );
        assert_eq!(parse_version("\n\n"), None);
    }

    #[test]
    fn sdks_are_found_by_every_kind_of_rule_and_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let (from_env, known, globbed_a, globbed_b) = (
            base.join("env-go"),
            base.join("known-go"),
            base.join("sdk/go1.20"),
            base.join("sdk/go1.21"),
        );
        for (root, version) in [
            (&from_env, "1.19"),
            (&known, "1.18"),
            (&globbed_a, "1.20"),
            (&globbed_b, "1.21"),
        ] {
            fake_sdk(root, "bin/go", &format!("go version go{version} test"));
        }
        // A folder without the binary isn't an SDK.
        fs::create_dir_all(base.join("sdk/go-broken")).unwrap();
        let env = Env::from_pairs([("GOROOT", from_env.to_str().unwrap()), ("PATH", "")]);
        let rules = [
            "env:GOROOT".to_string(),
            format!("dir:{}", known.display()),
            format!("glob:{}/sdk/go*", base.display()),
            format!("dir:{}", from_env.display()),
        ];
        let rules: Vec<&str> = rules.iter().map(String::as_str).collect();
        let found = detect_sdks(&spec("bin/go", &rules), &env);
        let roots: Vec<&Path> = found.iter().map(|s| s.root.as_path()).collect();
        assert_eq!(
            roots,
            [
                from_env.as_path(),
                known.as_path(),
                globbed_b.as_path(),
                globbed_a.as_path()
            ]
        );
        assert_eq!(found[0].source, SdkSource::EnvVar("GOROOT".into()));
        assert_eq!(found[0].version.as_deref(), Some("1.19"));
    }

    #[test]
    fn an_sdk_on_path_is_traced_back_through_symlinks_to_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let root = base.join("Cellar/go/1.26/libexec");
        fake_sdk(&root, "bin/go", "go version go1.26 x");
        let bin = base.join("bin");
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(root.join("bin/go"), bin.join("go")).unwrap();
        let env = Env::from_pairs([("PATH", bin.to_str().unwrap())]);
        let found = detect_sdks(&spec("bin/go", &["path:go"]), &env);
        assert_eq!(found[0].root, root);
        assert_eq!(found[0].source, SdkSource::Path);
    }

    #[test]
    fn a_binary_at_the_root_works_too() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        fake_sdk(&base.join("Odin"), "odin", "odin version dev-2026-06:abc");
        let env = Env::from_pairs([("PATH", base.join("Odin").to_str().unwrap())]);
        let found = detect_sdks(&spec("odin", &["path:odin"]), &env);
        assert_eq!(found[0].root, base.join("Odin"));
        assert_eq!(found[0].version.as_deref(), Some("dev-2026-06"));
    }

    #[test]
    fn the_sdk_environment_fills_in_its_root_and_extends_path() {
        let sdk = Sdk {
            kind: "go".into(),
            root: "/sdk/go".into(),
            version: None,
            source: SdkSource::Known,
        };
        let base = Env::from_pairs([("PATH", "/usr/bin"), ("HOME", "/h")]);
        let env = sdk.env(&spec("bin/go", &[]), &base);
        assert_eq!(env.get("GOROOT"), Some("/sdk/go"));
        assert_eq!(env.get("PATH"), Some("/sdk/go/bin:/usr/bin"));
        assert_eq!(env.get("HOME"), Some("/h"));
        // No stray separator when there was no PATH to extend.
        let env = sdk.env(&spec("bin/go", &[]), &Env::new());
        assert_eq!(env.get("PATH"), Some("/sdk/go/bin"));
    }

    #[test]
    fn this_machine_finds_its_toolchains_without_a_shell_path() {
        // Detection as an app launched from the Dock would do it: no profile, minimal PATH.
        let env = Env::from_pairs([
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            ("HOME", std::env::var("HOME").unwrap().leak()),
        ]);
        for package in crate::registry().packages() {
            let Some(spec) = &package.manifest.sdk else {
                continue;
            };
            let found = detect_sdks(spec, &env);
            println!(
                "{}: {:?}",
                spec.kind,
                found.iter().map(Sdk::label).collect::<Vec<_>>()
            );
        }
    }
}
