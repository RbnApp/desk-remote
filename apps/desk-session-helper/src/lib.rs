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
    /// SYSTEM helper for the secure desktop (UAC prompt, lock screen): no UI, picture and input only.
    secure_desktop: bool,
}

fn parse_args(args: &[String]) -> HelperArgs {
    let mut parsed = HelperArgs::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--data-dir" => parsed.data_dir = it.next().map(PathBuf::from),
            "--portable" => parsed.portable = true,
            desk_paths::SECURE_DESKTOP_FLAG => parsed.secure_desktop = true,
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

/// Entry point shared by every binary that can be started as the helper (`desk-service`, and
/// `desk-panel` for the panel-in-app host): when this process was launched with the `helper`
/// subcommand it runs the helper and returns its exit code, otherwise `None`. Call it first in
/// `main`, before any logger or single-instance setup — the helper owns its own.
#[must_use]
pub fn run_if_requested() -> Option<std::process::ExitCode> {
    if std::env::args().nth(1).as_deref() != Some(desk_paths::HELPER_SUBCOMMAND) {
        return None;
    }
    let rest: Vec<String> = std::env::args().skip(2).collect();
    Some(run(&rest))
}

/// Runs the helper until its tray UI ends; `args` are the arguments after `helper`.
pub fn run(args: &[String]) -> std::process::ExitCode {
    let args = parse_args(args);
    // Physical pixels everywhere: capture, injection and the monitor rectangle must agree.
    // System-aware would win over the later per-monitor call and skew every non-primary monitor.
    desk_capture::ensure_per_monitor_dpi_aware();
    let paths = match args.data_dir {
        Some(dir) if args.portable => DeskPaths::portable(dir),
        Some(dir) => DeskPaths::custom(dir.join("system"), dir.join("user")),
        None => desk_paths::canonical(),
    };
    let _guard = desk_logger::init(&LoggerConfig {
        level: LogLevel::Info,
        log_dir: Some(paths.user_log_dir()),
        file_name_prefix: if args.secure_desktop {
            "desk-secure-helper"
        } else {
            "desk-session-helper"
        }
        .to_string(),
        console_output: false,
        json_format: false,
    })
    .ok()
    .flatten();

    if args.secure_desktop {
        let done = Arc::new(AtomicBool::new(false));
        return match link::run_secure(&paths, &done) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(error = %e, "secure-desktop helper failed");
                std::process::ExitCode::FAILURE
            }
        };
    }
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
