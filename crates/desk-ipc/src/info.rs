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
    /// The helper stopped unexpectedly; the service starts it again.
    Failed,
    /// Not running (disabled, not installed, failed or not reported yet).
    #[default]
    Off,
}

/// One paired panel as the tray list shows it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedPeerInfo {
    /// Device ID, the handle for "remove pairing".
    pub device_id: String,
    /// Name the panel last announced (empty until it connected once).
    pub name: String,
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
    /// Browser login URL when TailnetState is NeedsLogin.
    #[serde(default)]
    pub tailnet_auth_url: Option<String>,
    /// True when official Tailscale takeover requires administrator privileges.
    #[serde(default)]
    pub takeover_needs_admin: bool,
    /// Stable device ID viewers see while pairing.
    pub device_id: String,
    /// Long form of the identity key digest (see `DeviceIdentity::fingerprint`).
    pub fingerprint: String,
    /// Viewers currently watching.
    pub viewers: u32,
    /// The desktop cannot be shown (no display). Lock screen and UAC prompt are shown by the
    /// secure-desktop helper and do not count.
    pub unavailable: bool,
    /// Remote input is blocked (watch only).
    pub view_only: bool,
    /// Panels paired with this PC (tray "Paired panels" list, DAPP-104).
    #[serde(default)]
    pub paired: Vec<PairedPeerInfo>,
}
