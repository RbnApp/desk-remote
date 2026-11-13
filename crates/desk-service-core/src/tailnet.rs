//! Embedded tailnet helper (userspace Tailscale) next to the service.
//!
//! This is the ONLY Tailscale path of the agent: no official client is required or used.
//! If an official client is running, the helper joins the tailnet first and, once that is
//! verified, [`crate::takeover`] stops and disables the official client.

use crate::control::Control;
use crate::tailnet_run::{ChildSlot, Runner, RESTART_MAX, RESTART_MIN};
use crate::takeover::{self, ScTailscale, TakeoverStatus};
use crate::tracker::Backoff;
use desk_error::{DeskError, DeskResult};
use desk_paths::{restrict_file_to_admins, restrict_to_admins, DeskPaths};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Embedded byte payload of `desk-tailnet-helper.exe` compiled during build.
pub const EMBEDDED_HELPER_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/desk-tailnet-helper.exe"));

const STATUS_FILE: &str = "tailnet-status.json";

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

/// Owns the helper child process and the thread that keeps it alive; stops both on drop.
pub struct TailnetGuard {
    stop: Arc<AtomicBool>,
    child: ChildSlot,
    worker: Option<JoinHandle<()>>,
}

impl TailnetGuard {
    /// Starts `desk-tailnet-helper listen` from `<data>/system/bin/` (or sibling) and keeps it
    /// running: it is restarted with a growing delay whenever it exits. The one-time sign-in is
    /// done by the user from the tray (`needs_auth` URL); the login stays in the state folder.
    /// The hand-over of an official client (if `take_over` is set) runs in the background once the
    /// helper is verified reachable on the tailnet.
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
        // The node state (its login) must not be readable by ordinary users in ANY mode.
        // Portable runs of a non-admin user cannot be locked to Administrators without locking
        // themselves out, so there the folder is limited to the current user instead.
        let restricted = if harden {
            restrict_to_admins(&state_dir)
        } else {
            restrict_to_current_user(&state_dir)
        };
        if let Err(e) = restricted {
            tracing::error!(error = %e, "cannot restrict the tailnet state folder; helper not started");
            return None;
        }
        let record = paths.takeover_record_path();
        let stop = Arc::new(AtomicBool::new(false));
        let child: ChildSlot = Arc::default();
        let spawn_dir = state_dir.clone();
        let runner = Runner {
            spawn: Box::new(move || spawn_helper(&exe, &spawn_dir, port)),
            take_over: Box::new(move |ips| {
                let ips = ips.to_vec();
                let verify = move || desk_transport::helper_online("listening", &ips, port);
                let status = takeover::take_over(&ScTailscale, take_over, verify, &record);
                tracing::info!(?status, "official Tailscale hand-over outcome");
                status
            }),
            publish: Box::new(move |state, takeover| write_status(&state_dir, state, takeover)),
            control,
            stop: Arc::clone(&stop),
            child: Arc::clone(&child),
            backoff: Backoff::new(RESTART_MIN, RESTART_MAX),
        };
        let worker = std::thread::Builder::new()
            .name("desk-tailnet".to_string())
            .spawn(move || runner.run())
            .map_err(|e| tracing::error!(error = %e, "cannot start the tailnet supervisor"))
            .ok()?;
        Some(Self {
            stop,
            child,
            worker: Some(worker),
        })
    }
}

/// Starts one helper process; its stdout carries the JSON reports.
fn spawn_helper(exe: &Path, state_dir: &Path, port: u16) -> Option<Child> {
    let child = desk_paths::hidden_command(exe)
        .arg("listen")
        .arg("--state-dir")
        .arg(state_dir)
        .arg("--tailnet-port")
        .arg(port.to_string())
        .arg("--bridge-addr")
        .arg(format!("127.0.0.1:{port}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| tracing::warn!(error = %e, "cannot start tailnet helper"))
        .ok()?;
    tracing::info!(pid = child.id(), "tailnet helper started");
    Some(child)
}

/// What `tailnet-status.json` holds: the helper's state and the official-client hand-over outcome.
#[derive(Serialize)]
struct StatusFile<'a> {
    /// `connected | needs_login | error | off`.
    state: &'a str,
    takeover: &'a TakeoverStatus,
}

fn write_status(state_dir: &Path, state: &str, takeover: &TakeoverStatus) {
    if let Ok(json) = serde_json::to_string_pretty(&StatusFile { state, takeover }) {
        let _ = desk_config::write_atomic(&state_dir.join(STATUS_FILE), &json);
    }
}

/// Limits `dir` to the current user and SYSTEM (portable, non-service runs).
fn restrict_to_current_user(dir: &Path) -> DeskResult<()> {
    desk_paths::restrict_to_user(dir, &desk_ipc::current_user_sid()?)
}

/// Restores an official Tailscale client that this agent took over (called on shutdown / uninstall).
pub fn restore_official(paths: &DeskPaths) -> bool {
    match takeover::restore(&ScTailscale, &paths.takeover_record_path()) {
        Ok(restored) => restored,
        Err(e) => {
            tracing::warn!(error = %e, "could not restore official Tailscale");
            false
        }
    }
}

impl Drop for TailnetGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let running = self.child.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(mut child) = running {
            let _ = child.kill();
            let _ = child.wait();
        }
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
