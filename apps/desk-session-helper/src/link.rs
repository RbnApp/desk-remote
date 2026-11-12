//! IPC worker: talks to the service, runs capture/encode and injects input.

use crate::inbox::FileInbox;
use crate::state::{CodeInfo, Shared};
use desk_capture::{list_monitors, MonitorSelector, ScreenRect};
use desk_codec::{EncodedSink, MediaPipeline, PipelineConfig};
use desk_config::{ConfigManager, ServiceConfig};
use desk_error::DeskResult;
use desk_input::{system_injector, SystemInjector};
use desk_ipc::{IpcClient, IpcConnection, IpcMessage};
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

/// Own session id (the pipe name embeds it).
#[must_use]
pub fn own_session_id() -> u32 {
    let mut id = 0u32;
    // SAFETY: valid out-parameter.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) };
    id
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
        }
    }

    fn join(&mut self) {
        match primary_rect() {
            None => {
                tracing::warn!("no monitor attached; viewer sees an offline status");
                let _ = self.conn.send(&IpcMessage::NoDisplay);
            }
            Some(rect) => {
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
            } => inj.mouse(x, y, buttons, i16::from(wheel)),
            InputPayload::Key { scancode, down } => match u16::try_from(scancode) {
                Ok(code) => inj.key(code, down),
                Err(_) => Ok(()),
            },
        };
        if let Err(e) = result {
            tracing::debug!(error = %e, "input injection failed");
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
            IpcMessage::AgentInfo(info) => shared.update(|ui| ui.info = Some(info)),
            IpcMessage::Shutdown => return false,
            other => tracing::debug!(?other, "ignored service message"),
        }
        true
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
        match conn.recv_timeout(Duration::from_millis(100)) {
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
