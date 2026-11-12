//! Active transport session and connection abstraction.
//!
//! Owns the TCP socket and drives the connection security protocol:
//!
//! 1. 5-byte preamble `DRM1` + mode (`PAIR` first time, `SESSION` afterwards).
//! 2. `PAIR` only: SPAKE2 key exchange keyed by the short code (the code is never sent),
//!    mutual key confirmation, then exchange of the static public keys sealed under the
//!    PAKE key. Both sides persist the peer key (pinning).
//! 3. Always: `Noise_XX_25519_ChaChaPoly_BLAKE2s` with static-key pinning. A peer whose
//!    static key is not in the trust store is rejected before any application data.
//! 4. Encrypted `Hello` exchange; the claimed `device_id` must equal the one derived from
//!    the authenticated static key.
//!
//! Application messages are `desk-protocol` binary frames, split into Noise-sized chunks.

use desk_crypto::pake::{PakeKeys, PakeRole, PakeState};
use desk_crypto::{
    device_id_from_public, fingerprint_from_public, DeviceIdentity, NoiseHandshake, NoiseTransport,
    PairingManager, TrustStore, KEY_LEN, NOISE_MAX_MESSAGE, NOISE_MAX_PLAINTEXT,
};
use desk_error::{DeskError, DeskResult};
use desk_protocol::{FrameCodec, Message, PROTOCOL_VERSION};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Connection preamble magic + wire-security version.
const PREAMBLE_MAGIC: [u8; 4] = *b"DRM1";
const MODE_SESSION: u8 = 0x02;
const MODE_PAIR: u8 = 0x01;

/// Locks the shared pairing state for one short step (never across network I/O).
fn pairing_step(mgr: &Mutex<PairingManager>) -> MutexGuard<'_, PairingManager> {
    mgr.lock().unwrap_or_else(|p| p.into_inner())
}

/// Encodes `msg` and encrypts it into length-prefixed Noise chunks ready for the socket.
pub(crate) fn seal_message(transport: &mut NoiseTransport, msg: &Message) -> DeskResult<Vec<u8>> {
    let frame = FrameCodec::encode(msg)?;
    let mut wire = Vec::with_capacity(frame.len() + frame.len() / 1024 * 24 + 64);
    for chunk in frame.chunks(NOISE_MAX_PLAINTEXT) {
        let ct = transport.encrypt(chunk)?;
        let len = u32::try_from(ct.len())
            .map_err(|_| DeskError::Transport("chunk length overflow".to_string()))?;
        wire.extend_from_slice(&len.to_be_bytes());
        wire.extend_from_slice(&ct);
    }
    Ok(wire)
}

