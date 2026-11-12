//! Read-only description of the agent that the tray menu shows (service -> helper).
//!
//! The service gathers it (network, tailnet, identity); the helper never queries the
//! network itself and only renders this snapshot.

use serde::{Deserialize, Serialize};

/// State of the embedded tailnet helper as the tray menu shows it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TailnetState {
    /// Joined the tailnet and listening.
    Connected,
    /// The node still needs an interactive sign-in.
    NeedsLogin,
    /// Not running (disabled, not installed, failed or not reported yet).
    #[default]
    Off,
}

/// Snapshot of the agent for the tray menu and the About window.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInfo {
    /// Computer name.
    pub pc_name: String,
    /// IPv4 address of the primary interface (`127.0.0.1` in loopback-only mode).
    pub local_ip: Option<String>,
    /// TCP port the agent really listens on.
    pub port: u16,
    pub tailnet: TailnetState,
    /// Tailnet addresses of this node (empty unless connected).
    pub tailnet_ips: Vec<String>,
    /// Stable device ID viewers see while pairing.
    pub device_id: String,
    /// Long form of the identity key digest (see `DeviceIdentity::fingerprint`).
    pub fingerprint: String,
    /// Viewers currently watching.
    pub viewers: u32,
    /// The desktop cannot be shown (locked, secure desktop or no display).
    pub unavailable: bool,
    /// Remote input is blocked (watch only).
    pub view_only: bool,
}
