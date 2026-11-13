//! # File transfer
//!
//! ## Responsibility
//! The one implementation of Desk-Remote file transfer rules, shared by every process that
//! sends or receives a file: name safety, the upload limit, the receiving state machine
//! (`.partial` file, SHA-256 check, atomic final name) and the streaming sender.
//!
//! ## Scope Boundaries
//! - No sockets of its own: the sender drives an authenticated `desk-transport` session and
//!   the receiver is fed already decoded `desk-protocol` messages.
//! - No UI and no consent: callers decide whether an offer may proceed (panel prompt, tray
//!   notification) before they hand it to the [`Receiver`].
//!
//! ## Dependencies
//! `desk-protocol` (messages), `desk-transport` (session), `desk-error`.

mod name;
mod receiver;
mod sender;
mod space;

pub use name::{claim_unique_path, is_safe_file_name};
pub use receiver::{Handled, Receiver, MB};
pub use sender::{new_transfer_id, prepare, send_prepared, Prepared, SendError, SendProgress};
pub use space::free_space;

/// Suffix of the temporary file a transfer is written to until its digest matched.
pub const PARTIAL_SUFFIX: &str = ".partial";
