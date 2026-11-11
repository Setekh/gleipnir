//! What GitHub says the latest release is, and which of its files is this platform's installer.

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The installer's file name, like `Gleipnir-0.2.0-macos-arm64.dmg`.
    pub asset: String,
    pub url: String,
    /// The installer's size in bytes, or 0 when GitHub doesn't say.
    pub size: u64,
    /// The `.sha256` file published beside the installer.
    pub checksum_url: String,
}

/// Reads GitHub's description of a release, taking the installer whose name ends in `suffix`,
/// like `-macos-arm64.dmg`.
pub fn parse(release: &Value, suffix: &str) -> Result<Release> {
    let tag = release["tag_name"]
        .as_str()
        .context("the release has no tag")?;
    let version = tag.strip_prefix('v').unwrap_or(tag).to_string();
    let assets = release["assets"].as_array().map_or(&[][..], Vec::as_slice);
    let link = |name: &str| {
        assets
            .iter()
            .find(|asset| asset["name"].as_str() == Some(name))
            .and_then(|asset| asset["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let Some(asset) = assets
        .iter()
        .filter_map(|asset| asset["name"].as_str())
        .find(|name| name.ends_with(suffix))
    else {
        bail!("release {version} has no installer for this machine (a file ending in {suffix})");
    };
    let url = link(asset).context("the installer has no download link")?;
    let size = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(asset))
        .and_then(|a| a["size"].as_u64())
        .unwrap_or(0);
    let checksum_url = link(&format!("{asset}.sha256"))
        .with_context(|| format!("release {version} has no checksum for {asset}"))?;
    Ok(Release {
        version,
        asset: asset.to_string(),
        url,
        size,
        checksum_url,
    })
}

/// The checksum in a `.sha256` file, which reads `<64 hex digits>  <file name>`.
pub fn checksum(file: &str) -> Result<String> {
    let sum = file.split_whitespace().next().unwrap_or_default();
    if sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("the release's checksum file isn't a SHA-256 checksum");
    }
    Ok(sum.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release() -> Value {
        let asset = |name: &str| {
            json!({
                "name": name,
                "size": 42,
                "browser_download_url": format!("https://example.com/{name}"),
            })
        };
        json!({
            "tag_name": "v0.2.0",
            "assets": [
                asset("Gleipnir-0.2.0-macos-arm64.dmg"),
                asset("Gleipnir-0.2.0-macos-arm64.dmg.sha256"),
                asset("gleipnir-0.2.0-linux-x86_64.tar.gz"),
                asset("gleipnir-0.2.0-linux-x86_64.tar.gz.sha256"),
            ],
        })
    }

    #[test]
    fn each_platform_gets_its_own_installer_and_checksum() {
        let mac = parse(&release(), "-macos-arm64.dmg").unwrap();
        assert_eq!(mac.version, "0.2.0");
        assert_eq!(mac.asset, "Gleipnir-0.2.0-macos-arm64.dmg");
        assert_eq!(
            mac.url,
            "https://example.com/Gleipnir-0.2.0-macos-arm64.dmg"
        );
        assert_eq!(mac.size, 42);
        assert_eq!(
            mac.checksum_url,
            "https://example.com/Gleipnir-0.2.0-macos-arm64.dmg.sha256"
        );
        let linux = parse(&release(), "-linux-x86_64.tar.gz").unwrap();
        assert_eq!(linux.asset, "gleipnir-0.2.0-linux-x86_64.tar.gz");
    }

    #[test]
    fn a_release_without_this_platform_or_its_checksum_is_refused() {
        let error = parse(&release(), "-linux-arm64.tar.gz").unwrap_err();
        assert!(error.to_string().contains("no installer for this machine"));

        let mut bare = release();
        bare["assets"].as_array_mut().unwrap().remove(1);
        let error = parse(&bare, "-macos-arm64.dmg").unwrap_err();
        assert!(error.to_string().contains("no checksum"));
    }

    #[test]
    fn checksum_files_are_read_and_anything_else_refused() {
        let sum = "A".repeat(64);
        assert_eq!(
            checksum(&format!("{sum}  Gleipnir.dmg\n")).unwrap(),
            "a".repeat(64)
        );
        assert!(checksum("<html>Not Found</html>").is_err());
        assert!(checksum("").is_err());
    }
}
