//! Tailscale detection and helper management.
//!
//! Detects system-installed Tailscale instances (`tailscale status --json`),
//! queries backend state, and provides control over the embedded `desk-tailnet-helper`.

use desk_error::{DeskError, DeskResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Status and IP information for a detected Tailscale client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TailscaleStatus {
    /// True if the official Tailscale CLI/daemon was located on the system.
    pub installed: bool,
    /// True if Tailscale is currently in the "Running" state.
    pub running: bool,
    /// Backend status label reported by Tailscale (e.g. "Running", "NeedsLogin", "Stopped").
    pub backend_state: String,
    /// Tailscale 100.x.y.z IP addresses assigned to this node.
    pub ips: Vec<String>,
}

#[derive(Deserialize)]
struct TailscaleCliStatus {
    #[serde(rename = "BackendState")]
    backend_state: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Option<Vec<String>>,
}

/// Helper that inspects the host system for native Tailscale installations.
pub struct TailscaleDetector;

impl TailscaleDetector {
    /// Detects system-installed Tailscale state using `tailscale status --json`.
    #[must_use]
    pub fn detect() -> TailscaleStatus {
        let mut cmd = Command::new("tailscale");
        cmd.args(["status", "--json"]);

        // Suppress console window on Windows
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let output = match cmd.output() {
            Ok(out) => out,
            Err(_) => {
                // Check default Windows installation directory fallback
                let fallback_path = PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe");
                if fallback_path.exists() {
                    let mut fallback_cmd = Command::new(&fallback_path);
                    fallback_cmd.args(["status", "--json"]);
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                        fallback_cmd.creation_flags(CREATE_NO_WINDOW);
                    }
                    match fallback_cmd.output() {
                        Ok(out) => out,
                        Err(_) => return Self::not_installed(),
                    }
                } else {
                    return Self::not_installed();
                }
            }
        };

        if !output.status.success() {
            return TailscaleStatus {
                installed: true,
                running: false,
                backend_state: "ErrorOrStopped".to_string(),
                ips: Vec::new(),
            };
        }

        let parsed: Result<TailscaleCliStatus, _> = serde_json::from_slice(&output.stdout);
        match parsed {
            Ok(status) => {
                let state = status
                    .backend_state
                    .unwrap_or_else(|| "Unknown".to_string());
                let running = state.eq_ignore_ascii_case("Running");
                let ips = status.tailscale_ips.unwrap_or_default();
                TailscaleStatus {
                    installed: true,
                    running,
                    backend_state: state,
                    ips,
                }
            }
            Err(_) => TailscaleStatus {
                installed: true,
                running: false,
                backend_state: "Unparseable".to_string(),
                ips: Vec::new(),
            },
        }
    }

    fn not_installed() -> TailscaleStatus {
        TailscaleStatus {
            installed: false,
            running: false,
            backend_state: "NotInstalled".to_string(),
            ips: Vec::new(),
        }
    }
}

/// Helper process controller for `apps/desk-tailnet-helper`.
pub struct TailnetHelperController {
    helper_binary: PathBuf,
}

impl TailnetHelperController {
    /// Creates a controller pointing to the given `desk-tailnet-helper` binary.
    #[must_use]
    pub fn new(helper_binary: impl AsRef<Path>) -> Self {
        Self {
            helper_binary: helper_binary.as_ref().to_path_buf(),
        }
    }

    /// Queries auth status or login URL from the helper.
    pub fn query_auth(&self, state_dir: &Path) -> DeskResult<String> {
        if !self.helper_binary.exists() {
            return Err(DeskError::Transport(format!(
                "desk-tailnet-helper binary not found at {}",
                self.helper_binary.display()
            )));
        }

        let mut cmd = Command::new(&self.helper_binary);
        cmd.args(["auth", "--state-dir", &state_dir.to_string_lossy()]);

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let output = cmd.output().map_err(|e| {
            DeskError::Transport(format!("Failed to execute desk-tailnet-helper: {e}"))
        })?;

        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detector_returns_valid_structure() {
        let status = TailscaleDetector::detect();
        // Should execute without panicking and return valid state
        if status.installed {
            assert!(!status.backend_state.is_empty());
        } else {
            assert_eq!(status.backend_state, "NotInstalled");
        }
    }
}
