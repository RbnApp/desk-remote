//! IPC worker: talks to the service, runs capture/encode and injects input.

use crate::inbox::FileInbox;
use crate::state::{CodeInfo, Notice, Shared};
use desk_capture::{list_monitors, CursorPos, CursorTracker, MonitorSelector, ScreenRect};
use desk_codec::{EncodedSink, MediaPipeline, PipelineConfig};
use desk_config::{ConfigManager, ServiceConfig};
use desk_error::DeskResult;
use desk_i18n::t;
use desk_input::{system_injector, SystemInjector};
use desk_ipc::{
    AgentInfo, IpcClient, IpcConnection, IpcMessage, PeerActivityKind, TailnetState, UpdateOutcome,
};
use desk_paths::DeskPaths;
use desk_protocol::{InputPayload, Message};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP,
};
use windows::Win32::System::Threading::GetCurrentProcessId;

/// The helper gives up when it has had no service connection for this long: a newer
/// helper (started by a restarted service) owns the session by then.
const ORPHAN_AFTER: Duration = Duration::from_secs(20);
const LOCK_POLL: Duration = Duration::from_secs(1);
/// Cursor sampling period while a viewer watches (about 60 Hz), and the idle receive timeout.
const CURSOR_POLL: Duration = Duration::from_millis(16);
const IDLE_POLL: Duration = Duration::from_millis(100);
/// How often the secure-desktop helper checks which desktop receives input.
const DESKTOP_POLL: Duration = Duration::from_millis(250);

/// Own session id (the pipe name embeds it).
#[must_use]
pub fn own_session_id() -> u32 {
    let mut id = 0u32;
    // SAFETY: valid out-parameter.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) };
    id
}

/// Tray balloon for a paired panel that connected or offered a file (DAPP-104 H-01/H-02).
fn peer_notice(kind: PeerActivityKind, peer: &str, detail: &str) -> Notice {
    let (title, body) = match kind {
        PeerActivityKind::Connected => ("tray.peer_connected_title", "tray.peer_connected_body"),
        PeerActivityKind::FileOffered => ("tray.peer_file_title", "tray.peer_file_body"),
    };
    Notice {
        title: t(title),
        body: t(body).replace("{peer}", peer).replace("{detail}", detail),
        open: None,
    }
}

/// Balloon answering the tray's "Check for updates" click.
fn update_notice(outcome: UpdateOutcome, version: &str) -> Notice {
    let body = match outcome {
        UpdateOutcome::UpToDate => "tray.update_uptodate",
        UpdateOutcome::Installing => "tray.update_installing",
        UpdateOutcome::Deferred => "tray.update_deferred",
        UpdateOutcome::Failed => "tray.update_failed",
    };
    Notice {
        title: t("tray.update_title"),
        body: t(body).replace("{v}", version),
        open: None,
    }
}

/// True while the tray has already told the user that Tailscale needs a sign-in.
static LOGIN_ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// Balloon asking for the one-time Tailscale sign-in; shown once per sign-in need (DAPP-113).
fn login_notice(info: &AgentInfo) -> Option<Notice> {
    if info.tailnet != TailnetState::NeedsLogin {
        LOGIN_ANNOUNCED.store(false, Ordering::SeqCst);
        return None;
    }
    (!LOGIN_ANNOUNCED.swap(true, Ordering::SeqCst)).then(|| Notice {
        title: t("tray.login_title"),
        body: t("tray.login_body"),
        open: None,
    })
}

/// Forwards encoded media to the service over the pipe.
struct IpcSink {
    conn: IpcConnection,
}

impl EncodedSink for IpcSink {
    fn on_config(&self, width: u32, height: u32, fps: u32) {
        let _ = self.conn.send(&IpcMessage::Wire(Message::VideoConfig {
            codec: "h264".to_string(),
            width,
            height,
            fps,
        }));
    }

    fn on_frame(&self, ts_us: u64, keyframe: bool, data: &[u8]) {
        let _ = self.conn.send(&IpcMessage::Wire(Message::VideoFrame {
            ts_us,
            keyframe,
            data: data.to_vec(),
        }));
    }
}

