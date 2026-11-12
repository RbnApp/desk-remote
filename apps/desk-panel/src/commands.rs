//! Tauri commands: the only bridge between the web UI and Rust.

use crate::backend::{Backend, Candidate, InputEvent, PcInfo, SessionEvent, ViewerSession};
use desk_config::{ConfigManager, PairedPc, PanelConfig};
use desk_theme::{ResolvedTheme, ThemeMode};
use desk_transport::TailscaleDetector;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

/// Bytes before the Annex-B payload in a binary frame message: keyframe flag + `ts_us` (LE).
const FRAME_HEADER_LEN: usize = 9;

/// Shared application state.
pub struct AppState {
    pub backend: Arc<dyn Backend>,
    pub config_path: PathBuf,
    pub config: Mutex<PanelConfig>,
    pub session: Mutex<Option<Box<dyn ViewerSession>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Static facts about this build.
#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    demo: bool,
}

/// Tailscale state shown in the sidebar and the settings screen.
#[derive(Serialize)]
pub struct TailscaleInfo {
    installed: bool,
    connected: bool,
    state: String,
    host: Option<String>,
    address: Option<String>,
}

/// Light and dark tokens of one palette, for the palette picker previews.
#[derive(Serialize)]
pub struct PalettePreview {
    id: &'static str,
    label_en: String,
    label_tr: String,
    light: ResolvedTheme,
    dark: ResolvedTheme,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: desk_version::CURRENT,
        demo: state.backend.is_demo(),
    }
}

#[tauri::command]
pub fn get_catalog(locale: String) -> serde_json::Value {
    desk_i18n::catalog(desk_i18n::Locale::from_str_loose(&locale))
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> PanelConfig {
    lock(&state.config).clone()
}

#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    palette: Option<String>,
    mode: Option<String>,
    language: Option<String>,
    list_view: Option<String>,
) -> Result<PanelConfig, String> {
    let mut cfg = lock(&state.config);
    if let Some(v) = palette {
        cfg.palette = v;
    }
    if let Some(v) = mode {
        cfg.mode = v;
    }
    if let Some(v) = language {
        cfg.language = v;
    }
    if let Some(v) = list_view {
        cfg.list_view = v;
    }
    cfg.sanitize();
    ConfigManager::save(&state.config_path, &*cfg).map_err(|e| e.to_string())?;
    Ok(cfg.clone())
}

#[tauri::command]
pub fn get_theme(palette: String, mode: String, system_is_dark: bool) -> ResolvedTheme {
    desk_theme::resolve(&palette, ThemeMode::from_str_loose(&mode), system_is_dark)
}

#[tauri::command]
pub fn palette_previews() -> Vec<PalettePreview> {
    desk_theme::available_palettes()
        .into_iter()
        .map(|id| PalettePreview {
            id,
            label_en: desk_theme::palette_label(id, "en"),
            label_tr: desk_theme::palette_label(id, "tr"),
            light: desk_theme::resolve(id, ThemeMode::Light, false),
            dark: desk_theme::resolve(id, ThemeMode::Dark, true),
        })
        .collect()
}

#[tauri::command]
pub fn list_pcs(state: State<'_, AppState>) -> Vec<PcInfo> {
    let paired = lock(&state.config).paired_pcs.clone();
    state.backend.list(&paired)
}

/// PCs that can be paired: mDNS neighbours and online tailnet peers.
#[tauri::command]
pub fn discover_pcs(state: State<'_, AppState>) -> Vec<Candidate> {
    let paired = lock(&state.config).paired_pcs.clone();
    state.backend.discover(&paired)
}

