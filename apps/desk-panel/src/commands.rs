//! Tauri commands: the only bridge between the web UI and Rust.

use crate::backend::{Backend, Candidate, InputEvent, PcInfo, SessionEvent};
use crate::sessions::{SessionInfo, Sessions};
use desk_config::{ConfigManager, PairedPc, PanelConfig};
use desk_theme::{ResolvedTheme, ThemeMode};
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
    pub sessions: Arc<Sessions>,
    #[cfg_attr(feature = "demo", allow(dead_code))]
    pub incoming: Arc<crate::incoming::IncomingManager>,
    pub picks: Arc<crate::shell_menu::PickCollector>,
    #[cfg(not(feature = "demo"))]
    pub transfers: Arc<crate::transfer::TransferManager>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Static facts about this build.
#[derive(Serialize)]
pub struct AppInfo {
    version: &'static str,
    demo: bool,
    /// The PC agent service is registered on this panel PC (misplaced; the UI shows a banner).
    agent_on_pc: bool,
}

/// Tailscale state shown in the sidebar and the settings screen.
#[derive(Serialize)]
pub struct TailscaleInfo {
    /// `off`, `starting`, `needs_login`, `running` or `failed`.
    state: desk_transport::PanelTailnetState,
    host: Option<String>,
    address: Option<String>,
    peers: usize,
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

/// The panel never owns the agent: an installed agent service on this PC is a misplacement.
/// The service itself is left alone (stopping it is an administrator decision).
fn agent_misplaced(service_installed: impl Fn() -> bool) -> bool {
    service_installed()
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: desk_version::CURRENT,
        demo: state.backend.is_demo(),
        agent_on_pc: agent_misplaced(desk_service_core::role::agent_service_installed),
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

/// Explorer right-click entry state for the settings switch.
#[tauri::command]
pub fn shell_menu_status() -> crate::shell_menu::ShellMenuStatus {
    crate::shell_menu::status()
}

/// Turns the Explorer entry on or off and remembers the choice.
#[tauri::command]
pub fn shell_menu_set(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<crate::shell_menu::ShellMenuStatus, String> {
    let mut cfg = lock(&state.config);
    if enabled {
        crate::shell_menu::enable(&cfg.language)?;
    } else {
        crate::shell_menu::disable()?;
    }
    cfg.shell_menu = enabled;
    ConfigManager::save(&state.config_path, &*cfg).map_err(|e| e.to_string())?;
    Ok(crate::shell_menu::status())
}

/// The picker UI calls this once after it registered its `transfer://pick` listener: later
/// picks are emitted live; a pick that arrived during start-up is returned here.
#[tauri::command]
pub fn shell_pick_take(state: State<'_, AppState>) -> Option<crate::shell_menu::PickPayload> {
    state.picks.claim()
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
    // Only this PC's old session is replaced; the other open PCs keep streaming.
    state.sessions.close(&pc_id);
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
            SessionEvent::Status { online } => InvokeResponseBody::Json(
                serde_json::json!({"type":"status","online":online}).to_string(),
            ),
            SessionEvent::Notice { kind } => InvokeResponseBody::Json(
                serde_json::json!({"type":"notice","kind":kind}).to_string(),
            ),
            SessionEvent::CursorShape {
                id,
                hot_x,
                hot_y,
                width,
                height,
                rgba,
            } => InvokeResponseBody::Json(
                serde_json::json!({"type":"cursor_shape","id":id,"hotX":hot_x,"hotY":hot_y,
                    "width":width,"height":height,"rgba":rgba})
                .to_string(),
            ),
            SessionEvent::CursorPos {
                x,
                y,
                shape_id,
                visible,
            } => InvokeResponseBody::Json(
                serde_json::json!({"type":"cursor_pos","x":x,"y":y,"shape":shape_id,"visible":visible})
                    .to_string(),
            ),
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
    state.sessions.insert(&pc_id, session);
    Ok(())
}

/// Leaves the session of `pc_id` (the tab was closed).
#[tauri::command]
pub fn viewer_disconnect(state: State<'_, AppState>, pc_id: String) {
    state.sessions.close(&pc_id);
}

/// Input for the session of `pc_id` only; dropped while that session is paused.
#[tauri::command]
pub fn viewer_input(state: State<'_, AppState>, pc_id: String, event: InputEvent) {
    state.sessions.input(&pc_id, &event);
}

#[tauri::command]
pub fn viewer_request_keyframe(state: State<'_, AppState>, pc_id: String) {
    state.sessions.request_keyframe(&pc_id);
}

/// Background tabs pause their video (`active = false`); the link, pairing and input path stay
/// open. Resuming brings a keyframe at once. The error value is `no_session`.
#[tauri::command]
pub fn viewer_set_active(
    state: State<'_, AppState>,
    pc_id: String,
    active: bool,
) -> Result<(), String> {
    if state.sessions.set_active(&pc_id, active) {
        Ok(())
    } else {
        Err("no_session".to_string())
    }
}

/// Open sessions, to restore the tab bar after a reload.
#[tauri::command]
pub fn viewer_sessions(state: State<'_, AppState>) -> Vec<SessionInfo> {
    state.sessions.list()
}

#[tauri::command]
pub async fn tailscale_status() -> Result<TailscaleInfo, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let snapshot = crate::tailnet::snapshot();
        TailscaleInfo {
            state: snapshot.state,
            host: snapshot.host_name,
            address: snapshot.ips.into_iter().next(),
            peers: snapshot.peers.len(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Starts the panel node's sign-in and opens the login page in the default browser.
#[tauri::command]
pub async fn tailscale_connect() -> Result<(), String> {
    let url = tauri::async_runtime::spawn_blocking(crate::tailnet::begin_sign_in)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if let Some(url) = url {
        tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Signs the panel node out of the tailnet (the official client is never touched).
#[tauri::command]
pub async fn tailscale_disconnect() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(crate::tailnet::sign_out)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Test hook of the demo backend; a no-op elsewhere.
#[tauri::command]
pub fn demo_drop(state: State<'_, AppState>) {
    state.backend.simulate_drop();
}

// ----------------- Incoming Connections (AnyDesk style) -----------------

// The demo build serves these from `incoming_demo`.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn incoming_get_state(state: State<'_, AppState>) -> crate::incoming::IncomingState {
    state.incoming.get_state()
}

// The demo build serves these from `incoming_demo`.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn incoming_set_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<crate::incoming::IncomingState, String> {
    state.incoming.set_enabled(app, enabled)
}

// The demo build serves these from `incoming_demo`.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn incoming_new_code(
    state: State<'_, AppState>,
) -> Result<crate::incoming::IncomingState, String> {
    state.incoming.new_code()
}

// The demo build serves these from `incoming_demo`.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn incoming_answer(
    state: State<'_, AppState>,
    request_id: String,
    decision: String,
    always_allow: bool,
) -> Result<(), String> {
    state.incoming.answer(&request_id, &decision, always_allow)
}

// The demo build serves these from `incoming_demo`.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn incoming_disconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.incoming.disconnect()
}

// The demo build serves these from `incoming_demo`.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn incoming_set_mode(state: State<'_, AppState>, mode: String) -> Result<(), String> {
    state.incoming.set_mode(&mode)
}

// ----------------- File transfer -----------------

/// Emits transfer events to the UI.
#[cfg(not(feature = "demo"))]
fn emitter(app: tauri::AppHandle) -> crate::transfer::Emit {
    use tauri::Emitter;
    Arc::new(move |event, payload| {
        let _ = app.emit(event, payload);
    })
}

/// Sends each path to a paired PC; returns the transfer ids at once, results arrive as
/// `transfer://progress` and `transfer://done` events.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn transfer_send(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    device_id: String,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    let (pc, limit) = {
        let cfg = lock(&state.config);
        let pc = cfg.paired_pcs.iter().find(|p| p.id == device_id).cloned();
        (pc, cfg.max_upload_mb)
    };
    let pc = pc.ok_or_else(|| "unpaired".to_string())?;
    Ok(state
        .transfers
        .send(&state.backend, &pc, paths, limit, &emitter(app)))
}

