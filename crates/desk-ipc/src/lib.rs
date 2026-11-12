//! # Service <-> session-helper IPC
//!
//! ## Responsibility
//! Owns the Windows named pipe that connects the SYSTEM service (session 0) with the
//! session helper running in the interactive user session. The pipe DACL admits only
//! `SYSTEM` and the one user the helper runs as; remote clients are rejected by the kernel.
//!
//! ## Scope Boundaries
//! - Frames use the `desk-protocol` binary encoding (`FrameCodec::encode_value`); there is
//!   no second wire format.
//! - Pure transport: the meaning of the messages lives in the service and helper.
//!
//! ## Dependencies
//! `desk-protocol` (framing, [`Message`] payloads), `desk-paths` (pipe names), `desk-error`.

mod info;
mod message;
#[cfg(windows)]
mod pipe;

pub use info::{AgentInfo, TailnetState};
pub use message::IpcMessage;
#[cfg(windows)]
pub use pipe::{
    current_user_sid, pipe_access_sddl, user_sid_of_token, IpcClient, IpcConnection, IpcServer,
};
