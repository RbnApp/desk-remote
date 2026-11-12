//! Debug-only stand-in for the file-transfer commands (DAPP-85 owns the real ones; the contract
//! is the same). Compiled only with `--features demo`, never shipped. Fake files, fake
//! progress, fake outcomes picked by the file name so every UI state can be reached.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

const MIB: u64 = 1024 * 1024;
const DEFAULT_LIMIT_MB: u64 = 100;
const MAX_LIMIT_MB: u64 = 1024;
const STEPS: u64 = 40;
const STEP_DELAY: Duration = Duration::from_millis(120);

/// Demo files offered by the "pick" test hook: (path, size in bytes).
const PICK_FILES: &[(&str, u64)] = &[
    ("C:\\Users\\demo\\Documents\\Quarterly-Report.pdf", 4 * MIB),
    ("C:\\Users\\demo\\Pictures\\Site-Photos.zip", 38 * MIB),
    (
        "C:\\Users\\demo\\Videos\\Training-Recording-big.mp4",
        250 * MIB,
    ),
    ("C:\\Users\\demo\\Documents\\Project Archive", 0),
];

#[derive(Serialize, Clone)]
pub struct FileEntry {
    path: String,
    name: String,
    size: u64,
    is_dir: bool,
}

#[derive(Serialize)]
pub struct Limits {
    max_upload_mb: u64,
}

struct Inner {
    limit_mb: u64,
    shell_menu: bool,
    next_id: u64,
    cancels: HashMap<String, Arc<AtomicBool>>,
}

pub struct DemoTransfer(Mutex<Inner>);

