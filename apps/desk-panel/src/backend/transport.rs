//! Real backend over `desk-transport`: pairing, discovery, status probes and viewer sessions.

use super::h264::avc1_codec;
use super::input_map::InputTranslator;
use super::{
    Backend, Candidate, EventSink, InputEvent, PairError, PcInfo, PcStatus, SessionEvent,
    ViewerSession,
};
use desk_config::PairedPc;
use desk_crypto::{DeviceIdentity, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use desk_protocol::{Message, NoticeKind};
use desk_transport::{
    connect_timeout, is_this_pc, primary_local_ip, LanBrowser, PeerTarget, Session, SessionReader,
    SessionWriter, TailscaleDetector, DEFAULT_PORT,
};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the background refresher rescans the LAN/tailnet and re-probes paired PCs.
const REFRESH: Duration = Duration::from_secs(8);
const MDNS_SCAN: Duration = Duration::from_millis(1500);
const PROBE_CONNECT: Duration = Duration::from_millis(1500);
const SESSION_CONNECT: Duration = Duration::from_secs(4);
const PAIR_CONNECT: Duration = Duration::from_secs(4);
/// Silence longer than this on a live session means the link is gone (pings answer every second).
const SESSION_IDLE: Duration = Duration::from_secs(8);
const PING_EVERY: Duration = Duration::from_secs(1);
/// A probe slower than this is shown as a warning.
const SLOW_MS: u32 = 200;
/// Fallback when the host sends a codec name the browser cannot use and the SPS is missing.
const FALLBACK_CODEC: &str = "avc1.42E01F";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(0))
}

struct Probe {
    status: PcStatus,
    /// `warn.*` key explaining a [`PcStatus::Warning`].
    warn: Option<&'static str>,
    latency_ms: Option<u32>,
    seen: Option<Instant>,
}

#[derive(Clone)]
struct Neighbour {
    name: String,
    address: String,
    device_id: Option<String>,
    source: &'static str,
}

#[derive(Default)]
struct State {
    probes: HashMap<String, Probe>,
    neighbours: Vec<Neighbour>,
    paired: Vec<PairedPc>,
    /// Device -> token of every PC being watched; those are not probed meanwhile.
    watching: HashMap<String, u64>,
    next_token: u64,
}

struct Inner {
    identity: Option<DeviceIdentity>,
    trust: Mutex<Option<TrustStore>>,
    state: Mutex<State>,
    nudge: Mutex<Sender<()>>,
}

/// Backend for every real build.
pub struct TransportBackend {
    inner: Arc<Inner>,
}

impl TransportBackend {
    pub fn new() -> Self {
        // Tell the SYSTEM-side agent guard that this PC is a panel (best effort; the guard
        // also scans profiles for the panel identity when ProgramData is not writable).
        if let Err(e) = DeskPaths::default().mark_panel_pc() {
            tracing::debug!(error = %e, "panel role marker not written");
        }
        let identity = DeviceIdentity::load_or_create(&DeskPaths::default().panel_identity_path())
            .map_err(|e| tracing::error!(error = %e, "panel identity unavailable"))
            .ok();
        let trust = TrustStore::open_panel_canonical()
            .map_err(|e| tracing::error!(error = %e, "trust store unavailable"))
            .ok();
        let (tx, rx) = mpsc::channel();
        let inner = Arc::new(Inner {
            identity,
            trust: Mutex::new(trust),
            state: Mutex::new(State::default()),
            nudge: Mutex::new(tx),
        });
        let worker = Arc::clone(&inner);
        let spawned = std::thread::Builder::new()
            .name("pc-refresher".to_string())
            .spawn(move || loop {
                worker.refresh();
                let _ = rx.recv_timeout(REFRESH);
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start the PC refresher");
        }
        Self { inner }
    }
}

impl Inner {
    fn nudge(&self) {
        let _ = lock(&self.nudge).send(());
    }

