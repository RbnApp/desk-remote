//! Central device identity, key management, and cryptographic primitives for Desk-Remote.
//!
//! - [`rand`]: OS CSPRNG (`getrandom`) for every secret.
//! - [`identity`]: persistent X25519 static key pair, DPAPI-protected at rest.
//! - [`trust`]: persistent pinned-peer list (the pairing result).
//! - [`pake`]: SPAKE2 first-time pairing driven by the short code (code never on the wire).
//! - [`noise`]: Noise_XX session channel (forward secrecy, per-direction keys, replay-safe).
//! - [`pairing`]: pairing-code lifecycle, rate limiting and trust bookkeeping.

pub mod dpapi;
pub mod identity;
pub mod noise;
pub mod pairing;
pub mod pake;
pub mod rand;
pub mod trust;

pub use dpapi::{protect, unprotect};
pub use identity::{device_id_from_public, DeviceIdentity, KEY_LEN, NOISE_PARAMS};
pub use noise::{
    NoiseHandshake, NoiseTransport, NOISE_MAX_MESSAGE, NOISE_MAX_PLAINTEXT, NOISE_TAG_LEN,
};
pub use pairing::{generate_pairing_code, PairingManager, MAX_FAILED_ATTEMPTS, PAIRING_CODE_TTL};
pub use pake::{normalize_code, PakeKeys, PakeRole, PakeState};
pub use trust::{TrustStore, TrustedPeer};
