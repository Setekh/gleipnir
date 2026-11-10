//! Installing tools. Every install is pinned to a version and verified: downloads by a sha256 in
//! the manifest, Go modules by Go's checksum database, npm packages by the registry's integrity
//! hashes. An install goes into a staging folder and only moves into place once it has succeeded,
//! so a failure leaves nothing half-installed.

use crate::env::Env;
use crate::manifest::{ArchiveFormat, Asset, InstallSpec, ToolSpec};
use crate::tools::{managed_binary, platform_binary};
use anyhow::{Context as _, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What an install needs to know about this machine.
pub struct InstallContext<'a> {
    pub managed_dir: &'a Path,
    /// The environment with the relevant SDK applied, so `go` and `npm` are the project's.
    pub env: &'a Env,
    /// For rustup components: the toolchain to add it to.
    pub rust_toolchain: Option<&'a str>,
}

/// A description of what an install will do, to show before asking for confirmation.
pub fn describe_install(name: &str, spec: &ToolSpec) -> String {
    match &spec.install {
        Some(InstallSpec::GoInstall { module, version }) => {
            format!("go install {module}@{version}")
        }
        Some(InstallSpec::Npm { packages }) => format!("npm install {}", packages.join(" ")),
        Some(InstallSpec::DotnetTool { package, version }) => {
            format!("dotnet tool install {package} --version {version}")
        }
        Some(InstallSpec::Download {
            version, assets, ..
        }) => match pick_asset(assets) {
            Some(asset) => format!("download {name} {version} from {}", asset.url),
            None => format!("{name} {version} has no download for this platform"),
        },
        Some(InstallSpec::RustupComponent { component }) => {
            format!("rustup component add {component}")
        }
        Some(InstallSpec::Manual { hint }) => hint.clone(),
        None => format!("{name} can't be installed automatically"),
    }
}

fn platform_key() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    format!("{os}-{arch}")
}

fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    let key = platform_key();
    assets
        .iter()
        .find(|a| a.platform == key)
        .or_else(|| assets.iter().find(|a| a.platform == "any"))
}

/// Installs a tool, returning the path of its executable.
pub fn install_tool(name: &str, spec: &ToolSpec, cx: &InstallContext) -> Result<PathBuf> {
    let install = spec
        .install
        .as_ref()
        .ok_or_else(|| anyhow!("{name} can't be installed automatically"))?;
    match install {
        InstallSpec::Manual { hint } => bail!("{name} has to be installed by hand: {hint}"),
        InstallSpec::RustupComponent { component } => {
            let mut command = Command::new("rustup");
            command.args(["component", "add", component]);
            if let Some(toolchain) = cx.rust_toolchain {
                command.args(["--toolchain", toolchain]);
            }
            cx.env.apply(&mut command);
            run(&mut command, "rustup")?;
            cx.env
                .which(&spec.binary)
                .ok_or_else(|| anyhow!("rustup succeeded but {} isn't on PATH", spec.binary))
        }
        _ => {
            let target = managed_binary(name, spec, cx.managed_dir)
                .ok_or_else(|| anyhow!("{name} has no install location"))?;
            let version = install.version().unwrap_or("unknown");
            let final_dir = cx.managed_dir.join(name).join(version);
            let staging = cx
                .managed_dir
                .join(name)
                .join(format!(".{version}.installing-{}", std::process::id()));
            if staging.exists() {
                fs::remove_dir_all(&staging)?;
            }
            fs::create_dir_all(&staging)?;
            let result = install_into(install, spec, &staging, cx);
            if let Err(error) = result {
                fs::remove_dir_all(&staging).ok();
                return Err(error);
            }
            if final_dir.exists() {
                fs::remove_dir_all(&final_dir)?;
            }
            fs::rename(&staging, &final_dir)?;
            if !target.exists() {
                bail!(
                    "the install finished, but {} isn't where it should be",
                    target.display()
                );
            }
            // An update replaces what it updates.
            for (_, folder) in crate::tools::older_installs(name, spec, cx.managed_dir) {
                fs::remove_dir_all(folder).ok();
            }
            Ok(target)
        }
    }
}

