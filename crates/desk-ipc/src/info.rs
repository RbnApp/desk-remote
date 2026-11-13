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

/// How the Windows service that hosts this agent starts (Settings > General).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceStart {
    /// Starts with Windows (the installed default).
    #[default]
    Automatic,
    /// Starts only when someone starts it: the PC is not reachable after a restart.
    Manual,
    /// No service on this PC (portable / double-click agent).
    NotInstalled,
}

/// What the Settings window shows and changes; the service owns every value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSettings {
    /// Name set by the user; empty means the Windows computer name.
    pub device_name: String,
    /// The agent looks for updates and installs them by itself.
    pub auto_update: bool,
    /// Remote access also works while nobody is at the PC (locked screen).
    pub allow_unattended: bool,
    /// Largest file this PC accepts, in MB.
    pub max_upload_mb: u32,
    pub service_start: ServiceStart,
    /// Newest version the last update check saw (empty until the first check finished).
    pub latest_version: String,
    /// When the last update check finished, seconds since the Unix epoch (`0` = none yet).
    pub last_update_check: u64,
}

impl Default for AgentSettings {
    /// Today's behavior, so a snapshot from an older service reads the same as before.
    fn default() -> Self {
        Self {
            device_name: String::new(),
            auto_update: true,
            allow_unattended: true,
            max_upload_mb: 100,
            service_start: ServiceStart::Automatic,
            latest_version: String::new(),
            last_update_check: 0,
        }
    }
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
    /// Values of the Settings window (an older service sends none: defaults apply).
    #[serde(default)]
    pub settings: AgentSettings,
}
