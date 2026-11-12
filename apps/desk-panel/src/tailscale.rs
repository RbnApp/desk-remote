//! Tailscale CLI adapter: connection status and the browser sign-in flow.
//!
//! NOTE: spawning child processes belongs to `desk-service-core` under the central-systems
//! rules; that crate is still a skeleton, so the panel wraps the CLI itself for now.

use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const CLI_FALLBACK: &str = r"C:\Program Files\Tailscale\tailscale.exe";
const SIGN_IN_WAIT: Duration = Duration::from_secs(20);
const SIGN_IN_TIMEOUT: &str = "--timeout=300s";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Tailscale state shown in the sidebar and the settings screen.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TailscaleStatus {
    /// The CLI could be started at all.
    pub installed: bool,
    /// `BackendState == "Running"`.
    pub connected: bool,
    /// Raw backend state, e.g. `NeedsLogin`.
    pub state: String,
    pub host: Option<String>,
    pub address: Option<String>,
}

impl TailscaleStatus {
    fn missing() -> Self {
        Self {
            installed: false,
            connected: false,
            state: "Missing".to_string(),
            host: None,
            address: None,
        }
    }
}

fn command(args: &[&str]) -> Command {
    let program = if Command::new("tailscale")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .is_ok()
    {
        "tailscale"
    } else {
        CLI_FALLBACK
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Parses the JSON printed by `tailscale status --json`.
pub fn parse_status(json: &str) -> TailscaleStatus {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return TailscaleStatus::missing();
    };
    let state = root
        .get("BackendState")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let me = root.get("Self");
    TailscaleStatus {
        installed: true,
        connected: state == "Running",
        host: me
            .and_then(|s| s.get("HostName"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        address: root
            .get("TailscaleIPs")
            .and_then(|v| v.get(0))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        state,
    }
}

/// Reads the current status (blocking; call from a worker thread).
pub fn status() -> TailscaleStatus {
    let output = command(&["status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(out) if !out.stdout.is_empty() => parse_status(&String::from_utf8_lossy(&out.stdout)),
        _ => TailscaleStatus::missing(),
    }
}

/// Finds the sign-in URL in a line of `tailscale up` output.
pub fn extract_login_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    url.contains("tailscale.com").then_some(url)
}

/// Starts `tailscale up` and returns the sign-in URL it prints (blocking).
///
/// The child keeps running in the background until sign-in finishes or it times out.
pub fn begin_sign_in() -> Result<Option<String>, String> {
    if status().connected {
        return Ok(None);
    }
    let mut child = command(&["up", SIGN_IN_TIMEOUT])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "tailscale stderr unavailable".to_string())?;
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
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(SIGN_IN_WAIT)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_state_is_connected() {
        let s = parse_status(
            r#"{"BackendState":"Running","TailscaleIPs":["100.1.2.3"],"Self":{"HostName":"box"}}"#,
        );
        assert!(s.installed && s.connected);
        assert_eq!(s.address.as_deref(), Some("100.1.2.3"));
        assert_eq!(s.host.as_deref(), Some("box"));
    }

    #[test]
    fn needs_login_is_not_connected() {
        let s = parse_status(r#"{"BackendState":"NeedsLogin"}"#);
        assert!(s.installed && !s.connected);
        assert_eq!(s.state, "NeedsLogin");
    }

    #[test]
    fn login_url_is_extracted() {
        let url = extract_login_url("\thttps://login.tailscale.com/a/abc123");
        assert_eq!(url.as_deref(), Some("https://login.tailscale.com/a/abc123"));
        assert!(extract_login_url("To authenticate, visit:").is_none());
    }
}
