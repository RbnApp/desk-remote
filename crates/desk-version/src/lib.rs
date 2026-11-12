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
        assert_eq!(CURRENT, "0.1.0");
    }

    #[test]
    fn test_version_info_fields() {
        let info = version_info();
        assert_eq!(info.version, "0.1.0");
        assert!(!info.target_arch.is_empty());
        assert!(!info.target_os.is_empty());
    }
}
