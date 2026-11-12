//! TCP listener for accepting incoming Desk-Remote connections.

use crate::session::Session;
use desk_error::{DeskError, DeskResult};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// Server socket listener accepting incoming sessions.
pub struct Listener {
    listener: TcpListener,
}

impl Listener {
    /// Binds a new TCP listener to the specified socket address.
    pub fn bind(addr: SocketAddr) -> DeskResult<Self> {
        let listener = TcpListener::bind(addr).map_err(DeskError::Io)?;
        Ok(Self { listener })
    }

    /// Binds to every IPv4 interface on `port` (`0` picks a free port).
    pub fn bind_any(port: u16) -> DeskResult<Self> {
        Self::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
    }

    /// Binds to 127.0.0.1 only on `port` (`0` picks a free port): development and tests,
    /// nothing is reachable from the network and no firewall prompt appears.
    pub fn bind_loopback(port: u16) -> DeskResult<Self> {
        Self::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }

    /// Port the listener is bound to.
    pub fn local_port(&self) -> DeskResult<u16> {
        Ok(self.local_addr()?.port())
    }

    /// Unblocks a thread parked in [`Self::accept`] by connecting to ourselves once.
    pub fn wake(&self) {
        if let Ok(port) = self.local_port() {
            let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            let _ = TcpStream::connect_timeout(&target, Duration::from_millis(500));
        }
    }

    /// Local socket address bound by the listener.
    pub fn local_addr(&self) -> DeskResult<SocketAddr> {
        self.listener.local_addr().map_err(DeskError::Io)
    }

    /// Accepts the next incoming TCP connection and wraps it in a [`Session`].
    pub fn accept(&self) -> DeskResult<Session> {
        let (stream, _) = self.listener.accept().map_err(DeskError::Io)?;
        Session::new(stream)
    }
}
