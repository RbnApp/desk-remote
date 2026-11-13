//! Persistent trust store: the list of peer static public keys this device has paired with.
//!
//! On the host this is the "trusted viewer panels" list; on a viewer panel it is the
//! "trusted PCs" list. A peer whose static key is not in the store cannot open a session.
//! The file is DPAPI-protected, so it cannot be edited offline to add a rogue key.

use crate::dpapi;
use crate::identity::{device_id_from_public, KEY_LEN};
use crate::rand::{from_hex, to_hex};
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One paired peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedPeer {
    /// Static X25519 public key, hex.
    pub public_key_hex: String,
    /// Device ID derived from the key (informational).
    pub device_id: String,
    /// Whether this peer is allowed to connect without interactive user consent.
    #[serde(default)]
    pub always_allow: bool,
    /// Mode to grant when `always_allow` connects ("view" or "control").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_mode: Option<String>,
    /// Name the peer last announced (shown in the "paired panels" list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    version: u8,
    peers: Vec<TrustedPeer>,
}

/// Set of trusted peer public keys, optionally backed by a DPAPI-protected file.
#[derive(Debug, Default)]
pub struct TrustStore {
    path: Option<PathBuf>,
    peers: Vec<TrustedPeer>,
}

impl TrustStore {
    /// Creates an empty in-memory store (nothing is persisted).
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Opens (or starts empty) a store persisted at `path`.
    pub fn open(path: &Path) -> DeskResult<Self> {
        let peers = if path.exists() {
            let raw = std::fs::read(path).map_err(|e| {
                DeskError::Crypto(format!(
                    "Failed to read trust store {}: {e}",
                    path.display()
                ))
            })?;
            let plain = dpapi::unprotect(&raw)?;
            let file: TrustFile = serde_json::from_slice(&plain)
                .map_err(|e| DeskError::Crypto(format!("Failed to parse trust store: {e}")))?;
            file.peers
        } else {
            Vec::new()
        };
        Ok(Self {
            path: Some(path.to_path_buf()),
            peers,
        })
    }

    /// The same peers without a backing file: handshakes can read and extend it while the
    /// shared store stays unlocked, and nothing is persisted from the copy.
    #[must_use]
    pub fn detached_copy(&self) -> Self {
        Self {
            path: None,
            peers: self.peers.clone(),
        }
    }

    /// Opens the canonical host-side store (trusted panels).
    pub fn open_host_canonical() -> DeskResult<Self> {
        Self::open(&DeskPaths::default().trusted_peers_path())
    }

    /// Opens the canonical panel-side store (trusted PCs).
    pub fn open_panel_canonical() -> DeskResult<Self> {
        Self::open(&DeskPaths::default().trusted_hosts_path())
    }

    /// Whether `public` belongs to a paired peer.
    #[must_use]
    pub fn is_trusted(&self, public: &[u8; KEY_LEN]) -> bool {
        let hex = to_hex(public);
        self.peers.iter().any(|p| p.public_key_hex == hex)
    }

    /// Adds a peer (idempotent) and persists the store.
    pub fn add(&mut self, public: &[u8; KEY_LEN]) -> DeskResult<()> {
        if self.is_trusted(public) {
            return Ok(());
        }
        self.peers.push(TrustedPeer {
            public_key_hex: to_hex(public),
            device_id: device_id_from_public(public),
            always_allow: false,
            always_mode: None,
            name: None,
        });
        self.persist()
    }

    /// Whether `device_id` belongs to a paired peer.
    #[must_use]
    pub fn is_device_trusted(&self, device_id: &str) -> bool {
        self.peers.iter().any(|p| p.device_id == device_id)
    }

    /// Whether `public` belongs to a peer marked `always_allow`.
    #[must_use]
    pub fn is_always_allowed(&self, public: &[u8; KEY_LEN]) -> bool {
        let hex = to_hex(public);
        self.peers
            .iter()
            .any(|p| p.public_key_hex == hex && p.always_allow)
    }

