//! Authenticated symmetric stream encryption for Desk-Remote sessions.
//!
//! Provides ChaCha20-Poly1305 authenticated encryption with unique nonces,
//! protecting LAN and loopback sessions against eavesdropping and tampering.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use desk_error::{DeskError, DeskResult};
use sha2::{Digest, Sha256};

/// Nonce length in bytes for ChaCha20-Poly1305.
pub const NONCE_LEN: usize = 12;

/// Derives a 256-bit symmetric key from a shared secret and context salt using SHA-256.
#[must_use]
pub fn derive_session_key(secret: &[u8], salt: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(b":desk-remote-session:");
    hasher.update(salt);
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Session cipher using ChaCha20-Poly1305 authenticated encryption.
pub struct SessionCipher {
    cipher: ChaCha20Poly1305,
    send_counter: u64,
    nonce_salt: [u8; 4],
}

impl std::fmt::Debug for SessionCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCipher")
            .field("send_counter", &self.send_counter)
            .field("nonce_salt", &self.nonce_salt)
            .finish_non_exhaustive()
    }
}

impl SessionCipher {
    /// Creates a new cipher instance initialized with a 32-byte key.
    #[must_use]
    pub fn new(key: [u8; 32]) -> Self {
        let key_ref = Key::from_slice(&key);
        let cipher = ChaCha20Poly1305::new(key_ref);

        let mut nonce_salt = [0u8; 4];
        for b in &mut nonce_salt {
            *b = fastrand::u8(..);
        }

        Self {
            cipher,
            send_counter: 0,
            nonce_salt,
        }
    }

    /// Encrypts plaintext bytes into a framed ciphertext with an embedded 12-byte nonce.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> DeskResult<Vec<u8>> {
        self.send_counter = self.send_counter.wrapping_add(1);

        let mut nonce_bytes = [0u8; NONCE_LEN];
        nonce_bytes[0..4].copy_from_slice(&self.nonce_salt);
        nonce_bytes[4..12].copy_from_slice(&self.send_counter.to_be_bytes());

        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| DeskError::Crypto(format!("Encryption failed: {e}")))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypts ciphertext with embedded 12-byte nonce and validates authentication tag.
    pub fn decrypt(&self, payload: &[u8]) -> DeskResult<Vec<u8>> {
        if payload.len() < NONCE_LEN + 16 {
            return Err(DeskError::Crypto("Ciphertext too short".to_string()));
        }

        let nonce = Nonce::from_slice(&payload[..NONCE_LEN]);
        let ciphertext = &payload[NONCE_LEN..];

        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| DeskError::Crypto(format!("Decryption or authentication failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_key_deterministic() {
        let secret = b"my-shared-secret";
        let salt = b"salt-123";
        let k1 = derive_session_key(secret, salt);
        let k2 = derive_session_key(secret, salt);
        assert_eq!(k1, k2);
        assert_ne!(k1, [0u8; 32]);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() -> DeskResult<()> {
        let key = derive_session_key(b"test-pass", b"salt-xyz");
        let mut sender = SessionCipher::new(key);
        let receiver = SessionCipher::new(key);

        let message = b"Hello Desk-Remote encrypted network world!";
        let encrypted = sender.encrypt(message)?;
        assert_ne!(encrypted, message);

        let decrypted = receiver.decrypt(&encrypted)?;
        assert_eq!(decrypted, message);
        Ok(())
    }

    #[test]
    fn test_tampered_ciphertext_fails_auth() -> DeskResult<()> {
        let key = derive_session_key(b"test-pass", b"salt-xyz");
        let mut sender = SessionCipher::new(key);
        let receiver = SessionCipher::new(key);

        let mut encrypted = sender.encrypt(b"Top secret")?;
        // Tamper with the last byte (auth tag)
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0x01;

        let res = receiver.decrypt(&encrypted);
        assert!(res.is_err());
        Ok(())
    }
}
