//! Overlapped named-pipe server/client with a user-restricted DACL.
//!
//! Handles are opened `FILE_FLAG_OVERLAPPED` so one thread can read while another writes
//! the same pipe (synchronous handles would serialize both directions and deadlock).

use crate::message::IpcMessage;
use desk_error::{DeskError, DeskResult};
use desk_protocol::FrameCodec;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::core::PWSTR;
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_INCOMPLETE,
    ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeClientSessionId, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const BUFFER_SIZE: u32 = 256 * 1024;
const READ_CHUNK: usize = 128 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

fn ipc_err(what: &str, e: impl std::fmt::Display) -> DeskError {
    DeskError::Ipc(format!("{what}: {e}"))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Builds the SDDL for the pipe: full access for `SYSTEM` plus read/write for exactly one
/// user SID (the owner of the helper's session). Nobody else, including other administrators
/// and network clients, is granted anything.
#[must_use]
pub fn pipe_access_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{user_sid})")
}

/// SID string (`S-1-5-21-...`) of the user a token belongs to.
pub fn user_sid_of_token(token: HANDLE) -> DeskResult<String> {
    let mut needed = 0u32;
    // SAFETY: size query only; the expected failure reports the required length.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if needed == 0 {
        return Err(DeskError::Ipc("token user size unavailable".to_string()));
    }
    // u64 backing keeps the buffer aligned for TOKEN_USER.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `needed` bytes.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
    }
    .map_err(|e| ipc_err("GetTokenInformation", e))?;
    // SAFETY: the buffer now holds a TOKEN_USER written by the call above.
    let user = unsafe { &*(buf.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_text = PWSTR::null();
    // SAFETY: the SID pointer lives inside `buf`; the string is freed with LocalFree.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_text) }
        .map_err(|e| ipc_err("ConvertSidToStringSidW", e))?;
    // SAFETY: NUL-terminated string allocated by the call above.
    let text = unsafe { sid_text.to_string() }.map_err(|e| ipc_err("SID text", e));
    // SAFETY: allocated by ConvertSidToStringSidW.
    let _ = unsafe { LocalFree(Some(HLOCAL(sid_text.0.cast()))) };
    text
}

/// SID string of the user running the current process.
pub fn current_user_sid() -> DeskResult<String> {
    let mut token = HANDLE::default();
    // SAFETY: querying the current process token.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| ipc_err("OpenProcessToken", e))?;
    let owned = OwnedHandle(token);
    user_sid_of_token(owned.0)
}

/// Owned kernel handle that may move between threads.
struct OwnedHandle(HANDLE);

// SAFETY: a kernel HANDLE is a plain token usable from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: see above; all mutable state is guarded by the owning structure's locks.
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Auto-reset event used for one overlapped operation.
fn new_event() -> DeskResult<OwnedHandle> {
    // SAFETY: plain event creation without security attributes or name.
    unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .map(OwnedHandle)
        .map_err(|e| ipc_err("CreateEventW", e))
}

fn millis(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// Waits for a pending overlapped operation; cancels it on timeout. Returns the number of
/// bytes transferred (also for a cancelled operation that had already moved data) or `None`
/// when nothing arrived before the deadline.
fn finish_overlapped(
    handle: HANDLE,
    overlapped: &mut OVERLAPPED,
    event: &OwnedHandle,
    wait: Duration,
) -> DeskResult<Option<u32>> {
    // SAFETY: `event` is a valid event handle.
    let waited = unsafe { WaitForSingleObject(event.0, millis(wait)) };
    let timed_out = waited != WAIT_OBJECT_0;
    if timed_out {
        // SAFETY: cancels the operation tied to this OVERLAPPED; a failure means it finished.
        let _ = unsafe { CancelIoEx(handle, Some(overlapped)) };
    }
    let mut transferred = 0u32;
    // SAFETY: `overlapped` belongs to an operation started on `handle`; when cancelling we
    // wait for the kernel to release the structure before it is reused or dropped.
    let result = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, timed_out) };
    match result {
        Ok(()) => Ok(Some(transferred)),
        Err(e) => {
            let code = e.code().0 as u32 & 0xFFFF;
            if timed_out && (code == ERROR_OPERATION_ABORTED.0 || code == ERROR_IO_INCOMPLETE.0) {
                return Ok(None);
            }
            if code == ERROR_BROKEN_PIPE.0 || code == ERROR_NO_DATA.0 {
                return Err(DeskError::Ipc("pipe closed by peer".to_string()));
            }
            Err(ipc_err("overlapped I/O", e))
        }
    }
}

