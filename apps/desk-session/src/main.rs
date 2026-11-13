//! The per-session helper as its own executable (`DeskRemote-Session.exe`).
//!
//! ## Responsibility
//! Runs the tray / capture / input helper (and the SYSTEM secure-desktop helper) so Task
//! Manager lists it under its own name instead of as a second "Agent". The agent service
//! starts this file next to itself with the `helper` subcommand; the code is the same
//! `desk-session-helper` library the agent executable also carries for the portable mode.
//!
//! ## Dependencies
//! `desk-session-helper`.

fn main() -> std::process::ExitCode {
    desk_session_helper::run_if_requested().unwrap_or(std::process::ExitCode::from(2))
}
