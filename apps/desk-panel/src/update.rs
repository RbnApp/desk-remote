//! Panel self-update: check, download, verify and install the panel's own installer.
//!
//! ## Responsibility
//! Drives `desk-update` for the panel component and turns its status into the one
//! [`UpdateInfo`] value the Updates tab renders (also pushed as the `update://status` event).
//!
//! ## Scope Boundaries
//! - Only the panel installer is fetched (`Component::Panel`); the agent is never touched.
//! - An open remote session is never cut: an update that is ready waits until no PC is
//!   connected (outgoing tabs or an incoming viewer), then the installer starts and the panel
//!   exits so its files can be replaced.
//!
//! ## Dependencies
//! `desk-update`, `desk-paths`, `desk-version`, the session and incoming managers.

use crate::incoming::IncomingManager;
use crate::sessions::Sessions;
use desk_update::{Component, NetTransport, UpdateStatus, Updater};
use desk_version::SemVer;
use serde::Serialize;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

/// Event the UI listens to.
pub const EVENT: &str = "update://status";
/// How often a ready update looks again for a moment without a connection.
const IDLE_POLL: Duration = Duration::from_secs(5);

/// Flags the status alone cannot express; shared with the status observer.
#[derive(Default)]
struct Flags {
    /// Ready, but a remote connection is open.
    waiting: AtomicBool,
    /// The installer was started; the panel is about to exit.
    installing: AtomicBool,
    /// A check/download cycle is running.
    busy: AtomicBool,
    /// Unix seconds of the last finished check (0 = none yet).
    last_check: AtomicU64,
}

/// What the Updates tab shows.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpdateInfo {
    pub current: &'static str,
    /// `idle`, `checking`, `available`, `downloading`, `ready`, `waiting`, `installing`
    /// or `failed`.
    pub state: &'static str,
    /// Version of the offered release.
    pub latest: Option<String>,
    pub received: u64,
    pub total: Option<u64>,
    /// Unix seconds of the last finished check.
    pub last_check: Option<u64>,
    /// Why the last attempt failed (only with `state == "failed"`).
    pub error: Option<String>,
}

