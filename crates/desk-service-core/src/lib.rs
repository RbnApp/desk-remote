//! # Desk-Remote service core
//!
//! ## Responsibility
//! Everything the SYSTEM service does: viewer connections (pairing, trust), the session
//! helper supervisor (start per console session, restart on crash/session change), and the
//! routing hub between the network and the helper's IPC pipe.
//!
//! ## Scope Boundaries
//! - No screen capture, input injection or UI: those live in the session helper.
//! - Process spawning for the agent lives here (`launch`, `tailnet`) and nowhere else.
//!
//! ## Dependencies
//! `desk-transport`, `desk-crypto`, `desk-ipc`, `desk-protocol`, `desk-config`, `desk-paths`.

#![cfg(windows)]

mod engine;
mod host;
mod hub;
mod launch;
pub mod role;
mod scm;
mod supervisor;
mod tailnet;
pub mod takeover;
mod tracker;

pub use host::{wait_for_console_stop, HostOptions, ServiceHost};
pub use hub::Hub;
pub use launch::{active_console_session, LaunchMode};
pub use scm::{install, run_dispatcher, uninstall, SERVICE_NAME};
pub use tracker::{Backoff, SessionEvent, SessionTracker};
