//! Reads a GitHub "latest release" document and the `SHA256SUMS.txt` manifest.

use crate::updater::Component;
use desk_error::{DeskError, DeskResult};
use desk_version::{SemVer, RELEASE_MANIFEST_ASSET, RELEASE_REPO_NAME, RELEASE_REPO_OWNER};
use serde::Deserialize;

/// What a newer release offers this component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: SemVer,
    /// Installer file name (`DeskRemote-Agent-Setup-<ver>.exe`).
    pub asset_name: String,
    pub asset_url: String,
    /// Size the release announces (0 when unknown).
    pub asset_size: u64,
    pub manifest_url: String,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// URL of the "latest release" document of the Desk-Remote repository.
#[must_use]
pub fn default_feed_url() -> String {
    format!("https://api.github.com/repos/{RELEASE_REPO_OWNER}/{RELEASE_REPO_NAME}/releases/latest")
}

/// Parses a release document for `component`. The installer and the manifest are looked up by
/// their exact contract names, so a release can never point at a file with another name.
pub fn parse_release(json: &[u8], component: Component) -> DeskResult<ReleaseInfo> {
    let raw: RawRelease = serde_json::from_slice(json)
        .map_err(|e| DeskError::Update(format!("release document is not valid: {e}")))?;
    if raw.draft || raw.prerelease {
        return Err(DeskError::Update(
            "the latest release is a draft or pre-release".to_string(),
        ));
    }
    let version = SemVer::parse(&raw.tag_name)
        .ok_or_else(|| DeskError::Update("release tag is not a version".to_string()))?;
    let wanted = component.asset_name(&version.to_string());
    let find = |name: &str| raw.assets.iter().find(|a| a.name == name);
    let asset = find(&wanted)
        .ok_or_else(|| DeskError::Update(format!("release {version} has no {wanted}")))?;
    let manifest = find(RELEASE_MANIFEST_ASSET).ok_or_else(|| {
        DeskError::Update(format!("release {version} has no {RELEASE_MANIFEST_ASSET}"))
    })?;
    Ok(ReleaseInfo {
        version,
        asset_name: asset.name.clone(),
        asset_url: asset.browser_download_url.clone(),
        asset_size: asset.size,
        manifest_url: manifest.browser_download_url.clone(),
    })
}

/// Finds the SHA-256 (lower-case hex) of `file_name` in a manifest of `<sha256>  <file name>`
/// lines (`sha256sum` style; a `*` before the name marks binary mode).
pub fn parse_manifest(text: &str, file_name: &str) -> DeskResult<String> {
    for line in text.lines() {
        let Some((hash, name)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim().trim_start_matches('*');
        if name == file_name {
            return if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(hash.to_ascii_lowercase())
            } else {
                Err(DeskError::Update(format!(
                    "manifest entry for {file_name} is not a SHA-256"
                )))
            };
        }
    }
    Err(DeskError::Update(format!(
        "manifest has no entry for {file_name}"
    )))
}
