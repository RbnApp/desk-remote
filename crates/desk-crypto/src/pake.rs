//! Password-authenticated key exchange for first-time pairing (SPAKE2, Ed25519 group).
//!
//! The short pairing code is the PAKE password: it is never sent on the wire, and a
//! passive or active attacker gets at most ONE online guess per connection (the host
//! additionally revokes the code after a few failures, see [`crate::PairingManager`]).
//!
//! Flow (after both SPAKE2 messages `A`=panel, `B`=host are exchanged):
//! 1. panel -> host: key-confirmation tag `HMAC(K, "panel" || A || B)`
//! 2. host verifies it (constant time) and ONLY THEN answers with its own tag, so a
//!    wrong-code client never receives a value usable for offline guessing.
//! 3. both sides send their static public key sealed with a key derived from `K`;
//!    each stores the peer key in its [`crate::TrustStore`].

use crate::identity::KEY_LEN;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use desk_error::{DeskError, DeskResult};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

type HmacSha256 = Hmac<Sha256>;

const ID_PANEL: &[u8] = b"desk-remote-panel";
const ID_HOST: &[u8] = b"desk-remote-host";

/// Which side of the pairing a party plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PakeRole {
    /// Viewer panel (SPAKE2 side A).
    Panel,
    /// Host PC (SPAKE2 side B).
    Host,
}

/// Normalises a user-typed code (`abcd-2345 ` -> `ABCD-2345`).
#[must_use]
pub fn normalize_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

/// In-flight SPAKE2 exchange.
pub struct PakeState {
    role: PakeRole,
    spake: Spake2<Ed25519Group>,
    outbound: Vec<u8>,
}

impl std::fmt::Debug for PakeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PakeState")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl PakeState {
    /// Starts the exchange; returns the state and the message to send to the peer.
    #[must_use]
    pub fn start(role: PakeRole, code: &str) -> (Self, Vec<u8>) {
        let pw = Password::new(normalize_code(code).as_bytes());
        let a = Identity::new(ID_PANEL);
        let b = Identity::new(ID_HOST);
        let (spake, outbound) = match role {
            PakeRole::Panel => Spake2::<Ed25519Group>::start_a(&pw, &a, &b),
            PakeRole::Host => Spake2::<Ed25519Group>::start_b(&pw, &a, &b),
        };
        let state = Self {
            role,
            spake,
            outbound: outbound.clone(),
        };
        (state, outbound)
    }

    /// Completes the exchange with the peer's SPAKE2 message.
    pub fn finish(self, peer_msg: &[u8]) -> DeskResult<PakeKeys> {
        let shared = self
            .spake
            .finish(peer_msg)
            .map_err(|e| DeskError::Crypto(format!("PAKE failed: {e}")))?;
        let (a, b) = match self.role {
            PakeRole::Panel => (self.outbound.as_slice(), peer_msg),
            PakeRole::Host => (peer_msg, self.outbound.as_slice()),
        };
        PakeKeys::derive(self.role, &shared, a, b)
    }
}

/// Keys derived from the PAKE shared secret.
pub struct PakeKeys {
    role: PakeRole,
    mac_key: [u8; 32],
    enc_key: [u8; 32],
    transcript: Vec<u8>,
}

impl std::fmt::Debug for PakeKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PakeKeys")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

fn hmac_of(key: &[u8], parts: &[&[u8]]) -> DeskResult<HmacSha256> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key)
        .map_err(|e| DeskError::Crypto(format!("HMAC key error: {e}")))?;
    for p in parts {
        mac.update(p);
    }
    Ok(mac)
}

impl PakeKeys {
    fn derive(role: PakeRole, shared: &[u8], a: &[u8], b: &[u8]) -> DeskResult<Self> {
        let mut transcript = Vec::with_capacity(a.len() + b.len());
        transcript.extend_from_slice(a);
        transcript.extend_from_slice(b);
        let sub = |label: &[u8]| -> DeskResult<[u8; 32]> {
            Ok(hmac_of(shared, &[label, &transcript])?
                .finalize()
                .into_bytes()
                .into())
        };
        Ok(Self {
            role,
            mac_key: sub(b"desk-remote/pair/confirm")?,
            enc_key: sub(b"desk-remote/pair/enc")?,
            transcript,
        })
    }

