//! Routing core between authenticated viewers (network) and the session helper (IPC).
//!
//! Frames flow helper -> hub -> every joined viewer; input and viewer lifecycle flow
//! viewer -> hub -> helper. The hub owns no sockets and no processes, which keeps the
//! routing rules unit-testable.

use desk_crypto::{DeviceIdentity, PairingManager, PAIRING_CODE_TTL};
use desk_ipc::{IpcConnection, IpcMessage};
use desk_protocol::Message;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard};

/// Frames queued per viewer before the slowest viewer starts dropping (and asks for an IDR).
const VIEWER_QUEUE: usize = 90;

/// Identifies one authenticated viewer connection inside the hub.
pub type ViewerId = u64;

struct Viewer {
    device_id: String,
    tx: SyncSender<Message>,
    joined: bool,
}

#[derive(Default)]
struct State {
    viewers: HashMap<ViewerId, Viewer>,
    helper: Option<(u64, IpcConnection)>,
    last_config: Option<Message>,
    session_locked: bool,
    helper_locked: bool,
    no_display: bool,
}

impl State {
    fn available(&self) -> bool {
        !(self.session_locked || self.helper_locked || self.no_display)
    }

    fn status(&self) -> Message {
        Message::Status {
            online: self.available(),
            latency_ms: 0,
        }
    }

    fn joined_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .viewers
            .values()
            .filter(|v| v.joined)
            .map(|v| v.device_id.clone())
            .collect();
        ids.sort();
        ids
    }

    fn broadcast(&self, msg: &Message) -> bool {
        let mut dropped = false;
        for viewer in self.viewers.values().filter(|v| v.joined) {
            if let Err(TrySendError::Full(_)) = viewer.tx.try_send(msg.clone()) {
                dropped = true;
            }
        }
        dropped
    }
}

/// Shared service state: viewers, the helper link and pairing.
pub struct Hub {
    state: Mutex<State>,
    pairing: Mutex<PairingManager>,
    identity: DeviceIdentity,
    next_id: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Hub {
    #[must_use]
    pub fn new(identity: DeviceIdentity, pairing: PairingManager) -> Self {
        Self {
            state: Mutex::new(State::default()),
            pairing: Mutex::new(pairing),
            identity,
            next_id: AtomicU64::new(1),
        }
    }

    #[must_use]
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// The shared pairing state; handshakes lock it only for short steps.
    #[must_use]
    pub fn pairing(&self) -> &Mutex<PairingManager> {
        &self.pairing
    }

    /// Runs `f` with the pairing manager locked (handshakes, code creation).
    pub fn with_pairing<R>(&self, f: impl FnOnce(&mut PairingManager) -> R) -> R {
        f(&mut lock(&self.pairing))
    }

    // ---- viewers ----

    /// Registers an authenticated viewer; frames for it arrive on the returned receiver.
    pub fn register_viewer(&self, device_id: &str) -> (ViewerId, Receiver<Message>) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = sync_channel(VIEWER_QUEUE);
        lock(&self.state).viewers.insert(
            id,
            Viewer {
                device_id: device_id.to_string(),
                tx,
                joined: false,
            },
        );
        (id, rx)
    }

    /// The viewer asked to watch: capture starts if it is the first one.
    pub fn viewer_join(&self, id: ViewerId) {
        let (helper, list) = {
            let mut st = lock(&self.state);
            let status = st.status();
            let config = st.last_config.clone();
            let Some(viewer) = st.viewers.get_mut(&id) else {
                return;
            };
            if viewer.joined {
                return;
            }
            viewer.joined = true;
            if let Some(cfg) = config {
                let _ = viewer.tx.try_send(cfg);
            }
            let _ = viewer.tx.try_send(status);
            (st.helper.as_ref().map(|(_, c)| c.clone()), st.joined_ids())
        };
        tracing::info!(viewer = id, "viewer joined");
        if let Some(conn) = helper {
            let _ = conn.send(&IpcMessage::Wire(Message::ViewerJoin));
            let _ = conn.send(&IpcMessage::Viewers { device_ids: list });
        }
    }

