//! End-to-end integration test for attended incoming connections (AnyDesk style).
//! Exercises the consent gate: request -> pending -> view -> control -> disconnect.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_error::DeskResult;
use desk_paths::DeskPaths;
use desk_protocol::{Message, NoticeKind};
use desk_service_core::{HostOptions, HostRole, ServiceHost};
use desk_transport::{connect, PeerTarget};
use std::sync::atomic::Ordering;
use std::time::Duration;

struct TestHostRig {
    host: ServiceHost,
    port: u16,
    _paths: DeskPaths,
    _temp_dir: tempfile::TempDir,
}

impl TestHostRig {
    fn start() -> DeskResult<Self> {
        let temp_dir = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
        let paths = DeskPaths::custom(temp_dir.path().join("system"), temp_dir.path().join("user"));
        paths.mark_panel_pc()?;

        let identity = DeviceIdentity::generate()?;
        let trust = TrustStore::open(&paths.trusted_hosts_path())?;

        let mut opts = HostOptions::for_panel_in_app(paths.clone(), Some(0), true);
        opts.custom_identity = Some(identity);
        opts.custom_trust = Some(trust);
        opts.role = HostRole::PanelInApp;
        opts.consent_gate = true;
        opts.loopback_only = true;

        let host = ServiceHost::start(opts)?;
        let port = host.port()?;

        Ok(Self {
            host,
            port,
            _paths: paths,
            _temp_dir: temp_dir,
        })
    }
}

