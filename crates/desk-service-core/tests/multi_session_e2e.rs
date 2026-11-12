//! End-to-end for several PC sessions open in one panel: two real hosts on 127.0.0.1 (own
//! port, own data dir, own identity), a viewer link to each over the authenticated Noise
//! transport, and a stand-in for the session helper that produces video only while a viewer
//! is joined (exactly what the capture pipeline does). Pause is `ViewerLeave` on a link that
//! stays open, resume is `ViewerJoin` + `RequestKeyframe`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_error::DeskResult;
use desk_ipc::{pipe_access_sddl, IpcClient, IpcMessage, IpcServer};
use desk_paths::DeskPaths;
use desk_protocol::{InputPayload, Message};
use desk_service_core::{HostOptions, ServiceHost};
use desk_transport::{connect, PeerTarget, Session};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FRAME_BYTES: usize = 4000;
const FRAME_EVERY: Duration = Duration::from_millis(33);

/// A host plus the fake capture pipeline behind it.
struct Agent {
    host: ServiceHost,
    port: u16,
    work: tempfile::TempDir,
    /// Frames the pipeline produced (it produces none without a joined viewer).
    produced: Arc<AtomicU64>,
    inputs: Arc<AtomicU32>,
    done: Arc<AtomicBool>,
}

impl Agent {
    fn start(consent_gate: bool) -> DeskResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(500);
        let work = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
        let paths = DeskPaths::custom(work.path().join("system"), work.path().join("user"));
        paths.mark_panel_pc()?;
        let mut opts = HostOptions::for_panel_in_app(paths.clone(), Some(0), true);
        opts.custom_identity = Some(DeviceIdentity::generate()?);
        opts.custom_trust = Some(TrustStore::open(&paths.trusted_hosts_path())?);
        opts.consent_gate = consent_gate;
        let host = ServiceHost::start(opts)?;
        let port = host.port()?;