/// Active remote desktop transport session.
pub struct Session {
    pub(crate) stream: TcpStream,
    pub(crate) peer_addr: SocketAddr,
    pub(crate) transport: Option<NoiseTransport>,
    pub(crate) plain_buf: Vec<u8>,
    pub(crate) peer_key: Option<[u8; KEY_LEN]>,
    pub(crate) peer_name: Option<String>,
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
            transport: None,
            plain_buf: Vec::with_capacity(64 * 1024),
            peer_key: None,
            peer_name: None,
        })
    }

    /// Remote peer socket address.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Authenticated static public key of the peer (after a successful handshake).
    #[must_use]
    pub fn peer_public_key(&self) -> Option<[u8; KEY_LEN]> {
        self.peer_key
    }

    /// Authenticated device ID of the peer (derived from its static key).
    #[must_use]
    pub fn peer_device_id(&self) -> Option<String> {
        self.peer_key.as_ref().map(device_id_from_public)
    }

    /// Visual fingerprint of the peer static public key (if authenticated).
    #[must_use]
    pub fn peer_fingerprint(&self) -> Option<String> {
        self.peer_key.as_ref().map(fingerprint_from_public)
    }

    /// Claimed computer/peer name sent during handshake.
    #[must_use]
    pub fn peer_name(&self) -> Option<&str> {
        self.peer_name.as_deref()
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

    // ---- raw length-prefixed frames (handshake and Noise chunks) ----

    fn write_raw(&mut self, payload: &[u8]) -> DeskResult<()> {
        if payload.len() > NOISE_MAX_MESSAGE {
            return Err(DeskError::Transport("raw frame too large".to_string()));
        }
        let len = u32::try_from(payload.len())
            .map_err(|_| DeskError::Transport("raw frame length overflow".to_string()))?;
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
        self.stream.write_all(&out).map_err(DeskError::Io)?;
        self.stream.flush().map_err(DeskError::Io)
    }

    pub(crate) fn read_raw(&mut self) -> DeskResult<Vec<u8>> {
        let mut len_bytes = [0u8; 4];
        self.read_exact_mapped(&mut len_bytes)?;
        let len = usize::try_from(u32::from_be_bytes(len_bytes))
            .map_err(|_| DeskError::Transport("length conversion failed".to_string()))?;
        if len > NOISE_MAX_MESSAGE {
            return Err(DeskError::Transport(format!(
                "raw frame of {len} bytes exceeds limit"
            )));
        }
        let mut payload = vec![0u8; len];
        self.read_exact_mapped(&mut payload)?;
        Ok(payload)
    }

    fn read_exact_mapped(&mut self, buf: &mut [u8]) -> DeskResult<()> {
        self.stream.read_exact(buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                DeskError::Transport("Peer disconnected".to_string())
            } else {
                DeskError::Io(e)
            }
        })
    }

    // ---- application messages ----

    /// Sends a protocol message; fails before authentication completed.
    pub fn send(&mut self, msg: &Message) -> DeskResult<()> {
        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| DeskError::Transport("session is not authenticated".to_string()))?;
        let wire = seal_message(transport, msg)?;
        self.stream.write_all(&wire).map_err(DeskError::Io)?;
        self.stream.flush().map_err(DeskError::Io)
    }

    /// Receives the next complete message from the transport.
    ///
    /// Any integrity failure (tampering, replay, reordering) is an error; the caller must
    /// drop the session afterwards.
    pub fn recv(&mut self) -> DeskResult<Message> {
        loop {
            if let Some((msg, used)) = FrameCodec::decode(&self.plain_buf)? {
                self.plain_buf.drain(..used);
                return Ok(msg);
            }
            let ct = self.read_raw()?;
            let transport = self
                .transport
                .as_mut()
                .ok_or_else(|| DeskError::Transport("session is not authenticated".to_string()))?;
            let plain = transport.decrypt(&ct)?;
            self.plain_buf.extend_from_slice(&plain);
        }
    }

    /// Like [`Self::recv`] but returns `Ok(None)` when no data arrives within `wait`.
    ///
    /// Readiness is probed with `peek`, so a timeout can never leave a half-read chunk
    /// behind: once the first byte is available the whole chunk is read with the normal
    /// read timeout. This lets one thread multiplex sending and receiving on a session.
    pub fn recv_timeout(&mut self, wait: Duration) -> DeskResult<Option<Message>> {
        if let Some((msg, used)) = FrameCodec::decode(&self.plain_buf)? {
            self.plain_buf.drain(..used);
            return Ok(Some(msg));
        }
        self.stream
            .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
            .map_err(DeskError::Io)?;
        let mut probe = [0u8; 1];
        let probed = self.stream.peek(&mut probe);
        self.stream
            .set_read_timeout(Some(DEFAULT_READ_TIMEOUT))
            .map_err(DeskError::Io)?;
        match probed {
            Ok(0) => Err(DeskError::Transport("Peer disconnected".to_string())),
            Ok(_) => self.recv().map(Some),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(DeskError::Io(e)),
        }
    }

    // ---- handshakes ----

    /// Host side: accepts a panel in `PAIR` or `SESSION` mode.
    ///
    /// * `PAIR` requires an active code in `pairing_mgr`; the panel key learned through the
    ///   PAKE is stored in the manager's trust store and must match the Noise static key.
    /// * Any panel whose static key is not trusted is rejected.
    ///
    /// The manager is locked only for the short steps that touch its state; a slow or
    /// stalled peer never blocks other handshakes while the network is being read.
    pub fn authenticate_as_host(
        &mut self,
        pairing_mgr: &Mutex<PairingManager>,
        identity: &DeviceIdentity,
    ) -> DeskResult<()> {
        self.set_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let result = self.host_handshake(pairing_mgr, identity);
        self.set_timeout(Some(DEFAULT_READ_TIMEOUT))?;
        result
    }

    fn host_handshake(
        &mut self,
        pairing_mgr: &Mutex<PairingManager>,
        identity: &DeviceIdentity,
    ) -> DeskResult<()> {
        let mut preamble = [0u8; 5];
        self.read_exact_mapped(&mut preamble)?;
        if preamble[..4] != PREAMBLE_MAGIC {
            return Err(DeskError::Protocol(
                "invalid connection preamble".to_string(),
            ));
        }

        let paired_key = match preamble[4] {
            MODE_PAIR => Some(self.host_pair(pairing_mgr, identity)?),
            MODE_SESSION => None,
            other => {
                return Err(DeskError::Protocol(format!(
                    "unknown connection mode {other:#04x}"
                )))
            }
        };

        let mut hs = NoiseHandshake::responder(identity)?;
        let m1 = self.read_raw()?;
        hs.read_message(&m1)?;
        let m2 = hs.write_message()?;
        self.write_raw(&m2)?;
        let m3 = self.read_raw()?;
        hs.read_message(&m3)?;

        let remote = hs
            .remote_static()
            .ok_or_else(|| DeskError::Crypto("peer static key missing".to_string()))?;
        if !pairing_step(pairing_mgr).is_trusted(&remote) {
            return Err(DeskError::Crypto(format!(
                "rejected unpaired peer {}",
                device_id_from_public(&remote)
            )));
        }
        if let Some(expected) = paired_key {
            if expected != remote {
                return Err(DeskError::Crypto(
                    "session key differs from the key exchanged during pairing".to_string(),
                ));
            }
        }
        self.transport = Some(hs.into_transport()?);
        self.peer_key = Some(remote);

        self.verify_peer_hello(&remote)?;
        self.send_hello(identity)
    }

    fn host_pair(
        &mut self,
        pairing_mgr: &Mutex<PairingManager>,
        identity: &DeviceIdentity,
    ) -> DeskResult<[u8; KEY_LEN]> {
        let code = pairing_step(pairing_mgr)
            .active_code()
            .map(str::to_string)
            .ok_or_else(|| DeskError::Crypto("no active pairing code".to_string()))?;

        let panel_msg = self.read_raw()?;
        let (state, host_msg) = PakeState::start(PakeRole::Host, &code);
        self.write_raw(&host_msg)?;
        let keys = match state.finish(&panel_msg) {
            Ok(k) => k,
            Err(e) => {
                pairing_step(pairing_mgr).record_failure();
                return Err(e);
            }
        };

        // The host verifies first and only answers on success, so a wrong-code client never
        // obtains a value it could test guesses against offline.
        let tag = self.read_raw()?;
        if !keys.verify_confirmation(PakeRole::Panel, &tag) {
            pairing_step(pairing_mgr).record_failure();
            return Err(DeskError::Crypto(
                "pairing rejected: invalid pairing code".to_string(),
            ));
        }
        self.write_raw(&keys.confirmation_tag(PakeRole::Host)?)?;

        let sealed = self.read_raw()?;
        let panel_pub = keys.open_static_key(&sealed)?;
        self.write_raw(&keys.seal_static_key(identity.public_key())?)?;

        let mut mgr = pairing_step(pairing_mgr);
        // The lock was released during network I/O: the code may have been used by a
        // concurrent pairing or revoked after failed attempts. Single use is decided here.
        if mgr.active_code() != Some(code.as_str()) {
            return Err(DeskError::Crypto(
                "pairing rejected: code already used or revoked".to_string(),
            ));
        }
        mgr.consume_code();
        mgr.add_trusted(&panel_pub)?;
        drop(mgr);
        Ok(panel_pub)
    }

    /// Panel side: connects to a host. With `pairing_code` the first-time PAKE pairing runs
    /// first and the host key is pinned in `trust`; without it the host key must already be
    /// pinned.
    pub fn authenticate_as_client(
        &mut self,
        identity: &DeviceIdentity,
        trust: &mut TrustStore,
        pairing_code: Option<&str>,
    ) -> DeskResult<()> {
        self.authenticate_as_client_with_name(identity, trust, pairing_code, None)
    }

    /// Panel side: connects to a host, announcing a specific peer computer name.
    pub fn authenticate_as_client_with_name(
        &mut self,
        identity: &DeviceIdentity,
        trust: &mut TrustStore,
        pairing_code: Option<&str>,
        peer_name: Option<&str>,
    ) -> DeskResult<()> {
        self.set_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let result = self.client_handshake(
            identity,
            trust,
            pairing_code,
            &identity.device_id(),
            peer_name,
        );
        self.set_timeout(Some(DEFAULT_READ_TIMEOUT))?;
        result
    }

    fn client_handshake(
        &mut self,
        identity: &DeviceIdentity,
        trust: &mut TrustStore,
        pairing_code: Option<&str>,
        claimed_device_id: &str,
        custom_name: Option<&str>,
    ) -> DeskResult<()> {
        let mode = if pairing_code.is_some() {
            MODE_PAIR
        } else {
            MODE_SESSION
        };
        let mut preamble = Vec::with_capacity(5);
        preamble.extend_from_slice(&PREAMBLE_MAGIC);
        preamble.push(mode);
        self.stream.write_all(&preamble).map_err(DeskError::Io)?;

        let paired_key = match pairing_code {
            Some(code) => {
                let host_pub = self.client_pair(identity, code)?;
                trust.add(&host_pub)?;
                Some(host_pub)
            }
            None => None,
        };

        let mut hs = NoiseHandshake::initiator(identity)?;
        let m1 = hs.write_message()?;
        self.write_raw(&m1)?;
        let m2 = self.read_raw()?;
        hs.read_message(&m2)?;

        // Pin check happens BEFORE our static key is revealed in message 3.
        let remote = hs
            .remote_static()
            .ok_or_else(|| DeskError::Crypto("host static key missing".to_string()))?;
        if !trust.is_trusted(&remote) {
            return Err(DeskError::Crypto(format!(
                "host {} is not paired (unknown key, possible interception)",
                device_id_from_public(&remote)
            )));
        }
        if let Some(expected) = paired_key {
            if expected != remote {
                return Err(DeskError::Crypto(
                    "session key differs from the key exchanged during pairing".to_string(),
                ));
            }
        }
        let m3 = hs.write_message()?;
        self.write_raw(&m3)?;
        self.transport = Some(hs.into_transport()?);
        self.peer_key = Some(remote);

        let name_to_send = custom_name
            .map(str::to_string)
            .unwrap_or_else(local_pc_name);
        self.send_hello_as(claimed_device_id, Some(&name_to_send))?;
        self.verify_peer_hello(&remote)
    }

    fn client_pair(&mut self, identity: &DeviceIdentity, code: &str) -> DeskResult<[u8; KEY_LEN]> {
        let (state, panel_msg) = PakeState::start(PakeRole::Panel, code);
        self.write_raw(&panel_msg)?;
        let rejected = |e: DeskError| match e {
            DeskError::Transport(_) | DeskError::Io(_) => DeskError::Crypto(
                "pairing rejected by host (wrong, expired or already used code)".to_string(),
            ),
            other => other,
        };
        let host_msg = self.read_raw().map_err(rejected)?;
        let keys: PakeKeys = state.finish(&host_msg)?;

        self.write_raw(&keys.confirmation_tag(PakeRole::Panel)?)?;
        let host_tag = self.read_raw().map_err(rejected)?;
        if !keys.verify_confirmation(PakeRole::Host, &host_tag) {
            return Err(DeskError::Crypto(
                "host failed key confirmation (wrong code or impostor)".to_string(),
            ));
        }

        self.write_raw(&keys.seal_static_key(identity.public_key())?)?;
        let sealed = self.read_raw().map_err(rejected)?;
        keys.open_static_key(&sealed)
    }

    fn send_hello(&mut self, identity: &DeviceIdentity) -> DeskResult<()> {
        let name = local_pc_name();
        self.send_hello_as(&identity.device_id(), Some(&name))
    }

    fn send_hello_as(&mut self, device_id: &str, peer_name: Option<&str>) -> DeskResult<()> {
        self.send(&Message::Hello {
            device_id: device_id.to_string(),
            version: PROTOCOL_VERSION.to_string(),
            peer_name: peer_name.map(str::to_string),
        })
    }

    /// Reads the peer's `Hello` and checks that the claimed ID belongs to its proven key.
    fn verify_peer_hello(&mut self, remote: &[u8; KEY_LEN]) -> DeskResult<()> {
        match self.recv()? {
            Message::Hello {
                device_id,
                version,
                peer_name,
            } => {
                if device_id != device_id_from_public(remote) {
                    return Err(DeskError::Crypto(format!(
                        "peer claimed device id {device_id} that does not match its key"
                    )));
                }
                if version != PROTOCOL_VERSION {
                    return Err(DeskError::Protocol(format!(
                        "protocol version mismatch: {version}"
                    )));
                }
                self.peer_name = peer_name.as_deref().map(sanitize_peer_name);
                Ok(())
            }
            other => Err(DeskError::Protocol(format!(
                "Expected Hello, received {other:?}"
            ))),
        }
    }

    /// Test hook: client handshake claiming a foreign device id.
    #[cfg(test)]
    pub(crate) fn authenticate_as_client_claiming(
        &mut self,
        identity: &DeviceIdentity,
        trust: &mut TrustStore,
        claimed_device_id: &str,
    ) -> DeskResult<()> {
        self.set_timeout(Some(HANDSHAKE_TIMEOUT))?;
        self.client_handshake(identity, trust, None, claimed_device_id, None)
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

/// Sanitizes a peer/PC name: removes control characters, trims whitespace, limits length to 64 chars.
#[must_use]
pub fn sanitize_peer_name(raw: &str) -> String {
    let clean: String = raw.chars().filter(|c| !c.is_control()).take(64).collect();
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        "Desk-PC".to_string()
    } else {
        trimmed.to_string()
    }
}

fn local_pc_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Desk-PC".to_string())
}
