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
mod tests {
    use super::*;
    use desk_crypto::PairingManager;
    use desk_protocol::Message;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_end_to_end_lan_pairing_and_10mb_payload() -> DeskResult<()> {
        let listener = listen(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let host_addr = listener.local_addr()?;

        let mut pairing_mgr = PairingManager::new();
        let valid_code = pairing_mgr.create_pairing_code();
        let code_for_client = valid_code.clone();

        // Host thread
        let host_handle = thread::spawn(move || -> DeskResult<()> {
            let mut host_session = listener.accept()?;
            host_session.authenticate_as_host(&mut pairing_mgr, "desk-host-test")?;

            loop {
                match host_session.recv() {
                    Ok(Message::Ping { ts }) => {
                        host_session.send(&Message::Pong { ts })?;
                    }
                    Ok(Message::VideoFrame {
                        ts_us,
                        keyframe: _,
                        data,
                    }) => {
                        assert_eq!(data.len(), 10 * 1024 * 1024);
                        let latency = u32::try_from(ts_us).unwrap_or(0);
                        host_session.send(&Message::Status {
                            online: true,
                            latency_ms: latency,
                        })?;
                        break;
                    }
                    Ok(other) => {
                        return Err(DeskError::Protocol(format!(
                            "Unexpected message: {other:?}"
                        )));
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
            Ok(())
        });

        // Client thread
        let client_handle = thread::spawn(move || -> DeskResult<()> {
            thread::sleep(Duration::from_millis(50));
            let mut client_session = connect(PeerTarget::Lan(host_addr))?;

            client_session.authenticate_as_client("desk-panel-test", &code_for_client)?;

            let latency = client_session.ping()?;
            assert!(latency < Duration::from_secs(2));

            let ten_mb_data = vec![0xABu8; 10 * 1024 * 1024];
            let large_frame = Message::VideoFrame {
                ts_us: 42,
                keyframe: true,
                data: ten_mb_data,
            };
            client_session.send(&large_frame)?;

            let status_reply = client_session.recv()?;
            assert_eq!(
                status_reply,
                Message::Status {
                    online: true,
                    latency_ms: 42,
                }
            );
            Ok(())
        });

        match host_handle.join() {
            Ok(res) => res?,
            Err(_) => return Err(DeskError::Other("Host thread panicked".to_string())),
        }

        match client_handle.join() {
            Ok(res) => res?,
            Err(_) => return Err(DeskError::Other("Client thread panicked".to_string())),
        }

        Ok(())
    }

    #[test]
    fn test_unpaired_invalid_code_rejected() -> DeskResult<()> {
        let listener = listen(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let host_addr = listener.local_addr()?;

        let mut pairing_mgr = PairingManager::new();
        let _valid_code = pairing_mgr.create_pairing_code();
        let invalid_code = "WRNG-9999";

        let host_handle = thread::spawn(move || {
            let session_res = listener.accept();
            if let Ok(mut host_session) = session_res {
                let result = host_session.authenticate_as_host(&mut pairing_mgr, "desk-host-test");
                assert!(result.is_err());
                if let Err(e) = result {
                    assert!(e.to_string().contains("invalid pairing code"));
                }
            }
        });

        let client_handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let conn_res = connect(PeerTarget::Lan(host_addr));
            if let Ok(mut client_session) = conn_res {
                let result =
                    client_session.authenticate_as_client("desk-panel-rogue", invalid_code);
                assert!(result.is_err());
                if let Err(e) = result {
                    assert!(e.to_string().contains("rejected"));
                }
            }
        });

        if host_handle.join().is_err() {
            return Err(DeskError::Other("Host join error".to_string()));
        }
        if client_handle.join().is_err() {
            return Err(DeskError::Other("Client join error".to_string()));
        }

        Ok(())
    }
}