fn install_into(
    install: &InstallSpec,
    spec: &ToolSpec,
    dir: &Path,
    cx: &InstallContext,
) -> Result<()> {
    match install {
        InstallSpec::GoInstall { module, version } => {
            let go = cx
                .env
                .which("go")
                .ok_or_else(|| anyhow!("Go isn't available. Choose a Go SDK first"))?;
            let mut command = Command::new(go);
            command.args(["install", &format!("{module}@{version}")]);
            cx.env.apply(&mut command);
            command.env("GOBIN", dir.join("bin"));
            // The checksum database and proxy stay on, which is what verifies the module.
            command.env_remove("GOFLAGS");
            run(&mut command, "go install")
        }
        InstallSpec::Npm { packages } => {
            let npm = cx
                .env
                .which("npm")
                .ok_or_else(|| anyhow!("npm isn't available. Choose a Node.js SDK first"))?;
            let mut command = Command::new(npm);
            command
                .arg("install")
                .arg("--prefix")
                .arg(dir)
                .args(["--no-fund", "--no-audit", "--save-exact"])
                .args(packages);
            cx.env.apply(&mut command);
            run(&mut command, "npm install")
        }
        InstallSpec::DotnetTool { package, version } => {
            let dotnet = cx
                .env
                .which("dotnet")
                .ok_or_else(|| anyhow!(".NET isn't available. Choose a .NET SDK first"))?;
            let mut command = Command::new(dotnet);
            command
                .args([
                    "tool",
                    "install",
                    package,
                    "--version",
                    version,
                    "--tool-path",
                ])
                .arg(dir);
            cx.env.apply(&mut command);
            run(&mut command, "dotnet tool install")
        }
        InstallSpec::Download { assets, binary, .. } => {
            let asset = pick_asset(assets)
                .ok_or_else(|| anyhow!("there's no download for {}", platform_key()))?;
            let bytes = fetch(&asset.url)?;
            verify_sha256(&bytes, &asset.sha256)?;
            let executable = dir.join(platform_binary(binary));
            if asset.format == ArchiveFormat::Binary {
                fs::write(&executable, &bytes)?;
            } else {
                extract(&bytes, asset.format, dir)?;
            }
            if !executable.exists() {
                bail!("the download doesn't contain {}", platform_binary(binary));
            }
            make_executable(&executable)?;
            let _ = spec;
            Ok(())
        }
        InstallSpec::RustupComponent { .. } | InstallSpec::Manual { .. } => {
            unreachable!("handled above")
        }
    }
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("couldn't start {what}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        bail!("{what} failed: {}", tail.join("\n"));
    }
    Ok(())
}

/// Reads a URL. `file://` works too, which is how tests avoid the network.
fn fetch(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return fs::read(path).with_context(|| format!("can't read {path}"));
    }
    let mut response = ureq::get(url)
        .call()
        .with_context(|| format!("couldn't download {url}"))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .with_context(|| format!("the download from {url} was cut short"))?;
    Ok(bytes)
}

pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected) {
        bail!(
            "the download's checksum is wrong (expected {expected}, got {actual}), so it wasn't installed"
        );
    }
    Ok(())
}

/// Unpacks an archive. Entries that would land outside `dir`, through `..` or absolute paths,
/// are refused rather than written.
fn extract(bytes: &[u8], format: ArchiveFormat, dir: &Path) -> Result<()> {
    match format {
        ArchiveFormat::TarGz => {
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
            for entry in archive.entries()? {
                let mut entry = entry?;
                // unpack_in refuses paths that escape the folder.
                if !entry.unpack_in(dir)? {
                    bail!(
                        "the archive has an entry outside its folder: {}",
                        entry.path()?.display()
                    );
                }
            }
            Ok(())
        }
        ArchiveFormat::Binary => unreachable!("written as it is, not extracted"),
        ArchiveFormat::Zip => {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            for index in 0..archive.len() {
                let mut file = archive.by_index(index)?;
                let Some(relative) = file.enclosed_name() else {
                    bail!(
                        "the archive has an entry outside its folder: {}",
                        file.name()
                    );
                };
                let path = dir.join(relative);
                if file.is_dir() {
                    fs::create_dir_all(&path)?;
                    continue;
                }
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = fs::File::create(&path)?;
                std::io::copy(&mut file, &mut out)?;
                if let Some(mode) = file.unix_mode() {
                    use std::os::unix::fs::PermissionsExt as _;
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
                }
            }
            Ok(())
        }
    }
}

fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn zip_with(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut out);
            for (name, data) in files {
                writer
                    .start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        out.into_inner()
    }

    fn download_spec(url: String, sha256: String, binary: &str) -> ToolSpec {
        ToolSpec {
            binary: "tool".into(),
            project_paths: vec![],
            sdk_paths: vec![],
            system_paths: vec![],
            install: Some(InstallSpec::Download {
                version: "1.0".into(),
                binary: binary.into(),
                assets: vec![Asset {
                    platform: "any".into(),
                    url,
                    sha256,
                    format: ArchiveFormat::Zip,
                }],
            }),
        }
    }

    #[test]
    fn a_verified_download_installs_and_is_executable() {
        let dir = tempfile::tempdir().unwrap();
        let archive = zip_with(&[("bin/tool", b"#!/bin/sh\necho hi\n")]);
        let archive_path = dir.path().join("tool.zip");
        fs::write(&archive_path, &archive).unwrap();
        let spec = download_spec(
            format!("file://{}", archive_path.display()),
            sha(&archive),
            "bin/tool",
        );
        let managed = dir.path().join("managed");
        let cx = InstallContext {
            managed_dir: &managed,
            env: &Env::new(),
            rust_toolchain: None,
        };
        let installed = install_tool("tool", &spec, &cx).unwrap();
        assert_eq!(installed, managed.join("tool/1.0/bin/tool"));
        assert!(crate::env::is_executable(&installed));
    }

    #[test]
    fn a_download_with_the_wrong_checksum_is_rejected_and_nothing_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let archive = zip_with(&[("bin/tool", b"tampered")]);
        let archive_path = dir.path().join("tool.zip");
        fs::write(&archive_path, &archive).unwrap();
        let wrong = "0".repeat(64);
        let spec = download_spec(
            format!("file://{}", archive_path.display()),
            wrong,
            "bin/tool",
        );
        let managed = dir.path().join("managed");
        let cx = InstallContext {
            managed_dir: &managed,
            env: &Env::new(),
            rust_toolchain: None,
        };
        let error = install_tool("tool", &spec, &cx).unwrap_err();
        assert!(format!("{error:#}").contains("checksum"), "{error:#}");
        assert!(!managed.join("tool/1.0").exists());
        assert_eq!(
            fs::read_dir(managed.join("tool")).unwrap().count(),
            0,
            "no staging folder is left"
        );
    }

    #[test]
    fn archives_cannot_write_outside_their_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut out);
            writer
                .start_file("../escaped", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"x").unwrap();
            writer.finish().unwrap();
        }
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        assert!(extract(&out.into_inner(), ArchiveFormat::Zip, &target).is_err());
        assert!(!dir.path().join("escaped").exists());
    }

    #[test]
    fn an_archive_missing_the_binary_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let archive = zip_with(&[("other", b"x")]);
        let archive_path = dir.path().join("tool.zip");
        fs::write(&archive_path, &archive).unwrap();
        let spec = download_spec(
            format!("file://{}", archive_path.display()),
            sha(&archive),
            "bin/tool",
        );
        let managed = dir.path().join("managed");
        let cx = InstallContext {
            managed_dir: &managed,
            env: &Env::new(),
            rust_toolchain: None,
        };
        assert!(install_tool("tool", &spec, &cx).is_err());
        assert!(!managed.join("tool/1.0").exists());
    }

    #[test]
    fn manual_tools_explain_themselves() {
        let spec = ToolSpec {
            binary: "x".into(),
            project_paths: vec![],
            sdk_paths: vec![],
            system_paths: vec![],
            install: Some(InstallSpec::Manual {
                hint: "use the system package manager".into(),
            }),
        };
        let cx = InstallContext {
            managed_dir: Path::new("/nonexistent"),
            env: &Env::new(),
            rust_toolchain: None,
        };
        let error = install_tool("x", &spec, &cx).unwrap_err();
        assert!(format!("{error}").contains("system package manager"));
    }

    #[test]
    fn missing_go_is_reported_plainly() {
        let spec = ToolSpec {
            binary: "gopls".into(),
            project_paths: vec![],
            sdk_paths: vec![],
            system_paths: vec![],
            install: Some(InstallSpec::GoInstall {
                module: "m".into(),
                version: "v1".into(),
            }),
        };
        let dir = tempfile::tempdir().unwrap();
        let cx = InstallContext {
            managed_dir: dir.path(),
            env: &Env::from_pairs([("PATH", "/nonexistent")]),
            rust_toolchain: None,
        };
        let error = install_tool("gopls", &spec, &cx).unwrap_err();
        assert!(format!("{error}").contains("Go SDK"), "{error}");
    }

    /// Real installs, over the network, into a temporary folder. Slow, so not run by default:
    /// cargo test -p gleipnir-packages -- --ignored --nocapture real_install
    #[test]
    #[ignore]
    fn real_install_of_every_installable_builtin_tool() {
        // REAL_INSTALL_ONLY=<tool> installs just that one.
        let only = std::env::var("REAL_INSTALL_ONLY").ok();
        let managed = tempfile::tempdir().unwrap();
        let base = crate::env::user_env();
        for package in crate::registry().packages() {
            let sdk_spec = package.manifest.sdk.as_ref();
            let sdk = sdk_spec.and_then(|s| crate::sdk::detect_sdks(s, &base).into_iter().next());
            let env = match (sdk_spec, &sdk) {
                (Some(spec), Some(sdk)) => sdk.env(spec, &base),
                _ => base.clone(),
            };
            for (name, spec) in &package.manifest.tools {
                if !matches!(
                    spec.install,
                    Some(
                        InstallSpec::GoInstall { .. }
                            | InstallSpec::Npm { .. }
                            | InstallSpec::Download { .. }
                            | InstallSpec::DotnetTool { .. }
                    )
                ) || only.as_deref().is_some_and(|only| only != name)
                {
                    continue;
                }
                let started = std::time::Instant::now();
                let cx = InstallContext {
                    managed_dir: managed.path(),
                    env: &env,
                    rust_toolchain: None,
                };
                let result = install_tool(name, spec, &cx);
                println!(
                    "{name}: {:?} in {:?}",
                    result.as_ref().map(|p| p.display().to_string()),
                    started.elapsed()
                );
                let installed = result.unwrap();
                assert!(installed.exists());
            }
        }
    }
}
