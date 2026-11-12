//! Desk-Remote operator desktop management panel entry point.

// Release builds run as a GUI app without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod commands;
pub mod incoming;

use commands::AppState;
use desk_config::{ConfigManager, PanelConfig};
use std::sync::{Arc, Mutex};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = desk_logger::LoggerConfig {
        file_name_prefix: "desk-panel".to_string(),
        ..Default::default()
    };
    let _guard = desk_logger::init(&config)?;

    let paths = desk_paths::DeskPaths::default();
    let config_path = paths.panel_config_path();
    let mut panel_config: PanelConfig = ConfigManager::load_or_default(&config_path)?;
    panel_config.sanitize();
    tracing::info!(path = %config_path.display(), "panel configuration loaded");

    let incoming = Arc::new(incoming::IncomingManager::new(paths, true));

    let state = AppState {
        backend: backend::select(),
        config_path,
        config: Mutex::new(panel_config),
        session: Mutex::new(None),
        incoming,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_catalog,
            commands::get_settings,
            commands::update_settings,
            commands::get_theme,
            commands::palette_previews,
            commands::list_pcs,
            commands::discover_pcs,
            commands::pair_pc,
            commands::viewer_connect,
            commands::viewer_disconnect,
            commands::viewer_input,
            commands::viewer_request_keyframe,
            commands::tailscale_status,
            commands::tailscale_connect,
            commands::demo_drop,
            commands::incoming_get_state,
            commands::incoming_set_enabled,
            commands::incoming_new_code,
            commands::incoming_answer,
            commands::incoming_disconnect,
            commands::incoming_set_mode,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
