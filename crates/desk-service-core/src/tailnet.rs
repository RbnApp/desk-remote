//! Embedded tailnet helper (userspace Tailscale) next to the service.
//!
//! This is the ONLY Tailscale path of the agent: no official client is required or used.
//! If an official client is running, the helper joins the tailnet first and, once that is
//! verified, [`crate::takeover`] stops and disables the official client.

use crate::control::Control;
use crate::takeover::{self, ScTailscale, TakeoverStatus};
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Embedded byte payload of `desk-tailnet-helper.exe` compiled during build.
pub const EMBEDDED_HELPER_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/desk-tailnet-helper.exe"));

/// One-time auth key dropped by the panel / installer; deleted after the first successful use.
const AUTH_KEY_FILE: &str = "authkey.txt";
const STATUS_FILE: &str = "tailnet-status.json";
const RECORD_FILE: &str = "official-tailscale.json";

#[derive(Deserialize)]
struct HelperReport {
    status: String,
    #[serde(default)]
    ips: Vec<String>,
    #[serde(default)]
    auth_url: Option<String>,
}

/// Canonical path where the embedded helper executable is stored.
pub fn helper_executable_path(paths: &DeskPaths) -> PathBuf {
    paths.tailnet_helper_path()
}

/// Ensures the embedded helper is extracted to `<data>/system/bin/desk-tailnet-helper.exe` with SHA verification.
/// If the file already exists and its SHA-256 matches, it is not rewritten.
pub fn ensure_helper_extracted(paths: &DeskPaths, harden: bool) -> DeskResult<PathBuf> {
    let target = helper_executable_path(paths);
    let bin_dir = paths.system_dir().join("bin");
    DeskPaths::ensure_dir(&bin_dir)?;

    if harden {
        let _ = restrict_to_admins(&bin_dir);
    }

    if EMBEDDED_HELPER_BYTES.is_empty() {
        return Ok(sibling_or(target));
    }
    if write_embedded_helper(&target)? && harden {
        let _ = restrict_file_to_admins(&target);
    }
    Ok(target)
}

/// The panel's copy of the embedded helper (`<AppData>/DeskRemote/bin/`, SHA verified). The panel
/// runs its own tailnet node from it, so no official Tailscale client is needed on the viewer PC.
pub fn ensure_panel_helper_extracted(paths: &DeskPaths) -> DeskResult<PathBuf> {
    let target = paths.panel_tailnet_helper_path();
    if let Some(dir) = target.parent() {
        DeskPaths::ensure_dir(dir)?;
    }
    if EMBEDDED_HELPER_BYTES.is_empty() {
        return Ok(sibling_or(target));
    }
    write_embedded_helper(&target)?;
    Ok(target)
}

/// A helper next to the running executable (development builds), else `fallback`.
fn sibling_or(fallback: PathBuf) -> PathBuf {
    match DeskPaths::sibling_exe("desk-tailnet-helper.exe") {
        Ok(sib) if sib.exists() => sib,
        _ => fallback,
    }
}

/// Writes the embedded helper to `target` unless a file with the same SHA-256 is already there.
/// Returns whether it was (re)written.
fn write_embedded_helper(target: &Path) -> DeskResult<bool> {
    let expected_hash = Sha256::digest(EMBEDDED_HELPER_BYTES);
    let current = std::fs::read(target).map(|existing| Sha256::digest(&existing));
    if current.is_ok_and(|hash| hash == expected_hash) {
        return Ok(false);
    }
    tracing::info!(
        target = %target.display(),
        bytes = EMBEDDED_HELPER_BYTES.len(),
        "extracting embedded tailnet helper (SHA-256 verified)"
    );
    std::fs::write(target, EMBEDDED_HELPER_BYTES)
        .map_err(|e| DeskError::Service(format!("cannot write tailnet helper: {e}")))?;
    Ok(true)
}

/// Owns the helper child process; it is killed when the guard drops.
pub struct TailnetGuard {
    child: Child,
    worker: Option<JoinHandle<()>>,
}

impl TailnetGuard {
    /// Starts `desk-tailnet-helper listen` from `<data>/system/bin/` (or sibling).
    /// The hand-over of an official client (if `take_over` is set) runs in the
    /// background once the helper is verified reachable on the tailnet.
    #[must_use]
    pub fn start(
        paths: &DeskPaths,
        port: u16,
        take_over: bool,
        harden: bool,
        control: Arc<Control>,
    ) -> Option<Self> {
        let exe = match ensure_helper_extracted(paths, harden) {
            Ok(p) if p.exists() => p,
            _ => match DeskPaths::sibling_exe("desk-tailnet-helper.exe") {
                Ok(p) if p.exists() => p,
                _ => {
                    tracing::info!("desk-tailnet-helper not installed; tailnet access disabled");
                    return None;
                }
            },
        };

        let state_dir = paths.system_dir().join("tailscale");
        DeskPaths::ensure_dir(&state_dir).ok()?;
        if harden {
            // The auth key (and node state) must not be readable by ordinary users.
            if let Err(e) = restrict_to_admins(&state_dir) {
                tracing::error!(error = %e, "cannot restrict the tailnet state folder; helper not started");
                return None;
            }
        }
        let key_file = state_dir.join(AUTH_KEY_FILE);
        let auth_key = std::fs::read_to_string(&key_file)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let mut cmd = desk_paths::hidden_command(&exe);
        cmd.arg("listen")
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--tailnet-port")
            .arg(port.to_string())
            .arg("--bridge-addr")
            .arg(format!("127.0.0.1:{port}"))
            .stdout(Stdio::piped());
        if let Some(key) = &auth_key {
            cmd.env("TS_AUTHKEY", key);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| tracing::warn!(error = %e, "cannot start tailnet helper"))
            .ok()?;
        tracing::info!(pid = child.id(), "tailnet helper started");
        let stdout = child.stdout.take()?;
        let worker = std::thread::Builder::new()
            .name("desk-tailnet".to_string())
            .spawn(move || {
                let status = supervise(stdout, port, take_over, &state_dir, &control);
                tracing::info!(?status, "official Tailscale hand-over outcome");
                if matches!(status, TakeoverStatus::TookOver | TakeoverStatus::NotNeeded) {
                    // The key is single-use; the node state in `state_dir` keeps the login.
                    let _ = std::fs::remove_file(&key_file);
                }
                if let Ok(json) = serde_json::to_string_pretty(&status) {
                    let _ = desk_config::write_atomic(&state_dir.join(STATUS_FILE), &json);
                }
            })
            .ok();
        Some(Self { child, worker })
    }
}