#[test]
fn two_panel_instances_e2e_incoming_consent_flow() -> DeskResult<()> {
    println!("[E2E-TEST] Step 1: Starting Panel A in-app host (loopback only)...");
    let rig = TestHostRig::start()?;
    let hub = rig.host.hub();
    let port = rig.port;
    println!("[E2E-TEST] Panel A host listening on 127.0.0.1:{port}");

    // Generate pairing code on Panel A
    let code = hub.with_pairing(PairingManager::create_pairing_code)?;
    println!("[E2E-TEST] Panel A generated pairing code: {code}");

    // Attach fake IPC session helper to Panel A so we can monitor frames and inputs
    use desk_ipc::{pipe_access_sddl, IpcClient, IpcMessage, IpcServer};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(100);
    let pipe_name = format!(
        r"\\.\pipe\DeskRemote-e2e-helper-{}",
        NEXT.fetch_add(1, Ordering::SeqCst)
    );
    let server = IpcServer::bind(
        &pipe_name,
        &pipe_access_sddl(&desk_ipc::current_user_sid()?),
    )?;
    let client = std::thread::spawn({
        let p = pipe_name.clone();
        move || IpcClient::connect(&p, Duration::from_secs(5))
    });
    let service_end = server
        .accept(Duration::from_secs(5))?
        .expect("server accept");
    let helper_end = client.join().unwrap().expect("client connect");
    hub.attach_helper(service_end);
    println!("[E2E-TEST] Panel A attached fake helper pipeline.");

    // Panel B (viewer) setup
    println!("[E2E-TEST] Step 2: Panel B connecting to Panel A via loopback...");
    let panel_b_temp = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
    let mut panel_b_trust = TrustStore::open(&panel_b_temp.path().join("panel_b_trust.json"))?;
    let panel_b_identity = DeviceIdentity::generate()?;
    let _panel_b_device_id = panel_b_identity.device_id();

    let mut viewer_session = connect(PeerTarget::loopback(port))?;
    viewer_session.authenticate_as_client(&panel_b_identity, &mut panel_b_trust, Some(&code))?;
    println!("[E2E-TEST] Panel B successfully authenticated with code.");

    // Viewer joins
    viewer_session.send(&Message::ViewerJoin)?;
    println!("[E2E-TEST] Panel B sent ViewerJoin request.");

    // Check Panel A's consent state: MUST BE PENDING
    std::thread::sleep(Duration::from_millis(100));
    let pending_req = hub
        .pending_request()
        .expect("Panel A must have pending request");
    println!(
        "[E2E-TEST] Panel A pending request received: id={}, known={}, peer={}",
        pending_req.request_id, pending_req.known, pending_req.peer_name
    );
    assert_eq!(hub.joined_count(), 0, "No viewer may join before consent");

    // Helper produces video frame: MUST NOT LEAK to viewer
    hub.on_helper_message(IpcMessage::Wire(Message::VideoFrame {
        ts_us: 100,
        keyframe: true,
        data: vec![0x00, 0x00, 0x00, 0x01, 0x67],
    }));
    let unapproved_msg = viewer_session.recv_timeout(Duration::from_millis(150))?;
    // May only receive Hello or nothing, NEVER a VideoFrame
    if let Some(ref m) = unapproved_msg {
        assert!(
            !matches!(m, Message::VideoFrame { .. }),
            "VideoFrame must NEVER leak before consent!"
        );
    }
    println!("[E2E-TEST] Verified: Onaysiz akis YOK (0 frames delivered before consent).");

    // Viewer sends input: MUST NOT reach helper
    viewer_session.send(&Message::InputEvent(desk_protocol::InputPayload::Key {
        scancode: 42,
        down: true,
    }))?;
    std::thread::sleep(Duration::from_millis(100));
    let helper_rx = helper_end.recv_timeout(Duration::from_millis(100))?;
    assert!(
        helper_rx.is_none() || !matches!(helper_rx, Some(IpcMessage::Wire(Message::InputEvent(_)))),
        "Input must NEVER reach helper before consent!"
    );
    println!("[E2E-TEST] Verified: Onaysiz girdi YOK (0 input delivered before consent).");

    // Step 3: Approve with "view" mode
    println!("[E2E-TEST] Step 3: Panel A answering consent with 'view' mode...");
    hub.answer_consent(&pending_req.request_id, "view", false)
        .expect("answer view");
    assert_eq!(hub.joined_count(), 1, "Viewer joined count must be 1");
    let active = hub.active_session().expect("active session must exist");
    assert_eq!(active.mode, "view");
    println!("[E2E-TEST] Panel A active session: mode={}", active.mode);

    // Now video frames MUST flow to viewer
    hub.on_helper_message(IpcMessage::Wire(Message::VideoFrame {
        ts_us: 200,
        keyframe: true,
        data: vec![0x00, 0x00, 0x00, 0x01, 0x65],
    }));
    let mut got_video_frame = false;
    for _ in 0..5 {
        if let Some(Message::VideoFrame { ts_us, .. }) =
            viewer_session.recv_timeout(Duration::from_millis(200))?
        {
            if ts_us == 200 {
                got_video_frame = true;
                break;
            }
        }
    }
    assert!(got_video_frame, "Video frame must flow after view approval");
    println!("[E2E-TEST] Verified: Video frame reached viewer after view approval.");

    // Viewer sends input while in 'view' mode: MUST BE DROPPED
    viewer_session.send(&Message::InputEvent(desk_protocol::InputPayload::Key {
        scancode: 55,
        down: true,
    }))?;
    std::thread::sleep(Duration::from_millis(100));
    let helper_input = helper_end.recv_timeout(Duration::from_millis(100))?;
    assert!(
        helper_input.is_none()
            || !matches!(helper_input, Some(IpcMessage::Wire(Message::InputEvent(_)))),
        "Input must be dropped while in view-only mode!"
    );
    println!("[E2E-TEST] Verified: View modunda girdi helper'a iletilmedi (dusuruldu).");

    // Step 4: Switch mode to "control"
    println!("[E2E-TEST] Step 4: Panel A changing mode to 'control'...");
    hub.set_incoming_mode("control");
    assert_eq!(
        hub.active_session().map(|s| s.mode),
        Some("control".to_string())
    );

    // Viewer sends input in 'control' mode: MUST REACH helper
    viewer_session.send(&Message::InputEvent(desk_protocol::InputPayload::Key {
        scancode: 55,
        down: true,
    }))?;
    let mut input_reached = false;
    for _ in 0..5 {
        if let Some(IpcMessage::Wire(Message::InputEvent(desk_protocol::InputPayload::Key {
            scancode,
            ..
        }))) = helper_end.recv_timeout(Duration::from_millis(200))?
        {
            if scancode == 55 {
                input_reached = true;
                break;
            }
        }
    }
    assert!(input_reached, "Input must reach helper in control mode");
    println!("[E2E-TEST] Verified: Control modunda girdi basariyla iletildi.");

    // Step 5: Disconnect
    println!("[E2E-TEST] Step 5: Panel A disconnecting session...");
    hub.disconnect_incoming();
    assert_eq!(hub.joined_count(), 0);
    assert!(hub.active_session().is_none());

    // Viewer receives Notice::DisconnectedByHost
    let mut notice_received = false;
    for _ in 0..5 {
        if let Some(Message::Notice { kind }) =
            viewer_session.recv_timeout(Duration::from_millis(200))?
        {
            if kind == NoticeKind::DisconnectedByHost {
                notice_received = true;
                break;
            }
        }
    }
    assert!(
        notice_received,
        "Viewer must receive DisconnectedByHost notice"
    );
    println!(
        "[E2E-TEST] Verified: Disconnect sonrasi viewer'a Notice bildirildi ve baglanti kapatildi."
    );

    rig.host.stop();
    println!("[E2E-TEST] Uctan uca test (istek -> view -> control -> disconnect) TAMAMLANDI!");
    Ok(())
}

