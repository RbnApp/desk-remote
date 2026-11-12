//! End-to-end security tests for the connection layer (real sockets, real handshakes).
//!
//! A recording TCP tap sits between panel and host so the tests can assert on the exact
//! bytes that cross the wire (eavesdropping proof) and inject replayed / corrupted frames.

use super::*;
use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_protocol::Message;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

type Rewrite = Arc<dyn Fn(usize, Vec<u8>) -> Vec<Vec<u8>> + Send + Sync>;

/// Everything seen on the wire, per direction.
#[derive(Default)]
struct Capture {
    panel_to_host: Vec<u8>,
    host_to_panel: Vec<u8>,
}

struct Tap {
    addr: SocketAddr,
    capture: Arc<Mutex<Capture>>,
}

/// Starts a one-connection recording proxy in front of `upstream`.
/// `rewrite(index, frame)` decides what is forwarded for each panel->host raw frame
/// (index counts frames after the 5-byte preamble; `frame` includes its length prefix).
fn start_tap(upstream: SocketAddr, rewrite: Rewrite) -> DeskResult<Tap> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(DeskError::Io)?;
    let addr = listener.local_addr().map_err(DeskError::Io)?;
    let capture = Arc::new(Mutex::new(Capture::default()));
    let cap = Arc::clone(&capture);

    thread::spawn(move || {
        let Ok((mut panel, _)) = listener.accept() else {
            return;
        };
        let Ok(mut host) = TcpStream::connect(upstream) else {
            return;
        };
        let (Ok(mut host_r), Ok(mut panel_w)) = (host.try_clone(), panel.try_clone()) else {
            return;
        };

        // host -> panel: plain byte copy with recording
        let cap_h = Arc::clone(&cap);
        thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            while let Ok(n) = host_r.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut c) = cap_h.lock() {
                    c.host_to_panel.extend_from_slice(&buf[..n]);
                }
                if panel_w.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            let _ = panel_w.shutdown(std::net::Shutdown::Both);
        });

        // panel -> host: frame-aware so frames can be duplicated / corrupted
        let mut preamble = [0u8; 5];
        if panel.read_exact(&mut preamble).is_err() {
            return;
        }
        if let Ok(mut c) = cap.lock() {
            c.panel_to_host.extend_from_slice(&preamble);
        }
        if host.write_all(&preamble).is_err() {
            return;
        }
        let mut index = 0usize;
        loop {
            let mut len_bytes = [0u8; 4];
            if panel.read_exact(&mut len_bytes).is_err() {
                break;
            }
            let len = u32::from_be_bytes(len_bytes) as usize;
            let mut frame = Vec::with_capacity(4 + len);
            frame.extend_from_slice(&len_bytes);
            frame.resize(4 + len, 0);
            if panel.read_exact(&mut frame[4..]).is_err() {
                break;
            }
            if let Ok(mut c) = cap.lock() {
                c.panel_to_host.extend_from_slice(&frame);
            }
            for out in rewrite(index, frame) {
                if host.write_all(&out).is_err() {
                    return;
                }
            }
            index += 1;
        }
        let _ = host.shutdown(std::net::Shutdown::Both);
    });

    Ok(Tap { addr, capture })
}

fn passthrough() -> Rewrite {
    Arc::new(|_, f| vec![f])
}

fn join<T>(h: JoinHandle<T>) -> DeskResult<T> {
    h.join()
        .map_err(|_| DeskError::Other("thread panicked".to_string()))
}

type HostOutcome = (DeskResult<()>, PairingManager);

/// Host thread: accept one connection and authenticate it; the manager is handed back.
fn host_auth_once(
    listener: Listener,
    mgr: PairingManager,
    identity: DeviceIdentity,
) -> JoinHandle<HostOutcome> {
    thread::spawn(move || {
        let mgr = Mutex::new(mgr);
        let res = listener
            .accept()
            .and_then(|mut s| s.authenticate_as_host(&mgr, &identity));
        (res, mgr.into_inner().unwrap_or_else(|p| p.into_inner()))
    })
}