struct Inner {
    handle: OwnedHandle,
    server_side: bool,
    closed: AtomicBool,
    read_buf: Mutex<Vec<u8>>,
    write_lock: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// One connected end of the pipe. Cloning shares the connection (one reader, one writer).
#[derive(Clone)]
pub struct IpcConnection {
    inner: Arc<Inner>,
}

impl IpcConnection {
    fn new(handle: OwnedHandle, server_side: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                handle,
                server_side,
                closed: AtomicBool::new(false),
                read_buf: Mutex::new(Vec::new()),
                write_lock: Mutex::new(()),
            }),
        }
    }

    /// Sends one message; fails if the peer does not drain the pipe within a few seconds.
    pub fn send(&self, msg: &IpcMessage) -> DeskResult<()> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(DeskError::Ipc("connection closed".to_string()));
        }
        let frame = FrameCodec::encode_value(msg)?;
        let _writing = lock(&self.inner.write_lock);
        let event = new_event()?;
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut offset = 0usize;
        while offset < frame.len() {
            let mut overlapped = OVERLAPPED {
                hEvent: event.0,
                ..Default::default()
            };
            // SAFETY: `frame[offset..]` and `overlapped` outlive the operation, which is
            // always completed or cancelled-and-drained by `finish_overlapped`.
            let started = unsafe {
                WriteFile(
                    self.inner.handle.0,
                    Some(&frame[offset..]),
                    None,
                    Some(&mut overlapped),
                )
            };
            if let Err(e) = started {
                if (e.code().0 as u32 & 0xFFFF) != ERROR_IO_PENDING.0 {
                    return Err(ipc_err("WriteFile", e));
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match finish_overlapped(self.inner.handle.0, &mut overlapped, &event, remaining)? {
                Some(n) => offset += n as usize,
                None => return Err(DeskError::Ipc("write timed out".to_string())),
            }
        }
        Ok(())
    }

    /// Receives the next message, or `Ok(None)` if nothing arrived within `wait`.
    pub fn recv_timeout(&self, wait: Duration) -> DeskResult<Option<IpcMessage>> {
        let mut buf = lock(&self.inner.read_buf);
        let deadline = Instant::now() + wait;
        loop {
            if let Some((msg, used)) = FrameCodec::decode_value::<IpcMessage>(&buf)? {
                buf.drain(..used);
                return Ok(Some(msg));
            }
            if self.inner.closed.load(Ordering::SeqCst) {
                return Err(DeskError::Ipc("connection closed".to_string()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = new_event()?;
            let mut overlapped = OVERLAPPED {
                hEvent: event.0,
                ..Default::default()
            };
            let mut chunk = vec![0u8; READ_CHUNK];
            // SAFETY: `chunk` and `overlapped` outlive the operation (see `send`).
            let started = unsafe {
                ReadFile(
                    self.inner.handle.0,
                    Some(&mut chunk),
                    None,
                    Some(&mut overlapped),
                )
            };
            if let Err(e) = started {
                let code = e.code().0 as u32 & 0xFFFF;
                if code == ERROR_BROKEN_PIPE.0 || code == ERROR_NO_DATA.0 {
                    return Err(DeskError::Ipc("pipe closed by peer".to_string()));
                }
                if code != ERROR_IO_PENDING.0 {
                    return Err(ipc_err("ReadFile", e));
                }
            }
            match finish_overlapped(self.inner.handle.0, &mut overlapped, &event, remaining)? {
                Some(n) if n > 0 => buf.extend_from_slice(&chunk[..n as usize]),
                Some(_) => {}
                None => return Ok(None),
            }
        }
    }

    /// Windows session id of the connected client (server side only).
    pub fn client_session_id(&self) -> DeskResult<u32> {
        let mut session = 0u32;
        // SAFETY: valid pipe handle and out-parameter.
        unsafe { GetNamedPipeClientSessionId(self.inner.handle.0, &mut session) }
            .map_err(|e| ipc_err("GetNamedPipeClientSessionId", e))?;
        Ok(session)
    }

    /// Process id of the connected client (server side only).
    pub fn client_process_id(&self) -> DeskResult<u32> {
        let mut pid = 0u32;
        // SAFETY: valid pipe handle and out-parameter.
        unsafe { GetNamedPipeClientProcessId(self.inner.handle.0, &mut pid) }
            .map_err(|e| ipc_err("GetNamedPipeClientProcessId", e))?;
        Ok(pid)
    }

    /// Marks the connection closed; the handle is released when the last clone drops.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        // SAFETY: cancels any pending I/O from other threads so they return promptly.
        let _ = unsafe { CancelIoEx(self.inner.handle.0, None) };
        if self.inner.server_side {
            // SAFETY: valid server pipe handle.
            let _ = unsafe { DisconnectNamedPipe(self.inner.handle.0) };
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.server_side {
            // SAFETY: valid server pipe handle.
            let _ = unsafe { DisconnectNamedPipe(self.handle.0) };
        }
    }
}

/// Named-pipe server restricted by DACL. Holds one listening instance at all times.
pub struct IpcServer {
    name: String,
    sddl: String,
    pending: Mutex<Option<OwnedHandle>>,
}

impl IpcServer {
    /// Creates the pipe (first instance, so nobody can squat the name) with the given SDDL
    /// (see [`pipe_access_sddl`]).
    pub fn bind(pipe_name: &str, sddl: &str) -> DeskResult<Self> {
        let first = create_instance(pipe_name, sddl, true)?;
        Ok(Self {
            name: pipe_name.to_string(),
            sddl: sddl.to_string(),
            pending: Mutex::new(Some(first)),
        })
    }

    /// Waits up to `wait` for a client. Returns `Ok(None)` on timeout.
    pub fn accept(&self, wait: Duration) -> DeskResult<Option<IpcConnection>> {
        let instance = match lock(&self.pending).take() {
            Some(h) => h,
            None => create_instance(&self.name, &self.sddl, false)?,
        };
        let event = new_event()?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: `overlapped` outlives the operation (completed or cancelled below).
        let started = unsafe { ConnectNamedPipe(instance.0, Some(&mut overlapped)) };
        let connected = match started {
            Ok(()) => true,
            Err(e) => {
                let code = e.code().0 as u32 & 0xFFFF;
                if code == ERROR_PIPE_CONNECTED.0 {
                    true
                } else if code == ERROR_IO_PENDING.0 {
                    finish_overlapped(instance.0, &mut overlapped, &event, wait)?.is_some()
                } else {
                    return Err(ipc_err("ConnectNamedPipe", e));
                }
            }
        };
        if !connected {
            *lock(&self.pending) = Some(instance);
            return Ok(None);
        }
        *lock(&self.pending) = Some(create_instance(&self.name, &self.sddl, false)?);
        Ok(Some(IpcConnection::new(instance, true)))
    }
}

fn create_instance(pipe_name: &str, sddl: &str, first: bool) -> DeskResult<OwnedHandle> {
    let sddl_w = wide(sddl);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: valid NUL-terminated SDDL; the descriptor is freed with LocalFree below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl_w.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| ipc_err("invalid pipe security descriptor", e))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let name_w = wide(pipe_name);
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: all pointers are valid for the duration of the call.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name_w.as_ptr()),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            Some(&attributes),
        )
    };
    // SAFETY: allocated by ConvertStringSecurityDescriptor...; no longer needed.
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    if handle.is_invalid() {
        return Err(ipc_err("CreateNamedPipeW", std::io::Error::last_os_error()));
    }
    Ok(OwnedHandle(handle))
}

