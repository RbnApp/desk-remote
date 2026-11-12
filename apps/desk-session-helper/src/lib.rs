//! Desk-Remote session helper: runs in the logged-in user's session.
//!
//! Captures the screen, injects input, and shows the tray icon + pairing-code window. All
//! networking, pairing and trust stay in the SYSTEM service; this process only talks to it
//! over a named pipe.
//!
//! This is a library: the agent executable (`apps/desk-service`) starts it as its `helper`
//! subcommand, so the whole agent ships as one file.

#![cfg(windows)]

mod about;
mod code;
mod dialog;
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

/// Helper options from the arguments after the `helper` subcommand.
#[derive(Debug, Default, PartialEq, Eq)]
struct HelperArgs {
    data_dir: Option<PathBuf>,
    /// Portable test mode: the tray menu gets a "Quit" entry that stops the whole agent.
    portable: bool,
}

fn parse_args(args: &[String]) -> HelperArgs {
    let mut parsed = HelperArgs::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--data-dir" => parsed.data_dir = it.next().map(PathBuf::from),
            "--portable" => parsed.portable = true,
            _ => {}
        }
    }
    parsed
}

/// Asks the helper of this user session to open its pairing-code window. `false` when no
/// helper is running.
#[must_use]
pub fn show_running_code() -> bool {
    ui::request_show_code()
}

/// Runs the helper until its tray UI ends; `args` are the arguments after `helper`.
pub fn run(args: &[String]) -> std::process::ExitCode {
    let args = parse_args(args);
    ui::set_portable(args.portable);
    // Physical pixels everywhere: capture, injection and the monitor rectangle must agree.
    // SAFETY: process-wide setting made before any window exists.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware() };
    let paths = match args.data_dir {
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