/// Replaces the folder's ACL with full control for SYSTEM and Administrators only (inherited
/// by files created later, such as `authkey.txt`). Well-known SIDs keep it language-neutral.
pub(crate) fn restrict_to_admins(dir: &Path) -> DeskResult<()> {
    let out = desk_paths::hidden_command(DeskPaths::system_tool("icacls.exe"))
        .arg(dir)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
        ])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(DeskError::Service(format!(
            "icacls failed: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        )))
    }
}

/// Replaces a single file's ACL with full control for SYSTEM and Administrators only.
pub(crate) fn restrict_file_to_admins(file: &Path) -> DeskResult<()> {
    let out = desk_paths::hidden_command(DeskPaths::system_tool("icacls.exe"))
        .arg(file)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:F",
            "*S-1-5-32-544:F",
        ])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(DeskError::Service(format!(
            "icacls file failed: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        )))
    }
}

/// Waits for helper reports (needs_auth, listening, etc.) and executes the hand-over once listening.
fn supervise(
    stdout: ChildStdout,
    port: u16,
    take_over: bool,
    state_dir: &Path,
    control: &Control,
) -> TakeoverStatus {
    let mut last_status = TakeoverStatus::Unverified;
    let stream =
        serde_json::Deserializer::from_reader(BufReader::new(stdout)).into_iter::<HelperReport>();

    for item in stream {
        let Ok(report) = item else { break };
        tracing::info!(status = %report.status, ips = ?report.ips, "tailnet helper report");
        control.set_tailnet_report(&report.status, &report.ips, report.auth_url.as_deref());

        if report.status == "listening" {
            let ips = report.ips.clone();
            let verify = move || desk_transport::helper_online("listening", &ips, port);
            last_status =
                takeover::take_over(&ScTailscale, take_over, verify, &record_path(state_dir));
            if last_status == TakeoverStatus::NeedsAdmin {
                control.set_takeover_needs_admin(true);
            }
            break;
        }
    }
    last_status
}

fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join(RECORD_FILE)
}

/// Restores an official Tailscale client that this agent took over (called on shutdown / uninstall).
pub fn restore_official(paths: &DeskPaths) -> bool {
    let record = record_path(&paths.system_dir().join("tailscale"));
    match takeover::restore(&ScTailscale, &record) {
        Ok(restored) => restored,
        Err(e) => {
            tracing::warn!(error = %e, "could not restore official Tailscale");
            false
        }
    }
}

impl Drop for TailnetGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints the folder ACL through `icacls` (system tool, absolute path).
    fn acl_listing(dir: &Path) -> DeskResult<String> {
        let out = desk_paths::hidden_command(DeskPaths::system_tool("icacls.exe"))
            .arg(dir)
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn state_folder_is_limited_to_system_and_administrators() -> DeskResult<()> {
        let tmp = tempfile::tempdir().map_err(DeskError::Io)?;
        let dir = tmp.path().join("tailscale");
        DeskPaths::ensure_dir(&dir)?;
        restrict_to_admins(&dir)?;
        let listing = acl_listing(&dir)?.replace(&dir.display().to_string(), "");
        // One ACE line per principal; the first line also carries the path.
        let aces: Vec<&str> = listing.lines().filter(|l| l.contains(":(")).collect();
        assert_eq!(aces.len(), 2, "unexpected ACL: {listing}");
        assert!(aces
            .iter()
            .any(|l| l.contains("SYSTEM") || l.contains("S-1-5-18")));
        assert!(aces
            .iter()
            .any(|l| l.contains("Administrators") || l.contains("S-1-5-32-544")));
        assert!(
            !aces
                .iter()
                .any(|l| l.contains("Users") || l.contains("Everyone")),
            "ordinary principals still listed: {listing}"
        );
        Ok(())
    }

    #[test]
    fn helper_extraction_creates_target_path() -> DeskResult<()> {
        let tmp = tempfile::tempdir().map_err(DeskError::Io)?;
        let paths = DeskPaths::custom(tmp.path().join("system"), tmp.path().join("user"));
        let target = ensure_helper_extracted(&paths, false)?;
        assert_eq!(target, paths.tailnet_helper_path());
        if !EMBEDDED_HELPER_BYTES.is_empty() {
            assert!(target.exists());
            let read_bytes = std::fs::read(&target).map_err(DeskError::Io)?;
            assert_eq!(read_bytes.len(), EMBEDDED_HELPER_BYTES.len());
        }
        Ok(())
    }
}
