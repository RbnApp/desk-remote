//! # Development host
//!
//! ## Responsibility
//! A fake Desk-Remote PC for panel development: it announces itself over mDNS, prints a
//! pairing code, authenticates panels (first pairing with the code, later codeless), streams
//! the real screen through [`MediaPipeline`] and logs every received input event.
//!
//! ## Scope Boundaries
//! - Example only; not the host service and not part of the shipped product.
//! - Input is logged, never injected.
//!
//! ## Usage
//! `cargo run -p desk-transport --example dev_host -- --ip 192.168.1.20 [--port 47800]
//! [--state-dir DIR] [--fps 30] [--max-width 1280] [--max-height 720] [--software]`

use desk_capture::MonitorSelector;
use desk_codec::{EncodedSink, MediaPipeline, PipelineConfig};
use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_logger::{init, LoggerConfig};
use desk_protocol::{InputPayload, Message, PROTOCOL_VERSION};
use desk_transport::{listen, LanAnnouncer, Session, SessionReader, SessionWriter, DEFAULT_PORT};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One connected panel.
struct Link {
    writer: Mutex<SessionWriter>,
    joined: AtomicBool,
}

impl Link {
    fn send(&self, msg: &Message) -> DeskResult<()> {
        lock(&self.writer).send(msg)
    }
}

/// Fans the encoder output out to every viewer that sent `ViewerJoin`.
#[derive(Default)]
struct Hub {
    links: Mutex<Vec<Arc<Link>>>,
    config: Mutex<Option<(u32, u32, u32)>>,
}

impl Hub {
    fn broadcast(&self, msg: &Message) {
        lock(&self.links).retain(|link| {
            if !link.joined.load(Ordering::Relaxed) {
                return true;
            }
            link.send(msg).is_ok()
        });
    }
}

impl EncodedSink for Hub {
    fn on_config(&self, width: u32, height: u32, fps: u32) {
        tracing::info!(width, height, fps, "stream config");
        *lock(&self.config) = Some((width, height, fps));
        self.broadcast(&config_message(width, height, fps));
    }

    fn on_frame(&self, ts_us: u64, keyframe: bool, data: &[u8]) {
        self.broadcast(&Message::VideoFrame {
            ts_us,
            keyframe,
            data: data.to_vec(),
        });
    }
}

fn config_message(width: u32, height: u32, fps: u32) -> Message {
    // The panel derives the browser codec string from the stream's SPS.
    Message::VideoConfig {
        codec: "h264".to_string(),
        width,
        height,
        fps,
    }
}

fn describe(payload: &InputPayload) -> String {
    match payload {
        InputPayload::Mouse {
            x,
            y,
            buttons,
            wheel,
        } => format!("mouse x={x} y={y} buttons={buttons:05b} wheel={wheel}"),
        InputPayload::Key { scancode, down } => {
            let state = if *down { "down" } else { "up" };
            format!("key scancode={scancode:#06x} {state}")
        }
    }
}

/// Serves one authenticated panel until it disconnects.
fn serve(mut reader: SessionReader, link: &Arc<Link>, hub: &Hub, pipeline: &MediaPipeline) {
    loop {
        match reader.recv() {
            Ok(Message::ViewerJoin) => {
                if !link.joined.swap(true, Ordering::Relaxed) {
                    if let Err(e) = pipeline.viewer_join() {
                        tracing::error!(error = %e, "cannot start capture");
                    }
                    if let Some((w, h, fps)) = *lock(&hub.config) {
                        let _ = link.send(&config_message(w, h, fps));
                    }
                    pipeline.request_keyframe();
                    tracing::info!("viewer joined");
                }
            }
            Ok(Message::ViewerLeave) => leave(link, pipeline),
            Ok(Message::RequestKeyframe) => pipeline.request_keyframe(),
            Ok(Message::Ping { ts }) => {
                let _ = link.send(&Message::Pong { ts });
            }
            Ok(Message::InputEvent(payload)) => tracing::info!("INPUT {}", describe(&payload)),
            Ok(other) => tracing::debug!(?other, "ignored"),
            Err(e) => {
                tracing::info!(error = %e, "panel disconnected");
                break;
            }
        }
    }
    leave(link, pipeline);
    lock(&hub.links).retain(|l| !Arc::ptr_eq(l, link));
}

