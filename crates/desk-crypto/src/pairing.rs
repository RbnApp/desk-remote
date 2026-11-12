//! One-time pairing code lifecycle and trusted-peer bookkeeping for Desk-Remote.
//!
//! The host shows a short code (`ABCD-2345`) to the user. The code is only ever used as
//! the PAKE password (see [`crate::pake`]); it is single use, expires after five minutes
//! and is revoked after [`MAX_FAILED_ATTEMPTS`] wrong attempts.

use crate::identity::KEY_LEN;
use crate::rand::random_index;
use crate::trust::TrustStore;
use desk_error::DeskResult;
use std::time::{Duration, Instant};

/// Standard validity duration for a pairing code (5 minutes).
pub const PAIRING_CODE_TTL: Duration = Duration::from_secs(300);

/// Wrong attempts after which the active code is revoked.
pub const MAX_FAILED_ATTEMPTS: u32 = 5;

/// Generates a pairing code matching `[A-Z]{4}-[0-9]{4}` from the OS CSPRNG.
pub fn generate_pairing_code() -> DeskResult<String> {
    const LETTERS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ"; // omitted I, O to prevent confusion
    const DIGITS: &[u8] = b"23456789"; // omitted 0, 1

    let mut code = String::with_capacity(9);
    for _ in 0..4 {
        code.push(LETTERS[random_index(LETTERS.len())?] as char);
    }
    code.push('-');
    for _ in 0..4 {
        code.push(DIGITS[random_index(DIGITS.len())?] as char);
    }
    Ok(code)
}

#[derive(Debug)]
struct ActiveChallenge {
    code: String,
    created_at: Instant,
    failures: u32,
}

/// Pairing manager: active code state plus the persistent list of trusted peer keys.
#[derive(Debug, Default)]
pub struct PairingManager {
    active_challenge: Option<ActiveChallenge>,
    trust: TrustStore,
}

impl PairingManager {
    /// Creates a manager with an in-memory trust store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a manager backed by the given (typically persistent) trust store.
    #[must_use]
    pub fn with_trust(trust: TrustStore) -> Self {
        Self {
            active_challenge: None,
            trust,
        }
    }

    /// Generates and activates a new pairing code, replacing any previous one.
    pub fn create_pairing_code(&mut self) -> DeskResult<String> {
        let code = generate_pairing_code()?;
        self.active_challenge = Some(ActiveChallenge {
            code: code.clone(),
            created_at: Instant::now(),
            failures: 0,
        });
        Ok(code)
    }

    /// Returns the currently active pairing code if still valid.
    #[must_use]
    pub fn active_code(&self) -> Option<&str> {
        let challenge = self.active_challenge.as_ref()?;
        (challenge.created_at.elapsed() < PAIRING_CODE_TTL).then_some(challenge.code.as_str())
    }

    /// Records a failed pairing attempt; revokes the code after [`MAX_FAILED_ATTEMPTS`].
    pub fn record_failure(&mut self) {
        let revoke = match &mut self.active_challenge {
            Some(c) => {
                c.failures += 1;
                c.failures >= MAX_FAILED_ATTEMPTS
            }
            None => false,
        };
        if revoke {
            self.active_challenge = None;
        }
    }

    /// Consumes the active code after a successful pairing (single use).
    pub fn consume_code(&mut self) {
        self.active_challenge = None;
    }

    /// Persistently trusts a peer's static public key.
    pub fn add_trusted(&mut self, public: &[u8; KEY_LEN]) -> DeskResult<()> {
        self.trust.add(public)
    }

    /// Whether the peer key has been paired.
    #[must_use]
    pub fn is_trusted(&self, public: &[u8; KEY_LEN]) -> bool {
        self.trust.is_trusted(public)
    }

    /// Read access to the trust store.
    #[must_use]
    pub fn trust(&self) -> &TrustStore {
        &self.trust
    }

    /// Mutable access to the trust store (revocation, listing).
    pub fn trust_mut(&mut self) -> &mut TrustStore {
        &mut self.trust
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_format() -> DeskResult<()> {
        let code = generate_pairing_code()?;
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(code[..4].chars().all(|c| c.is_ascii_uppercase()));
        assert!(code[5..].chars().all(|c| c.is_ascii_digit()));
        Ok(())
    }

    #[test]
    fn consume_makes_code_single_use() -> DeskResult<()> {
        let mut mgr = PairingManager::new();
        let code = mgr.create_pairing_code()?;
        assert_eq!(mgr.active_code(), Some(code.as_str()));
        mgr.consume_code();
        assert_eq!(mgr.active_code(), None);
        Ok(())
    }

    #[test]
    fn code_revoked_after_max_failures() -> DeskResult<()> {
        let mut mgr = PairingManager::new();
        mgr.create_pairing_code()?;
        for _ in 0..MAX_FAILED_ATTEMPTS - 1 {
            mgr.record_failure();
            assert!(mgr.active_code().is_some());
        }
        mgr.record_failure();
        assert_eq!(mgr.active_code(), None);
        Ok(())
    }

    #[test]
    fn trust_bookkeeping() -> DeskResult<()> {
        let mut mgr = PairingManager::new();
        let key = [3u8; KEY_LEN];
        assert!(!mgr.is_trusted(&key));
        mgr.add_trusted(&key)?;
        assert!(mgr.is_trusted(&key));
        assert!(!mgr.is_trusted(&[4u8; KEY_LEN]));
        Ok(())
    }
}