    /// Key-confirmation tag for `sender`.
    pub fn confirmation_tag(&self, sender: PakeRole) -> DeskResult<[u8; 32]> {
        let label: &[u8] = match sender {
            PakeRole::Panel => b"panel",
            PakeRole::Host => b"host",
        };
        Ok(hmac_of(&self.mac_key, &[label, &self.transcript])?
            .finalize()
            .into_bytes()
            .into())
    }

    /// Verifies the peer's confirmation tag in constant time.
    pub fn verify_confirmation(&self, sender: PakeRole, tag: &[u8]) -> bool {
        let label: &[u8] = match sender {
            PakeRole::Panel => b"panel",
            PakeRole::Host => b"host",
        };
        match hmac_of(&self.mac_key, &[label, &self.transcript]) {
            Ok(mac) => mac.verify_slice(tag).is_ok(),
            Err(_) => false,
        }
    }

    fn nonce_for(role: PakeRole) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0] = match role {
            PakeRole::Panel => 1,
            PakeRole::Host => 2,
        };
        n
    }

    /// Seals this side's static public key for the peer.
    pub fn seal_static_key(&self, public: &[u8; KEY_LEN]) -> DeskResult<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.enc_key));
        cipher
            .encrypt(
                Nonce::from_slice(&Self::nonce_for(self.role)),
                public.as_slice(),
            )
            .map_err(|e| DeskError::Crypto(format!("static key seal failed: {e}")))
    }

    /// Opens the peer's sealed static public key.
    pub fn open_static_key(&self, sealed: &[u8]) -> DeskResult<[u8; KEY_LEN]> {
        let peer = match self.role {
            PakeRole::Panel => PakeRole::Host,
            PakeRole::Host => PakeRole::Panel,
        };
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.enc_key));
        let plain = cipher
            .decrypt(Nonce::from_slice(&Self::nonce_for(peer)), sealed)
            .map_err(|_| DeskError::Crypto("peer static key failed authentication".to_string()))?;
        plain
            .try_into()
            .map_err(|_| DeskError::Crypto("peer static key has wrong length".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(panel_code: &str, host_code: &str) -> DeskResult<(PakeKeys, PakeKeys)> {
        let (p, pm) = PakeState::start(PakeRole::Panel, panel_code);
        let (h, hm) = PakeState::start(PakeRole::Host, host_code);
        Ok((p.finish(&hm)?, h.finish(&pm)?))
    }

    #[test]
    fn matching_codes_agree_and_confirm() -> DeskResult<()> {
        let (p, h) = run("ABCD-2345", "abcd-2345 ")?;
        let tag = p.confirmation_tag(PakeRole::Panel)?;
        assert!(h.verify_confirmation(PakeRole::Panel, &tag));
        let tag = h.confirmation_tag(PakeRole::Host)?;
        assert!(p.verify_confirmation(PakeRole::Host, &tag));
        Ok(())
    }

    #[test]
    fn wrong_code_fails_confirmation() -> DeskResult<()> {
        let (p, h) = run("ABCD-2345", "WXYZ-9876")?;
        let tag = p.confirmation_tag(PakeRole::Panel)?;
        assert!(!h.verify_confirmation(PakeRole::Panel, &tag));
        Ok(())
    }

    #[test]
    fn static_key_roundtrip_and_tamper() -> DeskResult<()> {
        let (p, h) = run("ABCD-2345", "ABCD-2345")?;
        let key = [5u8; KEY_LEN];
        let mut sealed = p.seal_static_key(&key)?;
        assert_eq!(h.open_static_key(&sealed)?, key);
        sealed[0] ^= 1;
        assert!(h.open_static_key(&sealed).is_err());
        Ok(())
    }
}
