//! # Desk-Remote Input - Remote Mouse and Keyboard Injection
//!
//! ## Responsibility
//! Turns remote input events (normalised absolute mouse position, button mask, wheel,
//! keyboard scancodes) into Windows `SendInput` calls on the captured monitor, and
//! guarantees no key or mouse button stays pressed after a viewer disconnects.
//!
//! ## Scope Boundaries
//! - Injection only; wire decoding is `desk-protocol`'s job and arrives as plain values.
//! - Input into elevated windows or the secure desktop needs a SYSTEM-level helper
//!   (the secure-desktop helper, DAPP-118); this crate does not elevate.
//! - Windows 10/11 only; the whole crate is empty on other platforms.
//!
//! ## Dependencies
//! `desk-capture` (monitor geometry, error context), `desk-error`, `tracing`.

#![cfg(windows)]

mod injector;
mod mapping;
mod sendinput;

pub use injector::{wheel_units, InputBackend, InputInjector, RawInput, WHEEL_DELTA};
pub use mapping::{button_changes, map_to_virtual_desktop, split_scancode, MouseButton};
pub use sendinput::{system_injector, SendInputBackend, SystemInjector};
