//! Pure session-change and restart-backoff logic (no OS calls, fully unit-testable).
//!
//! The supervisor feeds [`SessionEvent`]s and the OS-reported console session into a
//! [`SessionTracker`]; it answers which session should host the helper and whether the
//! workstation is locked.

use std::time::Duration;

/// Session id meaning "no console session" in the Windows API.
pub const NO_SESSION: u32 = 0xFFFF_FFFF;

/// A session notification (`WTS_SESSION_*`) in OS-independent form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    ConsoleConnect(u32),
    ConsoleDisconnect(u32),
    Logon(u32),
    Logoff(u32),
    Lock(u32),
    Unlock(u32),
}

/// Tracks the console session and its lock state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionTracker {
    console: Option<u32>,
    locked: bool,
}

impl SessionTracker {
    /// Tracker seeded with the console session reported by the OS at startup.
    #[must_use]
    pub fn with_console(console: u32) -> Self {
        Self {
            console: Self::usable(console),
            locked: false,
        }
    }

    /// Session 0 is the services session and `NO_SESSION` means nobody sits at the console:
    /// neither can host an interactive helper.
    fn usable(session: u32) -> Option<u32> {
        (session != 0 && session != NO_SESSION).then_some(session)
    }

    /// Applies one notification. Returns `true` when the helper target or lock state changed.
    pub fn apply(&mut self, event: SessionEvent) -> bool {
        let before = (self.console, self.locked);
        match event {
            SessionEvent::ConsoleConnect(id) => {
                self.console = Self::usable(id);
                self.locked = false;
            }
            SessionEvent::ConsoleDisconnect(id) => {
                if self.console == Some(id) {
                    self.console = None;
                }
            }
            SessionEvent::Logon(id) => {
                if self.console.is_none() {
                    self.console = Self::usable(id);
                }
                if self.console == Some(id) {
                    self.locked = false;
                }
            }
            SessionEvent::Logoff(id) => {
                // A logged-off session has no user token any more: keeping it as the target
                // would send the supervisor into a failing spawn/backoff loop. The next
                // console connect or logon picks the new target.
                if self.console == Some(id) {
                    self.console = None;
                    self.locked = false;
                }
            }
            SessionEvent::Lock(id) => {
                if self.console == Some(id) {
                    self.locked = true;
                }
            }
            SessionEvent::Unlock(id) => {
                if self.console == Some(id) {
                    self.locked = false;
                }
            }
        }
        before != (self.console, self.locked)
    }

    /// Session that should currently run the helper.
    #[must_use]
    pub fn target(&self) -> Option<u32> {
        self.console
    }

    /// Whether the console session is locked (secure desktop in front).
    #[must_use]
    pub fn locked(&self) -> bool {
        self.locked
    }
}

/// Exponential restart delay for a crashing helper.
#[derive(Debug, Clone)]
pub struct Backoff {
    current: Duration,
    min: Duration,
    max: Duration,
}

impl Backoff {
    #[must_use]
    pub fn new(min: Duration, max: Duration) -> Self {
        Self {
            current: min,
            min,
            max,
        }
    }

    /// Delay to wait before the next attempt; doubles up to the maximum.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.max);
        delay
    }

    /// Called after the helper ran stably.
    pub fn reset(&mut self) {
        self.current = self.min;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_connect_moves_the_target_and_clears_lock() {
        let mut t = SessionTracker::with_console(1);
        assert_eq!(t.target(), Some(1));
        assert!(t.apply(SessionEvent::Lock(1)));
        assert!(t.locked());
        assert!(t.apply(SessionEvent::ConsoleConnect(3)));
        assert_eq!(t.target(), Some(3));
        assert!(!t.locked());
    }

    #[test]
    fn user_switch_and_logoff_paths() {
        let mut t = SessionTracker::with_console(1);
        // Fast user switching: console leaves session 1 and shows the logon screen.
        assert!(t.apply(SessionEvent::ConsoleDisconnect(1)));
        assert_eq!(t.target(), None);
        // Another user logs on at the console in session 2.
        assert!(t.apply(SessionEvent::Logon(2)));
        assert_eq!(t.target(), Some(2));
        // Logging off drops the target (its token is gone) and resets the lock state.
        assert!(t.apply(SessionEvent::Lock(2)));
        assert!(t.apply(SessionEvent::Logoff(2)));
        assert!(!t.locked());
        assert_eq!(t.target(), None);
    }

    #[test]
    fn session_zero_and_no_session_never_host_a_helper() {
        assert_eq!(SessionTracker::with_console(0).target(), None);
        assert_eq!(SessionTracker::with_console(NO_SESSION).target(), None);
        let mut t = SessionTracker::default();
        assert!(!t.apply(SessionEvent::Logon(0)));
        assert_eq!(t.target(), None);
    }

    #[test]
    fn lock_of_a_background_session_is_ignored() {
        let mut t = SessionTracker::with_console(1);
        assert!(!t.apply(SessionEvent::Lock(5)));
        assert!(!t.locked());
        assert!(!t.apply(SessionEvent::Unlock(5)));
    }

    #[test]
    fn backoff_doubles_to_the_cap_and_resets() {
        let mut b = Backoff::new(Duration::from_millis(500), Duration::from_secs(4));
        let delays: Vec<_> = (0..5).map(|_| b.next_delay()).collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(4)
            ]
        );
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_millis(500));
    }
}
