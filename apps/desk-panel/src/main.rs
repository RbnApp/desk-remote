//! Desk-Remote operator desktop management panel entry point.

// Release builds run as a GUI app without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod commands;
#[cfg(feature = "demo")]
mod incoming_demo;

use commands::AppState;
use desk_config::{ConfigManager, PanelConfig};
use std::sync::Mutex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = desk_logger::LoggerConfig {
        file_name_prefix: "desk-panel".to_string(),
        ..Default::default()
    };
    let _guard = desk_logger::init(&config)?;

    let config_path = desk_paths::DeskPaths::default().panel_config_path();
    let mut panel_config: PanelConfig = ConfigManager::load_or_default(&config_path)?;
    panel_config.sanitize();
    tracing::info!(path = %config_path.display(), "panel configuration loaded");

    let state = AppState {
        backend: backend::select(),
        config_path,
        config: Mutex::new(panel_config),
        session: Mutex::new(None),
    };

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state);
    #[cfg(feature = "demo")]
    let builder = builder.manage(incoming_demo::DemoIncoming::new());

    #[cfg(feature = "demo")]
    let builder = builder.invoke_handler(tauri::generate_handler![
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
        commands::demo_notice,
        commands::demo_connect_failure,
        incoming_demo::incoming_get_state,
        incoming_demo::incoming_set_enabled,
        incoming_demo::incoming_new_code,
        incoming_demo::incoming_answer,
        incoming_demo::incoming_disconnect,
        incoming_demo::incoming_set_mode,
        incoming_demo::demo_incoming_request,
        incoming_demo::demo_incoming_peer_left,
    ]);
    #[cfg(not(feature = "demo"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
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
        commands::demo_notice,
        commands::demo_connect_failure,
    ]);

    builder.run(tauri::generate_context!())?;
    Ok(())
}