    /// Rescans the LAN and tailnet, then probes every paired PC that is not being watched.
    fn refresh(&self) {
        let mut found = Vec::new();
        match LanBrowser::browse(MDNS_SCAN) {
            Ok(peers) => {
                let own_id = self.identity.as_ref().map(|i| i.device_id().to_string());
                let local = primary_local_ip();
                for peer in peers {
                    if is_this_pc(&peer.addresses, local, &peer.device_id, own_id.as_deref()) {
                        continue;
                    }
                    let ip = peer
                        .addresses
                        .iter()
                        .find(|a| a.is_ipv4() && !a.is_loopback())
                        .or_else(|| peer.addresses.first());
                    if let Some(ip) = ip {
                        found.push(Neighbour {
                            name: friendly_name(&peer.host_name),
                            address: format!("{ip}:{}", peer.port),
                            device_id: Some(peer.device_id),
                            source: "lan",
                        });
                    }
                }
            }
            Err(e) => tracing::debug!(error = %e, "mDNS scan failed"),
        }
        for peer in TailscaleDetector::snapshot().peers {
            let ip = peer
                .ips
                .iter()
                .find(|ip| ip.contains('.') && !ip.contains(':'));
            if let (true, Some(ip)) = (peer.online, ip) {
                found.push(Neighbour {
                    name: peer.host_name,
                    address: format!("{ip}:{DEFAULT_PORT}"),
                    device_id: None,
                    source: "tailscale",
                });
            }
        }
        let (paired, watching) = {
            let mut st = lock(&self.state);
            st.neighbours = found;
            (st.paired.clone(), st.watching.clone())
        };
        for pc in paired.iter().filter(|pc| !watching.contains_key(&pc.id)) {
            let result = self.probe(pc);
            self.record(&pc.id, result);
        }
    }

    fn record(&self, id: &str, result: Result<u32, PairError>) {
        let mut st = lock(&self.state);
        let reachable = matches!(result, Ok(_) | Err(PairError::Unpaired));
        let seen = if reachable {
            Some(Instant::now())
        } else {
            st.probes.get(id).and_then(|p| p.seen)
        };
        let (status, warn, latency_ms) = match result {
            Err(PairError::Unpaired) => (PcStatus::Warning, Some("unpaired"), None),
            Err(_) => (PcStatus::Offline, None, None),
            Ok(ms) if ms > SLOW_MS => (PcStatus::Warning, Some("latency"), Some(ms)),
            Ok(ms) => (PcStatus::Online, None, Some(ms)),
        };
        st.probes.insert(
            id.to_string(),
            Probe {
                status,
                warn,
                latency_ms,
                seen,
            },
        );
    }

    /// Opens a session, pings once and returns the round trip in milliseconds.
    fn probe(&self, pc: &PairedPc) -> Result<u32, PairError> {
        let mut session = self.open(pc, PROBE_CONNECT)?;
        let rtt = session.ping().map_err(|_| PairError::Network)?;
        u32::try_from(rtt.as_millis()).map_err(|_| PairError::Network)
    }

    /// Addresses to try for `pc`: the live mDNS entry first, then the one saved at pairing.
    fn addresses(&self, pc: &PairedPc) -> Vec<String> {
        let mut out: Vec<String> = lock(&self.state)
            .neighbours
            .iter()
            .filter(|n| n.device_id.as_deref() == Some(pc.id.as_str()))
            .map(|n| n.address.clone())
            .collect();
        if let Some(saved) = &pc.address {
            if !out.contains(saved) {
                out.push(saved.clone());
            }
        }
        out
    }

    /// Connects to a paired PC without a code; the host key must already be pinned.
    fn open(&self, pc: &PairedPc, connect_within: Duration) -> Result<Session, PairError> {
        let identity = self.identity.as_ref().ok_or(PairError::Network)?;
        let mut outcome = PairError::Network;
        for address in self.addresses(pc) {
            let Ok(target) = PeerTarget::parse(&address) else {
                continue;
            };
            let Ok(mut session) = connect_timeout(&target, connect_within) else {
                continue;
            };
            let mut guard = lock(&self.trust);
            let trust = guard.as_mut().ok_or(PairError::Unpaired)?;
            match session.authenticate_as_client(identity, trust, None) {
                Ok(()) if session.peer_device_id().as_deref() == Some(pc.id.as_str()) => {
                    return Ok(session);
                }
                Ok(()) => outcome = PairError::Unpaired,
                Err(e) => {
                    tracing::debug!(error = %e, %address, "connect refused");
                    outcome = classify(&e, false);
                }
            }
        }
        Err(outcome)
    }
}

/// `desk_ab12cd.local.` -> `desk-ab12cd`.
fn friendly_name(host: &str) -> String {
    host.trim_end_matches('.')
        .trim_end_matches(".local")
        .replace('_', "-")
}

/// Maps a transport failure to the code shown in the UI.
fn classify(error: &DeskError, pairing: bool) -> PairError {
    let text = error.to_string();
    match error {
        DeskError::Crypto(_) if text.contains("rejected by host") => PairError::Rejected,
        DeskError::Crypto(_) if text.contains("wrong code") || text.contains("invalid pairing") => {
            PairError::Invalid
        }
        DeskError::Crypto(_) => PairError::Unpaired,
        // The TCP link came up and then died inside the handshake: the host dropped us.
        DeskError::Io(e)
            if !pairing && !matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) =>
        {
            PairError::Unpaired
        }
        _ => PairError::Network,
    }
}