/// Pairs with the PC at `target` using its code; the error value is the `pair.err.*` code.
#[tauri::command]
pub async fn pair_pc(
    state: State<'_, AppState>,
    target: String,
    code: String,
    name: Option<String>,
) -> Result<PairedPc, String> {
    let backend = Arc::clone(&state.backend);
    let pc =
        tauri::async_runtime::spawn_blocking(move || backend.pair(&target, &code, name.as_deref()))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.code().to_string())?;
    let mut cfg = lock(&state.config);
    cfg.paired_pcs.retain(|p| p.id != pc.id);
    cfg.paired_pcs.push(pc.clone());
    ConfigManager::save(&state.config_path, &*cfg).map_err(|e| e.to_string())?;
    Ok(pc)
}

/// Joins a PC and streams `VideoConfig`/`VideoFrame`/`Rtt`/`Closed` into `events`.
///
/// Frames travel as raw bytes (`[keyframe u8][ts_us u64 LE][Annex-B data]`); every other
/// event is a JSON text message with a `type` field.
#[tauri::command]
pub async fn viewer_connect(
    state: State<'_, AppState>,
    pc_id: String,
    events: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    viewer_disconnect(state.clone());
    let pc = lock(&state.config)
        .paired_pcs
        .iter()
        .find(|p| p.id == pc_id)
        .cloned()
        .ok_or_else(|| "unpaired".to_string())?;
    let sink = Arc::new(move |event: SessionEvent| {
        let body = match event {
            SessionEvent::Frame {
                ts_us,
                keyframe,
                data,
            } => {
                let mut buf = Vec::with_capacity(FRAME_HEADER_LEN + data.len());
                buf.push(u8::from(keyframe));
                buf.extend_from_slice(&ts_us.to_le_bytes());
                buf.extend_from_slice(&data);
                InvokeResponseBody::Raw(buf)
            }
            SessionEvent::Config {
                codec,
                fps,
                width,
                height,
            } => InvokeResponseBody::Json(
                serde_json::json!({"type":"config","codec":codec,"fps":fps,"width":width,"height":height})
                    .to_string(),
            ),
            SessionEvent::Rtt { ms } => {
                InvokeResponseBody::Json(serde_json::json!({"type":"rtt","ms":ms}).to_string())
            }
            SessionEvent::Closed => {
                InvokeResponseBody::Json(serde_json::json!({"type":"closed"}).to_string())
            }
        };
        if let Err(e) = events.send(body) {
            tracing::debug!(error = %e, "viewer channel closed");
        }
    });
    let backend = Arc::clone(&state.backend);
    let session = tauri::async_runtime::spawn_blocking(move || backend.connect(&pc, sink))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.code().to_string())?;
    *lock(&state.session) = Some(session);
    Ok(())
}

#[tauri::command]
pub fn viewer_disconnect(state: State<'_, AppState>) {
    if let Some(session) = lock(&state.session).take() {
        session.close();
    }
}

#[tauri::command]
pub fn viewer_input(state: State<'_, AppState>, event: InputEvent) {
    if let Some(session) = lock(&state.session).as_ref() {
        session.send_input(&event);
    }
}

#[tauri::command]
pub fn viewer_request_keyframe(state: State<'_, AppState>) {
    if let Some(session) = lock(&state.session).as_ref() {
        session.request_keyframe();
    }
}

#[tauri::command]
pub async fn tailscale_status() -> Result<TailscaleInfo, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let snapshot = TailscaleDetector::snapshot();
        TailscaleInfo {
            installed: snapshot.status.installed,
            connected: snapshot.status.running,
            state: snapshot.status.backend_state,
            host: snapshot.host_name,
            address: snapshot.status.ips.into_iter().next(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Starts the Tailscale sign-in and opens the login page in the default browser.
#[tauri::command]
pub async fn tailscale_connect() -> Result<(), String> {
    let url = tauri::async_runtime::spawn_blocking(TailscaleDetector::begin_sign_in)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if let Some(url) = url {
        tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Test hook of the demo backend; a no-op elsewhere.
#[tauri::command]
pub fn demo_drop(state: State<'_, AppState>) {
    state.backend.simulate_drop();
}
