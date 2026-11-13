//! TCP listener for accepting incoming Desk-Remote connections.

use crate::session::Session;
use desk_error::{DeskError, DeskResult};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Most connections one listener keeps open at the same time (unauthenticated handshakes,
/// viewers and file senders together). One more is closed on arrival, so a paired but
/// misbehaving panel cannot exhaust threads and memory (DAPP-103 M-01).
pub const MAX_LIVE_CONNECTIONS: usize = 32;

/// Whether a peer address belongs to the networks an agent serves (DAPP-104 H-03): this PC
/// itself (the tailnet bridge connects from loopback), private LAN ranges 10/8, 172.16/12,
/// 192.168/16 and the tailnet/CGNAT range 100.64/10. Everything else is closed on arrival,
/// so binding to every interface never exposes the agent to the public internet.
#[must_use]
pub fn is_private_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || a == 10
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 100 && (64..=127).contains(&b))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                // Unique local fc00::/7 (the tailnet's IPv6 range is fd7a:115c:a1e0::/48).
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || v6.to_ipv4_mapped().is_some_and(|v4| is_private_peer(IpAddr::V4(v4)))
        }
    }
}

/// Holds one live-connection slot; released when the [`Session`] that owns it is dropped.
pub(crate) struct LiveSlot(Arc<AtomicUsize>);

impl Drop for LiveSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Server socket listener accepting incoming sessions.
pub struct Listener {
    listener: TcpListener,
    live: Arc<AtomicUsize>,
    max_live: usize,
}

impl Listener {
    /// Binds a new TCP listener to the specified socket address.
    pub fn bind(addr: SocketAddr) -> DeskResult<Self> {
        let listener = TcpListener::bind(addr).map_err(DeskError::Io)?;
        Ok(Self {
            listener,
            live: Arc::new(AtomicUsize::new(0)),
            max_live: MAX_LIVE_CONNECTIONS,
        })
    }

    /// Overrides [`MAX_LIVE_CONNECTIONS`] (tests).
    #[must_use]
    pub fn with_max_live(mut self, max_live: usize) -> Self {
        self.max_live = max_live;
        self
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
    /// Connections beyond the live limit are closed at once and never returned.
    pub fn accept(&self) -> DeskResult<Session> {
        loop {
            let (stream, peer) = self.listener.accept().map_err(DeskError::Io)?;
            if !is_private_peer(peer.ip()) {
                tracing::warn!(%peer, "connection from a non-private network refused");
                continue;
            }
            if self.live.fetch_add(1, Ordering::SeqCst) >= self.max_live {
                self.live.fetch_sub(1, Ordering::SeqCst);
                tracing::warn!(%peer, limit = self.max_live, "too many open connections, closed");
                continue;
            }
            let slot = LiveSlot(Arc::clone(&self.live));
            let mut session = Session::new(stream)?;
            session.live_slot = Some(slot);
            return Ok(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;

    /// True when the peer closed the socket without sending anything.
    fn closed_by_server(client: &mut TcpStream) -> bool {
        let _ = client.set_read_timeout(Some(Duration::from_secs(3)));
        let mut byte = [0u8; 1];
        matches!(client.read(&mut byte), Ok(0))
    }

    #[test]
    fn only_loopback_private_lan_and_tailnet_sources_are_served() {
        let ok = |s: &str| s.parse::<IpAddr>().is_ok_and(is_private_peer);
        for allowed in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.20",
            "100.64.0.7",
            "100.127.255.255",
            "::1",
            "fd7a:115c:a1e0::1",
            "::ffff:192.168.0.5",
        ] {
            assert!(ok(allowed), "{allowed} must be served");
        }
        for refused in [
            "8.8.8.8",
            "172.15.0.1",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "192.169.0.1",
            "11.0.0.1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!ok(refused), "{refused} must be refused");
        }
    }

    #[test]
    fn connections_over_the_live_limit_are_closed_and_slots_are_reused() -> DeskResult<()> {
        let listener = Arc::new(Listener::bind_loopback(0)?.with_max_live(2));
        let port = listener.local_port()?;
        let (tx, rx) = mpsc::channel();
        let accepting = Arc::clone(&listener);
        std::thread::spawn(move || {
            while let Ok(session) = accepting.accept() {
                if tx.send(session).is_err() {
                    break;
                }
            }
        });
        let connect = || TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(DeskError::Io);
        let _a = connect()?;
        let first = rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|e| DeskError::Transport(e.to_string()))?;
        let _b = connect()?;
        let _second = rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|e| DeskError::Transport(e.to_string()))?;
        let mut over = connect()?;
        assert!(
            closed_by_server(&mut over),
            "third connection must be closed"
        );
        assert!(
            rx.try_recv().is_err(),
            "third connection must not reach the caller"
        );
        // Releasing one session frees its slot for a new connection.
        drop(first);
        let _c = connect()?;
        assert!(rx.recv_timeout(Duration::from_secs(3)).is_ok());
        listener.wake();
        Ok(())
    }
}