impl Backend for TransportBackend {
    fn list(&self, paired: &[PairedPc]) -> Vec<PcInfo> {
        let mut st = lock(&self.inner.state);
        let changed = st
            .paired
            .iter()
            .map(|p| &p.id)
            .ne(paired.iter().map(|p| &p.id));
        if changed {
            st.paired = paired.to_vec();
        }
        let rows = paired
            .iter()
            .map(|pc| {
                let probe = st.probes.get(&pc.id);
                let live = st.watching.contains_key(&pc.id);
                let status = match probe {
                    _ if live => PcStatus::Online,
                    Some(p) => p.status,
                    None => PcStatus::Connecting,
                };
                PcInfo {
                    id: pc.id.clone(),
                    name: pc.name.clone(),
                    status,
                    last_seen_min: match probe.and_then(|p| p.seen) {
                        Some(at) => u32::try_from(at.elapsed().as_secs() / 60).unwrap_or(u32::MAX),
                        None => u32::MAX,
                    },
                    warn: probe.and_then(|p| p.warn).map(str::to_string),
                    latency_ms: probe.and_then(|p| p.latency_ms),
                }
            })
            .collect();
        drop(st);
        if changed {
            self.inner.nudge();
        }
        rows
    }

    fn discover(&self, paired: &[PairedPc]) -> Vec<Candidate> {
        lock(&self.inner.state)
            .neighbours
            .iter()
            .map(|n| Candidate {
                name: n.name.clone(),
                address: n.address.clone(),
                source: n.source,
                paired: paired.iter().any(|p| {
                    n.device_id.as_deref() == Some(p.id.as_str())
                        || p.address.as_deref() == Some(n.address.as_str())
                }),
            })
            .collect()
    }

    fn pair(&self, target: &str, code: &str, name: Option<&str>) -> Result<PairedPc, PairError> {
        let identity = self.inner.identity.as_ref().ok_or(PairError::Network)?;
        let peer = PeerTarget::parse(target).map_err(|_| PairError::Network)?;
        let mut session = connect_timeout(&peer, PAIR_CONNECT).map_err(|e| {
            tracing::debug!(error = %e, "pairing connect failed");
            PairError::Network
        })?;
        {
            let mut guard = lock(&self.inner.trust);
            let trust = guard.as_mut().ok_or(PairError::Network)?;
            session
                .authenticate_as_client(identity, trust, Some(code))
                .map_err(|e| {
                    tracing::info!(error = %e, "pairing failed");
                    classify(&e, true)
                })?;
        }
        let id = session.peer_device_id().ok_or(PairError::Network)?;
        let address = peer.socket_addr().to_string();
        let known = lock(&self.inner.state)
            .neighbours
            .iter()
            .find(|n| n.address == address)
            .map(|n| n.name.clone());
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .or(known)
            .unwrap_or_else(|| target.trim().to_string());
        tracing::info!(device_id = %id, %address, "paired");
        // The PC just answered, so it is online until the next probe says otherwise.
        lock(&self.inner.state).probes.insert(
            id.clone(),
            Probe {
                status: PcStatus::Online,
                warn: None,
                latency_ms: None,
                seen: Some(Instant::now()),
            },
        );
        self.inner.nudge();
        Ok(PairedPc {
            id,
            name,
            address: Some(address),
        })
    }

    fn open_session(&self, pc: &PairedPc) -> Result<Session, PairError> {
        self.inner.open(pc, SESSION_CONNECT)
    }

