//! Tailnet facts shared by the agent and the panel.
//!
//! Neither side needs the official Tailscale client: the tailnet path is ALWAYS the embedded
//! `desk-tailnet-helper` (agent: `listen` mode in the service, panel: [`crate::panel_tailnet`]).

use serde::{Deserialize, Serialize};

/// Another node of the tailnet as reported by the embedded helper.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TailscalePeer {
    pub host_name: String,
    #[serde(default)]
    pub ips: Vec<String>,
    #[serde(default)]
    pub online: bool,
}

/// True when the helper reported `listening` with a tailnet IP and its local bridge target
/// (`127.0.0.1:bridge_port`) accepts TCP. This is the gate before an official client is stopped.
#[must_use]
pub fn helper_online(status: &str, ips: &[String], bridge_port: u16) -> bool {
    let has_ip = ips.iter().any(|ip| ip.parse::<std::net::IpAddr>().is_ok());
    if status != "listening" || !has_ip {
        return false;
    }
    let bridge = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bridge_port));
    std::net::TcpStream::connect_timeout(&bridge, std::time::Duration::from_secs(3)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ips(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn helper_online_needs_status_ip_and_open_bridge() {
        assert!(!helper_online("listening", &[], 1));
        assert!(!helper_online("error", &ips(&["100.64.0.1"]), 1));
        assert!(!helper_online("listening", &ips(&["not-an-ip"]), 1));
        let Ok(l) = std::net::TcpListener::bind("127.0.0.1:0") else {
            panic!("bind failed");
        };
        let Ok(addr) = l.local_addr() else {
            panic!("no local addr");
        };
        let port = addr.port();
        assert!(helper_online("listening", &ips(&["100.64.0.1"]), port));
        drop(l);
        assert!(!helper_online("listening", &ips(&["100.64.0.1"]), port));
    }
}
