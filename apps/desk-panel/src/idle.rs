//! Idle auto-close of remote sessions (DAPP-109, owner decision K-581209).
//!
//! The rule is a pure function of the configured limit and how long a session has had no input,
//! so it is tested without clocks or windows. Every session is judged on its own. Only input
//! sent to that session (mouse, keyboard, a file transfer, "Stay connected") counts as activity;
//! watching alone is idle on purpose.

use serde::Serialize;
use std::time::Duration;

/// How long before the close the warning starts.
pub const WARN_LEAD: Duration = Duration::from_secs(60);

/// What the viewer should do with one session right now.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IdleVerdict {
    /// Nothing to do (also the answer when the limit is off).
    Active,
    /// Show "closes soon — Stay connected"; `secs_left` counts down to the close.
    Warn { secs_left: u64 },
    /// The limit is reached: close the session.
    Expired,
}

/// Judges a session that has had no input for `idle_for`; `idle_minutes == 0` never closes.
#[must_use]
pub fn evaluate(idle_minutes: u32, idle_for: Duration) -> IdleVerdict {
    if idle_minutes == 0 {
        return IdleVerdict::Active;
    }
    let limit = Duration::from_secs(u64::from(idle_minutes) * 60);
    if idle_for >= limit {
        return IdleVerdict::Expired;
    }
    let left = limit - idle_for;
    if left <= WARN_LEAD {
        // Round up so the last second shows 1, never 0.
        IdleVerdict::Warn {
            secs_left: left.as_secs() + u64::from(left.subsec_nanos() > 0),
        }
    } else {
        IdleVerdict::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn off_never_closes() {
        assert_eq!(evaluate(0, Duration::ZERO), IdleVerdict::Active);
        assert_eq!(evaluate(0, mins(100_000)), IdleVerdict::Active);
    }

    #[test]
    fn quiet_until_one_minute_before_the_limit() {
        assert_eq!(evaluate(30, Duration::ZERO), IdleVerdict::Active);
        assert_eq!(evaluate(30, mins(28)), IdleVerdict::Active);
        assert_eq!(
            evaluate(30, mins(29) - Duration::from_secs(1)),
            IdleVerdict::Active
        );
    }

    #[test]
    fn warns_in_the_last_minute_with_a_countdown() {
        assert_eq!(evaluate(30, mins(29)), IdleVerdict::Warn { secs_left: 60 });
        assert_eq!(
            evaluate(30, mins(29) + Duration::from_secs(45)),
            IdleVerdict::Warn { secs_left: 15 }
        );
        assert_eq!(
            evaluate(30, mins(30) - Duration::from_millis(400)),
            IdleVerdict::Warn { secs_left: 1 }
        );
    }

    #[test]
    fn expires_at_and_after_the_limit() {
        assert_eq!(evaluate(30, mins(30)), IdleVerdict::Expired);
        assert_eq!(evaluate(30, mins(500)), IdleVerdict::Expired);
        assert_eq!(evaluate(15, mins(15)), IdleVerdict::Expired);
        assert_eq!(
            evaluate(120, mins(119) + Duration::from_secs(59)),
            IdleVerdict::Warn { secs_left: 1 }
        );
    }

    #[test]
    fn input_resets_the_count() {
        // After input the idle time is zero again, so even a session in its warning is quiet.
        let warned = evaluate(30, mins(29) + Duration::from_secs(30));
        assert!(matches!(warned, IdleVerdict::Warn { .. }));
        assert_eq!(evaluate(30, Duration::ZERO), IdleVerdict::Active);
    }
}
