//! Network side: accepts viewer connections, authenticates them and bridges them to the hub.

use crate::hub::Hub;
use desk_error::DeskResult;
use desk_protocol::Message;
use desk_transport::{Listener, Session};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Read slice while a viewer is watching (keeps input latency low).
const WATCH_SLICE: Duration = Duration::from_millis(5);
/// Read slice while a viewer is connected but not watching.
const IDLE_SLICE: Duration = Duration::from_millis(100);

/// Accept loop; returns when `stop` is set (the caller wakes the listener).
pub fn accept_loop(listener: &Listener, hub: &Arc<Hub>, stop: &Arc<AtomicBool>) {
    loop {
        let session = listener.accept();
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match session {
            Ok(session) => {
                let hub = Arc::clone(hub);
                let stop = Arc::clone(stop);
                let spawned = std::thread::Builder::new()
                    .name("desk-viewer".to_string())
                    .spawn(move || {
                        if let Err(e) = serve_viewer(session, &hub, &stop) {
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

fn serve_viewer(mut session: Session, hub: &Arc<Hub>, stop: &Arc<AtomicBool>) -> DeskResult<()> {
    let peer = session.peer_addr();
    hub.with_pairing(|pm| session.authenticate_as_host(pm, hub.identity()))?;
    let device_id = session.peer_device_id().unwrap_or_default();
    tracing::info!(%peer, device = %device_id, "viewer authenticated");
    session.send(&Message::Hello {
        device_id: hub.identity().device_id(),
        version: desk_version::CURRENT.to_string(),
    })?;
    let (id, rx) = hub.register_viewer(&device_id);
    let result = bridge(&mut session, hub, stop, id, &rx);
    hub.remove_viewer(id);
    result
}

fn bridge(
    session: &mut Session,
    hub: &Arc<Hub>,
    stop: &Arc<AtomicBool>,
    id: u64,
    rx: &std::sync::mpsc::Receiver<Message>,
) -> DeskResult<()> {
    let mut watching = false;
    while !stop.load(Ordering::SeqCst) {
        while let Ok(out) = rx.try_recv() {
            session.send(&out)?;
        }
        let slice = if watching { WATCH_SLICE } else { IDLE_SLICE };
        let Some(msg) = session.recv_timeout(slice)? else {
            continue;
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