    fn connect(&self, pc: &PairedPc, sink: EventSink) -> Result<Box<dyn ViewerSession>, PairError> {
        let mut session = self.inner.open(pc, SESSION_CONNECT)?;
        session
            .set_timeout(Some(SESSION_IDLE))
            .map_err(|_| PairError::Network)?;
        let (reader, mut writer) = session.split().map_err(|_| PairError::Network)?;
        writer
            .send(&Message::ViewerJoin)
            .and_then(|()| writer.send(&Message::RequestKeyframe))
            .map_err(|_| PairError::Network)?;

        let token = {
            let mut st = lock(&self.inner.state);
            st.next_token += 1;
            let token = st.next_token;
            st.watching.insert(pc.id.clone(), token);
            st.next_token
        };
        let live = Arc::new(Live {
            writer: Mutex::new(writer),
            stop: AtomicBool::new(false),
            active: AtomicBool::new(true),
            keys: Mutex::new(InputTranslator::default()),
        });
        let spawned = spawn_reader(
            reader,
            &live,
            sink,
            Arc::clone(&self.inner),
            pc.id.clone(),
            token,
        )
        .and_then(|()| spawn_pinger(&live));
        if spawned.is_err() {
            live.shutdown();
            return Err(PairError::Network);
        }
        Ok(Box::new(Handle(live)))
    }
}

/// Writer half plus the stop flag shared by the reader, pinger and the UI handle.
struct Live {
    writer: Mutex<SessionWriter>,
    stop: AtomicBool,
    /// The video is wanted (`false` while the tab is in the background).
    active: AtomicBool,
    keys: Mutex<InputTranslator>,
}

impl Live {
    fn send(&self, msg: &Message) {
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        if let Err(e) = lock(&self.writer).send(msg) {
            tracing::debug!(error = %e, "session write failed");
            self.shutdown();
        }
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        lock(&self.writer).shutdown();
    }
}

struct Handle(Arc<Live>);

impl ViewerSession for Handle {
    fn send_input(&self, event: &InputEvent) {
        let payload = lock(&self.0.keys).translate(event);
        if let Some(payload) = payload {
            self.0.send(&Message::InputEvent(payload));
        }
    }

    fn request_keyframe(&self) {
        self.0.send(&Message::RequestKeyframe);
    }

    /// The host stops capturing and encoding when its last viewer leaves, so a paused tab
    /// costs no bandwidth; joining again makes it send the config and a keyframe.
    fn set_active(&self, active: bool) {
        if self.0.active.swap(active, Ordering::Relaxed) == active {
            return;
        }
        if active {
            self.0.send(&Message::ViewerJoin);
            self.0.send(&Message::RequestKeyframe);
        } else {
            self.0.send(&Message::ViewerLeave);
        }
    }

    fn is_open(&self) -> bool {
        !self.0.stop.load(Ordering::Relaxed)
    }

    fn close(&self) {
        self.0.send(&Message::ViewerLeave);
        self.0.shutdown();
    }
}

fn spawn_pinger(live: &Arc<Live>) -> DeskResult<()> {
    let live = Arc::clone(live);
    std::thread::Builder::new()
        .name("viewer-ping".to_string())
        .spawn(move || {
            while !live.stop.load(Ordering::Relaxed) {
                live.send(&Message::Ping { ts: now_us() });
                let until = Instant::now() + PING_EVERY;
                while Instant::now() < until && !live.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        })
        .map(|_| ())
        .map_err(DeskError::Io)
}

fn spawn_reader(
    mut reader: SessionReader,
    live: &Arc<Live>,
    sink: EventSink,
    inner: Arc<Inner>,
    device_id: String,
    token: u64,
) -> DeskResult<()> {
    let live = Arc::clone(live);
    std::thread::Builder::new()
        .name("viewer-read".to_string())
        .spawn(move || {
            let mut stream = StreamState::default();
            loop {
                match reader.recv() {
                    Ok(Message::Pong { ts }) => {
                        let ms =
                            u32::try_from(now_us().saturating_sub(ts) / 1000).unwrap_or(u32::MAX);
                        inner.record(&device_id, Ok(ms));
                        sink(SessionEvent::Rtt { ms });
                    }
                    Ok(Message::Notice { kind }) => sink(SessionEvent::Notice {
                        kind: notice_code(kind),
                    }),
                    // Frames already in flight when the tab was paused are not shown.
                    Ok(msg) if !live.active.load(Ordering::Relaxed) => {
                        tracing::trace!(?msg, "dropped while paused");
                    }
                    Ok(msg) => stream.handle(msg, &sink),
                    Err(e) => {
                        if !live.stop.load(Ordering::Relaxed) {
                            tracing::info!(error = %e, "viewer session ended");
                        }
                        break;
                    }
                }
            }
            live.shutdown();
            {
                let mut st = lock(&inner.state);
                if st.watching.get(&device_id) == Some(&token) {
                    st.watching.remove(&device_id);
                }
            }
            inner.nudge();
            sink(SessionEvent::Closed);
        })
        .map(|_| ())
        .map_err(DeskError::Io)
}

/// Stable snake_case code of a host notice, handed to the viewer UI.
fn notice_code(kind: NoticeKind) -> &'static str {
    match kind {
        NoticeKind::DisconnectedByHost => "disconnected_by_host",
        NoticeKind::ViewOnlyOn => "view_only_on",
        NoticeKind::ViewOnlyOff => "view_only_off",
    }
}

