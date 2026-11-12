//! Demo-build viewer commands for several PCs at once (DAPP-92 command contract).
//!
//! Compiled only with `--features demo`. Each connected PC gets its own looping sample stream,
//! its own event channel and an activity flag; a background session sends nothing. Counters and
//! an event log let a test prove which PC got which frame, input and activity change.

use crate::backend::demo::{codec_string, split_access_units, stream, SAMPLE};
use crate::backend::{InputEvent, PcStatus};
use crate::commands::{channel_sink, AppState};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

const LOG_LIMIT: usize = 400;

struct Entry {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    inputs: u64,
    keyframe_requests: u64,
}

/// Open demo sessions by PC id, plus the event log.
#[derive(Default)]
pub struct DemoViewers {
    sessions: Mutex<HashMap<String, Entry>>,
    frames: Mutex<HashMap<String, Arc<AtomicU64>>>,
    log: Mutex<Vec<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DemoViewers {
    pub fn new() -> Self {
        Self::default()
    }

    fn note(&self, line: String) {
        let mut log = lock(&self.log);
        if log.len() >= LOG_LIMIT {
            log.remove(0);
        }
        log.push(line);
    }
}

#[derive(Serialize)]
pub struct SessionInfo {
    pc_id: String,
    state: &'static str,
    active: bool,
}

#[derive(Serialize)]
pub struct SessionStats {
    pc_id: String,
    active: bool,
    frames_sent: u64,
    inputs: u64,
    keyframe_requests: u64,
}

fn close_entry(entry: &Entry) {
    entry.stop.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub async fn viewer_connect(
    state: State<'_, AppState>,
    demo: State<'_, DemoViewers>,
    pc_id: String,
    events: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    let paired = lock(&state.config).paired_pcs.clone();
    let row = state
        .backend
        .list(&paired)
        .into_iter()
        .find(|pc| pc.id == pc_id)
        .ok_or_else(|| "unpaired".to_string())?;
    if !matches!(row.status, PcStatus::Online | PcStatus::Warning) {
        return Err("network".to_string());
    }
    let units = split_access_units(SAMPLE);
    let codec = codec_string(&units).ok_or_else(|| "network".to_string())?;
    let (stop, paused) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let counter = Arc::new(AtomicU64::new(0));
    let base = channel_sink(events);
    let sink: crate::backend::EventSink = {
        let counter = Arc::clone(&counter);
        Arc::new(move |event| {
            if matches!(event, crate::backend::SessionEvent::Frame { .. }) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            base(event);
        })
    };
    if let Some(old) = lock(&demo.sessions).insert(
        pc_id.clone(),
        Entry {
            stop: Arc::clone(&stop),
            paused: Arc::clone(&paused),
            inputs: 0,
            keyframe_requests: 0,
        },
    ) {
        close_entry(&old);
    }
    lock(&demo.frames).insert(pc_id.clone(), counter);
    demo.note(format!("connect {pc_id}"));
    std::thread::Builder::new()
        .name(format!("demo-session-{pc_id}"))
        .spawn(move || stream(&units, &codec, &sink, &stop, &paused))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn viewer_disconnect(demo: State<'_, DemoViewers>, pc_id: String) {
    if let Some(entry) = lock(&demo.sessions).remove(&pc_id) {
        close_entry(&entry);
        demo.note(format!("disconnect {pc_id}"));
    }
}

#[tauri::command]
pub fn viewer_input(demo: State<'_, DemoViewers>, pc_id: String, event: InputEvent) {
    let kind = match event {
        InputEvent::MouseMove { .. } => "mouse_move",
        InputEvent::MouseButton { .. } => "mouse_button",
        InputEvent::Wheel { .. } => "wheel",
        InputEvent::Key { .. } => "key",
    };
    if let Some(entry) = lock(&demo.sessions).get_mut(&pc_id) {
        entry.inputs += 1;
        demo.note(format!("input {pc_id} {kind}"));
    }
}

#[tauri::command]
pub fn viewer_request_keyframe(demo: State<'_, DemoViewers>, pc_id: String) {
    if let Some(entry) = lock(&demo.sessions).get_mut(&pc_id) {
        entry.keyframe_requests += 1;
    }
}

#[tauri::command]
pub fn viewer_set_active(demo: State<'_, DemoViewers>, pc_id: String, active: bool) {
    if let Some(entry) = lock(&demo.sessions).get(&pc_id) {
        entry.paused.store(!active, Ordering::Relaxed);
        demo.note(format!("set_active {pc_id} {active}"));
    }
}

#[tauri::command]
pub fn viewer_sessions(demo: State<'_, DemoViewers>) -> Vec<SessionInfo> {
    lock(&demo.sessions)
        .iter()
        .map(|(pc_id, entry)| SessionInfo {
            pc_id: pc_id.clone(),
            state: if entry.stop.load(Ordering::Relaxed) {
                "closed"
            } else {
                "live"
            },
            active: !entry.paused.load(Ordering::Relaxed),
        })
        .collect()
}

/// Test hook: ends one PC's link like a network drop (its channel receives `closed`).
#[tauri::command]
pub fn demo_viewer_drop(demo: State<'_, DemoViewers>, pc_id: String) {
    if let Some(entry) = lock(&demo.sessions).get(&pc_id) {
        close_entry(entry);
        demo.note(format!("drop {pc_id}"));
    }
}

/// Test hook: the event log, oldest first.
#[tauri::command]
pub fn demo_viewer_log(demo: State<'_, DemoViewers>) -> Vec<String> {
    lock(&demo.log).clone()
}

/// Test hook: per-PC frame, input and keyframe-request counters.
#[tauri::command]
pub fn demo_viewer_stats(demo: State<'_, DemoViewers>) -> Vec<SessionStats> {
    let frames = lock(&demo.frames);
    lock(&demo.sessions)
        .iter()
        .map(|(pc_id, entry)| SessionStats {
            pc_id: pc_id.clone(),
            active: !entry.paused.load(Ordering::Relaxed),
            frames_sent: frames
                .get(pc_id)
                .map_or(0, |counter| counter.load(Ordering::Relaxed)),
            inputs: entry.inputs,
            keyframe_requests: entry.keyframe_requests,
        })
        .collect()
}