    /// Whether `device_id` belongs to a peer marked `always_allow`.
    #[must_use]
    pub fn is_device_always_allowed(&self, device_id: &str) -> bool {
        self.peers
            .iter()
            .any(|p| p.device_id == device_id && p.always_allow)
    }

    /// Returns the saved `always_mode` ("view" or "control") for a device.
    #[must_use]
    pub fn device_always_mode(&self, device_id: &str) -> Option<String> {
        self.peers
            .iter()
            .find(|p| p.device_id == device_id && p.always_allow)
            .and_then(|p| p.always_mode.clone())
    }

    /// Updates the `always_allow` flag and granted mode for a peer by public key.
    pub fn set_always_allowed_with_mode(
        &mut self,
        public: &[u8; KEY_LEN],
        allow: bool,
        mode: Option<&str>,
    ) -> DeskResult<()> {
        let hex = to_hex(public);
        if let Some(peer) = self.peers.iter_mut().find(|p| p.public_key_hex == hex) {
            peer.always_allow = allow;
            peer.always_mode = if allow {
                mode.map(str::to_string)
            } else {
                None
            };
            self.persist()?;
        }
        Ok(())
    }

    /// Updates the `always_allow` flag and granted mode for a peer by device ID.
    pub fn set_device_always_allowed_with_mode(
        &mut self,
        device_id: &str,
        allow: bool,
        mode: Option<&str>,
    ) -> DeskResult<()> {
        if let Some(peer) = self.peers.iter_mut().find(|p| p.device_id == device_id) {
            peer.always_allow = allow;
            peer.always_mode = if allow {
                mode.map(str::to_string)
            } else {
                None
            };
            self.persist()?;
        }
        Ok(())
    }

    /// Updates the `always_allow` flag for a peer by public key.
    pub fn set_always_allowed(&mut self, public: &[u8; KEY_LEN], allow: bool) -> DeskResult<()> {
        self.set_always_allowed_with_mode(public, allow, if allow { Some("control") } else { None })
    }

    /// Updates the `always_allow` flag for a peer by device ID.
    pub fn set_device_always_allowed(&mut self, device_id: &str, allow: bool) -> DeskResult<()> {
        self.set_device_always_allowed_with_mode(
            device_id,
            allow,
            if allow { Some("control") } else { None },
        )
    }

    /// Computes or retrieves fingerprint for a trusted peer by device ID.
    #[must_use]
    pub fn fingerprint_for_device(&self, device_id: &str) -> Option<String> {
        let peer = self.peers.iter().find(|p| p.device_id == device_id)?;
        let key = from_hex::<KEY_LEN>(&peer.public_key_hex).ok()?;
        Some(crate::identity::fingerprint_from_public(&key))
    }

    /// Removes a peer by key and persists the store.
    pub fn remove(&mut self, public: &[u8; KEY_LEN]) -> DeskResult<()> {
        let hex = to_hex(public);
        self.peers.retain(|p| p.public_key_hex != hex);
        self.persist()
    }

    /// Remembers the name `device_id` announced; persists only when it changed.
    pub fn set_device_name(&mut self, device_id: &str, name: &str) -> DeskResult<()> {
        let Some(peer) = self.peers.iter_mut().find(|p| p.device_id == device_id) else {
            return Ok(());
        };
        if peer.name.as_deref() == Some(name) {
            return Ok(());
        }
        peer.name = Some(name.to_string());
        self.persist()
    }