/// True when the input desktop is not ours to use (lock screen, UAC prompt, Ctrl+Alt+Del).
fn input_desktop_unavailable() -> bool {
    // SAFETY: plain query; the handle is closed right away.
    match unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) } {
        Ok(desktop) => {
            // SAFETY: handle returned above.
            let _ = unsafe { CloseDesktop(desktop) };
            false
        }
        Err(_) => true,
    }
}

fn primary_rect() -> Option<ScreenRect> {
    let monitors = list_monitors().ok()?;
    MonitorSelector::Primary
        .select(&monitors)
        .ok()
        .map(|m| m.rect)
}

struct Session {
    conn: IpcConnection,
    pipeline: MediaPipeline,
    injector: Option<SystemInjector>,
    locked: bool,
    last_lock_poll: Instant,
    inbox: FileInbox,
    cursor: CursorTracker,
    /// Monitor the viewers see; the cursor is reported relative to it.
    rect: Option<ScreenRect>,
    last_cursor: Option<CursorPos>,
}

impl Session {
    fn new(conn: IpcConnection, cfg: &ServiceConfig, inbox_dir: PathBuf) -> Self {
        let pipeline = MediaPipeline::new(
            PipelineConfig {
                monitor: MonitorSelector::Primary,
                fps: cfg.capture_fps,
                max_width: cfg.capture_max_width,
                max_height: cfg.capture_max_height,
                bitrate_bps: None,
                allow_software: true,
            },
            Arc::new(IpcSink { conn: conn.clone() }),
        );
        Self {
            inbox: FileInbox::new(inbox_dir, cfg.max_upload_mb, conn.clone()),
            conn,
            pipeline,
            injector: None,
            locked: false,
            last_lock_poll: Instant::now(),
            cursor: CursorTracker::new(),
            rect: None,
            last_cursor: None,
        }
    }

    fn join(&mut self) {
        match primary_rect() {
            None => {
                tracing::warn!("no monitor attached; viewer sees an offline status");
                let _ = self.conn.send(&IpcMessage::NoDisplay);
            }
            Some(rect) => {
                self.rect = Some(rect);
                // A joining viewer has none of the shapes yet.
                self.cursor.reset();
                self.last_cursor = None;
                if self.injector.is_none() {
                    self.injector = Some(system_injector(rect));
                }
                if let Err(e) = self.pipeline.viewer_join() {
                    tracing::error!(error = %e, "cannot start capture");
                }
            }
        }
    }

    fn leave(&mut self) {
        self.pipeline.viewer_leave();
        if self.pipeline.viewer_count() == 0 {
            if let Some(inj) = self.injector.as_mut() {
                let _ = inj.release_all();
            }
        }
    }

    fn input(&mut self, payload: InputPayload) {
        let Some(inj) = self.injector.as_mut() else {
            return;
        };
        let result = match payload {
            InputPayload::Mouse {
                x,
                y,
                buttons,
                wheel,
            } => inj.mouse(x, y, buttons, desk_input::wheel_units(wheel)),
            InputPayload::Key { scancode, down } => match u16::try_from(scancode) {
                Ok(code) => inj.key(code, down),
                Err(_) => Ok(()),
            },
        };
        if let Err(e) = result {
            tracing::debug!(error = %e, "input injection failed");
        }
    }

    /// Sends the cursor shape (once) and position (on change) to the viewers.
    fn poll_cursor(&mut self) {
        let Some(rect) = self.rect.filter(|_| self.pipeline.viewer_count() > 0) else {
            return;
        };
        let Some(update) = self.cursor.poll(rect) else {
            return;
        };
        if let Some(shape) = update.new_shape {
            let _ = self.conn.send(&IpcMessage::Wire(Message::CursorShape {
                id: shape.id,
                hot_x: shape.hot_x,
                hot_y: shape.hot_y,
                width: shape.width,
                height: shape.height,
                rgba: shape.rgba,
            }));
        }
        if self.last_cursor != Some(update.pos) {
            self.last_cursor = Some(update.pos);
            let _ = self.conn.send(&IpcMessage::Wire(Message::CursorPos {
                x: update.pos.x,
                y: update.pos.y,
                shape_id: update.pos.shape_id,
                visible: update.pos.visible,
            }));
        }
    }

