//! # Desk-Remote Capture - DXGI Desktop Duplication Engine
//!
//! ## Responsibility
//! Enumerates monitors, captures the selected monitor with DXGI Desktop Duplication,
//! keeps the last frame as a GPU texture, recovers from lost access (mode change,
//! UAC / lock screen transitions) and paces capture ticks at a fixed frame rate.
//!
//! ## Scope Boundaries
//! - Frames never leave the GPU here; encoding lives in `desk-codec`.
//! - Does NOT composite the mouse cursor (DXGI excludes it).
//! - Windows 10/11 only; the whole crate is empty on other platforms.
//!
//! ## Dependencies
//! `desk-error` for failures, `tracing` for logging (configured by `desk-logger`).

#![cfg(windows)]

mod capturer;
mod monitor;
mod pacer;
mod win;

pub use capturer::{Capturer, Grab};
pub use monitor::{list_monitors, luid_to_u64, MonitorInfo, MonitorSelector, ScreenRect};
pub use pacer::FramePacer;
pub use win::WinContext;
