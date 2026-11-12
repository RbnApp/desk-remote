//! Desk-Remote session helper: runs in the logged-in user's session.
//!
//! Captures the screen, injects input, and shows the tray icon + pairing-code window. All
//! networking, pairing and trust stay in the SYSTEM service; this process only talks to it
//! over a named pipe.

#![cfg(windows)]
#![windows_subsystem = "windows"]

mod about;
mod inbox;
mod link;
mod menu;
mod state;
mod ui;
mod viewers;

use desk_logger::{LogLevel, LoggerConfig};
use desk_paths::DeskPaths;
use state::Shared;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn data_dir_arg() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--data-dir" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

fn main() -> std::process::ExitCode {
    // Physical pixels everywhere: capture, injection and the monitor rectangle must agree.
    // SAFETY: process-wide setting made before any window exists.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware() };
    let paths = match data_dir_arg() {
        Some(dir) => DeskPaths::custom(dir.join("system"), dir.join("user")),
        None => desk_paths::canonical(),
    };
    let _guard = desk_logger::init(&LoggerConfig {
        level: LogLevel::Info,
        log_dir: Some(paths.user_log_dir()),
        file_name_prefix: "desk-session-helper".to_string(),
        console_output: false,
        json_format: false,
    })
    .ok()
    .flatten();

    let shared = Arc::new(Shared::default());
    let done = Arc::new(AtomicBool::new(false));
    let worker = {
        let (shared, paths, done) = (Arc::clone(&shared), paths.clone(), Arc::clone(&done));
        std::thread::Builder::new()
            .name("desk-helper-link".to_string())
            .spawn(move || {
                if let Err(e) = link::run(&shared, &paths, &done) {
                    tracing::error!(error = %e, "helper link failed");
                }
                ui::quit(&shared);
            })
    };
    if let Err(e) = worker {
        tracing::error!(error = %e, "cannot start helper link thread");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(e) = ui::run(shared, &paths) {
        tracing::error!(error = %e, "tray UI failed");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