/// Cancels a queued or running transfer.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn transfer_cancel(state: State<'_, AppState>, transfer_id: String) {
    state.transfers.cancel(&transfer_id);
}

/// Upload limit shown in settings.
#[cfg(not(feature = "demo"))]
#[derive(Serialize)]
pub struct TransferLimits {
    max_upload_mb: u32,
}

#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn transfer_limits(state: State<'_, AppState>) -> TransferLimits {
    TransferLimits {
        max_upload_mb: lock(&state.config).max_upload_mb,
    }
}

/// Saves the limit (1..=1024 MB) and applies it to sending and receiving at once.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn transfer_set_limit(
    state: State<'_, AppState>,
    max_upload_mb: u32,
) -> Result<TransferLimits, String> {
    if !(1..=desk_config::MAX_UPLOAD_MB_CEILING).contains(&max_upload_mb) {
        return Err("invalid_limit".to_string());
    }
    let mut cfg = lock(&state.config);
    cfg.max_upload_mb = max_upload_mb;
    ConfigManager::save(&state.config_path, &*cfg).map_err(|e| e.to_string())?;
    state.incoming.set_max_upload_mb(max_upload_mb);
    Ok(TransferLimits { max_upload_mb })
}

/// Name, size and kind of the chosen paths.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn transfer_inspect(paths: Vec<String>) -> Vec<crate::transfer::FileInfo> {
    crate::transfer::inspect(&paths)
}

/// Opens the native file picker; empty when the user cancels.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub async fn transfer_pick_files(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_files())
            .await
            .map_err(|e| e.to_string())?;
    Ok(picked
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// Accepts or declines an incoming file offer.
#[cfg(not(feature = "demo"))]
#[tauri::command]
pub fn transfer_answer(
    state: State<'_, AppState>,
    transfer_id: String,
    accept: bool,
) -> Result<(), String> {
    state.incoming.transfer_answer(&transfer_id, accept)
}

/// Test hook of the demo backend; a no-op elsewhere.
#[tauri::command]
pub fn demo_notice(state: State<'_, AppState>, kind: String) {
    state.backend.simulate_notice(&kind);
}

/// Test hook of the demo backend; a no-op elsewhere.
#[tauri::command]
pub fn demo_status(state: State<'_, AppState>, online: bool) {
    state.backend.simulate_status(online);
}

/// Test hook of the demo backend; a no-op elsewhere.
#[tauri::command]
pub fn demo_connect_failure(state: State<'_, AppState>, code: String) {
    state.backend.simulate_connect_failure(&code);
}

#[cfg(test)]
mod tests {
    use super::agent_misplaced;

    #[test]
    fn banner_follows_the_service_state() {
        assert!(agent_misplaced(|| true));
        assert!(!agent_misplaced(|| false));
    }
}