    /// The viewer stopped watching (explicit leave or disconnect). Idempotent.
    pub fn viewer_leave(&self, id: ViewerId) {
        let (helper, list) = {
            let mut st = lock(&self.state);
            match st.viewers.get_mut(&id) {
                Some(viewer) if viewer.joined => viewer.joined = false,
                _ => return,
            }
            (st.helper.as_ref().map(|(_, c)| c.clone()), st.joined_ids())
        };
        tracing::info!(viewer = id, "viewer left");
        if let Some(conn) = helper {
            let _ = conn.send(&IpcMessage::Wire(Message::ViewerLeave));
            let _ = conn.send(&IpcMessage::Viewers { device_ids: list });
        }
    }

    /// Drops the viewer entirely (connection closed).
    pub fn remove_viewer(&self, id: ViewerId) {
        self.viewer_leave(id);
        lock(&self.state).viewers.remove(&id);
    }

    /// Forwards viewer input / keyframe requests to the helper. Ignored unless the viewer
    /// has joined, so a merely paired panel cannot drive the PC.
    pub fn forward_from_viewer(&self, id: ViewerId, msg: Message) {
        let helper = {
            let st = lock(&self.state);
            match st.viewers.get(&id) {
                Some(v) if v.joined => st.helper.as_ref().map(|(_, c)| c.clone()),
                _ => None,
            }
        };
        if let Some(conn) = helper {
            let _ = conn.send(&IpcMessage::Wire(msg));
        }
    }

    /// Number of viewers that are currently watching.
    #[must_use]
    pub fn joined_count(&self) -> usize {
        lock(&self.state).joined_ids().len()
    }

    // ---- helper link ----

    /// A helper connected: replay the viewer state it missed (helper restarts keep viewers).
    pub fn attach_helper(&self, conn: IpcConnection) -> u64 {
        let token = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (joined, list) = {
            let mut st = lock(&self.state);
            st.helper = Some((token, conn.clone()));
            st.helper_locked = false;
            st.no_display = false;
            let list = st.joined_ids();
            (list.len(), list)
        };
        for _ in 0..joined {
            let _ = conn.send(&IpcMessage::Wire(Message::ViewerJoin));
        }
        let _ = conn.send(&IpcMessage::Viewers { device_ids: list });
        tracing::info!(viewers = joined, "session helper attached");
        token
    }

    /// The helper connection ended.
    pub fn detach_helper(&self, token: u64) {
        let mut st = lock(&self.state);
        if matches!(&st.helper, Some((t, _)) if *t == token) {
            st.helper = None;
            tracing::info!("session helper detached");
        }
    }

    #[must_use]
    pub fn helper_connected(&self) -> bool {
        lock(&self.state).helper.is_some()
    }

    /// Handles one message coming from the helper.
    pub fn on_helper_message(&self, msg: IpcMessage) {
        match msg {
            IpcMessage::Wire(wire @ Message::VideoConfig { .. }) => {
                let mut st = lock(&self.state);
                st.last_config = Some(wire.clone());
                st.broadcast(&wire);
            }
            IpcMessage::Wire(wire @ Message::VideoFrame { .. }) => {
                let (dropped, helper) = {
                    let st = lock(&self.state);
                    (
                        st.broadcast(&wire),
                        st.helper.as_ref().map(|(_, c)| c.clone()),
                    )
                };
                if dropped {
                    // A viewer lost frames: the next picture must be self-contained.
                    if let Some(conn) = helper {
                        let _ = conn.send(&IpcMessage::Wire(Message::RequestKeyframe));
                    }
                }
            }
            IpcMessage::DesktopLocked { locked } => {
                self.update_availability(|st| st.helper_locked = locked);
            }
            IpcMessage::NoDisplay => self.update_availability(|st| st.no_display = true),
            IpcMessage::PairingCodeRequest => self.answer_pairing_request(),
            IpcMessage::HelperHello { session_id, pid } => {
                tracing::info!(session_id, pid, "helper hello");
            }
            other => tracing::debug!(?other, "ignored helper message"),
        }
    }

    /// Lock state reported by the OS session notifications (`WTS_SESSION_LOCK`/`UNLOCK`).
    pub fn set_session_locked(&self, locked: bool) {
        self.update_availability(|st| st.session_locked = locked);
    }

    fn update_availability(&self, change: impl FnOnce(&mut State)) {
        let mut st = lock(&self.state);
        let before = st.available();
        change(&mut st);
        if st.available() != before {
            let status = st.status();
            tracing::info!(available = st.available(), "desktop availability changed");
            st.broadcast(&status);
        }
    }