impl DemoTransfer {
    pub fn new() -> Self {
        Self(Mutex::new(Inner {
            limit_mb: DEFAULT_LIMIT_MB,
            shell_menu: true,
            next_id: 1,
            cancels: HashMap::new(),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn base_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

fn entry(path: &str) -> FileEntry {
    let normalized = path.replace('/', "\\");
    let known = PICK_FILES.iter().find(|(p, _)| *p == normalized);
    let name = base_name(path);
    let is_dir = !name.contains('.');
    let size = known.map_or(2 * MIB, |(_, s)| *s);
    FileEntry {
        path: path.to_string(),
        name,
        size,
        is_dir,
    }
}

#[tauri::command]
pub fn transfer_inspect(paths: Vec<String>) -> Vec<FileEntry> {
    paths.iter().map(|p| entry(p)).collect()
}

#[tauri::command]
pub fn transfer_pick_files() -> Vec<String> {
    vec![PICK_FILES[0].0.to_string(), PICK_FILES[1].0.to_string()]
}

#[tauri::command]
pub fn transfer_limits(state: State<'_, DemoTransfer>) -> Limits {
    Limits {
        max_upload_mb: state.lock().limit_mb,
    }
}

#[tauri::command]
pub fn transfer_set_limit(
    state: State<'_, DemoTransfer>,
    max_upload_mb: u64,
) -> Result<(), String> {
    if !(1..=MAX_LIMIT_MB).contains(&max_upload_mb) {
        return Err("invalid_limit".to_string());
    }
    state.lock().limit_mb = max_upload_mb;
    Ok(())
}

#[tauri::command]
pub fn shell_menu_status(state: State<'_, DemoTransfer>) -> bool {
    state.lock().shell_menu
}

#[tauri::command]
pub fn shell_menu_set(state: State<'_, DemoTransfer>, enabled: bool) -> bool {
    state.lock().shell_menu = enabled;
    enabled
}

/// Error a demo file ends with, chosen by its name (`None` = success).
fn planned_error(name: &str, size: u64, limit_mb: u64) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if size > limit_mb * MIB {
        Some("too_large")
    } else if lower.contains("denied") {
        Some("denied")
    } else if lower.contains("noanswer") {
        Some("no_answer")
    } else if lower.contains("busy") {
        Some("busy")
    } else if lower.contains("disk") {
        Some("disk_full")
    } else if lower.contains("sha") {
        Some("sha_mismatch")
    } else {
        None
    }
}

#[tauri::command]
pub fn transfer_send(
    app: AppHandle,
    state: State<'_, DemoTransfer>,
    device_id: String,
    paths: Vec<String>,
) -> Vec<String> {
    tracing::debug!(%device_id, count = paths.len(), "demo: transfer send");
    let limit_mb = state.lock().limit_mb;
    let mut ids = Vec::new();
    for path in paths {
        let file = entry(&path);
        let cancel = Arc::new(AtomicBool::new(false));
        let id = {
            let mut inner = state.lock();
            let id = format!("tx-{}", inner.next_id);
            inner.next_id += 1;
            inner.cancels.insert(id.clone(), cancel.clone());
            id
        };
        ids.push(id.clone());
        let app = app.clone();
        std::thread::spawn(move || run_send(&app, &id, &file, limit_mb, &cancel));
    }
    ids
}

fn run_send(app: &AppHandle, id: &str, file: &FileEntry, limit_mb: u64, cancel: &AtomicBool) {
    let done = |ok: bool, error: Option<&str>| {
        let _ = app.emit(
            "transfer://done",
            serde_json::json!({"transfer_id": id, "name": file.name, "ok": ok, "error": error}),
        );
    };
    if file.is_dir {
        return done(false, Some("unsupported_folder"));
    }
    let plan = planned_error(&file.name, file.size, limit_mb);
    if matches!(plan, Some("too_large" | "no_answer" | "denied" | "busy")) {
        std::thread::sleep(STEP_DELAY * 4);
        return done(false, plan);
    }
    for step in 1..=STEPS {
        std::thread::sleep(STEP_DELAY);
        if cancel.load(Ordering::Relaxed) {
            return done(false, Some("canceled"));
        }
        if plan == Some("disk_full") && step == STEPS / 2 {
            return done(false, plan);
        }
        let sent = file.size * step / STEPS;
        let _ = app.emit(
            "transfer://progress",
            serde_json::json!({"transfer_id": id, "name": file.name, "sent": sent, "total": file.size}),
        );
    }
    done(plan.is_none(), plan);
}

#[tauri::command]
pub fn transfer_cancel(state: State<'_, DemoTransfer>, transfer_id: String) {
    let inner = state.lock();
    if let Some(flag) = inner.cancels.get(&transfer_id) {
        flag.store(true, Ordering::Relaxed);
    }
}

/// The receiving side answers a file offer ("this PC" P2P mode).
#[tauri::command]
pub fn transfer_answer(
    app: AppHandle,
    state: State<'_, DemoTransfer>,
    transfer_id: String,
    accept: bool,
    name: Option<String>,
) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut inner = state.lock();
        if accept {
            inner.cancels.insert(transfer_id.clone(), cancel.clone());
        }
    }
    tracing::debug!(%transfer_id, accept, "demo: file offer answered");
    if !accept {
        return;
    }
    let file = FileEntry {
        path: String::new(),
        name: name.unwrap_or_else(|| "file".to_string()),
        size: 24 * MIB,
        is_dir: false,
    };
    std::thread::spawn(move || run_send(&app, &transfer_id, &file, MAX_LIMIT_MB, &cancel));
}

/// Test hook: the Explorer context menu asks the panel to pick a PC for these files.
#[tauri::command]
pub fn demo_transfer_pick(app: AppHandle, folder: bool) {
    let paths: Vec<&str> = if folder {
        vec![PICK_FILES[3].0]
    } else {
        PICK_FILES.iter().take(3).map(|(p, _)| *p).collect()
    };
    let _ = app.emit("transfer://pick", serde_json::json!({ "paths": paths }));
}

/// Test hook: a remote device offers a file to this PC.
#[tauri::command]
pub fn demo_transfer_offer(app: AppHandle, state: State<'_, DemoTransfer>) {
    let id = {
        let mut inner = state.lock();
        let id = format!("in-{}", inner.next_id);
        inner.next_id += 1;
        id
    };
    let _ = app.emit(
        "transfer://offer",
        serde_json::json!({
            "transfer_id": id,
            "peer_name": "Laptop-Mehmet",
            "name": "Price-List-2027.xlsx",
            "size": 24 * MIB,
        }),
    );
}
