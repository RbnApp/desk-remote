//! Active transport session and connection abstraction.
//!
//! Owns the underlying TCP socket, wire framing, session encryption via
//! [`desk_crypto::SessionCipher`], and pairing handshake verification.

use desk_crypto::{derive_session_key, PairingManager, SessionCipher};
use desk_error::{DeskError, DeskResult};
use desk_protocol::{FrameCodec, Message, PROTOCOL_VERSION};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Active remote desktop transport session.
pub struct Session {
    stream: TcpStream,
    peer_addr: SocketAddr,
    cipher: Option<SessionCipher>,
    read_buf: Vec<u8>,
}

impl Session {
    /// Wraps an established TCP stream into a [`Session`].
    pub fn new(stream: TcpStream) -> DeskResult<Self> {
        let peer_addr = stream.peer_addr().map_err(DeskError::Io)?;
        stream.set_nodelay(true).map_err(DeskError::Io)?;
        stream
            .set_read_timeout(Some(DEFAULT_READ_TIMEOUT))
            .map_err(DeskError::Io)?;
        stream
            .set_write_timeout(Some(DEFAULT_WRITE_TIMEOUT))
            .map_err(DeskError::Io)?;

        Ok(Self {
            stream,
            peer_addr,
            cipher: None,
            read_buf: Vec::with_capacity(64 * 1024),
        })
    }

    /// Remote peer socket address.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Sets socket read and write timeouts.
    pub fn set_timeout(&mut self, timeout: Option<Duration>) -> DeskResult<()> {
        self.stream
            .set_read_timeout(timeout)
            .map_err(DeskError::Io)?;
        self.stream
            .set_write_timeout(timeout)
            .map_err(DeskError::Io)?;
        Ok(())
    }

    /// Sends a protocol message across the transport.
    pub fn send(&mut self, msg: &Message) -> DeskResult<()> {
        let frame_bytes = FrameCodec::encode(msg)?;

        let payload_to_wire = match &mut self.cipher {
            Some(cipher) => cipher.encrypt(&frame_bytes)?,
            None => frame_bytes,
        };

        let len_u32 = u32::try_from(payload_to_wire.len())
            .map_err(|_| DeskError::Transport("Payload length exceeds u32 limit".to_string()))?;

        self.stream
            .write_all(&len_u32.to_be_bytes())
            .map_err(DeskError::Io)?;
        self.stream
            .write_all(&payload_to_wire)
            .map_err(DeskError::Io)?;
        self.stream.flush().map_err(DeskError::Io)?;

        Ok(())
    }

    /// Receives the next complete message from the transport.
    pub fn recv(&mut self) -> DeskResult<Message> {
        loop {
            // Check if we have at least 4 bytes for length prefix
            if self.read_buf.len() >= 4 {
                let len_bytes: [u8; 4] = self.read_buf[..4]
                    .try_into()
                    .map_err(|_| DeskError::Transport("Slice conversion failed".to_string()))?;
                let frame_len = usize::try_from(u32::from_be_bytes(len_bytes))
                    .map_err(|_| DeskError::Transport("Length conversion failed".to_string()))?;

                let total_needed = 4 + frame_len;
                if self.read_buf.len() >= total_needed {
                    let raw_payload = &self.read_buf[4..total_needed];

                    let decrypted = match &self.cipher {
                        Some(cipher) => cipher.decrypt(raw_payload)?,
                        None => raw_payload.to_vec(),
                    };

                    self.read_buf.drain(..total_needed);

                    let (msg, _) = FrameCodec::decode(&decrypted)?.ok_or_else(|| {
                        DeskError::Protocol("Incomplete frame in packet".to_string())
                    })?;

                    return Ok(msg);
                }
            }

            // Read more bytes from the socket
            let mut chunk = [0u8; 32 * 1024];
            let read_bytes = self.stream.read(&mut chunk).map_err(DeskError::Io)?;
            if read_bytes == 0 {
                return Err(DeskError::Transport("Peer disconnected".to_string()));
            }
            self.read_buf.extend_from_slice(&chunk[..read_bytes]);
        }
    }

