//! Tailscale detection and helper management.
//!
//! Detects system-installed Tailscale instances (`tailscale status --json`),
//! queries backend state and tailnet peers, drives the browser sign-in, and provides
//! control over the embedded `desk-tailnet-helper`. This is the only place that starts
//! the Tailscale CLI; applications call [`TailscaleDetector`] instead of spawning it.

use desk_error::{DeskError, DeskResult};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const CLI_FALLBACK: &str = r"C:\Program Files\Tailscale\tailscale.exe";
const SIGN_IN_WAIT: Duration = Duration::from_secs(20);
const SIGN_IN_TIMEOUT_ARG: &str = "--timeout=300s";

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

/// Another node of the tailnet as reported by the Tailscale client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TailscalePeer {
    pub host_name: String,
    pub ips: Vec<String>,
    pub online: bool,
}

/// Local status plus the tailnet neighbours, read with a single CLI call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TailscaleSnapshot {
    pub status: TailscaleStatus,
    /// This machine's name on the tailnet.
    pub host_name: Option<String>,
    pub peers: Vec<TailscalePeer>,
}

#[derive(Deserialize)]
struct TailscaleCliStatus {
    #[serde(rename = "BackendState")]
    backend_state: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Option<Vec<String>>,
    #[serde(rename = "Self")]
    me: Option<TailscaleCliNode>,
    #[serde(rename = "Peer")]
    peers: Option<std::collections::BTreeMap<String, TailscaleCliNode>>,
}

#[derive(Deserialize)]
struct TailscaleCliNode {
    #[serde(rename = "HostName")]
    host_name: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Option<Vec<String>>,
    #[serde(rename = "Online")]
    online: Option<bool>,
}

/// Suppresses the console window on Windows.
fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Builds a Tailscale CLI command: `PATH` first, then the default Windows install folder.
fn cli_command(args: &[&str]) -> Option<Command> {
    let mut probe = Command::new("tailscale");
    probe
        .arg("version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let program = if hidden(&mut probe).status().is_ok() {
        PathBuf::from("tailscale")
    } else if Path::new(CLI_FALLBACK).exists() {
        PathBuf::from(CLI_FALLBACK)
    } else {
        return None;
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    hidden(&mut cmd);
    Some(cmd)
}

/// Finds the sign-in URL in a line of `tailscale up` output.
#[must_use]
pub fn extract_login_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    url.contains("tailscale.com").then_some(url)
}

/// Helper that inspects the host system for native Tailscale installations.
pub struct TailscaleDetector;

impl TailscaleDetector {
    /// Detects system-installed Tailscale state using `tailscale status --json`.
    #[must_use]
    pub fn detect() -> TailscaleStatus {
        Self::snapshot().status
    }

    /// Reads local status, own host name and the tailnet peers (one CLI call).
    #[must_use]
    pub fn snapshot() -> TailscaleSnapshot {
        let Some(mut cmd) = cli_command(&["status", "--json"]) else {
            return Self::without_peers(Self::not_installed());
        };
        let Ok(output) = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() else {
            return Self::without_peers(Self::not_installed());
        };
        if !output.status.success() && output.stdout.is_empty() {
            return Self::without_peers(TailscaleStatus {
                installed: true,
                running: false,
                backend_state: "ErrorOrStopped".to_string(),
                ips: Vec::new(),
            });
        }
        Self::parse(&output.stdout)
    }

    /// Parses the JSON printed by `tailscale status --json`.
    #[must_use]
    pub fn parse(json: &[u8]) -> TailscaleSnapshot {
        let Ok(raw) = serde_json::from_slice::<TailscaleCliStatus>(json) else {
            return Self::without_peers(TailscaleStatus {
                installed: true,
                running: false,
                backend_state: "Unparseable".to_string(),
                ips: Vec::new(),
            });
        };
        let state = raw.backend_state.unwrap_or_else(|| "Unknown".to_string());
        let peers = raw
            .peers
            .unwrap_or_default()
            .into_values()
            .filter_map(|n| {
                Some(TailscalePeer {
                    host_name: n.host_name?,
                    ips: n.tailscale_ips.unwrap_or_default(),
                    online: n.online.unwrap_or(false),
                })
            })
            .collect();
        TailscaleSnapshot {
            status: TailscaleStatus {
                installed: true,
                running: state.eq_ignore_ascii_case("Running"),
                backend_state: state,
                ips: raw.tailscale_ips.unwrap_or_default(),
            },
            host_name: raw.me.and_then(|m| m.host_name),
            peers,
        }
    }

    /// Starts `tailscale up` and returns the browser sign-in URL it prints (blocking).
    ///
    /// Returns `Ok(None)` when already connected. The child keeps running in the
    /// background until the sign-in finishes or its own timeout expires.
    pub fn begin_sign_in() -> DeskResult<Option<String>> {
        if Self::detect().running {
            return Ok(None);
        }
        let mut cmd = cli_command(&["up", SIGN_IN_TIMEOUT_ARG])
            .ok_or_else(|| DeskError::Transport("Tailscale is not installed".to_string()))?;
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(DeskError::Io)?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| DeskError::Transport("tailscale stderr unavailable".to_string()))?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tailscale-up".to_string())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(url) = extract_login_url(&line) {
                        let _ = tx.send(url);
                    }
                }
                let _ = child.wait();
            })
            .map_err(DeskError::Io)?;
        rx.recv_timeout(SIGN_IN_WAIT)
            .map(Some)
            .map_err(|e| DeskError::Transport(format!("no Tailscale sign-in URL: {e}")))
    }

    fn without_peers(status: TailscaleStatus) -> TailscaleSnapshot {
        TailscaleSnapshot {
            status,
            host_name: None,
            peers: Vec::new(),
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
        hidden(&mut cmd);

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

    #[test]
    fn snapshot_parses_self_and_peers() {
        let json = br#"{"BackendState":"Running","TailscaleIPs":["100.1.2.3"],
            "Self":{"HostName":"box"},
            "Peer":{"nodekey:a":{"HostName":"office","TailscaleIPs":["100.9.9.9"],"Online":true},
                    "nodekey:b":{"HostName":"farm","TailscaleIPs":["100.8.8.8"]}}}"#;
        let snap = TailscaleDetector::parse(json);
        assert!(snap.status.running);
        assert_eq!(snap.status.ips, vec!["100.1.2.3".to_string()]);
        assert_eq!(snap.host_name.as_deref(), Some("box"));
        assert_eq!(snap.peers.len(), 2);
        assert!(snap
            .peers
            .iter()
            .any(|p| p.host_name == "office" && p.online));
        assert!(snap
            .peers
            .iter()
            .any(|p| p.host_name == "farm" && !p.online));
    }

    #[test]
    fn needs_login_is_not_running() {
        let snap = TailscaleDetector::parse(br#"{"BackendState":"NeedsLogin"}"#);
        assert!(snap.status.installed && !snap.status.running);
        assert_eq!(snap.status.backend_state, "NeedsLogin");
    }

    #[test]
    fn login_url_is_extracted() {
        let url = extract_login_url("\thttps://login.tailscale.com/a/abc123");
        assert_eq!(url.as_deref(), Some("https://login.tailscale.com/a/abc123"));
        assert!(extract_login_url("To authenticate, visit:").is_none());
    }
}
