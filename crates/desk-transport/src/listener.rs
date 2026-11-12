//! TCP listener for accepting incoming Desk-Remote connections.

use crate::session::Session;
use desk_error::{DeskError, DeskResult};
use std::net::{SocketAddr, TcpListener};

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
