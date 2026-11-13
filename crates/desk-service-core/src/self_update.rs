//! The agent updates itself: check at start and every few hours, install between sessions.
//!
//! ## Responsibility
//! Runs `desk-update` for the agent component, hands the verified installer to the OS and
//! answers the tray's "check for updates" click.
//!
//! ## Scope Boundaries
//! - Only the agent install is touched; the panel updates itself.
//! - A running service cannot replace its own exe, so the verified `DeskRemote-Agent-Setup`
//!   runs as a separate process (`/S`): it stops the service, swaps the files (keeping a
//!   backup it restores on failure) and starts the service again.
//! - An update waits until no viewer is connected.
//!
//! ## Dependencies
//! `desk-update`, `desk-ipc`, `desk-version`, the hub.

use crate::hub::Hub;
use crate::launch::LaunchMode;
use desk_error::{DeskError, DeskResult};
use desk_ipc::{IpcMessage, UpdateOutcome};
use desk_paths::DeskPaths;
use desk_update::{Component, NetTransport, UpdateStatus, Updater};
use desk_version::SemVer;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// How often a deferred update looks again for a free moment.
const IDLE_POLL: Duration = Duration::from_secs(5);
/// Granularity of the waits so a stopping service is not held up.
const STOP_POLL: Duration = Duration::from_millis(500);

struct Context {
    hub: Arc<Hub>,
    updater: Updater<NetTransport>,
    mode: LaunchMode,
    install_dir: PathBuf,
    stop: Arc<AtomicBool>,
    /// A cycle is running (check, download or waiting for viewers to leave).
    busy: AtomicBool,
    /// The installer was started; nothing else may run.
    applied: AtomicBool,
}

/// Starts the update thread and registers the tray request handler.
pub(crate) fn start(
    hub: &Arc<Hub>,
    paths: &DeskPaths,
    feed_url: Option<String>,
    mode: LaunchMode,
    stop: &Arc<AtomicBool>,
) -> DeskResult<JoinHandle<()>> {
    let current = SemVer::current()
        .ok_or_else(|| DeskError::Update("the running version is not a version".to_string()))?;
    let exe = DeskPaths::current_exe()?;
    let install_dir = exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| DeskError::Update("the executable has no folder".to_string()))?;
    let ctx = Arc::new(Context {
        hub: Arc::clone(hub),
        updater: Updater::new(
            Component::Agent,
            current,
            feed_url,
            paths.agent_update_dir(),
            NetTransport,
        ),
        mode,
        install_dir,
        stop: Arc::clone(stop),
        busy: AtomicBool::new(false),
        applied: AtomicBool::new(false),
    });
    let on_request = Arc::clone(&ctx);
    hub.set_update_handler(Some(Arc::new(move || {
        let ctx = Arc::clone(&on_request);
        let started = std::thread::Builder::new()
            .name("desk-update-now".to_string())
            .spawn(move || ctx.cycle(true));
        if let Err(e) = started {
            tracing::warn!(error = %e, "cannot start the update check");
        }
    })));
    std::thread::Builder::new()
        .name("desk-update".to_string())
        .spawn(move || {
            while !ctx.stop.load(Ordering::SeqCst) && !ctx.applied.load(Ordering::SeqCst) {
                ctx.cycle(false);
                if !ctx.sleep(desk_update::AGENT_CHECK_INTERVAL) {
                    break;
                }
            }
        })
        .map_err(|e| DeskError::Update(format!("cannot start the update thread: {e}")))
}

impl Context {
    /// Sleeps `total`; `false` when the service is stopping or an update was started.
    fn sleep(&self, total: Duration) -> bool {
        let mut left = total;
        while !left.is_zero() {
            if self.stop.load(Ordering::SeqCst) || self.applied.load(Ordering::SeqCst) {
                return false;
            }
            let step = left.min(STOP_POLL);
            std::thread::sleep(step);
            left -= step;
        }
        true
    }

