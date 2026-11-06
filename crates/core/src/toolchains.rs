//! Which SDK each language uses in a project, and where each tool is, kept current.

use anyhow::{Context as _, Result, anyhow, bail};
use gleipnir_packages::env::user_env;
use gleipnir_packages::install::{InstallContext, describe_install, install_tool};
use gleipnir_packages::sdk::{SdkSource, is_sdk_root, read_version};
use gleipnir_packages::{
    Env, Resolution, ResolveContext, Sdk, detect_sdks, managed_tools_dir, registry, resolve_tool,
    state_dir,
};
use gpui::{AppContext as _, Context, EventEmitter, Task};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum ToolchainEvent {
    Changed,
}

/// A tool's state, for showing in settings.
#[derive(Clone, Debug)]
pub struct ToolStatus {
    pub name: String,
    pub resolution: Resolution,
    pub can_install: bool,
    /// What installing it does, or how to get it when we can't.
    pub install_description: String,
    pub version: Option<String>,
    pub installing: bool,
    pub error: Option<String>,
    /// An older version we installed, when the manifest now pins a newer one.
    pub outdated: Option<String>,
}

/// One package's toolchain.
#[derive(Clone, Debug)]
pub struct PackageToolchain {
    pub package_id: String,
    pub name: String,
    pub sdk_kind: Option<String>,
    pub sdk_name: Option<String>,
    pub candidates: Vec<Sdk>,
    pub chosen: Option<Sdk>,
    pub tools: Vec<ToolStatus>,
}

/// What's remembered across projects: SDKs added by hand and tools pointed at by hand.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct GlobalSettings {
    #[serde(default)]
    sdks: Vec<UserSdk>,
    #[serde(default)]
    tools: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct UserSdk {
    kind: String,
    root: PathBuf,
}

fn settings_path() -> PathBuf {
    state_dir().join("toolchains.json")
}

fn load_settings() -> GlobalSettings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &GlobalSettings) -> Result<()> {
    let path = settings_path();
    std::fs::create_dir_all(path.parent().context("no state folder")?)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(settings)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

/// Orders candidates for picking a default: added by hand first, then what the environment points
/// at, then what's on `PATH`, then whatever's installed, newest first.
fn default_choice(candidates: &[Sdk]) -> Option<Sdk> {
    let mut sorted = candidates.to_vec();
    sorted.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| version_key(&b.version).cmp(&version_key(&a.version)))
    });
    sorted.into_iter().next()
}

fn version_key(version: &Option<String>) -> Vec<u64> {
    version
        .as_deref()
        .unwrap_or("")
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse().ok())
        .collect()
}

pub struct Toolchains {
    root: Option<PathBuf>,
    packages: Vec<PackageToolchain>,
    /// The project's SDK choices, by kind.
    choices: BTreeMap<String, PathBuf>,
    settings: GlobalSettings,
    base_env: Option<Env>,
    managed_dir: PathBuf,
    detecting: bool,
    installing: BTreeSet<String>,
    errors: BTreeMap<String, String>,
}

impl EventEmitter<ToolchainEvent> for Toolchains {}