fn leave(link: &Link, pipeline: &MediaPipeline) {
    if link.joined.swap(false, Ordering::Relaxed) {
        pipeline.viewer_leave();
        tracing::info!("viewer left");
    }
}

/// Keeps one pairing code active and prints a new one whenever it was used up or expired.
fn keep_code_fresh(pairing: &Mutex<PairingManager>) {
    let mut shown = String::new();
    loop {
        {
            let mut mgr = lock(pairing);
            if mgr.active_code().is_none() {
                if let Err(e) = mgr.create_pairing_code() {
                    tracing::error!(error = %e, "cannot create a pairing code");
                }
            }
            if let Some(code) = mgr.active_code() {
                if code != shown {
                    tracing::info!("PAIRING CODE: {code}");
                    shown = code.to_string();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn handle(
    mut session: Session,
    pairing: &Mutex<PairingManager>,
    identity: &DeviceIdentity,
    hub: &Hub,
    pipeline: &MediaPipeline,
) {
    let peer = session.peer_addr();
    let auth = session.authenticate_as_host(pairing, identity);
    if let Err(e) = auth {
        tracing::warn!(%peer, error = %e, "REFUSED");
        return;
    }
    tracing::info!(%peer, panel = ?session.peer_device_id(), "AUTHENTICATED");
    let (reader, writer) = match session.split() {
        Ok(halves) => halves,
        Err(e) => {
            tracing::warn!(error = %e, "cannot split session");
            return;
        }
    };
    let link = Arc::new(Link {
        writer: Mutex::new(writer),
        joined: AtomicBool::new(false),
    });
    lock(&hub.links).push(Arc::clone(&link));
    serve(reader, &link, hub, pipeline);
}

fn main() -> DeskResult<()> {
    let _guard = init(&LoggerConfig::default())?;
    let ip: IpAddr = arg("--ip", IpAddr::from([127, 0, 0, 1]));
    let port: u16 = arg("--port", DEFAULT_PORT);
    let state_dir: PathBuf = arg("--state-dir", std::env::temp_dir().join("desk-dev-host"));
    std::fs::create_dir_all(&state_dir).map_err(DeskError::Io)?;

    let identity = DeviceIdentity::load_or_create(&state_dir.join("identity.key"))?;
    let trust = TrustStore::open(&state_dir.join("trusted_peers.dat"))?;
    tracing::info!(
        device_id = %identity.device_id(),
        trusted_panels = trust.peers().len(),
        state_dir = %state_dir.display(),
        "dev host identity"
    );
    let pairing = Arc::new(Mutex::new(PairingManager::with_trust(trust)));
    let for_codes = Arc::clone(&pairing);
    std::thread::spawn(move || keep_code_fresh(&for_codes));

    let listener = listen(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), port))?;
    let _announcer = LanAnnouncer::start(
        &identity.device_id(),
        &ip.to_string(),
        port,
        PROTOCOL_VERSION,
    )?;
    tracing::info!("listening on {ip}:{port} (announced over mDNS)");

    let hub = Arc::new(Hub::default());
    let pipeline = Arc::new(MediaPipeline::new(
        PipelineConfig {
            monitor: MonitorSelector::Primary,
            fps: arg("--fps", 30),
            max_width: arg("--max-width", 1280),
            max_height: arg("--max-height", 720),
            bitrate_bps: None,
            allow_software: std::env::args().any(|a| a == "--software"),
        },
        Arc::clone(&hub) as Arc<dyn EncodedSink>,
    ));

    loop {
        let session = match listener.accept() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                continue;
            }
        };
        let (pairing, identity) = (Arc::clone(&pairing), identity.clone());
        let (hub, pipeline) = (Arc::clone(&hub), Arc::clone(&pipeline));
        std::thread::spawn(move || handle(session, &pairing, &identity, &hub, &pipeline));
    }
}
