//! Desk-Remote operator desktop management panel entry point.

// Release builds run as a GUI app without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod commands;
pub mod incoming;
#[cfg(feature = "demo")]
mod incoming_demo;
mod sessions;
mod shell_menu;
#[cfg(not(feature = "demo"))]
mod transfer;
#[cfg(feature = "demo")]
mod transfer_demo;

use commands::AppState;
use desk_config::{ConfigManager, PanelConfig};
use shell_menu::PickCollector;
use std::sync::{Arc, Mutex};
use tauri::Manager;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let config = desk_logger::LoggerConfig {
        file_name_prefix: "desk-panel".to_string(),
        ..Default::default()
    };
    let _guard = desk_logger::init(&config)?;

    if shell_menu::handle_cli_removal(&args) || shell_menu::handle_cli_enable(&args) {
        return Ok(());
    }

    let paths = desk_paths::DeskPaths::default();
    let config_path = paths.panel_config_path();
    let mut panel_config: PanelConfig = ConfigManager::load_or_default(&config_path)?;
    panel_config.sanitize();
    tracing::info!(path = %config_path.display(), "panel configuration loaded");

    let loopback = std::env::args().any(|arg| arg == "--loopback");
    let incoming = Arc::new(incoming::IncomingManager::new(paths, loopback));
    incoming.set_max_upload_mb(panel_config.max_upload_mb);
    let picks = PickCollector::new();
    let (menu_enabled, menu_language) = (panel_config.shell_menu, panel_config.language.clone());

    let state = AppState {
        backend: backend::select(),
        config_path,
        config: Mutex::new(panel_config),
        sessions: Arc::default(),
        incoming,
        picks: Arc::clone(&picks),
        #[cfg(not(feature = "demo"))]
        transfers: Arc::default(),
    };

    // One panel only: a second `--send` start hands its paths to the running panel and exits.
    let single_picks = Arc::clone(&picks);
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            move |app, argv, _cwd| match shell_menu::parse_send_args(&argv) {
                Some(paths) => single_picks.push(app, paths),
                None => shell_menu::bring_to_front(app),
            },
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .on_page_load({
            let picks = Arc::clone(&picks);
            move |webview, payload| {
                if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                    picks.claim_after_grace(webview.app_handle());
                }
            }
        })
        .setup(move |app| {
            if let Some(paths) = shell_menu::parse_send_args(&args) {
                picks.push(app.handle(), paths);
            }
            std::thread::spawn(move || shell_menu::sync(menu_enabled, &menu_language));
            Ok(())
        })
        .manage(state);
    #[cfg(feature = "demo")]
    let builder = builder
        .manage(incoming_demo::DemoIncoming::new())
        .manage(transfer_demo::DemoTransfer::new());

    #[cfg(feature = "demo")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_info,
        commands::get_catalog,
        commands::get_settings,
        commands::update_settings,
        commands::get_theme,
        commands::shell_menu_status,
        commands::shell_menu_set,
        commands::shell_pick_take,
        commands::palette_previews,
        commands::list_pcs,
        commands::discover_pcs,
        commands::pair_pc,
        commands::viewer_connect,
        commands::viewer_disconnect,
        commands::viewer_input,
        commands::viewer_request_keyframe,
        commands::viewer_set_active,
        commands::viewer_sessions,
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
        transfer_demo::transfer_inspect,
        transfer_demo::transfer_pick_files,
        transfer_demo::transfer_limits,
        transfer_demo::transfer_set_limit,
        transfer_demo::transfer_send,
        transfer_demo::transfer_cancel,
        transfer_demo::transfer_answer,
        transfer_demo::demo_transfer_pick,
        transfer_demo::demo_transfer_offer,
    ]);
    #[cfg(not(feature = "demo"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_info,
        commands::get_catalog,
        commands::get_settings,
        commands::update_settings,
        commands::get_theme,
        commands::shell_menu_status,
        commands::shell_menu_set,
        commands::shell_pick_take,
        commands::palette_previews,
        commands::list_pcs,
        commands::discover_pcs,
        commands::pair_pc,
        commands::viewer_connect,
        commands::viewer_disconnect,
        commands::viewer_input,
        commands::viewer_request_keyframe,
        commands::viewer_set_active,
        commands::viewer_sessions,
        commands::tailscale_status,
        commands::tailscale_connect,
        commands::demo_drop,
        commands::demo_notice,
        commands::demo_connect_failure,
        commands::incoming_get_state,
        commands::incoming_set_enabled,
        commands::incoming_new_code,
        commands::incoming_answer,
        commands::incoming_disconnect,
        commands::incoming_set_mode,
        commands::transfer_send,
        commands::transfer_cancel,
        commands::transfer_limits,
        commands::transfer_set_limit,
        commands::transfer_inspect,
        commands::transfer_pick_files,
        commands::transfer_answer,
    ]);

    builder.run(tauri::generate_context!())?;
    Ok(())
}