    /// Tells the tray what happened (only a click gets an answer).
    fn answer(&self, manual: bool, outcome: UpdateOutcome, version: &str) {
        if manual {
            let _ = self.hub.send_to_helper(&IpcMessage::UpdateResult {
                outcome,
                version: version.to_string(),
            });
        }
    }

    /// One check; installs a newer, verified release as soon as no viewer is connected.
    fn cycle(&self, manual: bool) {
        if self.applied.load(Ordering::SeqCst) {
            self.answer(manual, UpdateOutcome::Installing, desk_version::CURRENT);
            return;
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            // A cycle is already working on it; a waiting one means viewers are connected.
            self.answer(manual, UpdateOutcome::Deferred, desk_version::CURRENT);
            return;
        }
        self.run_cycle(manual);
        self.busy.store(false, Ordering::SeqCst);
    }

    fn run_cycle(&self, manual: bool) {
        let (release, path) = match self.updater.check_and_download() {
            Ok(UpdateStatus::Ready { release, path }) => (release, path),
            Ok(_) => {
                self.answer(manual, UpdateOutcome::UpToDate, desk_version::CURRENT);
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "update check failed");
                self.answer(manual, UpdateOutcome::Failed, desk_version::CURRENT);
                return;
            }
        };
        let version = release.version.to_string();
        if self.hub.joined_count() > 0 {
            tracing::info!(%version, "update ready, waiting for the viewers to leave");
            self.answer(manual, UpdateOutcome::Deferred, &version);
            while self.hub.joined_count() > 0 {
                if !self.sleep(IDLE_POLL) {
                    return;
                }
            }
        }
        match launch_installer(&path, self.mode, &self.install_dir) {
            Ok(()) => {
                self.applied.store(true, Ordering::SeqCst);
                tracing::info!(%version, "update installer started");
                self.answer(manual, UpdateOutcome::Installing, &version);
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot start the update installer");
                self.answer(manual, UpdateOutcome::Failed, &version);
            }
        }
    }
}

/// Arguments of the silent installer: the service install is machine-wide; the console test
/// mode updates the folder it runs from, for the current user.
fn installer_args(mode: LaunchMode, install_dir: &Path) -> Vec<String> {
    match mode {
        LaunchMode::SystemService => vec!["/S".to_string()],
        LaunchMode::CurrentUser => vec![
            "/S".to_string(),
            "/CURRENTUSER".to_string(),
            // NSIS wants /D last and unquoted.
            format!("/D={}", install_dir.display()),
        ],
    }
}

/// Starts the verified installer as its own process and keeps a watcher that logs its result.
fn launch_installer(installer: &Path, mode: LaunchMode, install_dir: &Path) -> DeskResult<()> {
    let mut command = desk_paths::hidden_command(installer);
    for arg in installer_args(mode, install_dir) {
        command.raw_arg(arg);
    }
    if mode == LaunchMode::CurrentUser {
        // A per-user install needs no administrator rights: run the installer with the
        // caller's rights instead of asking for a UAC prompt nobody is there to answer.
        command.env("__COMPAT_LAYER", "RunAsInvoker");
    }
    let mut child = command
        .spawn()
        .map_err(|e| DeskError::Update(format!("cannot start the installer: {e}")))?;
    std::thread::Builder::new()
        .name("desk-update-watch".to_string())
        .spawn(move || match child.wait() {
            Ok(status) => tracing::info!(%status, "update installer finished"),
            Err(e) => tracing::warn!(error = %e, "cannot wait for the update installer"),
        })
        .map_err(|e| DeskError::Update(format!("cannot watch the installer: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_installs_machine_wide_and_console_mode_in_place() {
        let dir = Path::new(r"C:\Test Dir\Agent");
        assert_eq!(installer_args(LaunchMode::SystemService, dir), ["/S"]);
        assert_eq!(
            installer_args(LaunchMode::CurrentUser, dir),
            ["/S", "/CURRENTUSER", r"/D=C:\Test Dir\Agent"],
            "/D is last and unquoted"
        );
    }
}
