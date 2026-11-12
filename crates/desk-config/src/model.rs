//! Typed configuration models for Desk-Remote applications.

use serde::{Deserialize, Serialize};

/// A remote PC that completed pairing with this panel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedPc {
    /// Stable identifier of the paired PC.
    pub id: String,
    /// Display name chosen at pairing time.
    pub name: String,
    /// Last known `host:port` (LAN or Tailscale) used to reach the PC without discovery.
    #[serde(default)]
    pub address: Option<String>,
}

/// Management panel configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PanelConfig {
    /// Active theme palette ("ink" default, "petrol").
    pub palette: String,
    /// Active theme mode ("system" default, "light", "dark").
    pub mode: String,
    /// Friendly device name shown to peers.
    pub device_name: String,
    /// Start on Windows user login.
    pub autostart: bool,
    /// Minimize to system tray instead of closing.
    pub minimize_to_tray: bool,
    /// UI language ("auto" follows the OS, "en", "tr").
    pub language: String,
    /// PC list layout ("cards" default, "rows").
    pub list_view: String,
    /// PCs paired with this panel.
    pub paired_pcs: Vec<PairedPc>,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            palette: "ink".to_string(),
            mode: "system".to_string(),
            device_name: "Desk-Operator".to_string(),
            autostart: true,
            minimize_to_tray: true,
            language: "auto".to_string(),
            list_view: "cards".to_string(),
            paired_pcs: Vec::new(),
        }
    }
}

impl PanelConfig {
    /// Normalizes configuration values, reverting invalid fields to defaults.
    pub fn sanitize(&mut self) {
        if self.palette != "ink" && self.palette != "petrol" {
            self.palette = "ink".to_string();
        }
        if self.mode != "system" && self.mode != "light" && self.mode != "dark" {
            self.mode = "system".to_string();
        }
        if !matches!(self.language.as_str(), "auto" | "en" | "tr") {
            self.language = "auto".to_string();
        }
        if self.list_view != "cards" && self.list_view != "rows" {
            self.list_view = "cards".to_string();
        }
    }
}

/// System service background daemon configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceConfig {
    pub control_port: u16,
    pub relay_url: Option<String>,
    pub log_level: String,
    pub allow_unattended: bool,
    /// Announce this PC on the local network (mDNS).
    pub lan_announce: bool,
    /// Start the embedded tailnet helper when no Tailscale client is installed.
    pub tailnet_helper: bool,
    /// Capture frame rate while a viewer is connected.
    pub capture_fps: u32,
    /// Largest streamed frame (the capture is downscaled to fit).
    pub capture_max_width: u32,
    pub capture_max_height: u32,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            control_port: desk_paths::DEFAULT_PORT,
            relay_url: None,
            log_level: "info".to_string(),
            allow_unattended: true,
            lan_announce: true,
            tailnet_helper: true,
            capture_fps: 30,
            capture_max_width: 1920,
            capture_max_height: 1080,
        }
    }
}

/// Standalone relay signaling daemon configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayConfig {
    pub bind_address: String,
    pub public_url: Option<String>,
    pub max_connections: u32,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:8080".to_string(),
            public_url: None,
            max_connections: 1000,
        }
    }
}
