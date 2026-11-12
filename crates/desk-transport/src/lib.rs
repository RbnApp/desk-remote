//! Central network transport layer for Desk-Remote.
//!
//! Provides the single-point network transport contract for LAN direct connections (mDNS),
//! Tailscale tailnet sessions (via official client or embedded tsnet helper),
//! encrypted [`Session`] handling, and mutual pairing handshake verification.

pub mod discovery;
pub mod listener;
pub mod session;
pub mod tailscale;

pub use discovery::{DiscoveredPeer, LanAnnouncer, LanBrowser, SERVICE_TYPE};
pub use listener::Listener;
pub use session::Session;
pub use tailscale::{TailnetHelperController, TailscaleDetector, TailscaleStatus};

use desk_error::{DeskError, DeskResult};
use std::net::{IpAddr, SocketAddr, TcpStream};

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

impl PeerTarget {
    /// Loopback target on `port` (same-machine viewer, tests, tsnet bridge).
    #[must_use]
    pub fn loopback(port: u16) -> Self {
        Self::Lan(SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)))
    }

    /// Parses `ip:port` text into a LAN target.
    pub fn lan_from_str(text: &str) -> DeskResult<Self> {
        text.parse::<SocketAddr>()
            .map(Self::Lan)
            .map_err(|e| DeskError::Transport(format!("invalid peer address '{text}': {e}")))
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

/// Binds a local listener to accept incoming transport sessions.
pub fn listen(bind_addr: SocketAddr) -> DeskResult<Listener> {
    Listener::bind(bind_addr)
}

#[cfg(test)]
mod tests;