/// Turns `VideoConfig`/`VideoFrame` into UI events; the browser needs an `avc1.*` codec string.
#[derive(Default)]
struct StreamState {
    config: Option<(String, u32, u32, u32)>,
    announced: bool,
}

impl StreamState {
    fn handle(&mut self, msg: Message, sink: &EventSink) {
        match msg {
            Message::VideoConfig {
                codec,
                width,
                height,
                fps,
            } => {
                self.config = Some((codec, width, height, fps));
                self.announced = false;
                self.announce(None, sink);
            }
            Message::VideoFrame {
                ts_us,
                keyframe,
                data,
            } => {
                if keyframe {
                    self.announce(Some(&data), sink);
                }
                if self.announced {
                    sink(SessionEvent::Frame {
                        ts_us,
                        keyframe,
                        data,
                    });
                }
            }
            Message::CursorShape {
                id,
                hot_x,
                hot_y,
                width,
                height,
                rgba,
            } => sink(SessionEvent::CursorShape {
                id,
                hot_x,
                hot_y,
                width,
                height,
                rgba,
            }),
            Message::CursorPos {
                x,
                y,
                shape_id,
                visible,
            } => sink(SessionEvent::CursorPos {
                x,
                y,
                shape_id,
                visible,
            }),
            other => tracing::debug!(?other, "ignored message"),
        }
    }

    fn announce(&mut self, keyframe: Option<&[u8]>, sink: &EventSink) {
        if self.announced {
            return;
        }
        let Some((codec, width, height, fps)) = &self.config else {
            return;
        };
        let codec = if codec.starts_with("avc1.") {
            codec.clone()
        } else if let Some(found) = keyframe.and_then(avc1_codec) {
            found
        } else if keyframe.is_some() {
            FALLBACK_CODEC.to_string()
        } else {
            return;
        };
        self.announced = true;
        sink(SessionEvent::Config {
            codec,
            fps: *fps,
            width: Some(*width),
            height: Some(*height),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_lose_the_mdns_suffix() {
        assert_eq!(friendly_name("desk_ab12.local."), "desk-ab12");
    }

    #[test]
    fn handshake_failures_map_to_ui_codes() {
        let wrong =
            DeskError::Crypto("host failed key confirmation (wrong code or impostor)".into());
        assert_eq!(classify(&wrong, true), PairError::Invalid);
        let refused = DeskError::Crypto("pairing rejected by host (wrong, expired or used)".into());
        assert_eq!(classify(&refused, true), PairError::Rejected);
        let unpinned = DeskError::Crypto("host desk-1 is not paired (unknown key)".into());
        assert_eq!(classify(&unpinned, false), PairError::Unpaired);
        let dropped = DeskError::Io(std::io::Error::from(ErrorKind::UnexpectedEof));
        assert_eq!(classify(&dropped, false), PairError::Unpaired);
        assert_eq!(classify(&dropped, true), PairError::Network);
    }

    #[test]
    fn non_avc_codec_names_are_replaced_by_the_sps() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = Arc::clone(&seen);
        let sink: EventSink = Arc::new(move |e| {
            if let SessionEvent::Config { codec, .. } = e {
                lock(&log).push(codec);
            }
        });
        let mut state = StreamState::default();
        state.handle(
            Message::VideoConfig {
                codec: "h264".into(),
                width: 640,
                height: 360,
                fps: 30,
            },
            &sink,
        );
        assert!(lock(&seen).is_empty());
        state.handle(
            Message::VideoFrame {
                ts_us: 0,
                keyframe: true,
                data: vec![0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1F, 0, 0, 0, 1, 0x65],
            },
            &sink,
        );
        assert_eq!(lock(&seen).as_slice(), ["avc1.42C01F"]);
    }
}
