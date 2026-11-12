//! Zero-configuration local network peer discovery via mDNS.
//!
//! Uses multicast DNS (`_deskremote._tcp.local.`) to locate Desk-Remote host agents
//! on the same local subnet without requiring external directory servers.

use desk_error::{DeskError, DeskResult};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Canonical mDNS service type for Desk-Remote.
pub const SERVICE_TYPE: &str = "_deskremote._tcp.local.";

/// Information about a Desk-Remote host discovered on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    /// Host device identifier announced in TXT record.
    pub device_id: String,
    /// Protocol version supported by host.
    pub version: String,
    /// Hostname announced via mDNS.
    pub host_name: String,
    /// Resolved IP addresses.
    pub addresses: Vec<IpAddr>,
    /// TCP listening port.
    pub port: u16,
}

/// Active mDNS service announcer for an agent.
pub struct LanAnnouncer {
    daemon: ServiceDaemon,
    fullname: String,
}

impl LanAnnouncer {
    /// Starts announcing on every local interface (the daemon tracks address changes).
    pub fn start_auto(device_id: &str, port: u16, version: &str) -> DeskResult<Self> {
        Self::start_inner(device_id, "", port, version, true)
    }

    /// Starts announcing a Desk-Remote host on the local subnet.
    pub fn start(device_id: &str, bind_ip: &str, port: u16, version: &str) -> DeskResult<Self> {
        Self::start_inner(device_id, bind_ip, port, version, false)
    }

    fn start_inner(
        device_id: &str,
        bind_ip: &str,
        port: u16,
        version: &str,
        auto_addr: bool,
    ) -> DeskResult<Self> {
        let daemon = ServiceDaemon::new().map_err(|e| {
            DeskError::Transport(format!("Failed to create mDNS ServiceDaemon: {e}"))
        })?;

        let instance_name = device_id.replace('-', "_");
        let host_name = format!("{instance_name}.local.");
        let properties = [("id", device_id), ("v", version)];

        let mut service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &host_name,
            bind_ip,
            port,
            &properties[..],
        )
        .map_err(|e| DeskError::Transport(format!("Failed to construct ServiceInfo: {e}")))?;

        if auto_addr {
            service_info = service_info.enable_addr_auto();
        }
        let fullname = service_info.get_fullname().to_string();

        daemon
            .register(service_info)
            .map_err(|e| DeskError::Transport(format!("Failed to register mDNS service: {e}")))?;

        Ok(Self { daemon, fullname })
    }
}

impl Drop for LanAnnouncer {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
    }
}

/// Browser that scans the local subnet for active Desk-Remote agents.
pub struct LanBrowser;

impl LanBrowser {
    /// Scans the local network for Desk-Remote agents within the given timeout.
    pub fn browse(timeout: Duration) -> DeskResult<Vec<DiscoveredPeer>> {
        let daemon = ServiceDaemon::new().map_err(|e| {
            DeskError::Transport(format!("Failed to create mDNS browser daemon: {e}"))
        })?;

        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| DeskError::Transport(format!("Failed to browse mDNS service: {e}")))?;

        let start = Instant::now();
        let mut peers = Vec::new();

        while start.elapsed() < timeout {
            if let Ok(ServiceEvent::ServiceResolved(info)) =
                receiver.recv_timeout(Duration::from_millis(100))
            {
                let device_id = info
                    .get_property_val_str("id")
                    .map(ToString::to_string)
                    .unwrap_or_else(|| info.get_fullname().to_string());

                let version = info
                    .get_property_val_str("v")
                    .map(ToString::to_string)
                    .unwrap_or_default();

                peers.push(DiscoveredPeer {
                    device_id,
                    version,
                    host_name: info.get_hostname().to_string(),
                    addresses: info.get_addresses().iter().copied().collect(),
                    port: info.get_port(),
                });
            }
        }

        let _ = daemon.stop_browse(SERVICE_TYPE);
        Ok(peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_announcer_creation_and_teardown() {
        let announcer = LanAnnouncer::start("agent-test-unit", "127.0.0.1", 7777, "0.1.0");
        assert!(announcer.is_ok());
    }
}
