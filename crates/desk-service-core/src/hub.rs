//! Routing core between authenticated viewers (network) and the session helper (IPC).
//!
//! Frames flow helper -> hub -> every joined viewer; input and viewer lifecycle flow
//! viewer -> hub -> helper. The hub owns no sockets and no processes, which keeps the
//! routing rules unit-testable.

use crate::control::Control;
use crate::transfer::TransferState;
use desk_crypto::{DeviceIdentity, PairingManager, PAIRING_CODE_TTL};
use desk_ipc::{IpcConnection, IpcMessage};
use desk_protocol::{Message, NoticeKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Frames queued per viewer before the slowest viewer starts dropping (and asks for an IDR).
const VIEWER_QUEUE: usize = 90;

/// Identifies one authenticated viewer connection inside the hub.
pub type ViewerId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    View,
    Control,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerConsent {
    Pending,
    Granted(SessionMode),
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsentRequest {
    pub request_id: String,
    pub peer_name: String,
    pub fingerprint: String,
    pub known: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveSessionInfo {
    pub peer_name: String,
    pub fingerprint: String,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentEvent {
    Request(ConsentRequest),
    Session(ActiveSessionInfo),
    Ended { reason: String },
}

pub type ConsentListener = Arc<dyn Fn(ConsentEvent) + Send + Sync + 'static>;

struct Viewer {
    device_id: String,
    tx: SyncSender<Message>,
    joined: bool,
    wants_join: bool,
    consent: ViewerConsent,
    peer_name: String,
    fingerprint: String,
    request_id: Option<String>,
}

#[derive(Default)]
struct State {
    viewers: HashMap<ViewerId, Viewer>,
    helper: Option<(u64, IpcConnection)>,
    last_config: Option<Message>,
    session_locked: bool,
    helper_locked: bool,
    no_display: bool,
    pending_request: Option<ConsentRequest>,
    active_session: Option<ActiveSessionInfo>,
    next_req_id: u64,
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
            .filter(|v| v.joined && matches!(v.consent, ViewerConsent::Granted(_)))
            .map(|v| v.device_id.clone())
            .collect();
        ids.sort();
        ids
    }

    fn broadcast(&self, msg: &Message) -> bool {
        let mut dropped = false;
        for viewer in self
            .viewers
            .values()
            .filter(|v| v.joined && matches!(v.consent, ViewerConsent::Granted(_)))
        {
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
    control: Arc<Control>,
    next_id: AtomicU64,
    consent_gate_enabled: bool,
    consent_listener: Mutex<Option<ConsentListener>>,
    transfers: TransferState,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Hub {
    #[must_use]
    pub fn new(identity: DeviceIdentity, pairing: PairingManager) -> Self {
        Self::new_with_options(identity, pairing, false)
    }

    #[must_use]
    pub fn new_with_options(
        identity: DeviceIdentity,
        pairing: PairingManager,
        consent_gate: bool,
    ) -> Self {
        Self {
            state: Mutex::new(State::default()),
            pairing: Mutex::new(pairing),
            identity,
            control: Arc::new(Control::default()),
            next_id: AtomicU64::new(1),
            consent_gate_enabled: consent_gate,
            consent_listener: Mutex::new(None),
            transfers: TransferState::default(),
        }
    }

    /// Whether the consent gate is enabled for this hub.
    #[must_use]
    pub fn consent_gate_enabled(&self) -> bool {
        self.consent_gate_enabled
    }

    /// Subscribes a listener to consent lifecycle events.
    pub fn set_consent_listener(&self, listener: Option<ConsentListener>) {
        *lock(&self.consent_listener) = listener;
    }

    fn emit_consent_event(&self, event: ConsentEvent) {
        let cb = lock(&self.consent_listener).clone();
        if let Some(cb) = cb {
            cb(event);
        }
    }

    /// File transfer bookkeeping (limit, received folder, offer listener, answers).
    #[must_use]
    pub fn transfers(&self) -> &TransferState {
        &self.transfers
    }

    #[must_use]
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// Tray-menu controls (watch-only switch, port, tailnet report).
    #[must_use]
    pub fn control(&self) -> &Arc<Control> {
        &self.control
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
        self.register_viewer_full(device_id, device_id, None, None)
    }

    /// Registers an authenticated viewer with peer address info.
    pub fn register_viewer_with_peer(
        &self,
        device_id: &str,
        peer_addr: &str,
    ) -> (ViewerId, Receiver<Message>) {
        self.register_viewer_full(device_id, peer_addr, None, None)
    }

    /// Registers an authenticated viewer with peer address, claimed peer name, and key fingerprint.
    pub fn register_viewer_full(
        &self,
        device_id: &str,
        _peer_addr: &str,
        peer_name: Option<&str>,
        peer_fingerprint: Option<&str>,
    ) -> (ViewerId, Receiver<Message>) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = sync_channel(VIEWER_QUEUE);

        let (peer_name, fingerprint, is_known, always_mode) = {
            let pairing = lock(&self.pairing);
            let trust = pairing.trust();
            let known = trust.is_device_trusted(device_id);
            let always_mode = if trust.is_device_always_allowed(device_id) {
                Some(
                    trust
                        .device_always_mode(device_id)
                        // A missing mode never widens the grant.
                        .unwrap_or_else(|| "view".to_string()),
                )
            } else {
                None
            };
            let fp = peer_fingerprint
                .map(str::to_string)
                .or_else(|| trust.fingerprint_for_device(device_id))
                .unwrap_or_else(|| {
                    let clean = device_id.strip_prefix("desk-").unwrap_or(device_id);
                    clean.to_string()
                });
            let name = peer_name.map(str::to_string).unwrap_or_else(|| {
                if known {
                    format!("Peer-{}", device_id.chars().take(8).collect::<String>())
                } else {
                    format!("Guest-{}", device_id.chars().take(8).collect::<String>())
                }
            });
            (name, fp, known, always_mode)
        };

        let mut st = lock(&self.state);
        let (consent, req_id, event_to_emit) = if !self.consent_gate_enabled {
            let active = ActiveSessionInfo {
                peer_name: peer_name.clone(),
                fingerprint: fingerprint.clone(),
                mode: "control".to_string(),
            };
            st.active_session = Some(active.clone());
            (
                ViewerConsent::Granted(SessionMode::Control),
                None,
                Some(ConsentEvent::Session(active)),
            )
        } else if let Some(ref mode_str) = always_mode {
            // Only an explicit "control" grants control; anything else is view.
            let (mode, mode_name) = if mode_str == "control" {
                (SessionMode::Control, "control")
            } else {
                (SessionMode::View, "view")
            };
            let active = ActiveSessionInfo {
                peer_name: peer_name.clone(),
                fingerprint: fingerprint.clone(),
                mode: mode_name.to_string(),
            };
            st.active_session = Some(active.clone());
            (
                ViewerConsent::Granted(mode),
                None,
                Some(ConsentEvent::Session(active)),
            )
        } else {
            st.next_req_id += 1;
            let req_id_str = format!("req-{}", st.next_req_id);
            let req = ConsentRequest {
                request_id: req_id_str.clone(),
                peer_name: peer_name.clone(),
                fingerprint: fingerprint.clone(),
                known: is_known,
            };
            st.pending_request = Some(req.clone());
            (
                ViewerConsent::Pending,
                Some(req_id_str),
                Some(ConsentEvent::Request(req)),
            )
        };

        st.viewers.insert(
            id,
            Viewer {
                device_id: device_id.to_string(),
                tx,
                joined: false,
                wants_join: false,
                consent,
                peer_name,
                fingerprint,
                request_id: req_id,
            },
        );
        drop(st);

        if let Some(ev) = event_to_emit {
            self.emit_consent_event(ev);
        }

        (id, rx)
    }

    /// Spawns a background timeout check for a consent request (30 seconds).
    pub fn spawn_consent_timeout(self: &Arc<Self>, request_id: String, viewer_id: ViewerId) {
        let hub = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("desk-consent-timeout".to_string())
            .spawn(move || {
                std::thread::sleep(Duration::from_secs(30));
                hub.handle_timeout(&request_id, viewer_id);
            });
    }

    /// If `request_id` is still pending, rejects the session due to timeout.
    pub fn handle_timeout(&self, request_id: &str, viewer_id: ViewerId) -> bool {
        let mut drop_it = false;
        {
            let mut st = lock(&self.state);
            if let Some(req) = &st.pending_request {
                if req.request_id == request_id {
                    st.pending_request = None;
                    drop_it = true;
                    if let Some(v) = st.viewers.get_mut(&viewer_id) {
                        v.consent = ViewerConsent::Rejected;
                        let _ = v.tx.try_send(Message::Status {
                            online: false,
                            latency_ms: 0,
                        });
                    }
                }
            }
        }
        if drop_it {
            self.remove_viewer(viewer_id);
            self.emit_consent_event(ConsentEvent::Ended {
                reason: "timeout".to_string(),
            });
            true
        } else {
            false
        }
    }

    /// The viewer asked to watch: capture starts if it is the first one and consent is granted.
    pub fn viewer_join(&self, id: ViewerId) {
        let (helper, list, should_start) = {
            let mut st = lock(&self.state);
            let status = st.status();
            let config = st.last_config.clone();
            let Some(viewer) = st.viewers.get_mut(&id) else {
                return;
            };
            viewer.wants_join = true;
            if !matches!(viewer.consent, ViewerConsent::Granted(_)) {
                // Pending consent or rejected: DO NOT forward join or stream frames.
                return;
            }
            if viewer.joined {
                return;
            }
            viewer.joined = true;
            if let Some(cfg) = config {
                let _ = viewer.tx.try_send(cfg);
            }
            let _ = viewer.tx.try_send(status);
            if self.control.view_only()
                || matches!(viewer.consent, ViewerConsent::Granted(SessionMode::View))
            {
                let _ = viewer.tx.try_send(Message::Notice {
                    kind: NoticeKind::ViewOnlyOn,
                });
            }
            (
                st.helper.as_ref().map(|(_, c)| c.clone()),
                st.joined_ids(),
                true,
            )
        };
        if should_start {
            tracing::info!(viewer = id, "viewer joined");
            if let Some(conn) = helper {
                let _ = conn.send(&IpcMessage::Wire(Message::ViewerJoin));
                let _ = conn.send(&IpcMessage::Viewers { device_ids: list });
            }
            self.push_agent_info();
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
        self.push_agent_info();
    }

    /// Drops the viewer entirely (connection closed).
    pub fn remove_viewer(&self, id: ViewerId) {
        self.viewer_leave(id);
        let mut event_to_emit = None;
        {
            let mut st = lock(&self.state);
            if let Some(v) = st.viewers.remove(&id) {
                if let Some(req) = &st.pending_request {
                    if v.request_id.as_deref() == Some(&req.request_id) {
                        st.pending_request = None;
                        event_to_emit = Some(ConsentEvent::Ended {
                            reason: "peer".to_string(),
                        });
                    }
                }
                if let Some(sess) = &st.active_session {
                    if sess.peer_name == v.peer_name || sess.fingerprint == v.fingerprint {
                        st.active_session = None;
                        event_to_emit = Some(ConsentEvent::Ended {
                            reason: "peer".to_string(),
                        });
                    }
                }
            }
        }
        if let Some(ev) = event_to_emit {
            self.emit_consent_event(ev);
        }
    }

    /// Answers a pending incoming connection consent request.
    pub fn answer_consent(
        &self,
        request_id: &str,
        decision: &str,
        always_allow: bool,
    ) -> Result<(), String> {
        let (event_to_emit, viewer_to_join, drop_viewer_id) = {
            let mut st = lock(&self.state);
            let Some(req) = st
                .pending_request
                .as_ref()
                .filter(|r| r.request_id == request_id)
            else {
                return Ok(());
            };
            let req = req.clone();
            st.pending_request = None;

            let viewer_id = st.viewers.iter().find_map(|(&id, v)| {
                if v.request_id.as_deref() == Some(request_id) {
                    Some(id)
                } else {
                    None
                }
            });

            match decision {
                "view" | "control" => {
                    let mode = if decision == "view" {
                        SessionMode::View
                    } else {
                        SessionMode::Control
                    };
                    let mut to_join = None;
                    let dev_id = if let Some(vid) = viewer_id {
                        if let Some(v) = st.viewers.get_mut(&vid) {
                            v.consent = ViewerConsent::Granted(mode);
                            if v.wants_join {
                                to_join = Some(vid);
                            }
                            Some(v.device_id.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if always_allow {
                        if let Some(did) = dev_id {
                            let _ = self.with_pairing(|p| {
                                p.trust_mut().set_device_always_allowed_with_mode(
                                    &did,
                                    true,
                                    Some(decision),
                                )
                            });
                        }
                    }

                    let sess = ActiveSessionInfo {
                        peer_name: req.peer_name,
                        fingerprint: req.fingerprint,
                        mode: decision.to_string(),
                    };
                    st.active_session = Some(sess.clone());
                    (Some(ConsentEvent::Session(sess)), to_join, None)
                }
                "reject" => {
                    let mut drop_vid = None;
                    if let Some(vid) = viewer_id {
                        if let Some(v) = st.viewers.get_mut(&vid) {
                            v.consent = ViewerConsent::Rejected;
                            let _ = v.tx.try_send(Message::Status {
                                online: false,
                                latency_ms: 0,
                            });
                        }
                        drop_vid = Some(vid);
                    }
                    (
                        Some(ConsentEvent::Ended {
                            reason: "rejected".to_string(),
                        }),
                        None,
                        drop_vid,
                    )
                }
                _ => return Err(format!("invalid decision: {decision}")),
            }
        };

        if let Some(vid) = drop_viewer_id {
            self.remove_viewer(vid);
        }
        if let Some(vid) = viewer_to_join {
            self.viewer_join(vid);
        }
        if let Some(ev) = event_to_emit {
            self.emit_consent_event(ev);
        }
        Ok(())
    }

    /// Disconnects the incoming connection session and notifies the user.
    pub fn disconnect_incoming(&self) {
        self.disconnect_all();
        {
            let mut st = lock(&self.state);
            st.pending_request = None;
            st.active_session = None;
        }
        self.emit_consent_event(ConsentEvent::Ended {
            reason: "host".to_string(),
        });
    }

    /// Changes the mode of the active incoming session ("view" or "control").
    pub fn set_incoming_mode(&self, mode: &str) {
        let is_view = mode != "control";
        self.set_view_only(is_view);
        let mut event_to_emit = None;
        {
            let mut st = lock(&self.state);
            let target_mode = if is_view {
                SessionMode::View
            } else {
                SessionMode::Control
            };
            for v in st.viewers.values_mut() {
                if matches!(v.consent, ViewerConsent::Granted(_)) {
                    v.consent = ViewerConsent::Granted(target_mode);
                }
            }
            if let Some(sess) = st.active_session.as_mut() {
                sess.mode = if is_view { "view" } else { "control" }.to_string();
                event_to_emit = Some(ConsentEvent::Session(sess.clone()));
            }
        }
        if let Some(ev) = event_to_emit {
            self.emit_consent_event(ev);
        }
    }

    /// Returns the currently pending consent request, if any.
    #[must_use]
    pub fn pending_request(&self) -> Option<ConsentRequest> {
        lock(&self.state).pending_request.clone()
    }

    /// Returns the active incoming session, if any.
    #[must_use]
    pub fn active_session(&self) -> Option<ActiveSessionInfo> {
        lock(&self.state).active_session.clone()
    }

    /// Forwards viewer input / keyframe requests to the helper. Ignored unless the viewer
    /// has joined and consent allows input, so a merely paired panel or view-only viewer cannot drive the PC.
    pub fn forward_from_viewer(&self, id: ViewerId, msg: Message) {
        let (helper, allow_input) = {
            let st = lock(&self.state);
            match st.viewers.get(&id) {
                Some(v) if v.joined => {
                    let input_allowed = match v.consent {
                        ViewerConsent::Granted(SessionMode::Control) => !self.control.view_only(),
                        _ => false,
                    };
                    (st.helper.as_ref().map(|(_, c)| c.clone()), input_allowed)
                }
                _ => (None, false),
            }
        };
        if matches!(msg, Message::InputEvent(_)) && !allow_input {
            return;
        }
        if let Some(conn) = helper {
            let _ = conn.send(&IpcMessage::Wire(msg));
        }
    }

    /// Tells every viewer the PC user ended the session and drops them all. Trust is kept,
    /// so a viewer can reconnect without a new pairing code. Returns how many were dropped.
    pub fn disconnect_all(&self) -> usize {
        let ids: Vec<ViewerId> = {
            let st = lock(&self.state);
            let notice = Message::Notice {
                kind: NoticeKind::DisconnectedByHost,
            };
            for viewer in st.viewers.values() {
                let _ = viewer.tx.try_send(notice.clone());
            }
            st.viewers.keys().copied().collect()
        };
        for id in &ids {
            // Leaving first keeps the helper's capture count right; removing the entry drops
            // the sender, which ends the viewer's bridge loop once it sent the notice.
            self.viewer_leave(*id);
            lock(&self.state).viewers.remove(id);
        }
        tracing::info!(
            dropped = ids.len(),
            "all viewers disconnected by the PC user"
        );
        ids.len()
    }

    /// Blocks (or allows) remote input and tells the connected viewers.
    pub fn set_view_only(&self, enabled: bool) {
        if !self.control.set_view_only(enabled) {
            return;
        }
        let kind = if enabled {
            NoticeKind::ViewOnlyOn
        } else {
            NoticeKind::ViewOnlyOff
        };
        for viewer in lock(&self.state).viewers.values() {
            let _ = viewer.tx.try_send(Message::Notice { kind });
        }
        tracing::info!(enabled, "watch-only mode changed");
        self.push_agent_info();
    }

    /// Sends the tray menu snapshot to the attached helper (if any).
    pub fn push_agent_info(&self) {
        let (helper, viewers, unavailable) = {
            let st = lock(&self.state);
            (
                st.helper.as_ref().map(|(_, c)| c.clone()),
                st.joined_ids().len(),
                !st.available(),
            )
        };
        if let Some(conn) = helper {
            let info = self.control.snapshot(&self.identity, viewers, unavailable);
            let _ = conn.send(&IpcMessage::AgentInfo(info));
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
        self.push_agent_info();
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

    /// Sends `msg` to the helper; `false` when none is connected or the pipe is broken.
    pub(crate) fn send_to_helper(&self, msg: &IpcMessage) -> bool {
        let helper = lock(&self.state).helper.as_ref().map(|(_, c)| c.clone());
        helper.is_some_and(|conn| conn.send(msg).is_ok())
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
            IpcMessage::Wire(wire @ (Message::CursorShape { .. } | Message::CursorPos { .. })) => {
                // Lossy on purpose: a slow viewer skips cursor updates instead of asking for a
                // keyframe; the panel falls back to an arrow for a shape it never received.
                lock(&self.state).broadcast(&wire);
            }
            IpcMessage::Wire(wire) if wire.transfer_id().is_some() => {
                self.transfers.route_from_helper(wire);
            }
            IpcMessage::DesktopLocked { locked } => {
                self.update_availability(|st| st.helper_locked = locked);
            }
            IpcMessage::NoDisplay => self.update_availability(|st| st.no_display = true),
            IpcMessage::PairingCodeRequest => self.answer_pairing_request(),
            IpcMessage::AgentInfoRequest => self.push_agent_info(),
            IpcMessage::DisconnectAllRequest => {
                self.disconnect_all();
            }
            IpcMessage::SetViewOnly { enabled } => self.set_view_only(enabled),
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
            drop(st);
            self.push_agent_info();
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    fn input() -> Message {
        Message::InputEvent(desk_protocol::InputPayload::Key {
            scancode: 30,
            down: true,
        })
    }

    /// Hub with a joined viewer and a connected helper pipe; returns the helper's end.
    fn hub_with_helper() -> DeskResult<(Hub, ViewerId, Receiver<Message>, IpcConnection)> {
        use desk_ipc::{pipe_access_sddl, IpcClient, IpcServer};
        use std::time::Duration;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            r"\\.\pipe\DeskRemote-hub-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let server = IpcServer::bind(&name, &pipe_access_sddl(&desk_ipc::current_user_sid()?))?;
        let client = std::thread::spawn({
            let name = name.clone();
            move || IpcClient::connect(&name, Duration::from_secs(5))
        });
        let service_end = server
            .accept(Duration::from_secs(5))?
            .ok_or_else(|| desk_error::DeskError::Ipc("no client".to_string()))?;
        let helper_end = client
            .join()
            .map_err(|_| desk_error::DeskError::Ipc("client thread".to_string()))??;
        let hub = hub()?;
        hub.attach_helper(service_end);
        let (id, rx) = hub.register_viewer("A");
        hub.viewer_join(id);
        Ok((hub, id, rx, helper_end))
    }

    fn drain(conn: &IpcConnection) -> Vec<IpcMessage> {
        let mut out = Vec::new();
        while let Ok(Some(m)) = conn.recv_timeout(std::time::Duration::from_millis(150)) {
            out.push(m);
        }
        out
    }

    #[test]
    fn view_only_drops_input_but_keeps_keyframe_requests_and_resets_per_hub() -> DeskResult<()> {
        let (hub, id, rx, helper) = hub_with_helper()?;
        let _ = drain(&helper);
        hub.forward_from_viewer(id, input());
        assert!(drain(&helper)
            .iter()
            .any(|m| matches!(m, IpcMessage::Wire(Message::InputEvent(_)))));

        hub.on_helper_message(IpcMessage::SetViewOnly { enabled: true });
        let seen = drain(&helper);
        assert!(seen
            .iter()
            .any(|m| matches!(m, IpcMessage::AgentInfo(i) if i.view_only)));
        hub.forward_from_viewer(id, input());
        hub.forward_from_viewer(id, Message::RequestKeyframe);
        let seen = drain(&helper);
        assert!(!seen
            .iter()
            .any(|m| matches!(m, IpcMessage::Wire(Message::InputEvent(_)))));
        assert!(seen
            .iter()
            .any(|m| matches!(m, IpcMessage::Wire(Message::RequestKeyframe))));
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|m| match m {
                Message::Notice { kind } => Some(kind),
                _ => None,
            })
            .collect();
        assert_eq!(notices, vec![NoticeKind::ViewOnlyOn]);

        hub.on_helper_message(IpcMessage::SetViewOnly { enabled: false });
        let _ = drain(&helper);
        hub.forward_from_viewer(id, input());
        assert!(drain(&helper)
            .iter()
            .any(|m| matches!(m, IpcMessage::Wire(Message::InputEvent(_)))));

        // A restarted service starts with the switch off.
        assert!(!self::hub()?.control().view_only());
        Ok(())
    }

    #[test]
    fn a_late_viewer_is_told_that_view_only_is_on() -> DeskResult<()> {
        let hub = hub()?;
        hub.set_view_only(true);
        let (id, rx) = hub.register_viewer("late");
        hub.viewer_join(id);
        let kinds: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(kinds.contains(&Message::Notice {
            kind: NoticeKind::ViewOnlyOn
        }));
        Ok(())
    }

    #[test]
    fn disconnect_all_notifies_closes_every_viewer_and_tells_the_helper() -> DeskResult<()> {
        let (hub, _id, rx_a, helper) = hub_with_helper()?;
        let (b, rx_b) = hub.register_viewer("B"); // paired but not watching
        let _ = b;
        let _ = drain(&helper);

        assert_eq!(hub.disconnect_all(), 2);
        assert_eq!(hub.joined_count(), 0);
        for rx in [&rx_a, &rx_b] {
            let mut got = Vec::new();
            let closed = loop {
                match rx.try_recv() {
                    Ok(m) => got.push(m),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break false,
                }
            };
            assert!(got.contains(&Message::Notice {
                kind: NoticeKind::DisconnectedByHost
            }));
            assert!(closed, "the sender must be dropped so the bridge ends");
        }
        let seen = drain(&helper);
        assert!(seen
            .iter()
            .any(|m| matches!(m, IpcMessage::Wire(Message::ViewerLeave))));
        // The bridge's own cleanup afterwards is a harmless no-op.
        hub.remove_viewer(b);
        Ok(())
    }

    #[test]
    fn pairing_request_without_a_helper_still_creates_a_code() -> DeskResult<()> {
        let hub = hub()?;
        hub.on_helper_message(IpcMessage::PairingCodeRequest);
        assert!(hub.with_pairing(|pm| pm.active_code().is_some()));
        Ok(())
    }

    #[test]
    fn consent_gate_blocks_frames_and_input_until_approved() -> DeskResult<()> {
        use desk_crypto::TrustStore;
        let mut trust = TrustStore::in_memory();
        let peer_identity = DeviceIdentity::generate()?;
        trust.add(peer_identity.public_key())?;

        let hub = Hub::new_with_options(
            DeviceIdentity::generate()?,
            PairingManager::with_trust(trust),
            true, // consent gate enabled
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let ev_clone = Arc::clone(&events);
        hub.set_consent_listener(Some(Arc::new(move |ev| {
            lock(&ev_clone).push(ev);
        })));

        let dev_id = peer_identity.device_id();
        let (id, rx) = hub.register_viewer(&dev_id);

        // 1. Pending consent request emitted
        let req = hub.pending_request().expect("must have pending request");
        assert_eq!(req.request_id, "req-1");
        assert!(req.known, "peer was added to trust store, must be known");

        // 2. Viewer requests to join before consent: NO frames or join dispatched
        hub.viewer_join(id);
        assert_eq!(hub.joined_count(), 0, "must not join while pending");

        // Helper sends video frame: MUST NOT reach viewer
        hub.on_helper_message(frame(true));
        assert!(
            rx.try_recv().is_err(),
            "no frame can leak to unapproved viewer"
        );

        // Viewer sends input: MUST NOT be forwarded
        hub.forward_from_viewer(id, input());

        // 3. User approves with "view" mode
        hub.answer_consent("req-1", "view", false)
            .expect("approval ok");
        assert_eq!(hub.joined_count(), 1, "viewer should be joined now");
        assert!(hub.pending_request().is_none());
        assert_eq!(
            hub.active_session().map(|s| s.mode),
            Some("view".to_string())
        );

        // Frames now reach the viewer
        hub.on_helper_message(frame(true));
        let got_frame = std::iter::from_fn(|| rx.try_recv().ok())
            .any(|m| matches!(m, Message::VideoFrame { .. }));
        assert!(got_frame, "frame must reach approved viewer");

        // But in view mode, input is dropped
        hub.forward_from_viewer(id, input());

        // 4. Switch mode to "control"
        hub.set_incoming_mode("control");
        assert_eq!(
            hub.active_session().map(|s| s.mode),
            Some("control".to_string())
        );

        // Disconnect
        hub.disconnect_incoming();
        assert_eq!(hub.joined_count(), 0);
        assert!(hub.active_session().is_none());

        Ok(())
    }

    #[test]
    fn consent_gate_reject_sends_status_and_disconnects() -> DeskResult<()> {
        let hub = Hub::new_with_options(
            DeviceIdentity::generate()?,
            PairingManager::new(),
            true, // consent gate enabled
        );
        let (_id, rx) = hub.register_viewer("stranger");
        assert!(hub.pending_request().is_some());

        hub.answer_consent("req-1", "reject", false)
            .expect("reject ok");
        assert!(hub.pending_request().is_none());
        assert!(hub.active_session().is_none());

        // Stranger receives offline status and sender drops
        let msg = rx.try_recv().expect("should receive status");
        assert_eq!(
            msg,
            Message::Status {
                online: false,
                latency_ms: 0
            }
        );
        assert_eq!(hub.joined_count(), 0);
        Ok(())
    }

    #[test]
    fn consent_gate_timeout_rejects_session() -> DeskResult<()> {
        let hub = Hub::new_with_options(
            DeviceIdentity::generate()?,
            PairingManager::new(),
            true, // consent gate enabled
        );
        let (id, rx) = hub.register_viewer("stranger");
        assert!(hub.pending_request().is_some());

        let expired = hub.handle_timeout("req-1", id);
        assert!(expired, "timeout should trigger rejection");
        assert!(hub.pending_request().is_none());

        let msg = rx.try_recv().expect("should receive status");
        assert_eq!(
            msg,
            Message::Status {
                online: false,
                latency_ms: 0
            }
        );
        Ok(())
    }

    #[test]
    fn consent_gate_always_allow_bypasses_prompt() -> DeskResult<()> {
        use desk_crypto::TrustStore;
        let mut trust = TrustStore::in_memory();
        let peer_identity = DeviceIdentity::generate()?;
        trust.add(peer_identity.public_key())?;
        trust.set_device_always_allowed(&peer_identity.device_id(), true)?;

        let hub = Hub::new_with_options(
            DeviceIdentity::generate()?,
            PairingManager::with_trust(trust),
            true, // consent gate enabled
        );

        let (_id, _rx) = hub.register_viewer(&peer_identity.device_id());
        assert!(
            hub.pending_request().is_none(),
            "always_allow peers must not enter pending"
        );
        assert_eq!(
            hub.active_session().map(|s| s.mode),
            Some("control".to_string())
        );
        Ok(())
    }

    #[test]
    fn consent_gate_always_allow_preserves_view_mode() -> DeskResult<()> {
        use desk_crypto::TrustStore;
        let mut trust = TrustStore::in_memory();
        let peer_identity = DeviceIdentity::generate()?;
        trust.add(peer_identity.public_key())?;
        trust.set_device_always_allowed_with_mode(
            &peer_identity.device_id(),
            true,
            Some("view"),
        )?;

        let hub = Hub::new_with_options(
            DeviceIdentity::generate()?,
            PairingManager::with_trust(trust),
            true, // consent gate enabled
        );

        let (id, _rx) = hub.register_viewer(&peer_identity.device_id());
        assert!(
            hub.pending_request().is_none(),
            "always_allow peers must not enter pending"
        );
        assert_eq!(
            hub.active_session().map(|s| s.mode),
            Some("view".to_string())
        );

        hub.viewer_join(id);

        // Verify that input is dropped in view mode
        use desk_ipc::{pipe_access_sddl, IpcClient, IpcServer};
        static NEXT_TEST: AtomicU64 = AtomicU64::new(500);
        let pipe_name = format!(
            r"\\.\pipe\DeskRemote-hub-test-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::SeqCst)
        );
        let server = IpcServer::bind(
            &pipe_name,
            &pipe_access_sddl(&desk_ipc::current_user_sid()?),
        )?;
        let client = std::thread::spawn({
            let name = pipe_name.clone();
            move || IpcClient::connect(&name, Duration::from_secs(5))
        });
        let service_end = server
            .accept(Duration::from_secs(5))?
            .ok_or_else(|| desk_error::DeskError::Ipc("no client".to_string()))?;
        let helper_end = client
            .join()
            .map_err(|_| desk_error::DeskError::Ipc("client thread".to_string()))??;

        hub.attach_helper(service_end);

        // Drain any handshake messages like ViewersChanged
        while let Ok(Some(_)) = helper_end.recv_timeout(Duration::from_millis(50)) {}

        hub.forward_from_viewer(
            id,
            Message::InputEvent(desk_protocol::InputPayload::Key {
                scancode: 42,
                down: true,
            }),
        );

        let received = helper_end.recv_timeout(Duration::from_millis(50))?;
        assert!(
            received.is_none()
                || !matches!(
                    received,
                    Some(desk_ipc::IpcMessage::Wire(Message::InputEvent(_)))
                ),
            "input must be dropped when always_allow granted view mode"
        );
        Ok(())
    }

    #[test]
    fn agent_service_mode_has_consent_gate_disabled() -> DeskResult<()> {
        let hub = Hub::new(DeviceIdentity::generate()?, PairingManager::new());
        assert!(!hub.consent_gate_enabled());

        let (id, _rx) = hub.register_viewer("farm_viewer");
        assert!(
            hub.pending_request().is_none(),
            "farm PC agent gate is always off"
        );
        hub.viewer_join(id);
        assert_eq!(hub.joined_count(), 1);
        Ok(())
    }
}
