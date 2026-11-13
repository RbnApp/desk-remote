//! Network side: accepts viewer connections, authenticates them and bridges them to the hub.

use crate::hub::Hub;
use desk_error::DeskResult;
use desk_protocol::Message;
use desk_transport::{Listener, Session};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;
use std::time::Duration;

/// How long a fresh connection may stay silent before it is treated as a viewer.
const FIRST_MESSAGE_WAIT: Duration = Duration::from_millis(500);
/// Read slice while a viewer is watching (keeps input latency low).
const WATCH_SLICE: Duration = Duration::from_millis(5);
/// Read slice while a viewer is connected but not watching.
const IDLE_SLICE: Duration = Duration::from_millis(100);

/// Most connections that may sit in the (unauthenticated) handshake at the same time.
/// A handshake lasts seconds at most, so legitimate use never comes close; the limit keeps
/// a flood of idle sockets from exhausting threads. Authenticated viewers are not counted.
pub const MAX_PENDING_HANDSHAKES: usize = 16;

/// Releases one handshake slot when dropped (also on early error return or panic).
struct PendingSlot(Arc<AtomicUsize>);

impl PendingSlot {
    /// Takes a slot unless `limit` handshakes are already pending.
    fn acquire(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        let taken = counter.fetch_add(1, Ordering::SeqCst);
        if taken >= limit {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Arc::clone(counter)))
    }
}

impl Drop for PendingSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accept loop; returns when `stop` is set (the caller wakes the listener).
pub fn accept_loop(listener: &Listener, hub: &Arc<Hub>, stop: &Arc<AtomicBool>) {
    accept_loop_limited(listener, hub, stop, MAX_PENDING_HANDSHAKES);
}

