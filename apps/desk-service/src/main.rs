//! Desk-Remote PC agent service (runs as SYSTEM, session 0).
//!
//! Usage:
//!   desk-service install                      register + start the Windows service (admin)
//!   desk-service uninstall                    stop + remove the service (admin)
//!   desk-service run                          entry point used by the Service Control Manager
//!   desk-service run --console [--loopback] [--data-dir D] [--port P] [--run-for SECS]
//!                                             run in the foreground as the current user (dev/test);
//!                                             --loopback: 127.0.0.1 only, no mDNS, no tailnet helper

#![cfg(windows)]

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
    loopback: bool,
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
            "--loopback" => args.loopback = true,
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
            let host = ServiceHost::start(HostOptions {
                paths,
                mode: LaunchMode::CurrentUser,
                port: args.port,
                data_dir: args.data_dir.clone(),
                loopback_only: args.loopback,
            })?;
            tracing::info!(port = host.port()?, "console mode: Ctrl+C stops");
            desk_service_core::wait_for_console_stop(args.run_for);
            host.stop();
            Ok(())
        }
        "run" => desk_service_core::run_dispatcher(paths),
        other => Err(DeskError::Config(format!(
            "unknown command '{other}' (install | uninstall | run [--console [--loopback]])"
        ))),
    }
}

fn main() -> std::process::ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(_) => return std::process::ExitCode::from(2),
    };
    let console = args.console || args.command != "run";
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
