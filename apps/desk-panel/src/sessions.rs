//! Registry of the live viewer sessions, keyed by PC id (several PCs can be open at once).

use crate::backend::{InputEvent, ViewerSession};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// One row of `viewer_sessions` (used to restore the tab bar).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub pc_id: String,
    /// `"connected"` while the link is up, `"closed"` once it ended (until the tab is closed).
    pub state: &'static str,
    /// The session streams video; background tabs are paused.
    pub active: bool,
}

struct Entry {
    session: Box<dyn ViewerSession>,
    active: bool,
}

/// All open sessions; replacing one PC's session never touches the others.
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, Entry>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Sessions {
    /// Stores a freshly connected (streaming) session and closes the one it replaces.
    pub fn insert(&self, pc_id: &str, session: Box<dyn ViewerSession>) {
        let old = lock(&self.map).insert(
            pc_id.to_string(),
            Entry {
                session,
                active: true,
            },
        );
        if let Some(old) = old {
            old.session.close();
        }
    }

    /// Leaves and forgets the session of `pc_id`.
    pub fn close(&self, pc_id: &str) {
        let old = lock(&self.map).remove(pc_id);
        if let Some(old) = old {
            old.session.close();
        }
    }

    /// Forwards input to the session of `pc_id` only; ignored while it is paused.
    pub fn input(&self, pc_id: &str, event: &InputEvent) {
        if let Some(entry) = lock(&self.map).get(pc_id).filter(|e| e.active) {
            entry.session.send_input(event);
        }
    }

    pub fn request_keyframe(&self, pc_id: &str) {
        if let Some(entry) = lock(&self.map).get(pc_id).filter(|e| e.active) {
            entry.session.request_keyframe();
        }
    }

    /// Pauses or resumes the video of `pc_id`; `false` when there is no such session.
    pub fn set_active(&self, pc_id: &str, active: bool) -> bool {
        let mut map = lock(&self.map);
        let Some(entry) = map.get_mut(pc_id) else {
            return false;
        };
        if entry.active != active {
            entry.active = active;
            entry.session.set_active(active);
        }
        true
    }

    /// Open sessions ordered by PC id.
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut rows: Vec<SessionInfo> = lock(&self.map)
            .iter()
            .map(|(pc_id, e)| SessionInfo {
                pc_id: pc_id.clone(),
                state: if e.session.is_open() {
                    "connected"
                } else {
                    "closed"
                },
                active: e.active,
            })
            .collect();
        rows.sort_by(|a, b| a.pc_id.cmp(&b.pc_id));
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[derive(Default)]
    struct Log(Mutex<Vec<String>>);

    struct Fake {
        name: &'static str,
        log: Arc<Log>,
        open: Arc<AtomicBool>,
    }

    impl ViewerSession for Fake {
        fn send_input(&self, _event: &InputEvent) {
            lock(&self.log.0).push(format!("{}:input", self.name));
        }
        fn request_keyframe(&self) {
            lock(&self.log.0).push(format!("{}:key", self.name));
        }
        fn set_active(&self, active: bool) {
            lock(&self.log.0).push(format!("{}:active={active}", self.name));
        }
        fn is_open(&self) -> bool {
            self.open.load(Ordering::Relaxed)
        }
        fn close(&self) {
            lock(&self.log.0).push(format!("{}:close", self.name));
        }
    }

    fn fake(name: &'static str, log: &Arc<Log>) -> (Box<Fake>, Arc<AtomicBool>) {
        let open = Arc::new(AtomicBool::new(true));
        (
            Box::new(Fake {
                name,
                log: Arc::clone(log),
                open: Arc::clone(&open),
            }),
            open,
        )
    }

    fn taken(log: &Log) -> Vec<String> {
        std::mem::take(&mut *lock(&log.0))
    }

    fn mv() -> InputEvent {
        InputEvent::MouseMove { x: 1, y: 2 }
    }

    #[test]
    fn several_sessions_coexist_and_input_goes_to_the_named_one() {
        let (log, sessions) = (Arc::new(Log::default()), Sessions::default());
        sessions.insert("a", fake("a", &log).0);
        sessions.insert("b", fake("b", &log).0);
        assert_eq!(sessions.list().len(), 2);
        assert!(taken(&log).is_empty(), "connecting b must not close a");
        sessions.input("b", &mv());
        sessions.request_keyframe("a");
        assert_eq!(taken(&log), ["b:input", "a:key"]);
    }

    #[test]
    fn reconnecting_replaces_only_that_pc() {
        let (log, sessions) = (Arc::new(Log::default()), Sessions::default());
        sessions.insert("a", fake("a1", &log).0);
        sessions.insert("b", fake("b", &log).0);
        sessions.insert("a", fake("a2", &log).0);
        assert_eq!(taken(&log), ["a1:close"]);
        sessions.input("a", &mv());
        assert_eq!(taken(&log), ["a2:input"]);
    }

    #[test]
    fn pause_resume_is_idempotent_and_blocks_input_while_paused() {
        let (log, sessions) = (Arc::new(Log::default()), Sessions::default());
        sessions.insert("a", fake("a", &log).0);
        assert!(sessions.set_active("a", false));
        assert!(sessions.set_active("a", false));
        sessions.input("a", &mv());
        assert_eq!(taken(&log), ["a:active=false"]);
        assert!(sessions.set_active("a", true));
        assert_eq!(taken(&log), ["a:active=true"]);
        assert!(!sessions.set_active("missing", true));
    }

    #[test]
    fn a_dropped_link_is_listed_closed_and_leaves_the_others_alone() {
        let (log, sessions) = (Arc::new(Log::default()), Sessions::default());
        let (a, a_open) = fake("a", &log);
        sessions.insert("a", a);
        sessions.insert("b", fake("b", &log).0);
        a_open.store(false, Ordering::Relaxed);
        let rows = sessions.list();
        assert_eq!(rows[0].state, "closed");
        assert_eq!(rows[1].state, "connected");
        sessions.close("a");
        assert_eq!(sessions.list().len(), 1);
    }
}