    /// Removes the peer with `device_id` and persists; `true` when one was paired (DAPP-104).
    pub fn remove_device(&mut self, device_id: &str) -> DeskResult<bool> {
        let before = self.peers.len();
        self.peers.retain(|p| p.device_id != device_id);
        let removed = self.peers.len() != before;
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    /// Removes every peer.
    pub fn clear(&mut self) -> DeskResult<()> {
        self.peers.clear();
        self.persist()
    }

    /// Current trusted peers.
    #[must_use]
    pub fn peers(&self) -> &[TrustedPeer] {
        &self.peers
    }

    /// Validates the on-disk key encodings (used by tests and diagnostics).
    pub fn validate(&self) -> DeskResult<()> {
        for p in &self.peers {
            from_hex::<KEY_LEN>(&p.public_key_hex)?;
        }
        Ok(())
    }

    fn persist(&self) -> DeskResult<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            DeskPaths::ensure_dir(parent)?;
        }
        let file = TrustFile {
            version: 1,
            peers: self.peers.clone(),
        };
        let json = serde_json::to_vec(&file)
            .map_err(|e| DeskError::Crypto(format!("Failed to serialize trust store: {e}")))?;
        let protected = dpapi::protect(&json)?;
        // Write-then-rename so a crash never leaves a truncated trust file.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, protected).map_err(|e| {
            DeskError::Crypto(format!(
                "Failed to write trust store {}: {e}",
                tmp.display()
            ))
        })?;
        std::fs::rename(&tmp, path).map_err(|e| {
            DeskError::Crypto(format!(
                "Failed to replace trust store {}: {e}",
                path.display()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_persists_across_reopen() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("trusted.dat");
        let key = [7u8; KEY_LEN];
        let other = [9u8; KEY_LEN];

        let mut store = TrustStore::open(&path)?;
        assert!(!store.is_trusted(&key));
        store.add(&key)?;
        store.add(&key)?;
        assert_eq!(store.peers().len(), 1);

        let reopened = TrustStore::open(&path)?;
        assert!(reopened.is_trusted(&key));
        assert!(!reopened.is_trusted(&other));
        reopened.validate()?;
        Ok(())
    }

    #[test]
    fn remove_device_forgets_only_that_peer_and_persists() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("trusted.dat");
        let (a, b) = ([1u8; KEY_LEN], [2u8; KEY_LEN]);
        let mut store = TrustStore::open(&path)?;
        store.add(&a)?;
        store.add(&b)?;
        let id_a = device_id_from_public(&a);
        assert!(store.remove_device(&id_a)?);
        assert!(!store.remove_device(&id_a)?, "second removal is a no-op");
        let reopened = TrustStore::open(&path)?;
        assert!(!reopened.is_trusted(&a));
        assert!(reopened.is_trusted(&b));
        Ok(())
    }

    #[test]
    fn always_allow_flag_persists_and_updates() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("trusted.dat");
        let key = [5u8; KEY_LEN];

        let mut store = TrustStore::open(&path)?;
        store.add(&key)?;
        assert!(!store.is_always_allowed(&key));

        store.set_always_allowed(&key, true)?;
        assert!(store.is_always_allowed(&key));

        let reopened = TrustStore::open(&path)?;
        assert!(reopened.is_always_allowed(&key));

        let dev_id = device_id_from_public(&key);
        assert!(reopened.is_device_always_allowed(&dev_id));
        assert!(reopened.fingerprint_for_device(&dev_id).is_some());

        // Test view mode persistence
        let mut store = reopened;
        store.set_device_always_allowed_with_mode(&dev_id, true, Some("view"))?;
        assert_eq!(store.device_always_mode(&dev_id), Some("view".to_string()));

        let reopened_view = TrustStore::open(&path)?;
        assert_eq!(
            reopened_view.device_always_mode(&dev_id),
            Some("view".to_string())
        );
        Ok(())
    }

    #[test]
    fn remove_and_clear() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = TrustStore::in_memory();
        let key = [1u8; KEY_LEN];
        store.add(&key)?;
        store.remove(&key)?;
        assert!(!store.is_trusted(&key));
        store.add(&key)?;
        store.clear()?;
        assert!(store.peers().is_empty());
        Ok(())
    }
}
