//! Persistent device identity: a long-lived X25519 static key pair.
//!
//! The private key never leaves the machine and is stored DPAPI-protected. The public
//! key is the device's cryptographic identity; `device_id` is derived from it
//! (`desk-` + first 8 bytes of SHA-256(public key), hex), so an ID cannot be claimed
//! without holding the matching private key.

use crate::dpapi;
use crate::rand::{from_hex, to_hex};
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Noise protocol suite used for every session.
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// X25519 key length in bytes.
pub const KEY_LEN: usize = 32;

/// Derives the canonical device ID from a static public key.
#[must_use]
pub fn device_id_from_public(public: &[u8; KEY_LEN]) -> String {
    let digest = Sha256::digest(public);
    format!("desk-{}", to_hex(&digest[..8]))
}

/// Computes the visual fingerprint from a raw static public key.
#[must_use]
pub fn fingerprint_from_public(public: &[u8; KEY_LEN]) -> String {
    let digest = Sha256::digest(public);
    digest[..16]
        .chunks(2)
        .map(to_hex)
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Serialize, Deserialize)]
struct KeyStore {
    version: u8,
    private_key_hex: String,
    public_key_hex: String,
}

/// Device cryptographic identity (X25519 static key pair).
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    private: [u8; KEY_LEN],
    public: [u8; KEY_LEN],
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id())
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    /// Generates a new key pair using the OS CSPRNG (snow default resolver).
    pub fn generate() -> DeskResult<Self> {
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|e| DeskError::Crypto(format!("invalid noise params: {e}")))?;
        let kp = snow::Builder::new(params)
            .generate_keypair()
            .map_err(|e| DeskError::Crypto(format!("key generation failed: {e}")))?;
        let private: [u8; KEY_LEN] = kp
            .private
            .try_into()
            .map_err(|_| DeskError::Crypto("unexpected private key size".to_string()))?;
        let public: [u8; KEY_LEN] = kp
            .public
            .try_into()
            .map_err(|_| DeskError::Crypto("unexpected public key size".to_string()))?;
        Ok(Self { private, public })
    }

    /// Canonical device ID derived from the public key.
    #[must_use]
    pub fn device_id(&self) -> String {
        device_id_from_public(&self.public)
    }

    /// Long form of the same SHA-256 digest the device ID is cut from: its first 16 bytes
    /// as eight groups of four lower-case hex digits (`0123-4567-…`), for comparing two
    /// devices by eye. The device ID is the first half of it.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint_from_public(&self.public)
    }

    /// Static public key (safe to share).
    #[must_use]
    pub fn public_key(&self) -> &[u8; KEY_LEN] {
        &self.public
    }

    /// Static private key, only for building Noise handshakes inside this crate.
    pub(crate) fn private_key(&self) -> &[u8; KEY_LEN] {
        &self.private
    }

    /// Saves the identity to disk, protected with Windows DPAPI.
    pub fn save_to_path(&self, path: &Path) -> DeskResult<()> {
        if let Some(parent) = path.parent() {
            DeskPaths::ensure_dir(parent)?;
        }
        let store = KeyStore {
            version: 2,
            private_key_hex: to_hex(&self.private),
            public_key_hex: to_hex(&self.public),
        };
        let json = serde_json::to_vec(&store)
            .map_err(|e| DeskError::Crypto(format!("Failed to serialize identity store: {e}")))?;
        let protected = dpapi::protect(&json)?;
        std::fs::write(path, protected).map_err(|e| {
            DeskError::Crypto(format!(
                "Failed to write identity file to {}: {e}",
                path.display()
            ))
        })
    }

    /// Loads a DPAPI-protected identity from the specified path.
    pub fn load_from_path(path: &Path) -> DeskResult<Self> {
        let raw = std::fs::read(path).map_err(|e| {
            DeskError::Crypto(format!(
                "Failed to read identity file {}: {e}",
                path.display()
            ))
        })?;
        let decrypted = dpapi::unprotect(&raw)?;
        let store: KeyStore = serde_json::from_slice(&decrypted)
            .map_err(|e| DeskError::Crypto(format!("Failed to parse identity store: {e}")))?;
        if store.version != 2 {
            return Err(DeskError::Crypto(format!(
                "Unsupported identity store version {}",
                store.version
            )));
        }
        Ok(Self {
            private: from_hex::<KEY_LEN>(&store.private_key_hex)?,
            public: from_hex::<KEY_LEN>(&store.public_key_hex)?,
        })
    }

    /// Loads existing identity from path, or generates and saves a new one.
    pub fn load_or_create(path: &Path) -> DeskResult<Self> {
        if path.exists() {
            Self::load_from_path(path)
        } else {
            let id = Self::generate()?;
            id.save_to_path(path)?;
            Ok(id)
        }
    }

    /// Loads or creates the canonical machine identity at `%ProgramData%\DeskRemote\identity.key`.
    pub fn load_canonical() -> DeskResult<Self> {
        let paths = DeskPaths::default();
        Self::load_or_create(&paths.identity_key_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_id_is_derived_from_public_key() -> DeskResult<()> {
        let id = DeviceIdentity::generate()?;
        assert!(id.device_id().starts_with("desk-"));
        assert_eq!(id.device_id().len(), 21);
        assert_eq!(id.device_id(), device_id_from_public(id.public_key()));
        let fp = id.fingerprint();
        assert_eq!(fp.len(), 8 * 4 + 7);
        assert!(
            fp.replace('-', "")
                .starts_with(&id.device_id()["desk-".len()..]),
            "the device ID is the first half of the fingerprint"
        );
        assert_ne!(id.private_key(), &[0u8; 32]);
        let other = DeviceIdentity::generate()?;
        assert_ne!(id.device_id(), other.device_id());
        Ok(())
    }

    #[test]
    fn identity_save_load_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let key_path = temp.path().join("identity.key");
        let original = DeviceIdentity::generate()?;
        original.save_to_path(&key_path)?;
        assert!(key_path.exists());
        let loaded = DeviceIdentity::load_from_path(&key_path)?;
        assert_eq!(original, loaded);
        Ok(())
    }
}
