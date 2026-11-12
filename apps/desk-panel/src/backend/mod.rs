//! Panel-side view of the transport layer.
//!
//! `desk-transport` / `desk-protocol` are still skeletons, so the panel talks to a
//! [`Backend`] trait that mirrors the planned `Session` contract (pairing, viewer
//! join/leave, `VideoConfig`/`VideoFrame`, `RequestKeyframe`, `InputEvent`, `Ping/Pong`).
//! When the transport lands, one adapter implementing [`Backend`] replaces the stand-ins.

// Without the demo feature nothing produces session events until `desk-transport` exists,
// so the contract types are intentionally unconstructed in that build.
#![cfg_attr(not(feature = "demo"), allow(dead_code))]

#[cfg(feature = "demo")]
pub mod demo;
#[cfg(not(feature = "demo"))]
mod none;

use desk_config::PairedPc;
use desk_error::DeskResult;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Connection state of a paired PC as shown in the list.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PcStatus {
    Online,
    Offline,
    Connecting,
    Warning,
}

/// One row of the PC list.
#[derive(Debug, Clone, Serialize)]
pub struct PcInfo {
    pub id: String,
    pub name: String,
    pub status: PcStatus,
    /// Minutes since the PC was last seen (0 = now).
    pub last_seen_min: u32,
    /// i18n key suffix under `warn.*` explaining a [`PcStatus::Warning`].
    pub warn: Option<String>,
}

/// Why a pairing attempt failed; the codes are the i18n keys under `pair.err.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    Invalid,
    Expired,
    Used,
    Network,
}

impl PairError {
    /// Stable code handed to the UI.
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Used => "used",
            Self::Network => "network",
        }
    }
}

/// Input forwarded to the remote PC; coordinates are normalized to `0..=65535`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: u8,
        pressed: bool,
        x: u16,
        y: u16,
    },
    Wheel {
        dx: i32,
        dy: i32,
    },
    Key {
        code: String,
        pressed: bool,
    },
}

/// Events a live session pushes to the viewer.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Stream parameters (`VideoConfig`).
    Config {
        codec: String,
        fps: u32,
        width: Option<u32>,
        height: Option<u32>,
    },
    /// One Annex-B access unit (`VideoFrame`).
    Frame {
        ts_us: u64,
        keyframe: bool,
        data: Vec<u8>,
    },
    /// Round-trip time from `Ping/Pong`.
    Rtt { ms: u32 },
    /// The link ended (peer gone, network drop, or local close).
    Closed,
}

/// Receiver of [`SessionEvent`]s, called from the session's own thread.
pub type EventSink = Arc<dyn Fn(SessionEvent) + Send + Sync>;

/// A live viewer session towards one PC.
pub trait ViewerSession: Send {
    /// Forwards one input event (`InputEvent`).
    fn send_input(&self, event: InputEvent);
    /// Asks the host for a fresh keyframe (`RequestKeyframe`).
    fn request_keyframe(&self);
    /// Leaves the session (`ViewerLeave`).
    fn close(&self);
}

/// Everything the panel needs from the connection layer.
pub trait Backend: Send + Sync {
    /// Current list rows for the paired PCs.
    fn list(&self, paired: &[PairedPc]) -> Vec<PcInfo>;
    /// Redeems a pairing code (`PairRequest/PairResult`).
    fn pair(&self, code: &str, name: Option<&str>) -> Result<PairedPc, PairError>;
    /// Joins a PC as viewer (`ViewerJoin`) and starts streaming into `sink`.
    fn connect(&self, pc_id: &str, sink: EventSink) -> DeskResult<Box<dyn ViewerSession>>;
    /// True for the synthetic test backend.
    fn is_demo(&self) -> bool {
        false
    }
    /// Test hook: break the active link and make the next connects fail.
    fn simulate_drop(&self) {}
}

/// Picks the backend for this build.
pub fn select() -> Arc<dyn Backend> {
    #[cfg(feature = "demo")]
    {
        Arc::new(demo::DemoBackend::new())
    }
    #[cfg(not(feature = "demo"))]
    {
        Arc::new(none::NoTransport)
    }
}
