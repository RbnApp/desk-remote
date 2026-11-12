//! Typed configuration models for Desk-Remote applications.

use serde::{Deserialize, Serialize};

/// Management panel configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            palette: "ink".to_string(),
            mode: "system".to_string(),
            device_name: "Desk-Operator".to_string(),
            autostart: true,
            minimize_to_tray: true,
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
    }
}

/// System service background daemon configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceConfig {
    pub control_port: u16,
    pub relay_url: Option<String>,
    pub log_level: String,
    pub allow_unattended: bool,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            control_port: 50051,
            relay_url: None,
            log_level: "info".to_string(),
            allow_unattended: true,
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
