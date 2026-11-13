//! Starts and stops the session helper (the only place that spawns processes for the agent).
//!
//! * [`LaunchMode::SystemService`]: SYSTEM service path. The user token of the target session
//!   comes from `WTSQueryUserToken` and the helper starts through `CreateProcessAsUserW` on
//!   `winsta0\default`, so it lives in the interactive session instead of session 0.
//! * [`LaunchMode::CurrentUser`]: `run --console` development path. A normal process cannot
//!   obtain other users' tokens, so the helper simply starts as the current user.

use desk_error::{DeskError, DeskResult};
use std::path::Path;
use std::process::Child;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, SetTokenInformation,
    TokenLinkedToken, TokenPrimary, TokenSessionId, TOKEN_ALL_ACCESS, TOKEN_LINKED_TOKEN,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken,
};
use windows::Win32::System::Threading::PROCESS_CREATION_FLAGS;
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
    TerminateProcess, WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
    STARTUPINFOW,
};

/// How the helper process is started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    SystemService,
    CurrentUser,
}

/// Session the OS currently shows on the physical console.
#[must_use]
pub fn active_console_session() -> u32 {
    // SAFETY: no preconditions.
    unsafe { WTSGetActiveConsoleSessionId() }
}

/// Session of the calling process.
#[must_use]
pub fn current_process_session() -> u32 {
    let mut session = 0u32;
    // SAFETY: valid out-parameter.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    session
}

enum Proc {
    Raw(HANDLE),
    Child(Child),
}

/// A running helper process owned by the supervisor.
pub struct HelperProcess {
    proc: Proc,
    pid: u32,
    session_id: u32,
}

// SAFETY: the raw HANDLE is only used through `&self`/`&mut self` methods of this type.
unsafe impl Send for HelperProcess {}

impl HelperProcess {
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    #[must_use]
    pub fn session_id(&self) -> u32 {
        self.session_id
    }

    /// Whether the process is still alive.
    pub fn is_running(&mut self) -> bool {
        match &mut self.proc {
            Proc::Raw(handle) => {
                // SAFETY: valid process handle owned by this object.
                unsafe { WaitForSingleObject(*handle, 0) != WAIT_OBJECT_0 }
            }
            Proc::Child(child) => matches!(child.try_wait(), Ok(None)),
        }
    }

