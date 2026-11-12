//! Noise_XX session channel (the single, central session-encryption path).
//!
//! `Noise_XX_25519_ChaChaPoly_BLAKE2s` via the audited `snow` crate gives mutual static-key
//! authentication, forward secrecy (ephemeral keys per connection), separate keys per
//! direction and strictly increasing implicit nonces, so a replayed or reordered
//! ciphertext fails authentication. Pinning of the peer static key is done by the caller
//! through [`NoiseHandshake::remote_static`] against a [`crate::TrustStore`].

use crate::identity::{DeviceIdentity, KEY_LEN, NOISE_PARAMS};
use desk_error::{DeskError, DeskResult};

/// Handshake prologue, bound into the transcript hash.
const PROLOGUE: &[u8] = b"desk-remote/noise/1";

/// Maximum Noise message size defined by the specification.
pub const NOISE_MAX_MESSAGE: usize = 65_535;
/// AEAD tag length appended to every transport message.
pub const NOISE_TAG_LEN: usize = 16;
/// Maximum plaintext that fits one transport message.
pub const NOISE_MAX_PLAINTEXT: usize = NOISE_MAX_MESSAGE - NOISE_TAG_LEN;

fn crypto_err(what: &str, e: &snow::Error) -> DeskError {
    DeskError::Crypto(format!("{what}: {e}"))
}

/// In-progress XX handshake.
pub struct NoiseHandshake {
    state: snow::HandshakeState,
}

impl std::fmt::Debug for NoiseHandshake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseHandshake").finish_non_exhaustive()
    }
}

impl NoiseHandshake {
    fn build(identity: &DeviceIdentity, initiator: bool) -> DeskResult<Self> {
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|e| crypto_err("invalid noise params", &e))?;
        let builder = snow::Builder::new(params)
            .local_private_key(identity.private_key())
            .prologue(PROLOGUE);
        let state = if initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(|e| crypto_err("noise handshake init failed", &e))?;
        Ok(Self { state })
    }

    /// Starts a handshake as the connecting side.
    pub fn initiator(identity: &DeviceIdentity) -> DeskResult<Self> {
        Self::build(identity, true)
    }

    /// Starts a handshake as the accepting side.
    pub fn responder(identity: &DeviceIdentity) -> DeskResult<Self> {
        Self::build(identity, false)
    }

    /// Produces the next outgoing handshake message.
    pub fn write_message(&mut self) -> DeskResult<Vec<u8>> {
        let mut buf = vec![0u8; NOISE_MAX_MESSAGE];
        let n = self
            .state
            .write_message(&[], &mut buf)
            .map_err(|e| crypto_err("noise handshake write failed", &e))?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Consumes the peer's handshake message.
    pub fn read_message(&mut self, msg: &[u8]) -> DeskResult<()> {
        let mut buf = vec![0u8; NOISE_MAX_MESSAGE];
        self.state
            .read_message(msg, &mut buf)
            .map_err(|e| crypto_err("noise handshake read failed", &e))?;
        Ok(())
    }

    /// Peer static public key, available once it was received in the handshake.
    #[must_use]
    pub fn remote_static(&self) -> Option<[u8; KEY_LEN]> {
        self.state
            .get_remote_static()
            .and_then(|k| <[u8; KEY_LEN]>::try_from(k).ok())
    }

    /// Whether all handshake messages were exchanged.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// Switches to transport mode once the handshake is finished.
    pub fn into_transport(self) -> DeskResult<NoiseTransport> {
        let inner = self
            .state
            .into_transport_mode()
            .map_err(|e| crypto_err("noise transport switch failed", &e))?;
        Ok(NoiseTransport { inner })
    }
}

/// Established encrypted channel.
pub struct NoiseTransport {
    inner: snow::TransportState,
}

impl std::fmt::Debug for NoiseTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseTransport").finish_non_exhaustive()
    }
}

impl NoiseTransport {
    /// Encrypts one chunk (at most [`NOISE_MAX_PLAINTEXT`] bytes).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> DeskResult<Vec<u8>> {
        if plaintext.len() > NOISE_MAX_PLAINTEXT {
            return Err(DeskError::Crypto("noise chunk too large".to_string()));
        }
        let mut out = vec![0u8; plaintext.len() + NOISE_TAG_LEN];
        let n = self
            .inner
            .write_message(plaintext, &mut out)
            .map_err(|e| crypto_err("noise encrypt failed", &e))?;
        out.truncate(n);
        Ok(out)
    }

    /// Decrypts one chunk; fails on tampering, reordering or replay.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> DeskResult<Vec<u8>> {
        if ciphertext.len() < NOISE_TAG_LEN || ciphertext.len() > NOISE_MAX_MESSAGE {
            return Err(DeskError::Crypto(
                "noise chunk has invalid size".to_string(),
            ));
        }
        let mut out = vec![0u8; ciphertext.len()];
        let n = self
            .inner
            .read_message(ciphertext, &mut out)
            .map_err(|e| crypto_err("noise decrypt failed", &e))?;
        out.truncate(n);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> DeskResult<(NoiseTransport, NoiseTransport, [u8; 32], [u8; 32])> {
        let ci = DeviceIdentity::generate()?;
        let hi = DeviceIdentity::generate()?;
        let mut c = NoiseHandshake::initiator(&ci)?;
        let mut h = NoiseHandshake::responder(&hi)?;
        let m1 = c.write_message()?;
        h.read_message(&m1)?;
        let m2 = h.write_message()?;
        c.read_message(&m2)?;
        let m3 = c.write_message()?;
        h.read_message(&m3)?;
        assert!(c.is_finished() && h.is_finished());
        assert_eq!(c.remote_static(), Some(*hi.public_key()));
        assert_eq!(h.remote_static(), Some(*ci.public_key()));
        Ok((
            c.into_transport()?,
            h.into_transport()?,
            *ci.public_key(),
            *hi.public_key(),
        ))
    }

    #[test]
    fn roundtrip_and_replay_rejected() -> DeskResult<()> {
        let (mut c, mut h, _, _) = pair()?;
        let ct = c.encrypt(b"hello")?;
        assert_eq!(h.decrypt(&ct)?, b"hello");
        // Replaying the same ciphertext must fail (nonce already consumed).
        assert!(h.decrypt(&ct).is_err());
        Ok(())
    }

    #[test]
    fn directions_use_different_keys() -> DeskResult<()> {
        let (mut c, mut h, _, _) = pair()?;
        let ct = c.encrypt(b"x")?;
        // Reflecting the client's own ciphertext back at the client must fail.
        assert!(c.decrypt(&ct).is_err());
        assert_eq!(h.decrypt(&ct)?, b"x");
        Ok(())
    }
}