    fn poll_lock(&mut self) {
        if self.pipeline.viewer_count() == 0 || self.last_lock_poll.elapsed() < LOCK_POLL {
            return;
        }
        self.last_lock_poll = Instant::now();
        let locked = input_desktop_unavailable();
        if locked != self.locked {
            self.locked = locked;
            tracing::info!(locked, "input desktop availability changed");
            let _ = self.conn.send(&IpcMessage::DesktopLocked { locked });
        }
    }

    fn handle(&mut self, msg: IpcMessage, shared: &Shared) -> bool {
        match msg {
            IpcMessage::Wire(Message::ViewerJoin) => self.join(),
            IpcMessage::Wire(Message::ViewerLeave) => self.leave(),
            IpcMessage::Wire(Message::RequestKeyframe) => self.pipeline.request_keyframe(),
            IpcMessage::Wire(Message::InputEvent(payload)) => self.input(payload),
            IpcMessage::FileIncoming {
                transfer_id,
                peer_name,
            } => self.inbox.announce(transfer_id, peer_name),
            IpcMessage::Wire(wire) if wire.transfer_id().is_some() => {
                self.inbox.handle(&wire, shared);
            }
            IpcMessage::Viewers { device_ids } => shared.update(|ui| ui.viewers = device_ids),
            IpcMessage::PairingCode { code, ttl_secs } => shared.update(|ui| {
                ui.code = (!code.is_empty()).then(|| CodeInfo {
                    code,
                    expires: Instant::now() + Duration::from_secs(u64::from(ttl_secs)),
                });
            }),
            IpcMessage::AgentInfo(info) => shared.update(|ui| {
                if let Some(notice) = login_notice(&info) {
                    ui.pending_notice.get_or_insert(notice);
                }
                ui.info = Some(info);
            }),
            IpcMessage::PeerActivity {
                kind,
                peer_name,
                detail,
            } => {
                shared.update(|ui| ui.pending_notice = Some(peer_notice(kind, &peer_name, &detail)))
            }
            IpcMessage::UpdateResult { outcome, version } => {
                shared.update(|ui| ui.pending_notice = Some(update_notice(outcome, &version)));
            }
            IpcMessage::Shutdown => return false,
            other => tracing::debug!(kind = other.kind(), "ignored service message"),
        }
        true
    }

    /// Secure-desktop helper only: moves capture and input to the desktop that receives input
    /// when it changes (UAC prompt, lock screen, back to the user's desktop).
    fn follow_desktop(&mut self, attached: &mut Option<String>) {
        if self.last_lock_poll.elapsed() < DESKTOP_POLL {
            return;
        }
        self.last_lock_poll = Instant::now();
        let Some(now) = desk_capture::input_desktop_name() else {
            return;
        };
        if attached.as_deref() == Some(now.as_str()) {
            return;
        }
        *attached = desk_capture::attach_input_desktop();
        tracing::info!(desktop = %now, "capture follows the input desktop");
        // A running capture belongs to the old desktop: restart it on the new one.
        let viewers = self.pipeline.viewer_count();
        for _ in 0..viewers {
            self.pipeline.viewer_leave();
        }
        for _ in 0..viewers {
            self.join();
        }
    }

    fn shutdown(&mut self) {
        self.inbox.abort();
        while self.pipeline.viewer_count() > 0 {
            self.pipeline.viewer_leave();
        }
        if let Some(inj) = self.injector.as_mut() {
            let _ = inj.release_all();
        }
    }
}

fn serve(conn: IpcConnection, cfg: &ServiceConfig, inbox_dir: &Path, shared: &Shared) -> bool {
    let mut session = Session::new(conn.clone(), cfg, inbox_dir.to_path_buf());
    shared.set_conn(Some(conn.clone()));
    shared.update(|ui| ui.service_online = true);
    let _ = conn.send(&IpcMessage::HelperHello {
        session_id: own_session_id(),
        pid: std::process::id(),
    });
    let keep_running = loop {
        let wait = if session.pipeline.viewer_count() > 0 {
            CURSOR_POLL
        } else {
            IDLE_POLL
        };
        match conn.recv_timeout(wait) {
            Ok(Some(msg)) => {
                if !session.handle(msg, shared) {
                    break false;
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "service connection lost");
                break true;
            }
        }
        session.poll_cursor();
        session.poll_lock();
    };
    session.shutdown();
    shared.set_conn(None);
    shared.update(|ui| {
        ui.service_online = false;
        ui.viewers.clear();
        ui.info = None;
    });
    conn.close();
    keep_running
}