        let pipe = format!(
            r"\\.\pipe\DeskRemote-multi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let server = IpcServer::bind(&pipe, &pipe_access_sddl(&desk_ipc::current_user_sid()?))?;
        let client = std::thread::spawn({
            let pipe = pipe.clone();
            move || IpcClient::connect(&pipe, Duration::from_secs(5))
        });
        let service_end = server.accept(Duration::from_secs(5))?.expect("accept");
        let helper_end = client.join().unwrap().expect("connect");
        let hub = Arc::clone(host.hub());
        hub.attach_helper(service_end);

        let agent = Self {
            host,
            port,
            work,
            produced: Arc::default(),
            inputs: Arc::default(),
            done: Arc::default(),
        };
        let (produced, inputs, done) = (
            Arc::clone(&agent.produced),
            Arc::clone(&agent.inputs),
            Arc::clone(&agent.done),
        );
        std::thread::spawn(move || {
            let (mut viewers, mut key, mut next) = (0u32, false, Instant::now());
            while !done.load(Ordering::SeqCst) {
                match helper_end.recv_timeout(Duration::from_millis(5)) {
                    Ok(Some(IpcMessage::Wire(Message::ViewerJoin))) => {
                        viewers += 1;
                        key = true;
                        hub.on_helper_message(IpcMessage::Wire(Message::VideoConfig {
                            codec: "h264".into(),
                            width: 640,
                            height: 360,
                            fps: 30,
                        }));
                    }
                    Ok(Some(IpcMessage::Wire(Message::ViewerLeave))) => {
                        viewers = viewers.saturating_sub(1);
                    }
                    Ok(Some(IpcMessage::Wire(Message::RequestKeyframe))) => key = true,
                    Ok(Some(IpcMessage::Wire(Message::InputEvent(_)))) => {
                        inputs.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
                if viewers > 0 && Instant::now() >= next {
                    next = Instant::now() + FRAME_EVERY;
                    produced.fetch_add(1, Ordering::SeqCst);
                    hub.on_helper_message(IpcMessage::Wire(Message::VideoFrame {
                        ts_us: 0,
                        keyframe: std::mem::take(&mut key),
                        data: vec![7; FRAME_BYTES],
                    }));
                }
            }
        });
        Ok(agent)
    }

    /// A paired viewer link, like the panel's `viewer_connect`.
    fn viewer(&self) -> DeskResult<Session> {
        let code = self
            .host
            .hub()
            .with_pairing(PairingManager::create_pairing_code)?;
        let mut trust = TrustStore::open(&self.work.path().join("viewer-trust.json"))?;
        let mut session = connect(PeerTarget::loopback(self.port))?;
        session.authenticate_as_client(&DeviceIdentity::generate()?, &mut trust, Some(&code))?;
        session.send(&Message::ViewerJoin)?;
        session.send(&Message::RequestKeyframe)?;
        Ok(session)
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// What a viewer link delivered during a window.
#[derive(Default, Debug)]
struct Seen {
    frames: u32,
    bytes: usize,
    first_key_after: Option<Duration>,
}

/// Reads `session` for `window` and counts video.
fn watch(session: &mut Session, window: Duration) -> Seen {
    let (start, mut seen) = (Instant::now(), Seen::default());
    while start.elapsed() < window {
        if let Ok(Some(Message::VideoFrame { keyframe, data, .. })) =
            session.recv_timeout(Duration::from_millis(20))
        {
            seen.frames += 1;
            seen.bytes += data.len();
            if keyframe && seen.first_key_after.is_none() {
                seen.first_key_after = Some(start.elapsed());
            }
        }
    }
    seen
}

/// Reads two links at once (one thread each would need `Send`; slicing keeps both fresh).
fn watch_both(a: &mut Session, b: &mut Session, window: Duration) -> (Seen, Seen) {
    let (start, mut seen) = (Instant::now(), (Seen::default(), Seen::default()));
    while start.elapsed() < window {
        let (sa, sb) = (
            watch(a, Duration::from_millis(40)),
            watch(b, Duration::from_millis(40)),
        );
        seen.0.frames += sa.frames;
        seen.0.bytes += sa.bytes;
        seen.1.frames += sb.frames;
        seen.1.bytes += sb.bytes;
    }
    seen
}

#[test]
fn two_hosts_stream_together_pause_costs_nothing_and_resume_brings_a_keyframe() -> DeskResult<()> {
    let (a, b) = (Agent::start(false)?, Agent::start(false)?);
    let (mut va, mut vb) = (a.viewer()?, b.viewer()?);

    let both = watch_both(&mut va, &mut vb, Duration::from_millis(1200));
    println!("[MULTI] both streaming: A {both:?} B {:?}", both.1);
    assert!(both.0.frames > 10 && both.1.frames > 10, "{both:?}");

    // Pause A (a background tab): the link stays, the host stops producing.
    va.send(&Message::ViewerLeave)?;
    watch_both(&mut va, &mut vb, Duration::from_millis(400)); // in-flight frames drain
    let produced_at_pause = a.produced.load(Ordering::SeqCst);
    let window = Duration::from_secs(3);
    let (paused, running) = watch_both(&mut va, &mut vb, window);
    let secs = window.as_secs_f64();
    println!(
        "[MULTI] A paused: {} B/s, {} frames; B running: {:.0} B/s, {} frames; A pipeline produced {} frames while paused",
        paused.bytes,
        paused.frames,
        running.bytes as f64 / secs,
        running.frames,
        a.produced.load(Ordering::SeqCst) - produced_at_pause
    );
    assert_eq!(paused.bytes, 0, "a paused session must not receive video");
    assert_eq!(a.produced.load(Ordering::SeqCst), produced_at_pause);
    assert!(running.frames > 30, "B keeps streaming: {running:?}");

    // The paused link is still alive and answers pings.
    va.send(&Message::Ping { ts: 42 })?;
    let pong = (0..20).any(|_| {
        matches!(
            va.recv_timeout(Duration::from_millis(50)),
            Ok(Some(Message::Pong { ts: 42 }))
        )
    });
    assert!(pong, "the paused link must stay open");

    // Resume: a keyframe within a second.
    va.send(&Message::ViewerJoin)?;
    va.send(&Message::RequestKeyframe)?;
    let resumed = watch(&mut va, Duration::from_millis(1000));
    println!(
        "[MULTI] A resumed: first keyframe after {:?}, {} frames",
        resumed.first_key_after, resumed.frames
    );
    assert!(
        resumed
            .first_key_after
            .is_some_and(|t| t <= Duration::from_secs(1)),
        "{resumed:?}"
    );

    // Input goes to the host of the session it was sent on, nowhere else.
    let key = Message::InputEvent(InputPayload::Key {
        scancode: 30,
        down: true,
    });
    va.send(&key)?;
    watch_both(&mut va, &mut vb, Duration::from_millis(300));
    assert_eq!(
        (
            a.inputs.load(Ordering::SeqCst),
            b.inputs.load(Ordering::SeqCst)
        ),
        (1, 0)
    );

    // B's link dies: A is unaffected.
    drop(vb);
    let after = watch(&mut va, Duration::from_millis(1000));
    println!("[MULTI] B gone, A: {after:?}");
    assert!(after.frames > 10, "{after:?}");
    Ok(())
}

#[test]
fn a_file_transfer_runs_beside_open_viewer_sessions() -> DeskResult<()> {
    use desk_service_core::TransferEvent;
    use desk_transfer::{new_transfer_id, prepare, send_prepared};

    let a = Agent::start(true)?;
    let hub = Arc::clone(a.host.hub());
    let mut va = a.viewer()?;
    let request = (0..50)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(40));
            hub.pending_request()
        })
        .expect("consent request");
    hub.answer_consent(&request.request_id, "control", false)
        .expect("approve");
    assert!(watch(&mut va, Duration::from_millis(500)).frames > 5);

    let answerer = Arc::clone(&hub);
    hub.transfers().set_max_upload_mb(10);
    hub.transfers().set_listener(Some(Arc::new(move |event| {
        let TransferEvent::Offer { transfer_id, .. } = &event;
        let (hub, id) = (Arc::clone(&answerer), transfer_id.clone());
        std::thread::spawn(move || {
            for _ in 0..50 {
                if hub.transfers().answer(&id, true) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    })));
    let file = a.work.path().join("note.bin");
    std::fs::write(&file, vec![9u8; 300_000]).unwrap();
    let never = AtomicBool::new(false);
    let prepared = prepare(&file, 10, new_transfer_id(), &never).expect("prepare");
    let code = hub.with_pairing(PairingManager::create_pairing_code)?;
    let mut trust = TrustStore::open(&a.work.path().join("sender-trust.json"))?;
    let mut sender = connect(PeerTarget::loopback(a.port))?;
    sender.authenticate_as_client(&DeviceIdentity::generate()?, &mut trust, Some(&code))?;
    send_prepared(&mut sender, &prepared, &never, |_| {}).expect("transfer");

    let got = std::fs::read(
        DeskPaths::custom(a.work.path().join("system"), a.work.path().join("user"))
            .received_files_dir()
            .join("note.bin"),
    )
    .unwrap();
    assert_eq!(got.len(), 300_000);
    let after = watch(&mut va, Duration::from_millis(600));
    println!("[MULTI] transfer done, viewer still streams: {after:?}");
    assert!(after.frames > 5, "{after:?}");
    Ok(())
}
