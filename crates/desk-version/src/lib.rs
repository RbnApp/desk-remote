//! Central build-time and runtime version contract for Desk-Remote.

use serde::{Deserialize, Serialize};

const RAW_VERSION: &str = include_str!("../../../VERSION");

const fn extract_version(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r' || bytes[end - 1] == b' ')
    {
        end -= 1;
    }
    match std::str::from_utf8(bytes.split_at(end).0) {
        Ok(v) => v,
        Err(_) => "",
    }
}

/// Canonical version string embedded directly from the root VERSION file at compile time.
pub const CURRENT: &str = extract_version(RAW_VERSION);

/// GitHub account that owns the public release repository (the only place it is written).
pub const RELEASE_REPO_OWNER: &str = "RbnApp";
/// Release repository name.
pub const RELEASE_REPO_NAME: &str = "desk-remote";
/// Release asset that lists `<sha256>  <file name>` for every installer of a release.
pub const RELEASE_MANIFEST_ASSET: &str = "SHA256SUMS.txt";

/// Git tag a release of `version` carries (`v0.1.0`).
#[must_use]
pub fn release_tag(version: &str) -> String {
    format!("v{version}")
}

/// Release asset name of the agent installer for `version` (docs/architecture/release.md).
#[must_use]
pub fn agent_asset_name(version: &str) -> String {
    format!("DeskRemote-Agent-Setup-{version}.exe")
}

/// Release asset name of the panel installer for `version` (docs/architecture/release.md).
#[must_use]
pub fn panel_asset_name(version: &str) -> String {
    format!("DeskRemote-Panel-Setup-{version}.exe")
}

/// A `major.minor.patch` version. Pre-release and build suffixes are not used by Desk-Remote
/// releases, so they are rejected instead of being compared wrongly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SemVer {
    /// Parses `1.2.3` or a release tag `v1.2.3`; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || -> Option<u32> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// Version of the running binary (the root `VERSION` file).
    #[must_use]
    pub fn current() -> Option<Self> {
        Self::parse(CURRENT)
    }
}

impl std::fmt::Display for SemVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Structured version details for diagnostics, protocol handshake, and reporting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionInfo {
    pub version: &'static str,
    pub target_arch: &'static str,
    pub target_os: &'static str,
    pub debug_build: bool,
}

/// Returns structured version information for the running binary.
pub fn version_info() -> VersionInfo {
    VersionInfo {
        version: CURRENT,
        target_arch: std::env::consts::ARCH,
        target_os: std::env::consts::OS,
        debug_build: cfg!(debug_assertions),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_version_matches_canonical() {
        assert_eq!(CURRENT, RAW_VERSION.trim());
    }

    #[test]
    fn semver_orders_numerically_and_accepts_a_tag_prefix() {
        let parse = |s| SemVer::parse(s);
        assert!(parse("0.10.0") > parse("0.9.9"));
        assert!(parse("v1.0.0") > parse("0.99.99"));
        assert_eq!(parse("v0.1.0"), parse("0.1.0"));
        assert_eq!(SemVer::current(), parse(CURRENT));
        assert_eq!(
            parse("0.1.0").map(|v| v.to_string()).as_deref(),
            Some("0.1.0")
        );
    }

    #[test]
    fn semver_rejects_anything_that_is_not_three_numbers() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "1.2.3-beta",
            "1.2.3+b",
            "-1.0.0",
            "1..3",
        ] {
            assert_eq!(SemVer::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn asset_names_follow_the_release_contract() {
        assert_eq!(
            agent_asset_name("0.1.0"),
            "DeskRemote-Agent-Setup-0.1.0.exe"
        );
        assert_eq!(
            panel_asset_name("0.1.0"),
            "DeskRemote-Panel-Setup-0.1.0.exe"
        );
        assert_eq!(release_tag("0.1.0"), "v0.1.0");
        assert_eq!(RELEASE_MANIFEST_ASSET, "SHA256SUMS.txt");
    }

    #[test]
    fn test_version_info_fields() {
        let info = version_info();
        assert_eq!(info.version, CURRENT);
        assert!(!info.target_arch.is_empty());
        assert!(!info.target_os.is_empty());
    }
}