/// The SYSTEM helper's view of the service link: picture, cursor and input only. It owns no UI
/// and no files; it exists to show and drive the UAC prompt and the lock screen (DAPP-118).
fn serve_secure(conn: IpcConnection, cfg: &ServiceConfig, inbox_dir: &Path) -> bool {
    let mut session = Session::new(conn.clone(), cfg, inbox_dir.to_path_buf());
    let mut attached = desk_capture::attach_input_desktop();
    let _ = conn.send(&IpcMessage::HelperHello {
        session_id: own_session_id(),
        pid: std::process::id(),
    });
    let keep_running = loop {
        let wait = if session.pipeline.viewer_count() > 0 {
            CURSOR_POLL
        } else {
            IDLE_POLL
        };
        match conn.recv_timeout(wait) {
            Ok(Some(IpcMessage::Wire(Message::ViewerJoin))) => session.join(),
            Ok(Some(IpcMessage::Wire(Message::ViewerLeave))) => session.leave(),
            Ok(Some(IpcMessage::Wire(Message::RequestKeyframe))) => {
                session.pipeline.request_keyframe();
            }
            Ok(Some(IpcMessage::Wire(Message::InputEvent(payload)))) => session.input(payload),
            Ok(Some(IpcMessage::Shutdown)) => break false,
            Ok(Some(_)) | Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "service connection lost");
                break true;
            }
        }
        session.follow_desktop(&mut attached);
        session.poll_cursor();
    };
    session.shutdown();
    conn.close();
    keep_running
}

/// Secure-desktop helper main: connect to the service, serve, end when the service is gone.
pub fn run_secure(paths: &DeskPaths, done: &Arc<AtomicBool>) -> DeskResult<()> {
    // Every capture thread of this process must sit on the desktop being shown.
    desk_capture::set_follow_input_desktop(true);
    let cfg: ServiceConfig =
        ConfigManager::load_or_default(&paths.service_config_path()).unwrap_or_default();
    let inbox_dir = paths.received_files_dir();
    let pipe = DeskPaths::pipe_secure_helper(own_session_id());
    let mut last_connected = Instant::now();
    while !done.load(Ordering::SeqCst) {
        match IpcClient::connect(&pipe, Duration::from_secs(2)) {
            Ok(conn) => {
                tracing::info!(%pipe, "secure-desktop helper connected to service");
                if !serve_secure(conn, &cfg, &inbox_dir) {
                    break;
                }
                last_connected = Instant::now();
            }
            Err(_) => {
                if last_connected.elapsed() > ORPHAN_AFTER {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    Ok(())
}

/// Worker main: connect (retrying), serve, reconnect; ends when told to or orphaned.
pub fn run(shared: &Arc<Shared>, paths: &DeskPaths, done: &Arc<AtomicBool>) -> DeskResult<()> {
    let cfg: ServiceConfig =
        ConfigManager::load_or_default(&paths.service_config_path()).unwrap_or_default();
    let inbox_dir = paths.received_files_dir();
    let pipe = DeskPaths::pipe_session_helper(own_session_id());
    let mut last_connected = Instant::now();
    while !done.load(Ordering::SeqCst) {
        match IpcClient::connect(&pipe, Duration::from_secs(2)) {
            Ok(conn) => {
                tracing::info!(%pipe, "connected to service");
                let keep_running = serve(conn, &cfg, &inbox_dir, shared);
                last_connected = Instant::now();
                if !keep_running {
                    break;
                }
            }
            Err(_) => {
                if last_connected.elapsed() > ORPHAN_AFTER {
                    tracing::info!("no service for a while; helper exits");
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    done.store(true, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_balloon_shows_once_per_need() {
        let info = |tailnet| AgentInfo {
            tailnet,
            ..AgentInfo::default()
        };
        assert!(login_notice(&info(TailnetState::NeedsLogin)).is_some());
        assert!(login_notice(&info(TailnetState::NeedsLogin)).is_none());
        assert!(login_notice(&info(TailnetState::Connected)).is_none());
        assert!(login_notice(&info(TailnetState::NeedsLogin)).is_some());
    }
}
