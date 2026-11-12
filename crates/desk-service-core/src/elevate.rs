//! # Elevation and "close the agent"
//!
//! ## Responsibility
//! - Tells whether this process is elevated and relaunches the executable through UAC (`runas`).
//!   An elevated agent is what lets a remote viewer control administrator windows: Windows
//!   (UIPI) drops input that a lower-integrity process injects into a higher-integrity window.
//! - The single "close Desk-Remote" path shared by the portable "Quit" and the installed-service
//!   tray entry: stop the console agent directly when there is one, otherwise ask Windows for
//!   administrator rights and run `DeskRemote-Ajan.exe stop`.
//!
//! ## Scope Boundaries
//! - The UAC secure desktop (the consent dialog itself, Ctrl+Alt+Del) stays out of reach.
//! - Pure decision logic is separate from the Win32 calls so it is unit-tested.
//!
//! ## Dependencies
//! `windows` (Security, Shell, Threading).

use desk_error::{DeskError, DeskResult};
use std::path::{Path, PathBuf};
use windows::core::{w, HSTRING, PCWSTR};
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
/// Argument of the stop subcommand.
pub const STOP_ARG: &str = "stop";

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
    Stopped,
    /// The UAC prompt was declined: everything keeps running.
    Cancelled,
    Failed,
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

/// The decision of "close Desk-Remote": a console agent is stopped directly; otherwise the
/// installed service needs administrator rights, so `elevated_stop` runs through UAC.
pub fn stop_agent_with(
    console_stop: impl FnOnce() -> bool,
    elevated_stop: impl FnOnce() -> Runas,
) -> StopOutcome {
    if console_stop() {
        return StopOutcome::Stopped;
    }
    match elevated_stop() {
        Runas::Done(0) => StopOutcome::Stopped,
        Runas::Cancelled => StopOutcome::Cancelled,
        Runas::Done(_) | Runas::Failed(_) => StopOutcome::Failed,
    }
}

/// "Close Desk-Remote" from the tray (portable and installed-service mode share this path).
pub fn quit_agent() -> DeskResult<StopOutcome> {
    let exe = desk_paths::DeskPaths::current_exe()?;
    let outcome = stop_agent_with(crate::request_console_stop, || {
        runas(&RunasRequest::new(&exe, STOP_ARG), true)
    });
    tracing::info!(?outcome, "close requested from the tray");
    Ok(outcome)
}

/// Security attributes for the console stop event: administrators and SYSTEM get everything,
/// any interactive user may signal/wait on it. Without this an elevated agent's event would be
/// invisible to the user's normal (medium integrity) double-click and `stop` processes.
pub(crate) struct StopEventSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl StopEventSecurity {
    pub(crate) fn new() -> DeskResult<Self> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: constant SDDL string; the descriptor is freed in `Drop`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:(A;;0x1F0003;;;SY)(A;;0x1F0003;;;BA)(A;;0x100002;;;IU)"),
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
    use std::cell::Cell;

    #[test]
    fn the_uac_request_runs_the_exe_with_the_runas_verb_and_stop_argument() {
        let exe = Path::new(r"C:\Program Files\Desk-Remote\DeskRemote-Ajan.exe");
        let request = RunasRequest::new(exe, STOP_ARG);
        assert_eq!(request.verb, "runas");
        assert_eq!(request.file, exe);
        assert_eq!(request.params, "stop");
    }

    #[test]
    fn declining_uac_maps_to_cancelled_and_other_errors_to_failed() {
        assert_eq!(classify_failure(1223), Runas::Cancelled);
        assert_eq!(classify_failure(5), Runas::Failed(5));
    }

    #[test]
    fn a_console_agent_is_stopped_without_any_uac_prompt() {
        let asked = Cell::new(false);
        let outcome = stop_agent_with(
            || true,
            || {
                asked.set(true);
                Runas::Done(0)
            },
        );
        assert_eq!(outcome, StopOutcome::Stopped);
        assert!(
            !asked.get(),
            "no UAC when the agent can be stopped directly"
        );
    }

    #[test]
    fn the_service_path_asks_uac_and_stops_only_when_confirmed() {
        assert_eq!(
            stop_agent_with(|| false, || Runas::Done(0)),
            StopOutcome::Stopped
        );
        assert_eq!(
            stop_agent_with(|| false, || Runas::Cancelled),
            StopOutcome::Cancelled,
            "cancelling UAC leaves everything running"
        );
        assert_eq!(
            stop_agent_with(|| false, || Runas::Done(1)),
            StopOutcome::Failed
        );
        assert_eq!(
            stop_agent_with(|| false, || Runas::Failed(2)),
            StopOutcome::Failed
        );
    }

    #[test]
    fn the_process_elevation_query_answers() {
        // Either answer is valid; the call itself must not fail or panic.
        let _ = is_elevated();
    }
}
