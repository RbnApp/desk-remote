//! Embedded tailnet helper (userspace Tailscale) next to the service.
//!
//! This is the ONLY Tailscale path of the agent: no official client is required or used.
//! If an official client is running, the helper joins the tailnet first and, once that is
//! verified, [`crate::takeover`] stops and disables the official client.

use crate::takeover::{self, ScTailscale, TakeoverStatus};
use desk_paths::DeskPaths;
use serde::Deserialize;
use std::io::BufReader;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// One-time auth key dropped by the panel / installer; deleted after the first successful use.
const AUTH_KEY_FILE: &str = "authkey.txt";
const STATUS_FILE: &str = "tailnet-status.json";
const RECORD_FILE: &str = "official-tailscale.json";

#[derive(Deserialize)]
struct HelperReport {
    status: String,
    #[serde(default)]
    ips: Vec<String>,
}

/// Owns the helper child process; it is killed when the guard drops.
pub struct TailnetGuard {
    child: Child,
    worker: Option<JoinHandle<()>>,
}

impl TailnetGuard {
    /// Starts `desk-tailnet-helper listen` when it ships next to the service. The hand-over of
    /// an official client (if `take_over` is set) runs in the background once the helper is up.
    #[must_use]
    pub fn start(paths: &DeskPaths, port: u16, take_over: bool) -> Option<Self> {
        let exe = DeskPaths::sibling_exe("desk-tailnet-helper.exe").ok()?;
        if !exe.exists() {
            tracing::info!("desk-tailnet-helper not installed; tailnet access disabled");
            return None;
        }
        let state_dir = paths.system_dir().join("tailscale");
        DeskPaths::ensure_dir(&state_dir).ok()?;
        let key_file = state_dir.join(AUTH_KEY_FILE);
        let auth_key = std::fs::read_to_string(&key_file)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let mut cmd = Command::new(&exe);
        cmd.arg("listen")
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--tailnet-port")
            .arg(port.to_string())
            .arg("--bridge-addr")
            .arg(format!("127.0.0.1:{port}"))
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);
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
                let status = supervise(stdout, port, take_over, &state_dir);
                tracing::info!(?status, "official Tailscale hand-over");
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

/// Waits for the helper's first report, then runs the hand-over gated on a real check.
fn supervise(stdout: ChildStdout, port: u16, take_over: bool, state_dir: &Path) -> TakeoverStatus {
    let report = serde_json::Deserializer::from_reader(BufReader::new(stdout))
        .into_iter::<HelperReport>()
        .next()
        .and_then(Result::ok);
    let verify = || {
        report
            .as_ref()
            .is_some_and(|r| desk_transport::helper_online(&r.status, &r.ips, port))
    };
    takeover::take_over(&ScTailscale, take_over, verify, &record_path(state_dir))
}

fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join(RECORD_FILE)
}

/// Restores an official Tailscale client that this agent took over (called by `uninstall`).
pub(crate) fn restore_official(paths: &DeskPaths) -> bool {
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
