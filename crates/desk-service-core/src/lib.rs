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

mod control;
mod elevate;
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
mod transfer;

pub use control::Control;
pub use elevate::{
    is_elevated, quit_agent, relaunch_elevated, runas, stop_agent_with, Runas, RunasRequest,
    StopOutcome, STOP_ARG,
};
pub use host::{
    console_agent_running, request_console_stop, wait_for_console_stop, HostOptions, ServiceHost,
};
pub use hub::{ConsentEvent, Hub};
pub use launch::{active_console_session, LaunchMode};
pub use role::{host_allowed, host_allowed_for_role, HostRole};
pub use scm::{install, run_dispatcher, stop_service, uninstall, SERVICE_NAME};
pub use tailnet::{ensure_helper_extracted, helper_executable_path, restore_official};
pub use tracker::{Backoff, SessionEvent, SessionTracker};
pub use transfer::{TransferEvent, TransferListener, TransferState};