    /// Force-terminates the process and waits briefly for it to disappear.
    pub fn terminate(&mut self) {
        match &mut self.proc {
            Proc::Raw(handle) => {
                // SAFETY: valid process handle owned by this object.
                unsafe {
                    let _ = TerminateProcess(*handle, 1);
                    let _ = WaitForSingleObject(*handle, 3000);
                }
            }
            Proc::Child(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        if let Proc::Raw(handle) = &self.proc {
            // SAFETY: the handle is owned and closed exactly once.
            let _ = unsafe { CloseHandle(*handle) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn service_err(what: &str, e: impl std::fmt::Display) -> DeskError {
    DeskError::Service(format!("{what}: {e}"))
}

/// Starts `exe` for `session_id` according to `mode`.
pub fn launch_helper(
    mode: LaunchMode,
    exe: &Path,
    args: &[String],
    session_id: u32,
) -> DeskResult<HelperProcess> {
    if !exe.exists() {
        return Err(DeskError::Service(format!(
            "session helper not found at {}",
            exe.display()
        )));
    }
    match mode {
        LaunchMode::CurrentUser => {
            let child = desk_paths::hidden_command(exe)
                .args(args)
                .spawn()
                .map_err(|e| service_err("cannot start session helper", e))?;
            Ok(HelperProcess {
                pid: child.id(),
                proc: Proc::Child(child),
                session_id: current_process_session(),
            })
        }
        LaunchMode::SystemService => launch_in_session(exe, args, session_id),
    }
}

/// Starts `exe` as SYSTEM inside `session_id`: the service's own token, moved to that session.
/// Only a SYSTEM process can open the secure desktop (UAC prompt, lock screen) to capture it
/// and click in it (DAPP-118). It never gets the user's token, tray or files.
pub fn launch_system_helper(
    exe: &Path,
    args: &[String],
    session_id: u32,
) -> DeskResult<HelperProcess> {
    if !exe.exists() {
        return Err(DeskError::Service(format!(
            "secure-desktop helper not found at {}",
            exe.display()
        )));
    }
    let mut own = HANDLE::default();
    // SAFETY: valid out-parameter; the pseudo handle needs no closing.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own) }
        .map_err(|e| service_err("OpenProcessToken", e))?;
    let mut primary = HANDLE::default();
    // SAFETY: `own` is a valid token handle owned here.
    let duplicated = unsafe {
        DuplicateTokenEx(
            own,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    };
    // SAFETY: `own` is no longer needed.
    let _ = unsafe { CloseHandle(own) };
    duplicated.map_err(|e| service_err("DuplicateTokenEx", e))?;
    // SAFETY: the buffer is the 4-byte session id the call expects.
    let moved = unsafe {
        SetTokenInformation(
            primary,
            TokenSessionId,
            (&raw const session_id).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    let result = match moved {
        Ok(()) => spawn_with_token(primary, exe, args, session_id),
        Err(e) => Err(service_err("SetTokenInformation(session)", e)),
    };
    // SAFETY: owned here, not needed after CreateProcessAsUserW.
    let _ = unsafe { CloseHandle(primary) };
    result
}

fn launch_in_session(exe: &Path, args: &[String], session_id: u32) -> DeskResult<HelperProcess> {
    let mut token = HANDLE::default();
    // SAFETY: valid out-parameter; requires SYSTEM (SeTcbPrivilege), which the service has.
    unsafe { WTSQueryUserToken(session_id, &mut token) }
        .map_err(|e| service_err(&format!("no user token for session {session_id}"), e))?;
    // An administrator's session token is the filtered (medium integrity) half of a split
    // token; Windows (UIPI) drops input injected into elevated windows from it. The helper
    // injects the remote input, so it runs with the user's own full (linked) token when one
    // exists. Standard users have none and keep the normal token. The UAC secure desktop
    // stays out of reach either way.
    let elevated = linked_primary_token(token);
    let spawn_token = elevated.unwrap_or(token);
    let result = spawn_with_token(spawn_token, exe, args, session_id);
    // SAFETY: both tokens are owned here and no longer needed after CreateProcessAsUserW.
    unsafe {
        if let Some(linked) = elevated {
            let _ = CloseHandle(linked);
        }
        let _ = CloseHandle(token);
    }
    result
}

/// The elevated twin of a filtered administrator token as a primary token, if the user has one.
fn linked_primary_token(token: HANDLE) -> Option<HANDLE> {
    let mut linked = TOKEN_LINKED_TOKEN::default();
    let mut returned = 0u32;
    // SAFETY: buffer and length describe `linked`.
    unsafe {
        GetTokenInformation(
            token,
            TokenLinkedToken,
            Some((&raw mut linked).cast()),
            std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut returned,
        )
    }
    .ok()?;
    let mut primary = HANDLE::default();
    // SAFETY: `linked.LinkedToken` is a valid token handle owned here; closed below.
    let duplicated = unsafe {
        DuplicateTokenEx(
            linked.LinkedToken,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    };
    // SAFETY: closing the handle GetTokenInformation handed us.
    let _ = unsafe { CloseHandle(linked.LinkedToken) };
    duplicated.ok()?;
    Some(primary)
}

fn spawn_with_token(
    token: HANDLE,
    exe: &Path,
    args: &[String],
    session_id: u32,
) -> DeskResult<HelperProcess> {
    let mut environment = std::ptr::null_mut();
    // SAFETY: valid out-parameter and token; destroyed below.
    unsafe { CreateEnvironmentBlock(&mut environment, Some(token), false) }
        .map_err(|e| service_err("CreateEnvironmentBlock", e))?;

    let exe_text = exe.to_string_lossy().to_string();
    let exe_w = wide(&exe_text);
    let mut command_line_text = format!("\"{exe_text}\"");
    for arg in args {
        command_line_text.push_str(&format!(" \"{arg}\""));
    }
    let mut command_line = wide(&command_line_text);
    let mut desktop = wide(r"winsta0\default");
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every pointer is valid for the call; the buffers outlive it.
    let created = unsafe {
        CreateProcessAsUserW(
            Some(token),
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | PROCESS_CREATION_FLAGS(desk_paths::CREATE_NO_WINDOW),
            Some(environment.cast_const()),
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    };
    // SAFETY: allocated by CreateEnvironmentBlock above.
    let _ = unsafe { DestroyEnvironmentBlock(environment) };
    created.map_err(|e| service_err("CreateProcessAsUserW", e))?;
    // SAFETY: the thread handle is not needed.
    let _ = unsafe { CloseHandle(info.hThread) };
    Ok(HelperProcess {
        proc: Proc::Raw(info.hProcess),
        pid: info.dwProcessId,
        session_id,
    })
}