#[test]
fn simultaneous_bidirectional_connections() -> DeskResult<()> {
    // Panel A and Panel B both host incoming sessions, and connect to each other at the same time
    println!("[BIDIR-TEST] Starting Panel A and Panel B in-app hosts...");
    let rig_a = TestHostRig::start()?;
    let rig_b = TestHostRig::start()?;

    let code_a = rig_a
        .host
        .hub()
        .with_pairing(PairingManager::create_pairing_code)?;
    let code_b = rig_b
        .host
        .hub()
        .with_pairing(PairingManager::create_pairing_code)?;

    let temp = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
    let mut trust_a_as_client = TrustStore::open(&temp.path().join("a_client.json"))?;
    let mut trust_b_as_client = TrustStore::open(&temp.path().join("b_client.json"))?;

    let id_client_a = DeviceIdentity::generate()?;
    let id_client_b = DeviceIdentity::generate()?;

    // A connects to B
    let mut session_a_to_b = connect(PeerTarget::loopback(rig_b.port))?;
    session_a_to_b.authenticate_as_client(&id_client_a, &mut trust_a_as_client, Some(&code_b))?;

    // B connects to A
    let mut session_b_to_a = connect(PeerTarget::loopback(rig_a.port))?;
    session_b_to_a.authenticate_as_client(&id_client_b, &mut trust_b_as_client, Some(&code_a))?;

    std::thread::sleep(Duration::from_millis(200));

    assert!(rig_a.host.hub().pending_request().is_some());
    assert!(rig_b.host.hub().pending_request().is_some());

    rig_a
        .host
        .hub()
        .answer_consent("req-1", "control", false)
        .map_err(desk_error::DeskError::Transport)?;
    rig_b
        .host
        .hub()
        .answer_consent("req-1", "view", false)
        .map_err(desk_error::DeskError::Transport)?;

    assert_eq!(rig_a.host.hub().active_session().unwrap().mode, "control");
    assert_eq!(rig_b.host.hub().active_session().unwrap().mode, "view");

    rig_a.host.stop();
    rig_b.host.stop();
    println!("[BIDIR-TEST] Cift yonlu paralel oturum basariyla calisti!");
    Ok(())
}

#[test]
fn netstat_no_port_when_disabled_and_cleared_on_stop() -> DeskResult<()> {
    println!("[PORT-TEST] Starting in-app host on loopback...");
    let rig = TestHostRig::start()?;
    let port = rig.port;
    println!("[PORT-TEST] Listening on loopback port {port}. Verifying loopback connectivity...");

    // Connect succeeds when enabled
    assert!(connect(PeerTarget::loopback(port)).is_ok());

    println!("[PORT-TEST] Stopping host...");
    rig.host.stop();

    // Connect fails immediately when stopped (port is closed and released)
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        connect(PeerTarget::loopback(port)).is_err(),
        "Port must NOT accept connections when host is stopped"
    );
    println!("[PORT-TEST] Verified: Port closed and released on stop. Kapaliyken port yok!");
    Ok(())
}