impl Toolchains {
    pub fn new(root: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            packages: Vec::new(),
            choices: BTreeMap::new(),
            settings: load_settings(),
            base_env: None,
            managed_dir: managed_tools_dir(),
            detecting: false,
            installing: BTreeSet::new(),
            errors: BTreeMap::new(),
        };
        this.detect(cx);
        this
    }

    /// Uses a fixed environment and tools folder, so tests don't depend on this machine.
    pub fn with_environment(
        root: Option<PathBuf>,
        env: Env,
        managed_dir: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            root,
            packages: Vec::new(),
            choices: BTreeMap::new(),
            settings: GlobalSettings::default(),
            base_env: Some(env),
            managed_dir,
            detecting: false,
            installing: BTreeSet::new(),
            errors: BTreeMap::new(),
        };
        this.detect(cx);
        this
    }

    pub fn packages(&self) -> &[PackageToolchain] {
        &self.packages
    }

    pub fn package(&self, id: &str) -> Option<&PackageToolchain> {
        self.packages.iter().find(|p| p.package_id == id)
    }

    pub fn is_detecting(&self) -> bool {
        self.detecting
    }

    pub fn choices(&self) -> &BTreeMap<String, PathBuf> {
        &self.choices
    }

    /// Restores the project's saved choices. An SDK that's since gone falls back to the default.
    pub fn set_choices(&mut self, choices: BTreeMap<String, PathBuf>, cx: &mut Context<Self>) {
        self.choices = choices;
        self.rebuild(cx);
    }

    /// Finds SDKs and resolves tools, off the UI thread. Getting the user's shell environment the
    /// first time takes about a second, which is another reason not to block on it.
    pub fn detect(&mut self, cx: &mut Context<Self>) {
        if self.detecting {
            return;
        }
        self.detecting = true;
        let fixed_env = self.base_env.clone();
        let user_sdks = self.settings.sdks.clone();
        let work = cx.background_spawn(async move {
            let env = fixed_env.unwrap_or_else(user_env);
            let mut found: BTreeMap<String, Vec<Sdk>> = BTreeMap::new();
            for package in registry().packages() {
                let Some(spec) = &package.manifest.sdk else {
                    continue;
                };
                let mut candidates: Vec<Sdk> = user_sdks
                    .iter()
                    .filter(|u| u.kind == spec.kind && is_sdk_root(spec, &u.root))
                    .map(|u| Sdk {
                        kind: u.kind.clone(),
                        root: u.root.clone(),
                        version: read_version(spec, &u.root),
                        source: SdkSource::User,
                    })
                    .collect();
                for sdk in detect_sdks(spec, &env) {
                    if !candidates.iter().any(|c| c.root == sdk.root) {
                        candidates.push(sdk);
                    }
                }
                found.insert(spec.kind.clone(), candidates);
            }
            (env, found)
        });
        cx.spawn(async move |this, cx| {
            let (env, found) = work.await;
            this.update(cx, |this, cx| {
                this.detecting = false;
                this.base_env = Some(env);
                this.packages = registry()
                    .packages()
                    .iter()
                    .map(|package| {
                        let spec = package.manifest.sdk.as_ref();
                        PackageToolchain {
                            package_id: package.id().to_string(),
                            name: package.name().to_string(),
                            sdk_kind: spec.map(|s| s.kind.clone()),
                            sdk_name: spec.map(|s| s.name.clone()),
                            candidates: spec
                                .and_then(|s| found.get(&s.kind).cloned())
                                .unwrap_or_default(),
                            chosen: None,
                            tools: Vec::new(),
                        }
                    })
                    .collect();
                this.rebuild(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Applies choices and re-resolves every tool. Resolving only checks files, so it's quick.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some(base_env) = self.base_env.clone() else {
            return;
        };
        for toolchain in &mut self.packages {
            let Some(package) = registry().package(&toolchain.package_id) else {
                continue;
            };
            toolchain.chosen = toolchain
                .sdk_kind
                .as_ref()
                .and_then(|kind| self.choices.get(kind))
                .and_then(|root| {
                    toolchain
                        .candidates
                        .iter()
                        .find(|c| &c.root == root)
                        .cloned()
                })
                .or_else(|| default_choice(&toolchain.candidates));
            let env = match (&package.manifest.sdk, &toolchain.chosen) {
                (Some(spec), Some(sdk)) => sdk.env(spec, &base_env),
                _ => base_env.clone(),
            };
            toolchain.tools = package
                .manifest
                .tools
                .iter()
                .map(|(name, spec)| {
                    let cx = ResolveContext {
                        project_root: self.root.as_deref(),
                        sdk: toolchain.chosen.as_ref(),
                        explicit: self.settings.tools.get(name).map(PathBuf::as_path),
                        managed_dir: &self.managed_dir,
                        env: &env,
                    };
                    ToolStatus {
                        name: name.clone(),
                        resolution: resolve_tool(name, spec, &cx),
                        can_install: spec.install.as_ref().is_some_and(|i| i.can_install()),
                        install_description: describe_install(name, spec),
                        version: spec
                            .install
                            .as_ref()
                            .and_then(|i| i.version().map(String::from)),
                        installing: self.installing.contains(name),
                        error: self.errors.get(name).cloned(),
                        outdated: gleipnir_packages::tools::older_installs(
                            name,
                            spec,
                            &self.managed_dir,
                        )
                        .pop()
                        .map(|(version, _)| version),
                    }
                })
                .collect();
        }
        cx.emit(ToolchainEvent::Changed);
        cx.notify();
    }

    /// The environment a package's tools run with: the user's, with the chosen SDK applied.
    pub fn env_for(&self, package_id: &str) -> Option<Env> {
        let base = self.base_env.clone()?;
        let package = registry().package(package_id)?;
        let toolchain = self.package(package_id)?;
        Some(match (&package.manifest.sdk, &toolchain.chosen) {
            (Some(spec), Some(sdk)) => sdk.env(spec, &base),
            _ => base,
        })
    }

    /// Where a tool is, if it's been found.
    pub fn tool_path(&self, package_id: &str, tool: &str) -> Option<PathBuf> {
        let toolchain = self.package(package_id)?;
        let status = toolchain.tools.iter().find(|t| t.name == tool)?;
        status.resolution.path().map(Path::to_path_buf)
    }

    pub fn choose_sdk(&mut self, kind: &str, root: PathBuf, cx: &mut Context<Self>) {
        self.choices.insert(kind.to_string(), root);
        self.rebuild(cx);
    }

    /// Adds an SDK by hand, if the folder really is one. It's remembered for every project.
    pub fn add_sdk(&mut self, kind: &str, root: PathBuf, cx: &mut Context<Self>) -> Result<()> {
        let spec = registry()
            .packages()
            .iter()
            .filter_map(|p| p.manifest.sdk.as_ref())
            .find(|s| s.kind == kind)
            .ok_or_else(|| anyhow!("there's no SDK kind called {kind}"))?;
        if !is_sdk_root(spec, &root) {
            bail!(
                "{} isn't a {}: there's no {} in it",
                root.display(),
                spec.name,
                spec.binary
            );
        }
        let sdk = Sdk {
            kind: kind.to_string(),
            version: read_version(spec, &root),
            root: root.clone(),
            source: SdkSource::User,
        };
        if !self
            .settings
            .sdks
            .iter()
            .any(|u| u.kind == kind && u.root == root)
        {
            self.settings.sdks.push(UserSdk {
                kind: kind.to_string(),
                root: root.clone(),
            });
            save_settings(&self.settings).ok();
        }
        for toolchain in &mut self.packages {
            if toolchain.sdk_kind.as_deref() == Some(kind)
                && !toolchain.candidates.iter().any(|c| c.root == root)
            {
                toolchain.candidates.insert(0, sdk.clone());
            }
        }
        self.choose_sdk(kind, root, cx);
        Ok(())
    }

    /// Installs a tool after the user has agreed to it, on a background thread.
    pub fn install(
        &mut self,
        package_id: &str,
        tool: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<PathBuf>> {
        let Some(package) = registry().package(package_id) else {
            return Task::ready(Err(anyhow!("unknown package {package_id}")));
        };
        let Some(spec) = package.manifest.tools.get(tool).cloned() else {
            return Task::ready(Err(anyhow!("{package_id} has no tool called {tool}")));
        };
        let Some(env) = self.env_for(package_id) else {
            return Task::ready(Err(anyhow!(
                "still looking for SDKs, try again in a moment"
            )));
        };
        let managed_dir = self.managed_dir.clone();
        let rust_toolchain = self
            .package(package_id)
            .and_then(|p| p.chosen.as_ref())
            .and_then(|sdk| sdk.root.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| n != ".cargo");
        let name = tool.to_string();
        self.installing.insert(name.clone());
        self.errors.remove(&name);
        self.rebuild(cx);
        let work = cx.background_spawn(async move {
            let cx = InstallContext {
                managed_dir: &managed_dir,
                env: &env,
                rust_toolchain: rust_toolchain.as_deref(),
            };
            install_tool(&name, &spec, &cx)
        });
        let name = tool.to_string();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.installing.remove(&name);
                if let Err(error) = &result {
                    this.errors.insert(name.clone(), format!("{error:#}"));
                }
                this.rebuild(cx);
            })?;
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext};
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    fn executable(path: &Path, script: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn wait(toolchains: &Entity<Toolchains>, cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if toolchains.read_with(cx, |t, _| !t.is_detecting() && !t.packages().is_empty()) {
                return;
            }
            assert!(Instant::now() < deadline, "detection never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A machine with two Go SDKs, one on PATH, and gopls on PATH.
    fn machine() -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        executable(&base.join("go-old/bin/go"), "echo go version go1.20.1");
        executable(&base.join("go-new/bin/go"), "echo go version go1.26.0");
        executable(&base.join("tools/gopls"), "");
        let path = format!(
            "{}:{}",
            base.join("go-old/bin").display(),
            base.join("tools").display()
        );
        (
            dir,
            Env::from_pairs([("PATH", path.leak() as &str), ("HOME", "/nonexistent")]),
        )
    }

    #[test]
    fn defaults_prefer_the_environment_then_the_newest() {
        let sdk = |root: &str, version: &str, source: SdkSource| Sdk {
            kind: "go".into(),
            root: root.into(),
            version: Some(version.into()),
            source,
        };
        let candidates = [
            sdk("/a", "1.20", SdkSource::Known),
            sdk("/b", "1.26", SdkSource::Known),
            sdk("/c", "1.9", SdkSource::Path),
        ];
        assert_eq!(
            default_choice(&candidates).unwrap().root,
            PathBuf::from("/c")
        );
        assert_eq!(
            default_choice(&candidates[..2]).unwrap().root,
            PathBuf::from("/b")
        );
        // Version 1.10 is newer than 1.9, which a text comparison would get wrong.
        let numeric = [
            sdk("/x", "1.9.0", SdkSource::Known),
            sdk("/y", "1.10.0", SdkSource::Known),
        ];
        assert_eq!(default_choice(&numeric).unwrap().root, PathBuf::from("/y"));
    }

    #[gpui::test]
    fn sdks_and_tools_are_found_and_the_sdk_can_be_changed(cx: &mut TestAppContext) {
        let (dir, env) = machine();
        let managed = dir.path().join("managed");
        let toolchains = cx.new(|cx| Toolchains::with_environment(None, env, managed, cx));
        wait(&toolchains, cx);

        let (chosen, gopls) = toolchains.read_with(cx, |t, _| {
            let go = t.package("go").unwrap();
            (
                go.chosen.clone().unwrap(),
                go.tools
                    .iter()
                    .find(|t| t.name == "gopls")
                    .cloned()
                    .unwrap(),
            )
        });
        assert!(
            chosen.root.ends_with("go-old"),
            "the one on PATH is the default"
        );
        assert_eq!(chosen.version.as_deref(), Some("1.20.1"));
        assert_eq!(
            gopls.resolution.found.as_ref().unwrap().1,
            gleipnir_packages::ToolSource::Path
        );
        assert!(gopls.can_install);

        let new_root = fs::canonicalize(dir.path()).unwrap().join("go-new");
        toolchains
            .update(cx, |t, cx| t.add_sdk("go", new_root.clone(), cx))
            .unwrap();
        let (chosen, env) = toolchains.read_with(cx, |t, _| {
            (
                t.package("go").unwrap().chosen.clone().unwrap(),
                t.env_for("go").unwrap(),
            )
        });
        assert_eq!(chosen.root, new_root);
        assert_eq!(env.get("GOROOT"), Some(new_root.to_str().unwrap()));
        assert!(
            env.get("PATH")
                .unwrap()
                .starts_with(&format!("{}/bin", new_root.display()))
        );
    }

    #[gpui::test]
    fn adding_a_folder_that_is_not_an_sdk_is_refused(cx: &mut TestAppContext) {
        let (dir, env) = machine();
        let toolchains =
            cx.new(|cx| Toolchains::with_environment(None, env, dir.path().join("m"), cx));
        wait(&toolchains, cx);
        let error = toolchains
            .update(cx, |t, cx| t.add_sdk("go", dir.path().join("tools"), cx))
            .unwrap_err();
        assert!(error.to_string().contains("bin/go"), "{error}");
    }

    #[gpui::test]
    fn a_saved_choice_that_no_longer_exists_falls_back_to_the_default(cx: &mut TestAppContext) {
        let (dir, env) = machine();
        let toolchains =
            cx.new(|cx| Toolchains::with_environment(None, env, dir.path().join("m"), cx));
        wait(&toolchains, cx);
        toolchains.update(cx, |t, cx| {
            t.set_choices(
                BTreeMap::from([("go".to_string(), PathBuf::from("/gone"))]),
                cx,
            );
        });
        let chosen = toolchains.read_with(cx, |t, _| t.package("go").unwrap().chosen.clone());
        assert!(chosen.unwrap().root.ends_with("go-old"));
    }

    #[gpui::test]
    fn a_failed_install_is_reported_on_the_tool(cx: &mut TestAppContext) {
        // With HOME unwritable, a go-install fails whichever Go this machine has, or none. The
        // reason varies by machine; what matters is that it's reported on the tool.
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("PATH", "/nonexistent"), ("HOME", "/nonexistent")]);
        let toolchains =
            cx.new(|cx| Toolchains::with_environment(None, env, dir.path().join("m"), cx));
        wait(&toolchains, cx);
        let task = toolchains.update(cx, |t, cx| t.install("go", "gopls", cx));
        assert!(toolchains.read_with(cx, |t, _| {
            t.package("go")
                .unwrap()
                .tools
                .iter()
                .any(|tool| tool.name == "gopls" && tool.installing)
        }));
        cx.executor().allow_parking();
        let mut result = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        let task = std::rc::Rc::new(std::cell::RefCell::new(Some(task)));
        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (t2, s2) = (task.clone(), slot.clone());
        cx.spawn(async move |_| {
            let task = t2.borrow_mut().take().unwrap();
            *s2.borrow_mut() = Some(task.await);
        })
        .detach();
        while result.is_none() {
            cx.run_until_parked();
            result = slot.borrow_mut().take();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(result.unwrap().is_err());
        let error = toolchains.read_with(cx, |t, _| {
            t.package("go")
                .unwrap()
                .tools
                .iter()
                .find(|tool| tool.name == "gopls")
                .unwrap()
                .error
                .clone()
        });
        let error = error.expect("the failure is recorded on the tool");
        assert!(!error.is_empty());
        let installing = toolchains.read_with(cx, |t, _| {
            t.package("go")
                .unwrap()
                .tools
                .iter()
                .any(|tool| tool.installing)
        });
        assert!(!installing);
    }
}
