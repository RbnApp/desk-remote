//! Desk-Remote PC agent service (runs as SYSTEM, session 0).
//!
//! Usage:
//!   desk-service install                      register + start the Windows service (admin)
//!   desk-service uninstall                    stop + remove the service (admin)
//!   desk-service stop                         stop a running `run --console` instance (no window)
//!   desk-service run                          entry point used by the Service Control Manager
//!   desk-service run --console [--loopback] [--allow-on-panel-pc] [--data-dir D] [--port P] [--run-for SECS]
//!                                             run in the foreground as the current user (dev/test);
//!                                             --loopback: 127.0.0.1 only, no mDNS, no tailnet helper
//!                                             --allow-on-panel-pc: developer override, console only;
//!                                             without it the host never starts on a panel PC

#![cfg(windows)]
// GUI subsystem in release: the binary NEVER opens a console window (not as service, not in
// `run --console`). Console-mode output goes to the log file; `--attach-console` is the only,
// explicit developer way to also print to the launching terminal (attach, never a new window).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use desk_error::{DeskError, DeskResult};
use desk_logger::{LogLevel, LoggerConfig};
use desk_paths::DeskPaths;
use desk_service_core::{HostOptions, LaunchMode, ServiceHost};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default)]
struct Args {
    command: String,
    console: bool,
    attach_console: bool,
    loopback: bool,
    allow_on_panel_pc: bool,
    data_dir: Option<PathBuf>,
    port: Option<u16>,
    run_for: Option<Duration>,
}

fn parse_args() -> DeskResult<Args> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    args.command = it.next().unwrap_or_default();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .ok_or_else(|| DeskError::Config(format!("{name} needs a value")))
        };
        match flag.as_str() {
            "--console" => args.console = true,
            "--attach-console" => args.attach_console = true,
            "--loopback" => args.loopback = true,
            "--allow-on-panel-pc" => args.allow_on_panel_pc = true,
            "--data-dir" => args.data_dir = Some(PathBuf::from(value("--data-dir")?)),
            "--port" => {
                args.port = Some(
                    value("--port")?
                        .parse()
                        .map_err(|e| DeskError::Config(format!("--port: {e}")))?,
                );
            }
            "--run-for" => {
                let secs: u64 = value("--run-for")?
                    .parse()
                    .map_err(|e| DeskError::Config(format!("--run-for: {e}")))?;
                args.run_for = Some(Duration::from_secs(secs));
            }
            other => return Err(DeskError::Config(format!("unknown argument {other}"))),
        }
    }
    Ok(args)
}

fn paths_for(args: &Args) -> DeskPaths {
    match &args.data_dir {
        Some(dir) => DeskPaths::custom(dir.join("system"), dir.join("user")),
        None => desk_paths::canonical(),
    }
}

fn run(args: &Args) -> DeskResult<()> {
    let paths = paths_for(args);
    match args.command.as_str() {
        "install" => {
            // Role check first: it needs no administrator rights and no UAC prompt.
            desk_service_core::role::refuse_install_on_panel_pc(&paths)?;
            desk_service_core::install(&DeskPaths::sibling_exe("desk-service.exe")?)?;
            tracing::info!("service installed and started");
            Ok(())
        }
        "uninstall" => {
            desk_service_core::uninstall(&paths)?;
            tracing::info!("service removed (ProgramData identity kept)");
            Ok(())
        }
        "run" if args.console => {
            if !desk_service_core::role::host_allowed(&paths, args.allow_on_panel_pc) {
                tracing::warn!(
                    "this PC is a management panel; the agent host is NOT started (developer override: --allow-on-panel-pc)"
                );
                return Ok(());
            }
            let host = ServiceHost::start(HostOptions {
                paths,
                mode: LaunchMode::CurrentUser,
                port: args.port,
                data_dir: args.data_dir.clone(),
                loopback_only: args.loopback,
                allow_on_panel_pc: args.allow_on_panel_pc,
            })?;
            tracing::info!(port = host.port()?, "console mode: Ctrl+C stops");
            desk_service_core::wait_for_console_stop(args.run_for);
            host.stop();
            Ok(())
        }
        "stop" => {
            if desk_service_core::request_console_stop() {
                Ok(())
            } else {
                Err(DeskError::Service(
                    "no console-mode agent is running".into(),
                ))
            }
        }
        "run" => desk_service_core::run_dispatcher(paths),
        other => Err(DeskError::Config(format!(
            "unknown command '{other}' (install | uninstall | stop | run [--console [--loopback]])"
        ))),
    }
}

/// Attaches to the launching terminal's console (never creates a window). `false` when the
/// parent has no console.
fn attach_parent_console() -> bool {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain Win32 call without pointers.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
}

fn main() -> std::process::ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(_) => return std::process::ExitCode::from(2),
    };
    // Only an explicit developer flag prints to a terminal, and only by attaching to the
    // parent's console; without it every message goes to the log file.
    let console = args.attach_console && attach_parent_console();
    let paths = paths_for(&args);
    let config = LoggerConfig {
        level: LogLevel::Info,
        log_dir: Some(paths.system_log_dir()),
        file_name_prefix: "desk-service".to_string(),
        console_output: console,
        json_format: false,
    };
    let _guard = desk_logger::init(&config).ok().flatten();
    match run(&args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "desk-service failed");
            std::process::ExitCode::FAILURE
        }
    }
}