    fn answer_pairing_request(&self) {
        let reply = match self.with_pairing(PairingManager::create_pairing_code) {
            Ok(code) => IpcMessage::PairingCode {
                code,
                ttl_secs: u32::try_from(PAIRING_CODE_TTL.as_secs()).unwrap_or(300),
            },
            Err(e) => {
                tracing::error!(error = %e, "cannot create pairing code");
                IpcMessage::PairingCode {
                    code: String::new(),
                    ttl_secs: 0,
                }
            }
        };
        let helper = lock(&self.state).helper.as_ref().map(|(_, c)| c.clone());
        if let Some(conn) = helper {
            let _ = conn.send(&reply);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desk_crypto::PairingManager;
    use desk_error::DeskResult;

    fn hub() -> DeskResult<Hub> {
        Ok(Hub::new(DeviceIdentity::generate()?, PairingManager::new()))
    }

    fn frame(key: bool) -> IpcMessage {
        IpcMessage::Wire(Message::VideoFrame {
            ts_us: 1,
            keyframe: key,
            data: vec![1, 2, 3],
        })
    }

    #[test]
    fn frames_reach_only_joined_viewers() -> DeskResult<()> {
        let hub = hub()?;
        let (a, rx_a) = hub.register_viewer("A");
        let (_b, rx_b) = hub.register_viewer("B");
        hub.viewer_join(a);
        // join queues the availability status first
        assert!(matches!(
            rx_a.try_recv(),
            Ok(Message::Status { online: true, .. })
        ));
        hub.on_helper_message(frame(true));
        assert!(matches!(rx_a.try_recv(), Ok(Message::VideoFrame { .. })));
        assert!(rx_b.try_recv().is_err());
        assert_eq!(hub.joined_count(), 1);
        Ok(())
    }

    #[test]
    fn late_viewer_gets_the_cached_config_first() -> DeskResult<()> {
        let hub = hub()?;
        hub.on_helper_message(IpcMessage::Wire(Message::VideoConfig {
            codec: "h264".to_string(),
            width: 1920,
            height: 1080,
            fps: 30,
        }));
        let (id, rx) = hub.register_viewer("late");
        hub.viewer_join(id);
        assert!(matches!(
            rx.try_recv(),
            Ok(Message::VideoConfig { width: 1920, .. })
        ));
        Ok(())
    }

    #[test]
    fn lock_is_reported_to_viewers_and_cleared_on_unlock() -> DeskResult<()> {
        let hub = hub()?;
        let (id, rx) = hub.register_viewer("A");
        hub.viewer_join(id);
        let _ = rx.try_recv();
        hub.set_session_locked(true);
        assert!(matches!(
            rx.try_recv(),
            Ok(Message::Status { online: false, .. })
        ));
        // The helper-side detection (secure desktop) is a second, independent source.
        hub.on_helper_message(IpcMessage::DesktopLocked { locked: true });
        assert!(
            rx.try_recv().is_err(),
            "no duplicate status while still unavailable"
        );
        hub.set_session_locked(false);
        assert!(
            rx.try_recv().is_err(),
            "still locked according to the helper"
        );
        hub.on_helper_message(IpcMessage::DesktopLocked { locked: false });
        assert!(matches!(
            rx.try_recv(),
            Ok(Message::Status { online: true, .. })
        ));
        Ok(())
    }

    #[test]
    fn input_from_an_unjoined_viewer_goes_nowhere_and_leave_is_idempotent() -> DeskResult<()> {
        let hub = hub()?;
        let (id, _rx) = hub.register_viewer("A");
        hub.forward_from_viewer(id, Message::RequestKeyframe);
        hub.viewer_leave(id);
        hub.viewer_join(id);
        hub.viewer_join(id);
        assert_eq!(hub.joined_count(), 1);
        hub.remove_viewer(id);
        assert_eq!(hub.joined_count(), 0);
        Ok(())
    }

    #[test]
    fn pairing_request_without_a_helper_still_creates_a_code() -> DeskResult<()> {
        let hub = hub()?;
        hub.on_helper_message(IpcMessage::PairingCodeRequest);
        assert!(hub.with_pairing(|pm| pm.active_code().is_some()));
        Ok(())
    }
}
