//! Windows Service Control Manager integration: dispatcher, install and uninstall.

use crate::host::{HostOptions, ServiceHost};
use crate::launch::LaunchMode;
use crate::tracker::SessionEvent;
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use std::ffi::OsString;
use std::sync::mpsc::channel;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType, SessionChangeReason,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

/// Registered service name.
pub const SERVICE_NAME: &str = "DeskRemoteService";
const DISPLAY_NAME: &str = "Desk-Remote Service";
const DESCRIPTION: &str = "Desk-Remote PC agent: pairing, trust and remote-session host.";

static HOST_OPTIONS: OnceLock<Mutex<Option<HostOptions>>> = OnceLock::new();

fn svc_err(what: &str, e: impl std::fmt::Display) -> DeskError {
    DeskError::Service(format!("{what}: {e}"))
}

/// Hands control to the SCM; blocks until the service stops.
pub fn run_dispatcher(paths: DeskPaths) -> DeskResult<()> {
    let slot = HOST_OPTIONS.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(HostOptions {
        paths,
        mode: LaunchMode::SystemService,
        port: None,
        data_dir: None,
        loopback_only: false,
        allow_on_panel_pc: false,
        role: crate::role::HostRole::AgentService,
        consent_gate: false,
        custom_identity: None,
        custom_trust: None,
        portable: false,
        self_update: true,
    });
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| svc_err("dispatcher", e))
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!(error = %e, "service terminated with an error");
    }
}

fn session_event(reason: SessionChangeReason, id: u32) -> Option<SessionEvent> {
    match reason {
        SessionChangeReason::ConsoleConnect => Some(SessionEvent::ConsoleConnect(id)),
        SessionChangeReason::ConsoleDisconnect => Some(SessionEvent::ConsoleDisconnect(id)),
        SessionChangeReason::SessionLogon => Some(SessionEvent::Logon(id)),
        SessionChangeReason::SessionLogoff => Some(SessionEvent::Logoff(id)),
        SessionChangeReason::SessionLock => Some(SessionEvent::Lock(id)),
        SessionChangeReason::SessionUnlock => Some(SessionEvent::Unlock(id)),
        _ => None,
    }
}

enum Cmd {
    Stop,
    Session(SessionEvent),
}

fn run_service() -> DeskResult<()> {
    let (tx, rx) = channel::<Cmd>();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(Cmd::Stop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(param) => {
            if let Some(ev) = session_event(param.reason, param.notification.session_id) {
                let _ = tx.send(Cmd::Session(ev));
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status_handle = service_control_handler::register(SERVICE_NAME, handler)
        .map_err(|e| svc_err("register handler", e))?;
    let report = |state: ServiceState, accept: ServiceControlAccept, code: u32| {
        let _ = status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        });
    };
    report(ServiceState::StartPending, ServiceControlAccept::empty(), 0);

    let options = HOST_OPTIONS
        .get()
        .and_then(|m| m.lock().ok().and_then(|mut g| g.take()))
        .ok_or_else(|| DeskError::Service("service options missing".to_string()))?;
    // On a panel PC the service stays up but idle: stopping with an error would only make
    // the recovery actions restart it forever.
    let host = if crate::role::host_allowed(&options.paths, options.allow_on_panel_pc) {
        match ServiceHost::start(options) {
            Ok(h) => Some(h),
            Err(e) => {
                report(ServiceState::Stopped, ServiceControlAccept::empty(), 1);
                return Err(e);
            }
        }
    } else {
        tracing::warn!("this PC is a management panel; the agent host is not started");
        None
    };
    report(
        ServiceState::Running,
        ServiceControlAccept::STOP
            | ServiceControlAccept::SHUTDOWN
            | ServiceControlAccept::SESSION_CHANGE,
        0,
    );
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Stop => break,
            Cmd::Session(ev) => {
                if let Some(h) = &host {
                    h.session_event(ev);
                }
            }
        }
    }
    report(ServiceState::StopPending, ServiceControlAccept::empty(), 0);
    if let Some(h) = host {
        h.stop();
    }
    report(ServiceState::Stopped, ServiceControlAccept::empty(), 0);
    Ok(())
}

/// The service definition: LocalSystem, automatic start, `run` entry point.
fn service_info(service_exe: &std::path::Path) -> ServiceInfo {
    ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: service_exe.to_path_buf(),
        launch_arguments: vec![OsString::from("run")],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    }
}

/// Registers the service (LocalSystem, delayed automatic start, restart on crash) and starts
/// it. Safe to run again: an existing service (an upgrade) is reconfigured in place and
/// started if it is not running. Needs admin.
pub fn install(service_exe: &std::path::Path) -> DeskResult<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| svc_err("open service manager (administrator rights required)", e))?;
    let info = service_info(service_exe);
    let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(existing) => {
            existing
                .change_config(&info)
                .map_err(|e| svc_err("update service", e))?;
            existing
        }
        Err(_) => manager
            .create_service(&info, access)
            .map_err(|e| svc_err("create service", e))?,
    };
    service
        .set_description(DESCRIPTION)
        .map_err(|e| svc_err("set description", e))?;
    service
        .set_delayed_auto_start(true)
        .map_err(|e| svc_err("set delayed auto-start", e))?;
    let restart = |secs| ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(secs),
    };
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart(1), restart(5), restart(30)]),
        })
        .map_err(|e| svc_err("set recovery actions", e))?;
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| svc_err("recovery on non-crash failures", e))?;
    let state = service
        .query_status()
        .map_err(|e| svc_err("query status", e))?
        .current_state;
    if state == ServiceState::Stopped {
        service
            .start::<&str>(&[])
            .map_err(|e| svc_err("start service", e))?;
    }
    Ok(())
}

/// Asks the Service Control Manager to stop the service (needs admin). `Ok` also when it is
/// already stopped.
pub fn stop_service() -> DeskResult<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| svc_err("open service manager (administrator rights required)", e))?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
        )
        .map_err(|e| svc_err("open service", e))?;
    let state = service
        .query_status()
        .map_err(|e| svc_err("query status", e))?
        .current_state;
    if state != ServiceState::Stopped {
        service.stop().map_err(|e| svc_err("stop service", e))?;
    }
    Ok(())
}

/// Stops and removes the service, then gives back an official Tailscale client this agent
/// took over. Needs admin. Leaves ProgramData (identity) untouched.
pub fn uninstall(paths: &DeskPaths) -> DeskResult<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| svc_err("open service manager (administrator rights required)", e))?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )
        .map_err(|e| svc_err("open service", e))?;
    let state = service
        .query_status()
        .map_err(|e| svc_err("query status", e))?
        .current_state;
    if state != ServiceState::Stopped {
        let _ = service.stop();
        for _ in 0..50 {
            if matches!(service.query_status(), Ok(s) if s.current_state == ServiceState::Stopped) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    service.delete().map_err(|e| svc_err("delete service", e))?;
    if crate::tailnet::restore_official(paths) {
        tracing::info!("official Tailscale restored to its previous state");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_starts_automatically_as_a_run_entry() {
        let info = service_info(std::path::Path::new(r"C:\Desk\DeskRemote-Ajan.exe"));
        assert_eq!(info.start_type, ServiceStartType::AutoStart);
        assert_eq!(info.launch_arguments, vec![OsString::from("run")]);
        assert!(info.account_name.is_none(), "LocalSystem");
    }
}
