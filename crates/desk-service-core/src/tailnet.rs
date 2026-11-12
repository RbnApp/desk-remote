//! Optional embedded tailnet helper (userspace Tailscale) next to the service.
//!
//! A system Tailscale client that is already running wins: viewers then reach the normal
//! control port over the tailnet and no second node is created.

use desk_paths::DeskPaths;
use desk_transport::TailscaleDetector;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Owns the helper child process; it is killed when the guard drops.
pub struct TailnetGuard {
    child: Child,
}

impl TailnetGuard {
    /// Starts `desk-tailnet-helper listen` when it ships next to the service and no
    /// system Tailscale is running. Returns `None` when nothing was started.
    #[must_use]
    pub fn start(paths: &DeskPaths, port: u16) -> Option<Self> {
        if TailscaleDetector::detect().running {
            tracing::info!("system Tailscale is running; embedded tailnet helper not needed");
            return None;
        }
        let exe = DeskPaths::sibling_exe("desk-tailnet-helper.exe").ok()?;
        if !exe.exists() {
            tracing::info!("desk-tailnet-helper not installed; tailnet access disabled");
            return None;
        }
        let state_dir = paths.system_dir().join("tailscale");
        DeskPaths::ensure_dir(&state_dir).ok()?;
        let child = Command::new(&exe)
            .arg("listen")
            .arg("--state-dir")
            .arg(&state_dir)
            .arg("--tailnet-port")
            .arg(port.to_string())
            .arg("--bridge-addr")
            .arg(format!("127.0.0.1:{port}"))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| tracing::warn!(error = %e, "cannot start tailnet helper"))
            .ok()?;
        tracing::info!(pid = child.id(), "tailnet helper started");
        Some(Self { child })
    }
}

impl Drop for TailnetGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
