//! Device identity and persistent cryptographic key management.
//!
//! Generates a unique, persistent host Device ID and 256-bit secret key, protected
//! at rest using Windows DPAPI.

use crate::dpapi;
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Serializable container for device key material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct KeyStore {
    device_id: String,
    secret_key_hex: String,
}

/// Host device cryptographic identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    device_id: String,
    secret_key: [u8; 32],
}

impl DeviceIdentity {
    /// Generates a new random host identity.
    #[must_use]
    pub fn generate() -> Self {
        let mut key = [0u8; 32];
        for byte in &mut key {
            *byte = fastrand::u8(..);
        }

        let mut id_bytes = [0u8; 8];
        for byte in &mut id_bytes {
            *byte = fastrand::u8(..);
        }

        let mut id_hex = String::with_capacity(16);
        for b in id_bytes {
            id_hex.push_str(&format!("{b:02x}"));
        }

        Self {
            device_id: format!("desk-{id_hex}"),
            secret_key: key,
        }
    }

    /// Permanent alphanumeric identifier for this device.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 32-byte raw secret key.
    #[must_use]
    pub fn secret_key(&self) -> &[u8; 32] {
        &self.secret_key
    }

    /// Saves the device identity to disk, protected with Windows DPAPI.
    pub fn save_to_path(&self, path: &Path) -> DeskResult<()> {
        if let Some(parent) = path.parent() {
            DeskPaths::ensure_dir(parent)?;
        }

        let mut key_hex = String::with_capacity(64);
        for b in self.secret_key {
            key_hex.push_str(&format!("{b:02x}"));
        }

        let store = KeyStore {
            device_id: self.device_id.clone(),
            secret_key_hex: key_hex,
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

        if store.secret_key_hex.len() != 64 {
            return Err(DeskError::Crypto(
                "Invalid secret key length in identity store".to_string(),
            ));
        }

        let mut key = [0u8; 32];
        for (i, item) in key.iter_mut().enumerate() {
            let hex_slice = &store.secret_key_hex[i * 2..i * 2 + 2];
            *item = u8::from_str_radix(hex_slice, 16)
                .map_err(|e| DeskError::Crypto(format!("Invalid hex character in key: {e}")))?;
        }

        Ok(Self {
            device_id: store.device_id,
            secret_key: key,
        })
    }

    /// Loads existing identity from path, or generates and saves a new one.
    pub fn load_or_create(path: &Path) -> DeskResult<Self> {
        if path.exists() {
            Self::load_from_path(path)
        } else {
            let id = Self::generate();
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
    fn test_identity_generate_format() {
        let id = DeviceIdentity::generate();
        assert!(id.device_id().starts_with("desk-"));
        assert_eq!(id.device_id().len(), 21); // "desk-" (5) + 16 hex = 21
        assert_ne!(id.secret_key(), &[0u8; 32]);
    }

    #[test]
    fn test_identity_save_load_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let key_path = temp.path().join("identity.key");

        let original = DeviceIdentity::generate();
        original.save_to_path(&key_path)?;

        assert!(key_path.exists());

        let loaded = DeviceIdentity::load_from_path(&key_path)?;
        assert_eq!(original, loaded);
        Ok(())
    }
}