fn info(status: &UpdateStatus, flags: &Flags) -> UpdateInfo {
    let (mut state, mut received, mut total, mut error) = match status {
        UpdateStatus::NoUpdate => ("idle", 0, None, None),
        UpdateStatus::Checking => ("checking", 0, None, None),
        UpdateStatus::Available(_) => ("available", 0, None, None),
        UpdateStatus::Downloading {
            received, total, ..
        } => ("downloading", *received, *total, None),
        UpdateStatus::Ready { .. } => ("ready", 0, None, None),
        UpdateStatus::Failed { message } => ("failed", 0, None, Some(message.clone())),
    };
    if state == "ready" && flags.waiting.load(Ordering::SeqCst) {
        state = "waiting";
    }
    if flags.installing.load(Ordering::SeqCst) {
        state = "installing";
        (received, total, error) = (0, None, None);
    }
    let last = flags.last_check.load(Ordering::SeqCst);
    UpdateInfo {
        current: desk_version::CURRENT,
        state,
        latest: status.release().map(|r| r.version.to_string()),
        received,
        total,
        last_check: (last > 0).then_some(last),
        error,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The panel's update client.
pub struct UpdateManager {
    updater: Arc<Updater<NetTransport>>,
    flags: Arc<Flags>,
    sessions: Arc<Sessions>,
    incoming: Arc<IncomingManager>,
}

impl UpdateManager {
    /// `feed_url` overrides the GitHub feed (tests); `None` = production.
    pub fn new(
        feed_url: Option<String>,
        sessions: Arc<Sessions>,
        incoming: Arc<IncomingManager>,
    ) -> Option<Self> {
        let current = SemVer::current()?;
        let dir = desk_paths::DeskPaths::default().panel_update_dir();
        Some(Self {
            updater: Arc::new(Updater::new(
                Component::Panel,
                current,
                feed_url,
                dir,
                NetTransport,
            )),
            flags: Arc::default(),
            sessions,
            incoming,
        })
    }

    /// Pushes every status change to the UI.
    pub fn attach(&self, app: AppHandle) {
        let flags = Arc::clone(&self.flags);
        self.updater.on_status(move |status| {
            let _ = app.emit(EVENT, info(status, &flags));
        });
    }

    /// Current state for the tab.
    pub fn state(&self) -> UpdateInfo {
        info(&self.updater.status(), &self.flags)
    }

    /// Is any PC connected right now (tabs or an incoming viewer)?
    fn connected(&self) -> bool {
        self.sessions.list().iter().any(|s| s.state == "connected")
            || self.incoming.get_state().session.is_some()
    }

    fn announce(&self, app: &AppHandle) {
        let _ = app.emit(EVENT, self.state());
    }

    /// Starts a background cycle. `check` only looks; `apply` also downloads and installs.
    /// A cycle already running makes this a no-op.
    pub fn run(self: &Arc<Self>, app: AppHandle, apply: bool) {
        if self.flags.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("desk-panel-update".to_string())
            .spawn(move || {
                me.cycle(&app, apply);
                me.flags.busy.store(false, Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            self.flags.busy.store(false, Ordering::SeqCst);
            tracing::warn!(error = %e, "cannot start the update thread");
        }
    }

    fn cycle(&self, app: &AppHandle, apply: bool) {
        let ready = if matches!(self.updater.status(), UpdateStatus::Ready { .. }) {
            true
        } else {
            let outcome = if apply {
                self.updater.check_and_download()
            } else {
                self.updater.check()
            };
            self.flags.last_check.store(now_secs(), Ordering::SeqCst);
            if let Err(e) = &outcome {
                tracing::warn!(error = %e, "panel update check failed");
            }
            matches!(outcome, Ok(UpdateStatus::Ready { .. }))
        };
        // Re-announce so the tab sees the new "last check" even when nothing changed.
        self.announce(app);
        if !ready {
            return;
        }
        let UpdateStatus::Ready { release, path } = self.updater.status() else {
            return;
        };
        if self.connected() {
            tracing::info!(version = %release.version, "update ready, waiting for the connections to end");
            self.flags.waiting.store(true, Ordering::SeqCst);
            self.announce(app);
            while self.connected() {
                std::thread::sleep(IDLE_POLL);
            }
            self.flags.waiting.store(false, Ordering::SeqCst);
        }
        match launch_installer(&path) {
            Ok(()) => {
                tracing::info!(version = %release.version, "panel update installer started");
                self.flags.installing.store(true, Ordering::SeqCst);
                self.announce(app);
                // The installer replaces desk-panel.exe, so the panel must be gone first.
                std::thread::sleep(Duration::from_millis(800));
                app.exit(0);
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot start the panel update installer");
                self.announce(app);
            }
        }
    }
}

/// Starts the verified Tauri NSIS installer silently; `/UPDATE` makes it start the panel again.
fn launch_installer(installer: &Path) -> std::io::Result<()> {
    let mut command = desk_paths::hidden_command(installer);
    command.raw_arg("/S").raw_arg("/UPDATE");
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_and_installing_override_a_ready_status() -> desk_error::DeskResult<()> {
        let flags = Flags::default();
        let release = desk_update::parse_release(
            br#"{"tag_name":"v9.9.9","assets":[
              {"name":"DeskRemote-Panel-Setup-9.9.9.exe","browser_download_url":"https://x/p.exe","size":3},
              {"name":"SHA256SUMS.txt","browser_download_url":"https://x/s"}]}"#,
            Component::Panel,
        );
        let release = release?;
        let ready = UpdateStatus::Ready {
            release,
            path: "p.exe".into(),
        };
        assert_eq!(info(&ready, &flags).state, "ready");
        assert_eq!(info(&ready, &flags).latest.as_deref(), Some("9.9.9"));
        flags.waiting.store(true, Ordering::SeqCst);
        assert_eq!(info(&ready, &flags).state, "waiting");
        flags.installing.store(true, Ordering::SeqCst);
        assert_eq!(info(&ready, &flags).state, "installing");
        Ok(())
    }

    #[test]
    fn a_failed_status_carries_its_message_and_no_check_means_no_time() {
        let flags = Flags::default();
        let failed = UpdateStatus::Failed {
            message: "offline".to_string(),
        };
        let shown = info(&failed, &flags);
        assert_eq!(shown.state, "failed");
        assert_eq!(shown.error.as_deref(), Some("offline"));
        assert_eq!(shown.last_check, None);
        flags.last_check.store(42, Ordering::SeqCst);
        assert_eq!(info(&failed, &flags).last_check, Some(42));
    }
}
