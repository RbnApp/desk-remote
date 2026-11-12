//! One-time pairing code generation and verification for Desk-Remote.
//!
//! Enforces mutual trust between the host machine and viewer panels.
//! Follows the DAPP-67 pairing UX: a 4-letter + 4-digit code (e.g. `ABCD-2345`).

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Standard validity duration for a pairing code (5 minutes).
pub const PAIRING_CODE_TTL: Duration = Duration::from_secs(300);

/// Generates a randomized pairing code matching the pattern `[A-Z]{4}-[0-9]{4}`.
#[must_use]
pub fn generate_pairing_code() -> String {
    const LETTERS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ"; // omitted I, O to prevent confusion
    const DIGITS: &[u8] = b"23456789"; // omitted 0, 1

    let mut code = String::with_capacity(9);
    for _ in 0..4 {
        let idx = fastrand::usize(..LETTERS.len());
        code.push(LETTERS[idx] as char);
    }
    code.push('-');
    for _ in 0..4 {
        let idx = fastrand::usize(..DIGITS.len());
        code.push(DIGITS[idx] as char);
    }
    code
}

/// Active pairing challenge state.
#[derive(Debug)]
struct ActiveChallenge {
    code: String,
    created_at: Instant,
}

/// Pairing manager maintaining active pairing code and authenticated peer whitelist.
#[derive(Debug, Default)]
pub struct PairingManager {
    active_challenge: Option<ActiveChallenge>,
    paired_peers: HashSet<String>,
}

impl PairingManager {
    /// Creates a new empty [`PairingManager`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Generates and activates a new pairing code, expiring any previous code.
    pub fn create_pairing_code(&mut self) -> String {
        let code = generate_pairing_code();
        self.active_challenge = Some(ActiveChallenge {
            code: code.clone(),
            created_at: Instant::now(),
        });
        code
    }

    /// Returns the currently active pairing code if still valid.
    #[must_use]
    pub fn active_code(&self) -> Option<&str> {
        let challenge = self.active_challenge.as_ref()?;
        if challenge.created_at.elapsed() < PAIRING_CODE_TTL {
            Some(&challenge.code)
        } else {
            None
        }
    }

    /// Verifies a submitted code against the active challenge.
    ///
    /// If valid and not expired, the code is consumed (one-time use) and returns `true`.
    /// Otherwise returns `false`.
    pub fn verify_code(&mut self, candidate: &str) -> bool {
        let is_valid = match &self.active_challenge {
            Some(challenge) if challenge.created_at.elapsed() < PAIRING_CODE_TTL => {
                candidate.trim().eq_ignore_ascii_case(&challenge.code)
            }
            _ => false,
        };

        if is_valid {
            // One-time consumption: expire code immediately upon successful match
            self.active_challenge = None;
            true
        } else {
            false
        }
    }

    /// Authorizes a peer device ID permanently for this session.
    pub fn add_paired_peer(&mut self, peer_id: impl Into<String>) {
        self.paired_peers.insert(peer_id.into());
    }

    /// Checks if a peer device ID has previously paired and is authorized.
    #[must_use]
    pub fn is_paired(&self, peer_id: &str) -> bool {
        self.paired_peers.contains(peer_id)
    }

    /// Removes all paired peers.
    pub fn clear_paired_peers(&mut self) {
        self.paired_peers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_format() {
        let code = generate_pairing_code();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        for ch in code[..4].chars() {
            assert!(ch.is_ascii_uppercase());
        }
        for ch in code[5..].chars() {
            assert!(ch.is_ascii_digit());
        }
    }

    #[test]
    fn test_verification_success_consumes_code() {
        let mut mgr = PairingManager::new();
        let code = mgr.create_pairing_code();
        assert_eq!(mgr.active_code(), Some(code.as_str()));

        // Verification succeeds
        assert!(mgr.verify_code(&code));

        // Code consumed, second attempt fails
        assert!(!mgr.verify_code(&code));
        assert_eq!(mgr.active_code(), None);
    }

    #[test]
    fn test_verification_rejects_invalid_code() {
        let mut mgr = PairingManager::new();
        let code = mgr.create_pairing_code();

        assert!(!mgr.verify_code("WRONG-1234"));
        // Original code remains active
        assert_eq!(mgr.active_code(), Some(code.as_str()));
    }

    #[test]
    fn test_paired_peers_authorization() {
        let mut mgr = PairingManager::new();
        assert!(!mgr.is_paired("peer-panel-01"));

        mgr.add_paired_peer("peer-panel-01");
        assert!(mgr.is_paired("peer-panel-01"));
        assert!(!mgr.is_paired("peer-panel-02"));
    }
}
