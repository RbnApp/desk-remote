//! Central network transport layer for Desk-Remote.
//!
//! Provides the single-point network transport contract for LAN direct connections (mDNS),
//! Tailscale tailnet sessions (always through the embedded tsnet helper),
//! encrypted [`Session`] handling, and mutual pairing handshake verification.

pub mod discovery;
pub mod listener;
pub mod local_addr;
pub mod panel_tailnet;
pub mod session;
pub mod split;
pub mod tailscale;

pub use discovery::{DiscoveredPeer, LanAnnouncer, LanBrowser, SERVICE_TYPE};
pub use listener::Listener;
pub use local_addr::primary_local_ip;
pub use panel_tailnet::{
    PanelTailnet, PanelTailnetConfig, PanelTailnetSnapshot, PanelTailnetState,
};
pub use session::{sanitize_peer_name, Session};
pub use split::{SessionReader, SessionWriter};
pub use tailscale::{helper_online, TailscalePeer};

use desk_error::{DeskError, DeskResult};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Target peer destination for outgoing connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerTarget {
    /// Direct local area network target.
    Lan(SocketAddr),
    /// Tailscale tailnet node IP and port.
    Tailscale { ip: IpAddr, port: u16 },
    /// Local loopback proxy bridge (forwarded through tsnet helper).
    LoopbackBridge(SocketAddr),
}

/// Defensive: a discovery result that is this very PC (loopback, own address, or the panel's
/// own device id) is never listed, so a PC cannot watch itself.
#[must_use]
pub fn is_this_pc(
    addresses: &[IpAddr],
    local: Option<IpAddr>,
    device_id: &str,
    own_id: Option<&str>,
) -> bool {
    own_id == Some(device_id)
        || addresses
            .iter()
            .any(|a| a.is_loopback() || local == Some(*a))
}

/// TCP port a Desk-Remote host listens on unless its address says otherwise (single source: desk-paths).
pub use desk_paths::DEFAULT_PORT;

impl PeerTarget {
    /// Loopback target on `port` (same-machine viewer, tests, tsnet bridge).
    #[must_use]
    pub fn loopback(port: u16) -> Self {
        Self::Lan(SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)))
    }

    /// Parses `ip`, `ip:port`, `host` or `host:port` (the port defaults to [`DEFAULT_PORT`]).
    ///
    /// Tailnet addresses (`100.64.0.0/10`) become [`PeerTarget::Tailscale`], everything else
    /// is a direct LAN target. Host names are resolved through the system resolver, which
    /// covers Tailscale MagicDNS names.
    pub fn parse(text: &str) -> DeskResult<Self> {
        let text = text.trim();
        let addr = text
            .parse::<SocketAddr>()
            .ok()
            .or_else(|| {
                text.parse::<IpAddr>()
                    .ok()
                    .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
            })
            .or_else(|| {
                let with_port = if text.contains(':') {
                    text.to_string()
                } else {
                    format!("{text}:{DEFAULT_PORT}")
                };
                with_port.to_socket_addrs().ok()?.next()
            })
            .ok_or_else(|| DeskError::Transport(format!("cannot resolve PC address '{text}'")))?;
        Ok(match addr.ip() {
            IpAddr::V4(v4) if v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]) => {
                Self::Tailscale {
                    ip: addr.ip(),
                    port: addr.port(),
                }
            }
            _ => Self::Lan(addr),
        })
    }

    /// Resolves the socket address to connect to.
    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        match self {
            Self::Lan(addr) | Self::LoopbackBridge(addr) => *addr,
            Self::Tailscale { ip, port } => SocketAddr::new(*ip, *port),
        }
    }
}

/// Initiates an outgoing transport session to the specified peer target.
pub fn connect(target: PeerTarget) -> DeskResult<Session> {
    let addr = target.socket_addr();
    let stream = TcpStream::connect(addr).map_err(DeskError::Io)?;
    Session::new(stream)
}

/// Like [`connect`] but gives up after `timeout` (unreachable PCs must not block the UI).
pub fn connect_timeout(target: &PeerTarget, timeout: Duration) -> DeskResult<Session> {
    let stream =
        TcpStream::connect_timeout(&target.socket_addr(), timeout).map_err(DeskError::Io)?;
    Session::new(stream)
}

/// Binds a local listener to accept incoming transport sessions.
pub fn listen(bind_addr: SocketAddr) -> DeskResult<Listener> {
    Listener::bind(bind_addr)
}

#[cfg(test)]
mod panel_tailnet_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod this_pc_tests {
    use super::*;

    #[test]
    fn this_pc_is_never_listed() {
        let v4 = |d| IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, d));
        let me = v4(5);
        let other = v4(9);
        let lo = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        assert!(is_this_pc(&[me], Some(me), "d1", None));
        assert!(is_this_pc(&[other, lo], Some(me), "d1", None));
        assert!(is_this_pc(&[other], Some(me), "mine", Some("mine")));
        assert!(!is_this_pc(&[other], Some(me), "d1", Some("mine")));
        assert!(!is_this_pc(&[other], None, "d1", None));
    }
}
