//! The panel's own tailnet node.
//!
//! The panel never needs the official Tailscale client: it runs `desk-tailnet-helper panel`
//! (tsnet, userspace) with its own state folder. The helper reports sign-in state and peers as
//! JSON lines on stdout and serves a loopback bridge protected by a random per-launch token:
//! `CONNECT <token> <tailnet-ip:port>` dials a tailnet address and then pipes bytes both ways,
//! `LOGOUT <token>` signs the node out. An official client on the same PC is never touched.

use crate::tailscale::TailscalePeer;
use crate::{PeerTarget, Session};
use desk_crypto::rand::{fill_random, to_hex};
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long the sign-in button waits for the helper to produce a login page.
const SIGN_IN_WAIT: Duration = Duration::from_secs(20);
/// Longest reply line the bridge may send.
const MAX_REPLY: usize = 256;
/// Time the helper gets to answer a sign-out.
const SIGN_OUT_WAIT: Duration = Duration::from_secs(20);
/// Time to reach the loopback bridge.
const BRIDGE_WAIT: Duration = Duration::from_secs(5);

/// Where the panel node stands.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PanelTailnetState {
    /// Helper not running (never started, signed out, or stopped).
    #[default]
    Off,
    /// Helper running, node not online yet.
    Starting,
    /// Browser sign-in is needed (`auth_url` is set).
    NeedsLogin,
    /// Online on the tailnet.
    Running,
    /// Helper ended unexpectedly or could not start.
    Failed,
}

/// What the Settings screen and the PC list read.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PanelTailnetSnapshot {
    pub state: PanelTailnetState,
    pub auth_url: Option<String>,
    pub host_name: Option<String>,
    pub ips: Vec<String>,
    pub peers: Vec<TailscalePeer>,
}

/// One JSON line printed by `desk-tailnet-helper panel`.
#[derive(Deserialize)]
struct Report {
    status: String,
    #[serde(default)]
    ips: Vec<String>,
    #[serde(default)]
    auth_url: Option<String>,
    #[serde(default)]
    bridge_addr: Option<String>,
    #[serde(default)]
    host_name: Option<String>,
    #[serde(default)]
    peers: Vec<TailscalePeer>,
}

/// Fixed inputs of the panel node.
#[derive(Debug, Clone)]
pub struct PanelTailnetConfig {
    /// `desk-tailnet-helper` executable.
    pub helper: PathBuf,
    /// Node state folder (the login lives here).
    pub state_dir: PathBuf,
    /// Name of this node on the tailnet.
    pub hostname: String,
    /// Control server override (tests); `None` is the Tailscale default.
    pub control_url: Option<String>,
}

#[derive(Default)]
struct Shared {
    snapshot: PanelTailnetSnapshot,
    bridge: Option<SocketAddr>,
}

type SharedState = Arc<(Mutex<Shared>, Condvar)>;

struct Running {
    child: Child,
    /// Held open: the helper exits when this closes.
    _stdin: ChildStdin,
}

