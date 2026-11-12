//! Canonical operating system paths and directory contracts for Desk-Remote.

use desk_error::{DeskError, DeskResult};
use std::path::{Path, PathBuf};

const APP_NAME: &str = "DeskRemote";
const DEFAULT_SYSTEM_BASE: &str = r"C:\ProgramData";
const DEFAULT_USER_BASE: &str = r"C:\Users\Default\AppData\Roaming";

/// Canonical path layout resolver.
#[derive(Debug, Clone)]
pub struct DeskPaths {
    system_root: PathBuf,
    user_root: PathBuf,
}

impl Default for DeskPaths {
    fn default() -> Self {
        let system_root = match std::env::var("ProgramData") {
            Ok(val) if !val.is_empty() => PathBuf::from(val).join(APP_NAME),
            _ => PathBuf::from(DEFAULT_SYSTEM_BASE).join(APP_NAME),
        };

        let user_root = match std::env::var("APPDATA") {
            Ok(val) if !val.is_empty() => PathBuf::from(val).join(APP_NAME),
            _ => PathBuf::from(DEFAULT_USER_BASE).join(APP_NAME),
        };

        Self {
            system_root,
            user_root,
        }
    }
}

impl DeskPaths {
    /// Constructs a path resolver with custom roots (for hermetic testing).
    pub fn custom(system_root: impl AsRef<Path>, user_root: impl AsRef<Path>) -> Self {
        Self {
            system_root: system_root.as_ref().to_path_buf(),
            user_root: user_root.as_ref().to_path_buf(),
        }
    }

    /// Machine-wide ProgramData base path for Desk-Remote.
    pub fn system_dir(&self) -> &Path {
        &self.system_root
    }

    /// User-specific AppData base path for Desk-Remote.
    pub fn user_dir(&self) -> &Path {
        &self.user_root
    }

    /// System-wide log directory (`<ProgramData>/DeskRemote/logs`).
    pub fn system_log_dir(&self) -> PathBuf {
        self.system_root.join("logs")
    }

    /// User-specific log directory (`<AppData>/DeskRemote/logs`).
    pub fn user_log_dir(&self) -> PathBuf {
        self.user_root.join("logs")
    }

    /// System configuration file path (`<ProgramData>/DeskRemote/service.json`).
    pub fn service_config_path(&self) -> PathBuf {
        self.system_root.join("service.json")
    }

    /// User configuration file path (`<AppData>/DeskRemote/panel.json`).
    pub fn panel_config_path(&self) -> PathBuf {
        self.user_root.join("panel.json")
    }

    /// Relay configuration file path (`<ProgramData>/DeskRemote/relay.json`).
    pub fn relay_config_path(&self) -> PathBuf {
        self.system_root.join("relay.json")
    }

    /// Identity keypair file path (`<ProgramData>/DeskRemote/identity.key`).
    pub fn identity_key_path(&self) -> PathBuf {
        self.system_root.join("identity.key")
    }

    /// Trusted viewer-panel list on the host (`<ProgramData>/DeskRemote/trusted_peers.dat`, DPAPI).
    pub fn trusted_peers_path(&self) -> PathBuf {
        self.system_root.join("trusted_peers.dat")
    }

    /// Trusted host list on a viewer panel (`<AppData>/DeskRemote/trusted_hosts.dat`, DPAPI).
    pub fn trusted_hosts_path(&self) -> PathBuf {
        self.user_root.join("trusted_hosts.dat")
    }

    /// Ensures that the specified directory exists, creating parent directories if needed.
    pub fn ensure_dir(dir: &Path) -> DeskResult<()> {
        std::fs::create_dir_all(dir).map_err(|e| {
            DeskError::Paths(format!(
                "Failed to create directory {}: {}",
                dir.display(),
                e
            ))
        })
    }

    /// Windows Named Pipe name for the central service.
    pub fn pipe_service() -> &'static str {
        r"\\.\pipe\DeskRemote-Service"
    }

    /// Windows Named Pipe name for an interactive session helper.
    pub fn pipe_session_helper(session_id: u32) -> String {
        format!(r"\\.\pipe\DeskRemote-Helper-{}", session_id)
    }
}

/// Convenience function returning the default canonical paths.
pub fn canonical() -> DeskPaths {
    DeskPaths::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_paths_and_structure() {
        let root = PathBuf::from(r"C:\test_data");
        let user = PathBuf::from(r"C:\test_user");
        let paths = DeskPaths::custom(&root, &user);

        assert_eq!(paths.system_dir(), &root);
        assert_eq!(paths.user_dir(), &user);
        assert_eq!(paths.system_log_dir(), root.join("logs"));
        assert_eq!(paths.user_log_dir(), user.join("logs"));
        assert_eq!(paths.service_config_path(), root.join("service.json"));
        assert_eq!(paths.panel_config_path(), user.join("panel.json"));
        assert_eq!(paths.relay_config_path(), root.join("relay.json"));
        assert_eq!(paths.identity_key_path(), root.join("identity.key"));
    }

    #[test]
    fn test_named_pipe_names() {
        assert_eq!(DeskPaths::pipe_service(), r"\\.\pipe\DeskRemote-Service");
        assert_eq!(
            DeskPaths::pipe_session_helper(1),
            r"\\.\pipe\DeskRemote-Helper-1"
        );
        assert_eq!(
            DeskPaths::pipe_session_helper(42),
            r"\\.\pipe\DeskRemote-Helper-42"
        );
    }
}
