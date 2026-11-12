//! Panel tailnet node tests, including the end-to-end run over a local test control server.

use crate::panel_tailnet::{PanelTailnet, PanelTailnetConfig, PanelTailnetState};
use crate::{listen, PeerTarget};
use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_protocol::Message;
use serde::Deserialize;
use std::io::{BufReader, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Kills a child process when the test ends, however it ends.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Deserialize)]
struct AgentReport {
    status: String,
    #[serde(default)]
    ips: Vec<String>,
}

fn helper_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desk-tailnet-helper")
}

fn go(args: &[&str]) -> Command {
    let mut cmd = desk_paths::hidden_command("go");
    cmd.args(args).current_dir(helper_dir());
    cmd
}

/// Agent side: the real helper in `listen` mode, bridging the tailnet port to `host_addr`.
fn start_agent(
    exe: &Path,
    state: &Path,
    control: &str,
    host_addr: SocketAddr,
) -> DeskResult<(Reaper, IpAddr)> {
    let mut child = desk_paths::hidden_command(exe)
        .arg("listen")
        .arg("--state-dir")
        .arg(state)
        .args(["--control-url", control, "--hostname", "deskremote-agent"])
        .args(["--tailnet-port", "47800"])
        .args(["--bridge-addr", &host_addr.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(DeskError::Io)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DeskError::Transport("agent stdout".into()))?;
    let reaper = Reaper(child);
    let reports =
        serde_json::Deserializer::from_reader(BufReader::new(stdout)).into_iter::<AgentReport>();
    for report in reports.map_while(Result::ok) {
        if report.status == "listening" {
            let ip = report
                .ips
                .iter()
                .filter_map(|ip| ip.parse::<IpAddr>().ok())
                .find(IpAddr::is_ipv4)
                .ok_or_else(|| DeskError::Transport("agent has no tailnet IPv4".into()))?;
            return Ok((reaper, ip));
        }
    }
    Err(DeskError::Transport("agent helper never listened".into()))
}

/// Waits until the Go test control server has written its URL.
fn wait_for_control(marker: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(url) = std::fs::read_to_string(marker) {
            if !url.is_empty() {
                return url;
            }
        }
        assert!(
            Instant::now() < deadline,
            "test control server did not start"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// End to end on 127.0.0.1 with no official Tailscale client: a local test control server,
/// the real agent helper (`listen`) in front of a real host session, and the panel node
/// connecting to it and receiving video frames. Needs the Go toolchain:
/// `cargo test -p desk-transport panel_node -- --ignored --nocapture`.
#[test]
#[ignore = "needs the Go toolchain and about a minute"]
fn panel_node_reaches_agent_over_testcontrol() -> DeskResult<()> {
    let tmp = tempfile::tempdir().map_err(DeskError::Io)?;
    let exe = tmp.path().join("desk-tailnet-helper.exe");
    let built = go(&["build", "-o", &exe.to_string_lossy(), "."])
        .status()
        .map_err(DeskError::Io)?;
    assert!(built.success(), "go build failed");

    // Local tailnet control server (Go testcontrol); its URL arrives through a file.
    let marker = tmp.path().join("control.url");
    let control_proc = go(&[
        "test",
        "-run",
        "TestServeTestnet",
        "-count=1",
        "-timeout",
        "150s",
        ".",
    ])
    .env("DESK_TESTNET_SERVE", &marker)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(DeskError::Io)?;
    let _control = Reaper(control_proc);
    let control_url = wait_for_control(&marker);

    // Host: a real encrypted session that streams three frames after pairing.
    let host_id = DeviceIdentity::generate()?;
    let mut pairing = PairingManager::new();
    let code = pairing.create_pairing_code()?;
    let listener = listen(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let host_addr = listener.local_addr()?;
    let host = std::thread::spawn(move || -> DeskResult<()> {
        let pairing = Mutex::new(pairing);
        let mut session = listener.accept()?;
        session.authenticate_as_host(&pairing, &host_id)?;
        session.send(&Message::VideoConfig {
            codec: "h264".to_string(),
            width: 640,
            height: 360,
            fps: 30,
        })?;
        for n in 0..3u64 {
            session.send(&Message::VideoFrame {
                ts_us: n,
                keyframe: n == 0,
                data: vec![0xAB; 4096],
            })?;
        }
        // Keep the session open until the panel has read everything.
        let _ = session.recv_timeout(Duration::from_secs(20));
        Ok(())
    });

    let (_agent, agent_ip) = start_agent(
        &exe,
        &tmp.path().join("agent-state"),
        &control_url,
        host_addr,
    )?;

    // Panel: its own node, no official client anywhere.
    let panel = PanelTailnet::new(PanelTailnetConfig {
        helper: exe,
        state_dir: tmp.path().join("panel-state"),
        hostname: "deskremote-panel".to_string(),
        control_url: Some(control_url),
    })?;
    panel.start()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while panel.snapshot().state != PanelTailnetState::Running {
        assert!(Instant::now() < deadline, "panel node never came online");
        std::thread::sleep(Duration::from_millis(200));
    }
    let snapshot = panel.snapshot();
    assert!(!snapshot.ips.is_empty());

    let target = PeerTarget::Tailscale {
        ip: agent_ip,
        port: 47800,
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut session = loop {
        match panel.connect(&target, Duration::from_secs(12)) {
            Ok(s) => break s,
            Err(e) => {
                assert!(Instant::now() < deadline, "cannot reach the agent: {e}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    };
    let panel_id = DeviceIdentity::generate()?;
    let mut trust = TrustStore::in_memory();
    session.authenticate_as_client(&panel_id, &mut trust, Some(&code))?;
    let mut frames = 0;
    while frames < 3 {
        match session.recv()? {
            Message::VideoFrame { data, .. } => {
                assert_eq!(data.len(), 4096);
                frames += 1;
            }
            Message::VideoConfig { width, .. } => assert_eq!(width, 640),
            other => panic!("unexpected {other:?}"),
        }
    }
    let _ = writeln!(
        std::io::stderr(),
        "PANEL_NODE_E2E frames={frames} agent={agent_ip} panel_ips={:?} peers={}",
        snapshot.ips,
        snapshot.peers.len()
    );
    drop(session);
    host.join()
        .map_err(|_| DeskError::Transport("host thread panicked".into()))??;
    let _ = std::fs::remove_file(&marker);
    Ok(())
}

#[test]
fn a_missing_helper_fails_cleanly() -> DeskResult<()> {
    let tmp = tempfile::tempdir().map_err(DeskError::Io)?;
    let panel = PanelTailnet::new(PanelTailnetConfig {
        helper: tmp.path().join("missing.exe"),
        state_dir: tmp.path().join("state"),
        hostname: "x".to_string(),
        control_url: None,
    })?;
    assert!(panel.start().is_err());
    assert_eq!(panel.snapshot().state, PanelTailnetState::Failed);
    // Not ready: dialing must fail without touching the network.
    let target = PeerTarget::Tailscale {
        ip: IpAddr::from([100, 64, 0, 1]),
        port: 47800,
    };
    assert!(panel.connect(&target, Duration::from_millis(200)).is_err());
    Ok(())
}