/// Handle to the panel node; the helper process ends with it.
pub struct PanelTailnet {
    config: PanelTailnetConfig,
    token: String,
    shared: SharedState,
    process: Mutex<Option<Running>>,
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl PanelTailnet {
    /// Prepares the node; nothing runs until [`PanelTailnet::start`].
    pub fn new(config: PanelTailnetConfig) -> DeskResult<Self> {
        let mut raw = [0u8; 24];
        fill_random(&mut raw)?;
        Ok(Self {
            config,
            token: to_hex(&raw),
            shared: Arc::default(),
            process: Mutex::new(None),
        })
    }

    /// Starts the helper if it is not running (idempotent).
    pub fn start(&self) -> DeskResult<()> {
        let mut process = guard(&self.process);
        if let Some(running) = process.as_mut() {
            if matches!(running.child.try_wait(), Ok(None)) {
                return Ok(());
            }
        }
        if !self.config.helper.exists() {
            self.publish(PanelTailnetState::Failed);
            return Err(DeskError::Transport(format!(
                "desk-tailnet-helper not found at {}",
                self.config.helper.display()
            )));
        }
        DeskPaths::ensure_dir(&self.config.state_dir)?;
        let mut cmd = desk_paths::hidden_command(&self.config.helper);
        cmd.arg("panel")
            .arg("--state-dir")
            .arg(&self.config.state_dir)
            .arg("--hostname")
            .arg(&self.config.hostname)
            .args(["--bridge-addr", "127.0.0.1:0"])
            .env("DESK_BRIDGE_TOKEN", &self.token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(url) = &self.config.control_url {
            cmd.arg("--control-url").arg(url);
        }
        let mut child = cmd.spawn().map_err(DeskError::Io)?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            let _ = child.kill();
            return Err(DeskError::Transport("helper pipes unavailable".to_string()));
        };
        self.publish(PanelTailnetState::Starting);
        let shared = Arc::clone(&self.shared);
        std::thread::Builder::new()
            .name("panel-tailnet".to_string())
            .spawn(move || read_reports(stdout, &shared))
            .map_err(DeskError::Io)?;
        tracing::info!(pid = child.id(), "panel tailnet node started");
        *process = Some(Running {
            child,
            _stdin: stdin,
        });
        Ok(())
    }

    /// Current state, own addresses and tailnet peers.
    #[must_use]
    pub fn snapshot(&self) -> PanelTailnetSnapshot {
        guard(&self.shared.0).snapshot.clone()
    }

    /// Starts the node when needed and returns the browser sign-in URL (`None`: already online).
    pub fn begin_sign_in(&self) -> DeskResult<Option<String>> {
        self.start()?;
        let (lock, cv) = &*self.shared;
        let deadline = Instant::now() + SIGN_IN_WAIT;
        let mut shared = guard(lock);
        loop {
            match shared.snapshot.state {
                PanelTailnetState::Running => return Ok(None),
                PanelTailnetState::NeedsLogin => {
                    if let Some(url) = shared.snapshot.auth_url.clone() {
                        return Ok(Some(url));
                    }
                }
                PanelTailnetState::Failed | PanelTailnetState::Off => {
                    return Err(DeskError::Transport("tailnet node is not running".into()));
                }
                PanelTailnetState::Starting => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(DeskError::Transport("no tailnet sign-in URL yet".into()));
            }
            shared = cv
                .wait_timeout(shared, left)
                .map_err(|_| DeskError::Transport("tailnet state lock poisoned".into()))?
                .0;
        }
    }

    /// Signs the node out of the tailnet; the helper ends and the next sign-in starts it afresh.
    pub fn sign_out(&self) -> DeskResult<()> {
        let mut stream = self.open_bridge(BRIDGE_WAIT)?;
        let reply = self.request(&mut stream, "LOGOUT", None, SIGN_OUT_WAIT)?;
        if reply != "OK" {
            return Err(DeskError::Transport(format!("tailnet sign-out: {reply}")));
        }
        // Off first: the reader must see a deliberate end, not a crash.
        self.publish(PanelTailnetState::Off);
        if let Some(mut running) = guard(&self.process).take() {
            let _ = running.child.wait();
        }
        Ok(())
    }

    /// Opens a [`Session`] to a tailnet address through the node (no OS network adapter involved).
    pub fn connect(&self, target: &PeerTarget, timeout: Duration) -> DeskResult<Session> {
        let mut stream = self.open_bridge(timeout)?;
        let addr = target.socket_addr().to_string();
        let reply = self.request(&mut stream, "CONNECT", Some(&addr), timeout)?;
        if reply != "OK" {
            return Err(DeskError::Transport(format!(
                "tailnet dial failed: {reply}"
            )));
        }
        Session::new(stream)
    }

    fn open_bridge(&self, timeout: Duration) -> DeskResult<TcpStream> {
        let bridge = guard(&self.shared.0)
            .bridge
            .ok_or_else(|| DeskError::Transport("tailnet node is not ready".to_string()))?;
        TcpStream::connect_timeout(&bridge, timeout).map_err(DeskError::Io)
    }

    /// Sends one bridge command and reads the one-line reply byte by byte, so no tunnel data
    /// that follows it is consumed.
    fn request(
        &self,
        stream: &mut TcpStream,
        command: &str,
        argument: Option<&str>,
        timeout: Duration,
    ) -> DeskResult<String> {
        stream
            .set_read_timeout(Some(timeout))
            .map_err(DeskError::Io)?;
        let line = match argument {
            Some(arg) => format!("{command} {} {arg}\n", self.token),
            None => format!("{command} {}\n", self.token),
        };
        stream.write_all(line.as_bytes()).map_err(DeskError::Io)?;
        let mut reply = Vec::new();
        let mut byte = [0u8; 1];
        while reply.len() < MAX_REPLY {
            stream.read_exact(&mut byte).map_err(DeskError::Io)?;
            if byte[0] == b'\n' {
                break;
            }
            reply.push(byte[0]);
        }
        Ok(String::from_utf8_lossy(&reply).trim().to_string())
    }

    /// Replaces the snapshot with an empty one in `state` and wakes sign-in waiters.
    fn publish(&self, state: PanelTailnetState) {
        set_empty(&self.shared, state);
    }
}

fn set_empty(shared: &SharedState, state: PanelTailnetState) {
    let (lock, cv) = &**shared;
    let mut inner = guard(lock);
    inner.snapshot = PanelTailnetSnapshot {
        state,
        ..PanelTailnetSnapshot::default()
    };
    if state != PanelTailnetState::Starting {
        inner.bridge = None;
    }
    cv.notify_all();
}

/// Reads helper reports until the helper ends, then marks the node stopped.
fn read_reports(stdout: ChildStdout, shared: &SharedState) {
    let (lock, cv) = &**shared;
    let reports =
        serde_json::Deserializer::from_reader(BufReader::new(stdout)).into_iter::<Report>();
    for report in reports.map_while(Result::ok) {
        let mut inner = guard(lock);
        if let Some(addr) = report.bridge_addr.as_deref().and_then(|a| a.parse().ok()) {
            inner.bridge = Some(addr);
        }
        let next = match report.status.as_str() {
            "running" => PanelTailnetState::Running,
            "needs_auth" => PanelTailnetState::NeedsLogin,
            "error" => PanelTailnetState::Failed,
            _ => PanelTailnetState::Starting,
        };
        inner.snapshot = PanelTailnetSnapshot {
            state: next,
            auth_url: report.auth_url,
            host_name: report.host_name,
            ips: report.ips,
            peers: report.peers,
        };
        cv.notify_all();
    }
    // A deliberate sign-out already moved the state to Off; anything else is a crash.
    let ended_by_itself = guard(lock).snapshot.state != PanelTailnetState::Off;
    if ended_by_itself {
        tracing::info!("panel tailnet node ended");
        set_empty(shared, PanelTailnetState::Failed);
    }
}

impl Drop for PanelTailnet {
    fn drop(&mut self) {
        if let Some(mut running) = guard(&self.process).take() {
            let _ = running.child.kill();
            let _ = running.child.wait();
        }
    }
}