/// Client side of the pipe.
pub struct IpcClient;

impl IpcClient {
    /// Connects to the server pipe, retrying while it is missing or busy, up to `wait`.
    pub fn connect(pipe_name: &str, wait: Duration) -> DeskResult<IpcConnection> {
        let name_w = wide(pipe_name);
        let deadline = Instant::now() + wait;
        loop {
            // SAFETY: valid NUL-terminated pipe name.
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(name_w.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_OVERLAPPED.0),
                    None,
                )
            };
            match opened {
                Ok(handle) => return Ok(IpcConnection::new(OwnedHandle(handle), false)),
                Err(e) => {
                    let code = e.code().0 as u32 & 0xFFFF;
                    if code != ERROR_FILE_NOT_FOUND.0 && code != ERROR_PIPE_BUSY.0 {
                        return Err(ipc_err("pipe connect refused", e));
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(DeskError::Ipc(format!(
                            "pipe {pipe_name} not available within {wait:?}"
                        )));
                    }
                    if code == ERROR_PIPE_BUSY.0 {
                        // SAFETY: valid pipe name.
                        let _ = unsafe {
                            WaitNamedPipeW(
                                PCWSTR(name_w.as_ptr()),
                                millis(remaining.min(Duration::from_millis(250))),
                            )
                        };
                    } else {
                        std::thread::sleep(remaining.min(Duration::from_millis(100)));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