fn local_listener() -> DeskResult<(Listener, SocketAddr)> {
    let l = listen(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let a = l.local_addr()?;
    Ok((l, a))
}

fn assert_err_contains<T: std::fmt::Debug>(res: DeskResult<T>, needle: &str) {
    match res {
        Ok(v) => panic!("expected error containing {needle:?}, got Ok({v:?})"),
        Err(e) => assert!(
            e.to_string().contains(needle),
            "error {e} does not contain {needle:?}"
        ),
    }
}

/// Full flow: first-time PAKE pairing, Noise session, Ping and a 10 MiB frame, with the
/// wire captured. Asserts the pairing code and plaintext never appear on the wire.
#[test]
fn e2e_pair_then_session_10mb_with_wire_capture() -> DeskResult<()> {
    let host_id = DeviceIdentity::generate()?;
    let panel_id = DeviceIdentity::generate()?;
    let mut mgr = PairingManager::new();
    let code = mgr.create_pairing_code()?;

    let (listener, host_addr) = local_listener()?;
    let tap = start_tap(host_addr, passthrough())?;

    let host = thread::spawn({
        let host_id = host_id.clone();
        move || -> (DeskResult<()>, PairingManager) {
            let mgr = Mutex::new(mgr);
            let res = (|| -> DeskResult<()> {
                let mut s = listener.accept()?;
                s.authenticate_as_host(&mgr, &host_id)?;
                loop {
                    match s.recv()? {
                        Message::Ping { ts } => s.send(&Message::Pong { ts })?,
                        Message::VideoFrame { data, .. } => {
                            assert_eq!(data.len(), 10 * 1024 * 1024);
                            s.send(&Message::Status {
                                online: true,
                                latency_ms: 42,
                            })?;
                            return Ok(());
                        }
                        other => return Err(DeskError::Protocol(format!("unexpected {other:?}"))),
                    }
                }
            })();
            (res, mgr.into_inner().unwrap_or_else(|p| p.into_inner()))
        }
    });

    let mut panel_trust = TrustStore::in_memory();
    let mut s = connect(PeerTarget::Lan(tap.addr))?;
    s.authenticate_as_client(&panel_id, &mut panel_trust, Some(&code))?;
    assert_eq!(s.peer_device_id(), Some(host_id.device_id()));
    assert!(panel_trust.is_trusted(host_id.public_key()));

    let latency = s.ping()?;
    assert!(latency < Duration::from_secs(2));
    let marker = vec![0xABu8; 10 * 1024 * 1024];
    s.send(&Message::VideoFrame {
        ts_us: 7,
        keyframe: true,
        data: marker.clone(),
    })?;
    assert_eq!(
        s.recv()?,
        Message::Status {
            online: true,
            latency_ms: 42
        }
    );
    drop(s);

    let (res, mgr) = join(host)?;
    res?;
    assert!(mgr.is_trusted(panel_id.public_key()));
    assert_eq!(mgr.active_code(), None, "code must be single use");

    // ---- wire evidence ----
    thread::sleep(Duration::from_millis(100));
    let cap = tap
        .capture
        .lock()
        .map_err(|_| DeskError::Other("capture lock poisoned".to_string()))?;
    let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    // Positive control: the detector does find bytes that really are on the wire.
    assert!(contains(&cap.panel_to_host, b"DRM1"));
    assert!(contains(&cap.panel_to_host, &cap.panel_to_host[20..40]));
    for (name, bytes) in [
        ("panel->host", &cap.panel_to_host),
        ("host->panel", &cap.host_to_panel),
    ] {
        assert!(!contains(bytes, code.as_bytes()), "{name}: code leaked");
        assert!(
            !contains(bytes, code.to_ascii_lowercase().as_bytes()),
            "{name}: lower-case code leaked"
        );
        assert!(
            !contains(bytes, &marker[..64]),
            "{name}: plaintext 0xAB run visible"
        );
        assert!(
            !contains(bytes, host_id.device_id().as_bytes())
                && !contains(bytes, panel_id.device_id().as_bytes()),
            "{name}: device id in clear"
        );
        assert!(!contains(bytes, b"Hello"), "{name}: Hello visible");
        assert!(!contains(bytes, b"0.1.0"), "{name}: version string visible");
    }
    #[allow(clippy::print_stdout)]
    {
        println!(
            "WIRE_EVIDENCE code={code} panel_to_host_bytes={} host_to_panel_bytes={} \
             code_found=false plaintext_run_found=false device_id_found=false",
            cap.panel_to_host.len(),
            cap.host_to_panel.len()
        );
        let head: String = cap.panel_to_host[..48]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("WIRE_HEAD panel_to_host[0..48]={head}");
    }
    Ok(())
}

/// Pairs once through a real socket and returns (host_id, panel_id, host mgr, panel trust).
fn pair_once() -> DeskResult<(DeviceIdentity, DeviceIdentity, PairingManager, TrustStore)> {
    let host_id = DeviceIdentity::generate()?;
    let panel_id = DeviceIdentity::generate()?;
    let mut mgr = PairingManager::new();
    let code = mgr.create_pairing_code()?;
    let (listener, addr) = local_listener()?;
    let host = host_auth_once(listener, mgr, host_id.clone());
    let mut trust = TrustStore::in_memory();
    connect(PeerTarget::Lan(addr))?.authenticate_as_client(&panel_id, &mut trust, Some(&code))?;
    let (res, mgr) = join(host)?;
    res?;
    Ok((host_id, panel_id, mgr, trust))
}

#[test]
fn red_wrong_code_is_rejected_and_trusts_nothing() -> DeskResult<()> {
    let host_id = DeviceIdentity::generate()?;
    let panel_id = DeviceIdentity::generate()?;
    let mut mgr = PairingManager::new();
    let code = mgr.create_pairing_code()?;
    let (listener, addr) = local_listener()?;
    let host = host_auth_once(listener, mgr, host_id.clone());

    let mut trust = TrustStore::in_memory();
    let wrong = if code == "ABCD-2345" {
        "WXYZ-9876"
    } else {
        "ABCD-2345"
    };
    let res =
        connect(PeerTarget::Lan(addr))?.authenticate_as_client(&panel_id, &mut trust, Some(wrong));
    assert_err_contains(res, "pairing rejected");

    let (host_res, mgr) = join(host)?;
    assert_err_contains(host_res, "invalid pairing code");
    assert!(!mgr.is_trusted(panel_id.public_key()));
    assert!(!trust.is_trusted(host_id.public_key()));
    assert!(
        mgr.active_code().is_some(),
        "one wrong try must not burn the code"
    );
    Ok(())
}

#[test]
fn red_correct_code_second_time_is_rejected() -> DeskResult<()> {
    // First pairing consumes the code; replaying the same (correct) code must fail.
    let host_id = DeviceIdentity::generate()?;
    let mut mgr = PairingManager::new();
    let code = mgr.create_pairing_code()?;
    let (l1, a1) = local_listener()?;
    let first = DeviceIdentity::generate()?;
    let h1 = host_auth_once(l1, mgr, host_id.clone());
    let mut t1 = TrustStore::in_memory();
    connect(PeerTarget::Lan(a1))?.authenticate_as_client(&first, &mut t1, Some(&code))?;
    let (r1, mgr) = join(h1)?;
    r1?;

    let (l2, a2) = local_listener()?;
    let attacker = DeviceIdentity::generate()?;
    let h2 = host_auth_once(l2, mgr, host_id.clone());
    let mut t2 = TrustStore::in_memory();
    let res = connect(PeerTarget::Lan(a2))?.authenticate_as_client(&attacker, &mut t2, Some(&code));
    assert_err_contains(res, "pairing rejected");
    let (r2, mgr) = join(h2)?;
    assert_err_contains(r2, "no active pairing code");
    assert!(!mgr.is_trusted(attacker.public_key()));
    Ok(())
}

#[test]
fn red_code_revoked_after_five_wrong_attempts() -> DeskResult<()> {
    let host_id = DeviceIdentity::generate()?;
    let mut mgr = PairingManager::new();
    let code = mgr.create_pairing_code()?;
    let wrong = if code == "ABCD-2345" {
        "WXYZ-9876"
    } else {
        "ABCD-2345"
    };
    for _ in 0..desk_crypto::MAX_FAILED_ATTEMPTS {
        let (l, a) = local_listener()?;
        let h = host_auth_once(l, mgr, host_id.clone());
        let p = DeviceIdentity::generate()?;
        let mut t = TrustStore::in_memory();
        let _ = connect(PeerTarget::Lan(a))?.authenticate_as_client(&p, &mut t, Some(wrong));
        let (_, m) = join(h)?;
        mgr = m;
    }
    assert_eq!(mgr.active_code(), None, "code must be revoked");

    // Even the right code is now useless.
    let (l, a) = local_listener()?;
    let h = host_auth_once(l, mgr, host_id.clone());
    let p = DeviceIdentity::generate()?;
    let mut t = TrustStore::in_memory();
    let res = connect(PeerTarget::Lan(a))?.authenticate_as_client(&p, &mut t, Some(&code));
    assert!(res.is_err());
    let (hr, _) = join(h)?;
    assert_err_contains(hr, "no active pairing code");
    Ok(())
}

#[test]
fn red_unpaired_static_key_is_rejected() -> DeskResult<()> {
    let (host_id, _panel_id, mgr, _t) = pair_once()?;
    let stranger = DeviceIdentity::generate()?;

    let (l, a) = local_listener()?;
    let h = host_auth_once(l, mgr, host_id.clone());
    // Stranger pins the host key (it could have read it) but the host does not know it.
    let mut trust = TrustStore::in_memory();
    trust.add(host_id.public_key())?;
    let res = connect(PeerTarget::Lan(a))?.authenticate_as_client(&stranger, &mut trust, None);
    assert!(res.is_err(), "unpaired panel must not get a session");
    let (hr, _) = join(h)?;
    assert_err_contains(hr, "rejected unpaired peer");
    Ok(())
}

#[test]
fn red_panel_rejects_unpinned_host_mitm() -> DeskResult<()> {
    let (_host_id, panel_id, _mgr, panel_trust) = pair_once()?;
    // A different machine (attacker) answers on the host address with its own key.
    let rogue = DeviceIdentity::generate()?;
    let mut rogue_mgr = PairingManager::new();
    rogue_mgr.add_trusted(panel_id.public_key())?; // even if it would accept the panel
    let (l, a) = local_listener()?;
    let h = host_auth_once(l, rogue_mgr, rogue);
    let mut trust = panel_trust;
    let res = connect(PeerTarget::Lan(a))?.authenticate_as_client(&panel_id, &mut trust, None);
    assert_err_contains(res, "not paired");
    let _ = join(h)?;
    Ok(())
}

#[test]
fn red_claiming_another_devices_id_is_rejected() -> DeskResult<()> {
    let (host_id, panel_id, mgr, mut panel_trust) = pair_once()?;
    let victim = DeviceIdentity::generate()?;

    let (l, a) = local_listener()?;
    let h = host_auth_once(l, mgr, host_id);
    let mut s = connect(PeerTarget::Lan(a))?;
    let _ = s.authenticate_as_client_claiming(&panel_id, &mut panel_trust, &victim.device_id());
    let (hr, _) = join(h)?;
    assert_err_contains(hr, "does not match its key");
    Ok(())
}

/// Establishes a paired session through a tap using `rewrite`, returns the panel session and
/// the host thread that serves exactly the messages the test expects.
fn session_through_tap(
    rewrite: Rewrite,
) -> DeskResult<(Session, JoinHandle<Vec<DeskResult<Message>>>)> {
    let (host_id, panel_id, mgr, mut panel_trust) = pair_once()?;
    let (l, a) = local_listener()?;
    let tap = start_tap(a, rewrite)?;
    let host = thread::spawn(move || {
        let mgr = Mutex::new(mgr);
        let mut out = Vec::new();
        match l.accept().and_then(|mut s| {
            s.authenticate_as_host(&mgr, &host_id)?;
            Ok(s)
        }) {
            Ok(mut s) => {
                for _ in 0..2 {
                    let r = s.recv();
                    let stop = r.is_err();
                    out.push(r);
                    if stop {
                        break;
                    }
                }
            }
            Err(e) => out.push(Err(e)),
        }
        out
    });
    let mut s = connect(PeerTarget::Lan(tap.addr))?;
    s.authenticate_as_client(&panel_id, &mut panel_trust, None)?;
    Ok((s, host))
}

/// Frame index of the first application frame: m1(0) m3(1) Hello(2) -> first app frame is 3.
const FIRST_APP_FRAME: usize = 3;

#[test]
fn red_replayed_ciphertext_frame_is_rejected() -> DeskResult<()> {
    let rewrite: Rewrite = Arc::new(|i, f| {
        if i == FIRST_APP_FRAME {
            vec![f.clone(), f] // deliver the same ciphertext frame twice
        } else {
            vec![f]
        }
    });
    let (mut s, host) = session_through_tap(rewrite)?;
    s.send(&Message::Ping { ts: 99 })?;
    let results = join(host)?;
    assert_eq!(results.len(), 2, "host saw {results:?}");
    assert_eq!(results[0].as_ref().ok(), Some(&Message::Ping { ts: 99 }));
    assert_err_contains(
        results
            .into_iter()
            .nth(1)
            .unwrap_or(Err(DeskError::Other("missing".into()))),
        "decrypt failed",
    );
    Ok(())
}

#[test]
fn red_corrupted_ciphertext_frame_is_rejected() -> DeskResult<()> {
    let rewrite: Rewrite = Arc::new(|i, mut f| {
        if i == FIRST_APP_FRAME {
            let last = f.len() - 1;
            f[last] ^= 0x01;
        }
        vec![f]
    });
    let (mut s, host) = session_through_tap(rewrite)?;
    s.send(&Message::Ping { ts: 1 })?;
    let results = join(host)?;
    assert_eq!(results.len(), 1);
    assert_err_contains(
        results
            .into_iter()
            .next()
            .unwrap_or(Err(DeskError::Other("missing".into()))),
        "decrypt failed",
    );
    Ok(())
}

#[test]
fn persistence_survives_restart_and_still_rejects_strangers() -> DeskResult<()> {
    let dir = tempfile::tempdir().map_err(DeskError::Io)?;
    let host_key = dir.path().join("host-identity.key");
    let host_trust = dir.path().join("trusted_peers.dat");
    let panel_key = dir.path().join("panel-identity.key");
    let panel_trust_path = dir.path().join("trusted_hosts.dat");

    // ---- first run: pair, then everything is dropped ("process exits") ----
    {
        let host_id = DeviceIdentity::load_or_create(&host_key)?;
        let panel_id = DeviceIdentity::load_or_create(&panel_key)?;
        let mut mgr = PairingManager::with_trust(TrustStore::open(&host_trust)?);
        let code = mgr.create_pairing_code()?;
        let (l, a) = local_listener()?;
        let h = host_auth_once(l, mgr, host_id);
        let mut t = TrustStore::open(&panel_trust_path)?;
        connect(PeerTarget::Lan(a))?.authenticate_as_client(&panel_id, &mut t, Some(&code))?;
        join(h)?.0?;
    }

    // ---- restart: reload everything from disk, no code involved ----
    let host_id = DeviceIdentity::load_or_create(&host_key)?;
    let panel_id = DeviceIdentity::load_or_create(&panel_key)?;
    let mgr = PairingManager::with_trust(TrustStore::open(&host_trust)?);
    assert_eq!(mgr.active_code(), None);
    let (l, a) = local_listener()?;
    let h = host_auth_once(l, mgr, host_id.clone());
    let mut t = TrustStore::open(&panel_trust_path)?;
    let mut s = connect(PeerTarget::Lan(a))?;
    s.authenticate_as_client(&panel_id, &mut t, None)?;
    assert_eq!(s.peer_device_id(), Some(host_id.device_id()));
    let (r, mgr) = join(h)?;
    r?;

    // ---- an unpaired panel is still refused after the restart ----
    let stranger = DeviceIdentity::generate()?;
    let (l2, a2) = local_listener()?;
    let h2 = host_auth_once(l2, mgr, host_id.clone());
    let mut st = TrustStore::in_memory();
    st.add(host_id.public_key())?;
    let res = connect(PeerTarget::Lan(a2))?.authenticate_as_client(&stranger, &mut st, None);
    assert!(res.is_err());
    assert_err_contains(join(h2)?.0, "rejected unpaired peer");
    Ok(())
}

#[test]
fn peer_target_parse_classifies_and_defaults_port() -> DeskResult<()> {
    assert_eq!(
        PeerTarget::parse("192.168.1.20")?,
        PeerTarget::Lan(SocketAddr::from(([192, 168, 1, 20], DEFAULT_PORT)))
    );
    assert_eq!(
        PeerTarget::parse(" 100.101.102.103:5000 ")?,
        PeerTarget::Tailscale {
            ip: IpAddr::from([100, 101, 102, 103]),
            port: 5000
        }
    );
    assert!(PeerTarget::parse("100.63.0.1")?
        .socket_addr()
        .ip()
        .is_ipv4());
    assert!(matches!(
        PeerTarget::parse("100.63.0.1")?,
        PeerTarget::Lan(_)
    ));
    assert!(PeerTarget::parse("no such host !!").is_err());
    Ok(())
}