    /// Performs the host-side pairing handshake.
    ///
    /// Receives `PairRequest { code }` from the viewer, validates it against `pairing_mgr`.
    /// If rejected, sends `PairResult { success: false }` and returns an error.
    /// If accepted, sends `PairResult { success: true }`, activates session encryption, and succeeds.
    pub fn authenticate_as_host(
        &mut self,
        pairing_mgr: &mut PairingManager,
        host_device_id: &str,
    ) -> DeskResult<()> {
        // Send Hello
        self.send(&Message::Hello {
            device_id: host_device_id.to_string(),
            version: PROTOCOL_VERSION.to_string(),
        })?;

        // Expect Hello from client
        let client_hello = self.recv()?;
        let client_id = match client_hello {
            Message::Hello { device_id, .. } => device_id,
            other => {
                return Err(DeskError::Protocol(format!(
                    "Expected Hello from client, received {other:?}"
                )))
            }
        };

        // Expect PairRequest
        let pair_msg = self.recv()?;
        let code = match pair_msg {
            Message::PairRequest { code } => code,
            other => {
                return Err(DeskError::Protocol(format!(
                    "Expected PairRequest, received {other:?}"
                )))
            }
        };

        let valid = pairing_mgr.verify_code(&code);
        if !valid {
            self.send(&Message::PairResult {
                success: false,
                error: Some("Invalid or expired pairing code".to_string()),
            })?;
            return Err(DeskError::Crypto(format!(
                "Host rejected pairing attempt from {client_id}: invalid pairing code"
            )));
        }

        self.send(&Message::PairResult {
            success: true,
            error: None,
        })?;

        pairing_mgr.add_paired_peer(&client_id);

        // Derive shared session key from verified pairing code
        let session_key = derive_session_key(code.as_bytes(), client_id.as_bytes());
        self.cipher = Some(SessionCipher::new(session_key));

        Ok(())
    }

    /// Performs the client-side pairing handshake with the remote host.
    pub fn authenticate_as_client(
        &mut self,
        client_device_id: &str,
        pairing_code: &str,
    ) -> DeskResult<()> {
        // Expect Hello from host
        let host_hello = self.recv()?;
        let host_id = match host_hello {
            Message::Hello { device_id, .. } => device_id,
            other => {
                return Err(DeskError::Protocol(format!(
                    "Expected Hello from host, received {other:?}"
                )))
            }
        };

        // Send Hello
        self.send(&Message::Hello {
            device_id: client_device_id.to_string(),
            version: PROTOCOL_VERSION.to_string(),
        })?;

        // Send PairRequest
        self.send(&Message::PairRequest {
            code: pairing_code.to_string(),
        })?;

        // Expect PairResult
        let result_msg = self.recv()?;
        match result_msg {
            Message::PairResult {
                success: true,
                error: _,
            } => {
                let session_key =
                    derive_session_key(pairing_code.as_bytes(), client_device_id.as_bytes());
                self.cipher = Some(SessionCipher::new(session_key));
                Ok(())
            }
            Message::PairResult {
                success: false,
                error,
            } => Err(DeskError::Crypto(format!(
                "Remote host {host_id} rejected pairing code: {}",
                error.unwrap_or_else(|| "unspecified error".to_string())
            ))),
            other => Err(DeskError::Protocol(format!(
                "Expected PairResult, received {other:?}"
            ))),
        }
    }

    /// Measures network latency by sending a `Ping` and awaiting the corresponding `Pong`.
    pub fn ping(&mut self) -> DeskResult<Duration> {
        let now_us = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| DeskError::Other(e.to_string()))?
                .as_micros(),
        )
        .map_err(|_| DeskError::Other("Timestamp overflow".to_string()))?;

        let start_instant = Instant::now();
        self.send(&Message::Ping { ts: now_us })?;

        loop {
            match self.recv()? {
                Message::Pong { ts } if ts == now_us => {
                    return Ok(start_instant.elapsed());
                }
                Message::Ping { ts } => {
                    // Echo back incoming ping if received concurrently
                    self.send(&Message::Pong { ts })?;
                }
                _ => {
                    // Discard or ignore interleaved non-pong messages during ping measurement
                }
            }
        }
    }
}
