//! Tailscale detection and helper management.
//!
//! The tailnet path is ALWAYS the embedded `desk-tailnet-helper`. [`TailscaleDetector`]
//! only reports an official client so the agent can take it over once the helper is
//! verified online (see `desk-service-core`); it never selects the transport.

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

/// True when the helper reported `listening` with a tailnet IP and its local bridge target
/// (`127.0.0.1:bridge_port`) accepts TCP. This is the gate before an official client is stopped.
#[must_use]
pub fn helper_online(status: &str, ips: &[String], bridge_port: u16) -> bool {
    let has_ip = ips.iter().any(|ip| ip.parse::<std::net::IpAddr>().is_ok());
    if status != "listening" || !has_ip {
        return false;
    }
    let bridge = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bridge_port));
    std::net::TcpStream::connect_timeout(&bridge, std::time::Duration::from_secs(3)).is_ok()
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

    fn ips(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn helper_online_needs_status_ip_and_open_bridge() {
        assert!(!helper_online("listening", &[], 1));
        assert!(!helper_online("error", &ips(&["100.64.0.1"]), 1));
        assert!(!helper_online("listening", &ips(&["not-an-ip"]), 1));
        let Ok(l) = std::net::TcpListener::bind("127.0.0.1:0") else {
            panic!("bind failed");
        };
        let Ok(addr) = l.local_addr() else {
            panic!("no local addr");
        };
        let port = addr.port();
        assert!(helper_online("listening", &ips(&["100.64.0.1"]), port));
        drop(l);
        assert!(!helper_online("listening", &ips(&["100.64.0.1"]), port));
    }

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
