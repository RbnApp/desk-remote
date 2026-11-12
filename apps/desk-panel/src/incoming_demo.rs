//! Debug-only stand-in for the "this PC accepts connections" commands (DAPP-81 owns the real
//! ones; the contract is the same). Compiled only with `--features demo`, never shipped.

use serde::Serialize;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

const CODE_TTL: Duration = Duration::from_secs(120);
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Clone, Serialize)]
pub struct RequestInfo {
    request_id: String,
    peer_name: String,
    fingerprint: String,
    known: bool,
}

#[derive(Clone, Serialize)]
pub struct SessionInfo {
    peer_name: String,
    fingerprint: String,
    /// `"view"` or `"control"`.
    mode: String,
}

#[derive(Serialize)]
pub struct IncomingState {
    enabled: bool,
    pc_name: String,
    code: String,
    code_ttl_s: u64,
    addresses: Vec<String>,
    pending: Option<RequestInfo>,
    session: Option<SessionInfo>,
}

struct Inner {
    enabled: bool,
    code: String,
    deadline: Instant,
    counter: u64,
    pending: Option<RequestInfo>,
    session: Option<SessionInfo>,
}

pub struct DemoIncoming(Mutex<Inner>);

impl DemoIncoming {
    pub fn new() -> Self {
        Self(Mutex::new(Inner {
            enabled: false,
            code: make_code(1),
            deadline: Instant::now() + CODE_TTL,
            counter: 1,
            pending: None,
            session: None,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn make_code(seed: u64) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut x = nanos ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = String::new();
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        out.push(char::from(ALPHABET[(x >> 33) as usize % ALPHABET.len()]));
    }
    out
}

fn snapshot(inner: &mut Inner) -> IncomingState {
    if Instant::now() >= inner.deadline {
        inner.counter += 1;
        inner.code = make_code(inner.counter);
        inner.deadline = Instant::now() + CODE_TTL;
    }
    IncomingState {
        enabled: inner.enabled,
        pc_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "DESKTOP-DEMO".to_string()),
        code: inner.code.clone(),
        code_ttl_s: inner
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs(),
        addresses: vec![
            "192.168.1.20:47800".to_string(),
            "100.64.0.7:47800".to_string(),
        ],
        pending: inner.pending.clone(),
        session: inner.session.clone(),
    }
}

#[tauri::command]
pub fn incoming_get_state(state: State<'_, DemoIncoming>) -> IncomingState {
    snapshot(&mut state.lock())
}

#[tauri::command]
pub fn incoming_set_enabled(
    app: AppHandle,
    state: State<'_, DemoIncoming>,
    enabled: bool,
) -> IncomingState {
    let mut inner = state.lock();
    inner.enabled = enabled;
    if !enabled {
        inner.pending = None;
        if inner.session.take().is_some() {
            let _ = app.emit("incoming://ended", serde_json::json!({"reason":"host"}));
        }
    }
    snapshot(&mut inner)
}

#[tauri::command]
pub fn incoming_new_code(state: State<'_, DemoIncoming>) -> IncomingState {
    let mut inner = state.lock();
    inner.counter += 1;
    inner.code = make_code(inner.counter);
    inner.deadline = Instant::now() + CODE_TTL;
    snapshot(&mut inner)
}

#[tauri::command]
pub fn incoming_answer(
    app: AppHandle,
    state: State<'_, DemoIncoming>,
    request_id: String,
    decision: String,
    always_allow: bool,
) {
    let mut inner = state.lock();
    let Some(request) = inner.pending.take().filter(|r| r.request_id == request_id) else {
        return;
    };
    tracing::debug!(always_allow, %decision, "demo: incoming answer");
    if decision == "view" || decision == "control" {
        let session = SessionInfo {
            peer_name: request.peer_name,
            fingerprint: request.fingerprint,
            mode: decision,
        };
        inner.session = Some(session.clone());
        let _ = app.emit("incoming://session", session);
    }
}

#[tauri::command]
pub fn incoming_disconnect(app: AppHandle, state: State<'_, DemoIncoming>) {
    if state.lock().session.take().is_some() {
        let _ = app.emit("incoming://ended", serde_json::json!({"reason":"host"}));
    }
}

#[tauri::command]
pub fn incoming_set_mode(app: AppHandle, state: State<'_, DemoIncoming>, mode: String) {
    let mut inner = state.lock();
    if let Some(session) = inner.session.as_mut() {
        session.mode = if mode == "control" { "control" } else { "view" }.to_string();
        let _ = app.emit("incoming://session", session.clone());
    }
}

/// Test hook: a remote device asks to connect (`known` = already trusted).
#[tauri::command]
pub fn demo_incoming_request(app: AppHandle, state: State<'_, DemoIncoming>, known: bool) {
    let mut inner = state.lock();
    if !inner.enabled || inner.session.is_some() {
        return;
    }
    inner.counter += 1;
    let request = RequestInfo {
        request_id: format!("req-{}", inner.counter),
        peer_name: if known {
            "Office-Accounting-01"
        } else {
            "Laptop-Mehmet"
        }
        .to_string(),
        fingerprint: "7F3A-91C2-D40E-55B8".to_string(),
        known,
    };
    inner.pending = Some(request.clone());
    let _ = app.emit("incoming://request", request);
}

/// Test hook: the remote device went away.
#[tauri::command]
pub fn demo_incoming_peer_left(app: AppHandle, state: State<'_, DemoIncoming>) {
    if state.lock().session.take().is_some() {
        let _ = app.emit("incoming://ended", serde_json::json!({"reason":"peer"}));
    }
}
