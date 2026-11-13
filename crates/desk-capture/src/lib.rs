//! # Desk-Remote Capture - DXGI Desktop Duplication Engine
//!
//! ## Responsibility
//! Enumerates monitors, captures the selected monitor with DXGI Desktop Duplication,
//! keeps the last frame as a GPU texture, recovers from lost access (mode change,
//! UAC / lock screen transitions) and paces capture ticks at a fixed frame rate.
//!
//! ## Scope Boundaries
//! - Frames never leave the GPU here; encoding lives in `desk-codec`.
//! - Does NOT composite the mouse cursor into frames (DXGI excludes it); the cursor is
//!   sampled separately by the `cursor` module and drawn by the viewer.
//! - Windows 10/11 only; the whole crate is empty on other platforms.
//!
//! ## Dependencies
//! `desk-error` for failures, `tracing` for logging (configured by `desk-logger`).

#![cfg(windows)]

mod capturer;
mod cursor;
mod desktop;
mod dpi;
mod monitor;
mod pacer;
mod win;

pub use capturer::{Capturer, Grab};
pub use cursor::{
    compose_rgba, normalize_to_monitor, CursorPos, CursorShape, CursorTracker, CursorUpdate,
};
pub use desktop::{
    attach_input_desktop, follows_input_desktop, input_desktop_name, set_follow_input_desktop,
};
pub use dpi::ensure_per_monitor_dpi_aware;
pub use monitor::{list_monitors, luid_to_u64, MonitorInfo, MonitorSelector, ScreenRect};
pub use pacer::FramePacer;
pub use win::WinContext;
