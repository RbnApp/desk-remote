//! # Process DPI awareness
//!
//! ## Responsibility
//! The single place that makes the process per-monitor DPI aware (v2), so DXGI monitor
//! rectangles, `GetSystemMetrics` virtual-screen metrics, cursor positions and `SendInput`
//! all speak physical pixels on every monitor, whatever its scale factor.
//!
//! ## Scope Boundaries
//! - Must run before the first window of the process exists. Windows keeps the FIRST
//!   awareness mode set, so a weaker call (system-aware) elsewhere would silently win.
//!
//! ## Dependencies
//! `windows` (HiDpi).

use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// Makes the process per-monitor DPI aware (idempotent; failure means it was already set).
pub fn ensure_per_monitor_dpi_aware() {
    // SAFETY: process-wide DPI mode without pointers.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}
