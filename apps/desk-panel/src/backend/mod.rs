//! Panel-side view of the transport layer.
//!
//! The panel talks to a [`Backend`]: [`transport::TransportBackend`] in every real build
//! (pairing, discovery, status probes and viewer sessions over `desk-transport`), and the
//! synthetic `demo` backend only when the `demo` feature is enabled for tests.

#[cfg(feature = "demo")]
pub mod demo;
mod h264;
#[cfg_attr(feature = "demo", allow(dead_code))]
mod input_map;
#[cfg_attr(feature = "demo", allow(dead_code))]
mod transport;

use desk_config::PairedPc;
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
    /// Round-trip time of the last status probe, when the PC answered.
    pub latency_ms: Option<u32>,
}

/// A PC the user could pair with, found on the LAN (mDNS) or in the tailnet.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub name: String,
    /// `ip:port` to pair with.
    pub address: String,
    /// `"lan"` or `"tailscale"`.
    pub source: &'static str,
    /// Already in the paired list.
    pub paired: bool,
}

/// Why a pairing attempt failed; the codes are the i18n keys under `pair.err.*`.
// Each backend produces a different subset: the real host cannot tell an expired or used
// code from a wrong one (it answers `Rejected`), and the demo backend knows no `Unpaired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PairError {
    Invalid,
    Expired,
    Used,
    Network,
    /// The host refused the code without saying why (wrong, expired or already used).
    Rejected,
    /// The host does not know this panel, or this panel does not know the host.
    Unpaired,
    /// The PC user declined the incoming connection.
    Denied,
    /// Nobody on the PC answered the incoming connection in time.
    NoAnswer,
}

impl PairError {
    /// Stable code handed to the UI.
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Used => "used",
            Self::Network => "network",
            Self::Rejected => "rejected",
            Self::Unpaired => "unpaired",
            Self::Denied => "denied",
            Self::NoAnswer => "no_answer",
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
    /// Vertical scroll in web pixels (positive = down); the horizontal part is not forwarded.
    Wheel {
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
    /// The PC user changed what this viewer may do (`Message::Notice`); `kind` is the
    /// snake_case `NoticeKind` (`disconnected_by_host`, `view_only_on`, `view_only_off`).
    Notice { kind: &'static str },
    /// One remote cursor image (`Message::CursorShape`), straight-alpha RGBA.
    CursorShape {
        id: u32,
        hot_x: u16,
        hot_y: u16,
        width: u16,
        height: u16,
        rgba: Vec<u8>,
    },
    /// Remote cursor position (`Message::CursorPos`, 0..=65535 over the captured monitor).
    CursorPos {
        x: u16,
        y: u16,
        shape_id: u32,
        visible: bool,
    },
    /// The link ended (peer gone, network drop, or local close).
    Closed,
}

/// Receiver of [`SessionEvent`]s, called from the session's own thread.
pub type EventSink = Arc<dyn Fn(SessionEvent) + Send + Sync>;

/// A live viewer session towards one PC.
pub trait ViewerSession: Send + Sync {
    /// Forwards one input event (`InputEvent`).
    fn send_input(&self, event: &InputEvent);
    /// Asks the host for a fresh keyframe (`RequestKeyframe`).
    fn request_keyframe(&self);
    /// Pauses (`false`, `ViewerLeave`) or resumes (`true`, `ViewerJoin` + keyframe) the video
    /// while the link, the pairing and the input path stay open.
    fn set_active(&self, active: bool);
    /// The link is still up.
    fn is_open(&self) -> bool;
    /// Leaves the session (`ViewerLeave`).
    fn close(&self);
}

/// Everything the panel needs from the connection layer.
pub trait Backend: Send + Sync {
    /// Current list rows for the paired PCs.
    fn list(&self, paired: &[PairedPc]) -> Vec<PcInfo>;
    /// PCs that could be paired (mDNS neighbours and online tailnet peers).
    fn discover(&self, paired: &[PairedPc]) -> Vec<Candidate>;
    /// Pairs with the PC at `target` (`ip[:port]` or a name) using its one-time code.
    fn pair(&self, target: &str, code: &str, name: Option<&str>) -> Result<PairedPc, PairError>;
    /// Joins a paired PC as viewer (`ViewerJoin`) and starts streaming into `sink`.
    fn connect(&self, pc: &PairedPc, sink: EventSink) -> Result<Box<dyn ViewerSession>, PairError>;
    #[cfg_attr(feature = "demo", allow(dead_code))]
    /// Opens an authenticated session to a paired PC for file transfer (no viewer join).
    fn open_session(&self, _pc: &PairedPc) -> Result<desk_transport::Session, PairError> {
        Err(PairError::Network)
    }
    /// True for the synthetic test backend.
    fn is_demo(&self) -> bool {
        false
    }
    /// Test hook: break the active link and make the next connects fail.
    fn simulate_drop(&self) {}
    /// Test hook: push a host notice (`kind` as in [`SessionEvent::Notice`]) to the live session.
    fn simulate_notice(&self, _kind: &str) {}
    /// Test hook: make the next connect fail with this `PairError` code.
    fn simulate_connect_failure(&self, _code: &str) {}
}

/// Picks the backend for this build.
pub fn select() -> Arc<dyn Backend> {
    #[cfg(feature = "demo")]
    {
        Arc::new(demo::DemoBackend::new())
    }
    #[cfg(not(feature = "demo"))]
    {
        Arc::new(transport::TransportBackend::new())
    }
}
