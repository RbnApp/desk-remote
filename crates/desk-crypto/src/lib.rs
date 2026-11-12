//! Central device identity, key management, and cryptographic primitives for Desk-Remote.
//!
//! Provides Windows DPAPI at-rest key protection, unique host identity generation,
//! temporary pairing code verification, and ChaCha20-Poly1305 session ciphering.

pub mod cipher;
pub mod dpapi;
pub mod identity;
pub mod pairing;

pub use cipher::{derive_session_key, SessionCipher, NONCE_LEN};
pub use dpapi::{protect, unprotect};
pub use identity::DeviceIdentity;
pub use pairing::{generate_pairing_code, PairingManager, PAIRING_CODE_TTL};
