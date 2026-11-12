//! State shared between the IPC worker and the tray UI thread.

use desk_ipc::IpcConnection;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Pairing code with its expiry as seen by the UI.
#[derive(Debug, Clone)]
pub struct CodeInfo {
    pub code: String,
    pub expires: Instant,
}

impl CodeInfo {
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.expires.saturating_duration_since(Instant::now())
    }
}

/// What the UI needs to draw.
#[derive(Debug, Default, Clone)]
pub struct UiState {
    pub viewers: Vec<String>,
    pub code: Option<CodeInfo>,
    pub service_online: bool,
}

/// Window message the worker posts to wake the UI thread.
pub const WM_STATE_CHANGED: u32 = 0x8000 + 1; // WM_APP + 1

/// Shared handle between threads.
#[derive(Default)]
pub struct Shared {
    ui: Mutex<UiState>,
    conn: Mutex<Option<IpcConnection>>,
    hwnd: AtomicIsize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Shared {
    /// Copy of the UI state.
    #[must_use]
    pub fn snapshot(&self) -> UiState {
        lock(&self.ui).clone()
    }

    /// Mutates the UI state and wakes the UI thread.
    pub fn update(&self, f: impl FnOnce(&mut UiState)) {
        f(&mut lock(&self.ui));
        crate::ui::notify(self);
    }

    pub fn set_hwnd(&self, hwnd: isize) {
        self.hwnd.store(hwnd, Ordering::SeqCst);
    }

    #[must_use]
    pub fn hwnd(&self) -> isize {
        self.hwnd.load(Ordering::SeqCst)
    }

    pub fn set_conn(&self, conn: Option<IpcConnection>) {
        *lock(&self.conn) = conn;
    }

    /// Connection to the service, if any.
    #[must_use]
    pub fn conn(&self) -> Option<IpcConnection> {
        lock(&self.conn).clone()
    }
}