fn accept_loop_limited(
    listener: &Listener,
    hub: &Arc<Hub>,
    stop: &Arc<AtomicBool>,
    pending_limit: usize,
) {
    let pending = Arc::new(AtomicUsize::new(0));
    loop {
        let session = listener.accept();
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match session {
            Ok(session) => {
                let Some(slot) = PendingSlot::acquire(&pending, pending_limit) else {
                    tracing::warn!(
                        peer = %session.peer_addr(),
                        limit = pending_limit,
                        "too many pending handshakes, connection closed"
                    );
                    continue;
                };
                let hub = Arc::clone(hub);
                let stop = Arc::clone(stop);
                let spawned = std::thread::Builder::new()
                    .name("desk-viewer".to_string())
                    .spawn(move || {
                        if let Err(e) = serve_viewer(session, &hub, &stop, slot) {
                            tracing::info!(error = %e, "viewer connection ended");
                        }
                    });
                if let Err(e) = spawned {
                    tracing::error!(error = %e, "cannot start viewer thread");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn serve_viewer(
    mut session: Session,
    hub: &Arc<Hub>,
    stop: &Arc<AtomicBool>,
    slot: PendingSlot,
) -> DeskResult<()> {
    let peer = session.peer_addr();
    // The pairing state is locked only for the short steps inside the handshake, never
    // while waiting for the peer, so one stalled client cannot block the others.
    session.authenticate_as_host(hub.pairing(), hub.identity())?;
    drop(slot);
    let device_id = session.peer_device_id().unwrap_or_default();
    if device_id == hub.identity().device_id() {
        return Err(desk_error::DeskError::Transport(
            "cannot connect to self".to_string(),
        ));
    }
    tracing::info!(%peer, device = %device_id, "viewer authenticated");
    let host_pc_name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Desk-PC".to_string());
    session.send(&Message::Hello {
        device_id: hub.identity().device_id(),
        version: desk_version::CURRENT.to_string(),
        peer_name: Some(host_pc_name),
    })?;
    let peer_name = session.peer_name().map(str::to_string);
    let shown_name = peer_name.clone().unwrap_or_else(|| "?".to_string());
    // A file sender announces itself with its first message; everyone else is a viewer.
    // Deciding before registration keeps a transfer from raising a viewer consent prompt.
    let first = session.recv_timeout(FIRST_MESSAGE_WAIT)?;
    if first.as_ref().is_some_and(|m| m.transfer_id().is_some()) {
        let sender = peer_name.as_deref().unwrap_or("?");
        return match first {
            Some(offer) => crate::transfer::serve(&mut session, hub, stop, offer, sender),
            None => Ok(()),
        };
    }
    // "Remove my pairing": acts on this authenticated device only, then the link ends.
    if matches!(first, Some(Message::Unpair)) {
        hub.unpair_device(&device_id);
        return Ok(());
    }
    let peer_fingerprint = session.peer_fingerprint();
    // The paired panel is let in without a question: leave a trace and tell the PC's user.
    hub.record_peer_connected(&device_id, &shown_name);
    let (id, rx) = hub.register_viewer_full(
        &device_id,
        &peer.to_string(),
        peer_name.as_deref(),
        peer_fingerprint.as_deref(),
    );
    if let Some(req) = hub.pending_request() {
        hub.spawn_consent_timeout(req.request_id, id);
    }
    let result = bridge(&mut session, hub, stop, id, &rx, first);
    hub.remove_viewer(id);
    result
}

fn bridge(
    session: &mut Session,
    hub: &Arc<Hub>,
    stop: &Arc<AtomicBool>,
    id: u64,
    rx: &std::sync::mpsc::Receiver<Message>,
    first: Option<Message>,
) -> DeskResult<()> {
    let mut watching = false;
    let mut next = first;
    while !stop.load(Ordering::SeqCst) {
        loop {
            match rx.try_recv() {
                Ok(out) => session.send(&out)?,
                Err(TryRecvError::Empty) => break,
                // The hub dropped this viewer ("disconnect all"): the queue is flushed.
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        let slice = if watching { WATCH_SLICE } else { IDLE_SLICE };
        let msg = match next.take() {
            Some(m) => m,
            None => match session.recv_timeout(slice)? {
                Some(m) => m,
                None => continue,
            },
        };
        match msg {
            Message::ViewerJoin => {
                watching = true;
                hub.viewer_join(id);
            }
            Message::ViewerLeave => {
                watching = false;
                hub.viewer_leave(id);
            }
            Message::Ping { ts } => session.send(&Message::Pong { ts })?,

            m @ (Message::InputEvent(_) | Message::RequestKeyframe) => {
                hub.forward_from_viewer(id, m)
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
    use desk_transport::{connect, PeerTarget};
    use std::time::Instant;

    struct Rig {
        hub: Arc<Hub>,
        port: u16,
        stop: Arc<AtomicBool>,
        listener: Arc<Listener>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Rig {
        fn start(limit: usize) -> DeskResult<Self> {
            let hub = Arc::new(Hub::new(DeviceIdentity::generate()?, PairingManager::new()));
            let listener = Arc::new(Listener::bind_loopback(0)?);
            let port = listener.local_port()?;
            let stop = Arc::new(AtomicBool::new(false));
            let (l, h, s) = (Arc::clone(&listener), Arc::clone(&hub), Arc::clone(&stop));
            let thread = std::thread::spawn(move || accept_loop_limited(&l, &h, &s, limit));
            Ok(Self {
                hub,
                port,
                stop,
                listener,
                thread: Some(thread),
            })
        }

        /// A connection that never sends its preamble: the host waits in the handshake.
        fn stalled_client(&self) -> DeskResult<Session> {
            connect(PeerTarget::loopback(self.port))
        }

        /// A well-behaved first-time pairing with `code`.
        fn pair_with(&self, code: &str) -> DeskResult<Duration> {
            let dir = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
            let mut trust = TrustStore::open(&dir.path().join("trust.json"))?;
            let identity = DeviceIdentity::generate()?;
            let started = Instant::now();
            let mut session = self.stalled_client()?;
            session.authenticate_as_client(&identity, &mut trust, Some(code))?;
            Ok(started.elapsed())
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.listener.wake();
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    #[test]
    fn a_stalled_handshake_does_not_block_another_viewer_from_pairing() -> DeskResult<()> {
        let rig = Rig::start(MAX_PENDING_HANDSHAKES)?;
        let code = rig.hub.with_pairing(PairingManager::create_pairing_code)?;
        let _stalled = rig.stalled_client()?;
        std::thread::sleep(Duration::from_millis(300)); // the stalled one is in the host
        let took = rig.pair_with(&code)?;
        assert!(
            took < Duration::from_secs(5),
            "second viewer waited {took:?} for the stalled handshake"
        );
        Ok(())
    }

    #[test]
    fn connections_beyond_the_pending_limit_are_refused_and_slots_are_freed() -> DeskResult<()> {
        let rig = Rig::start(2)?;
        let code = rig.hub.with_pairing(PairingManager::create_pairing_code)?;
        let first = rig.stalled_client()?;
        let _second = rig.stalled_client()?;
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            rig.pair_with(&code).is_err(),
            "a third pending connection must be closed by the host"
        );
        drop(first);
        std::thread::sleep(Duration::from_millis(500)); // the host sees EOF and frees the slot
        assert!(
            rig.pair_with(&code).is_ok(),
            "a freed slot must accept a new viewer"
        );
        Ok(())
    }

    #[test]
    fn self_connection_is_rejected() -> DeskResult<()> {
        let rig = Rig::start(MAX_PENDING_HANDSHAKES)?;
        let code = rig.hub.with_pairing(PairingManager::create_pairing_code)?;
        let mut session = rig.stalled_client()?;
        let dir = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
        let mut trust = TrustStore::open(&dir.path().join("trust.json"))?;
        // Client uses the SAME identity as the host!
        let host_identity = rig.hub.identity().clone();
        let auth_res = session.authenticate_as_client(&host_identity, &mut trust, Some(&code));
        // Should either fail authentication or host immediately closes connection
        if auth_res.is_ok() {
            // If handshake finished, receiving next message should be Err because host rejected
            let msg = session.recv_timeout(Duration::from_millis(500));
            assert!(msg.is_err() || msg.unwrap().is_none());
        }
        Ok(())
    }
}
