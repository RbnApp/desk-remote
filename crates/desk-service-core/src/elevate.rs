//! # Elevation and "close the agent"
//!
//! ## Responsibility
//! - Tells whether this process is elevated and relaunches the executable through UAC (`runas`).
//!   An elevated agent is what lets a remote viewer control administrator windows: Windows
//!   (UIPI) drops input that a lower-integrity process injects into a higher-integrity window.
//! - The single "close Desk-Remote" path of the tray (DAPP-104 M-09): it stops a console
//!   (portable) agent the user started themselves and otherwise only closes the tray. It never
//!   stops the installed Windows service, so a tray click cannot take unattended access down.
//!
//! ## Scope Boundaries
//! - The UAC secure desktop (the consent dialog itself, Ctrl+Alt+Del) stays out of reach.
//! - Pure decision logic is separate from the Win32 calls so it is unit-tested.
//!
//! ## Dependencies
//! `windows` (Security, Shell, Threading).

use desk_error::{DeskError, DeskResult};
use std::path::{Path, PathBuf};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// `ERROR_CANCELLED`: the user pressed "No" on the UAC prompt.
const ERROR_CANCELLED: u32 = 1223;

/// What a `runas` call came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runas {
    /// The elevated process ran (and, when waited for, exited with this code).
    Done(u32),
    /// The user declined the UAC prompt; nothing was started.
    Cancelled,
    /// Windows could not start it (Win32 error code).
    Failed(u32),
}

/// Result of "close Desk-Remote".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// A console (portable) agent of this user session was told to stop.
    Stopped,
    /// There is no console agent: only the tray is closed, the service keeps running.
    TrayOnly,
}

/// The exact `ShellExecuteEx` request of an elevated start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunasRequest {
    pub verb: &'static str,
    pub file: PathBuf,
    pub params: String,
}

impl RunasRequest {
    #[must_use]
    pub fn new(exe: &Path, params: &str) -> Self {
        Self {
            verb: "runas",
            file: exe.to_path_buf(),
            params: params.to_string(),
        }
    }
}

/// Maps a `ShellExecuteEx` failure code to a [`Runas`].
#[must_use]
pub fn classify_failure(code: u32) -> Runas {
    if code == ERROR_CANCELLED {
        Runas::Cancelled
    } else {
        Runas::Failed(code)
    }
}

/// Whether this process runs with an elevated (full administrator) token.
#[must_use]
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: valid out-parameter; the token handle is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: buffer and length describe `elevation`.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }
    .is_ok();
    // SAFETY: handle opened above.
    let _ = unsafe { CloseHandle(token) };
    ok && elevation.TokenIsElevated != 0
}

/// Runs the request through UAC; with `wait` blocks until the elevated process exits.
pub fn runas(request: &RunasRequest, wait: bool) -> Runas {
    let verb = HSTRING::from(request.verb);
    let file = HSTRING::from(request.file.as_os_str());
    let params = HSTRING::from(request.params.as_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call; `info` is fully initialised.
    if unsafe { ShellExecuteExW(&mut info) }.is_err() {
        // SAFETY: no preconditions.
        return classify_failure(unsafe { GetLastError() }.0);
    }
    if info.hProcess.is_invalid() {
        return Runas::Done(0);
    }
    let mut code = 0u32;
    if wait {
        // SAFETY: the process handle is valid until closed below.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
        }
    }
    // SAFETY: handle returned by ShellExecuteExW with SEE_MASK_NOCLOSEPROCESS.
    let _ = unsafe { CloseHandle(info.hProcess) };
    Runas::Done(code)
}

/// Relaunches `exe` elevated without arguments (the double-click portable mode) and returns at
/// once; the caller exits when this is [`Runas::Done`].
pub fn relaunch_elevated(exe: &Path) -> Runas {
    runas(&RunasRequest::new(exe, ""), false)
}

/// The decision of "close Desk-Remote": a console agent is stopped directly; anything else
/// (the installed service) is left running and the caller closes the tray only.
pub fn stop_agent_with(console_stop: impl FnOnce() -> bool) -> StopOutcome {
    if console_stop() {
        StopOutcome::Stopped
    } else {
        StopOutcome::TrayOnly
    }
}

/// "Close Desk-Remote" from the tray. On [`StopOutcome::TrayOnly`] the caller closes its own
/// tray; no UAC prompt and no service control call is ever made here.
pub fn quit_agent() -> StopOutcome {
    let outcome = stop_agent_with(crate::request_console_stop);
    tracing::info!(?outcome, "close requested from the tray");
    outcome
}

/// SDDL of the console stop event: SYSTEM, Administrators and the creating user only. Other
/// interactive users of the PC cannot signal (or wait on) it (DAPP-104 H-05). The user's own
/// non-elevated tray/`stop` process shares the SID of an elevated agent, so it still can.
fn stop_event_sddl(user_sid: &str) -> String {
    format!("D:(A;;0x1F0003;;;SY)(A;;0x1F0003;;;BA)(A;;0x1F0003;;;{user_sid})")
}

/// Security attributes for the console stop event (see [`stop_event_sddl`]). Without an
/// explicit descriptor an elevated agent's event would be invisible to the user's normal
/// (medium integrity) double-click and `stop` processes.
pub(crate) struct StopEventSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl StopEventSecurity {
    pub(crate) fn new() -> DeskResult<Self> {
        let sddl = HSTRING::from(stop_event_sddl(&desk_ipc::current_user_sid()?));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the SDDL string outlives the call; the descriptor is freed in `Drop`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|e| DeskError::Service(format!("stop event security: {e}")))?;
        Ok(Self {
            attributes: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
            descriptor,
        })
    }

    pub(crate) fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &raw const self.attributes
    }
}

impl Drop for StopEventSecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        let _ = unsafe { windows::Win32::Foundation::LocalFree(Some(HLOCAL(self.descriptor.0))) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uac_request_uses_the_runas_verb() {
        let exe = Path::new(r"C:\Program Files\Desk-Remote\DeskRemote-Ajan.exe");
        let request = RunasRequest::new(exe, "");
        assert_eq!(request.verb, "runas");
        assert_eq!(request.file, exe);
    }

    #[test]
    fn declining_uac_maps_to_cancelled_and_other_errors_to_failed() {
        assert_eq!(classify_failure(1223), Runas::Cancelled);
        assert_eq!(classify_failure(5), Runas::Failed(5));
    }

    #[test]
    fn a_console_agent_is_stopped_and_anything_else_closes_only_the_tray() {
        assert_eq!(stop_agent_with(|| true), StopOutcome::Stopped);
        // No console agent (the installed service): nothing is stopped, no UAC, no SCM call.
        assert_eq!(stop_agent_with(|| false), StopOutcome::TrayOnly);
    }

    #[test]
    fn the_stop_event_is_limited_to_system_admins_and_the_creating_user() {
        let sddl = stop_event_sddl("S-1-5-21-1-2-3-1001");
        assert!(sddl.contains(";;;SY)") && sddl.contains(";;;BA)"));
        assert!(sddl.contains(";;;S-1-5-21-1-2-3-1001)"));
        assert!(
            !sddl.contains(";;;IU)"),
            "interactive users must not be granted"
        );
        assert!(!sddl.contains(";;;WD)"), "Everyone must not be granted");
        // The real descriptor parses and carries the current user.
        assert!(StopEventSecurity::new().is_ok());
    }

    #[test]
    fn the_process_elevation_query_answers() {
        // Either answer is valid; the call itself must not fail or panic.
        let _ = is_elevated();
    }
}
