//! Host side of file transfer: one transfer at a time per host, received either in-process
//! (the panel's attended host asks its user first) or by the session helper in the signed-in
//! user's context (the unattended agent).

use crate::hub::Hub;
use desk_config::DEFAULT_MAX_UPLOAD_MB;
use desk_error::DeskResult;
use desk_ipc::IpcMessage;
use desk_protocol::{FileRejectReason, Message};
use desk_transfer::{Handled, Receiver};
use desk_transport::Session;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver as ChannelRx, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long the user of an attended host has to answer "X wants to send you a file".
const CONSENT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the helper may take to answer an offer.
const HELPER_ANSWER_TIMEOUT: Duration = Duration::from_secs(40);
/// A transfer that receives nothing for this long is abandoned.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a refused connection is read before it is closed.
const DRAIN: Duration = Duration::from_millis(300);
/// Read slice of the transfer loop.
const SLICE: Duration = Duration::from_millis(20);

/// What the panel's UI is told about a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// A paired PC wants to send a file; answer with [`TransferState::answer`].
    Offer {
        transfer_id: String,
        peer_name: String,
        name: String,
        size: u64,
    },
}

/// Receives [`TransferEvent`]s (called from connection threads).
pub type TransferListener = Arc<dyn Fn(TransferEvent) + Send + Sync + 'static>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Transfer bookkeeping shared by every connection of one host.
pub struct TransferState {
    active: Mutex<Option<String>>,
    /// Helper replies, routed to the connection thread that owns the transfer.
    routes: Mutex<HashMap<String, Sender<Message>>>,
    /// Offers waiting for the user's answer.
    pending: Mutex<HashMap<String, Sender<bool>>>,
    listener: Mutex<Option<TransferListener>>,
    max_upload_mb: AtomicU32,
    received_dir: Mutex<PathBuf>,
}

impl Default for TransferState {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            routes: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            listener: Mutex::new(None),
            max_upload_mb: AtomicU32::new(DEFAULT_MAX_UPLOAD_MB),
            received_dir: Mutex::new(PathBuf::new()),
        }
    }
}

/// Holds the host-wide transfer slot until dropped.
struct Slot<'a>(&'a TransferState);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *lock(&self.0.active) = None;
    }
}

impl TransferState {
    /// Subscribes the UI to incoming offers.
    pub fn set_listener(&self, listener: Option<TransferListener>) {
        *lock(&self.listener) = listener;
    }

    /// Upload limit enforced on offers this host receives itself (already clamped).
    pub fn set_max_upload_mb(&self, mb: u32) {
        self.max_upload_mb.store(mb, Ordering::SeqCst);
    }

    /// Folder an attended host stores received files in.
    pub fn set_received_dir(&self, dir: PathBuf) {
        *lock(&self.received_dir) = dir;
    }

    /// The user's answer to an offer; `false` when no such offer is waiting any more.
    pub fn answer(&self, transfer_id: &str, accept: bool) -> bool {
        match lock(&self.pending).remove(transfer_id) {
            Some(tx) => tx.send(accept).is_ok(),
            None => false,
        }
    }

    /// Hands a helper reply to the connection that owns the transfer.
    pub(crate) fn route_from_helper(&self, msg: Message) {
        let Some(id) = msg.transfer_id() else { return };
        if let Some(tx) = lock(&self.routes).get(id) {
            let _ = tx.send(msg);
        }
    }

    fn claim(&self, id: &str) -> Option<Slot<'_>> {
        let mut active = lock(&self.active);
        if active.is_some() {
            return None;
        }
        *active = Some(id.to_string());
        Some(Slot(self))
    }

    fn emit(&self, event: TransferEvent) {
        let cb = lock(&self.listener).clone();
        if let Some(cb) = cb {
            cb(event);
        }
    }
}

/// The offer fields, as received.
struct Offer {
    id: String,
    name: String,
    size: u64,
    sha256: [u8; 32],
}

/// Where the bytes of one transfer end up.
trait Sink {
    /// Decides on the offer. `Ok` means "accepted, send chunks".
    fn offer(&mut self, offer: &Offer, peer_name: &str) -> Result<(), FileRejectReason>;
    /// Feeds `FileChunk`/`FileDone`; `Some(reply)` is the final verdict for the sender.
    fn data(&mut self, msg: &Message) -> Option<Message>;
    /// A final verdict produced elsewhere (the helper), if one arrived.
    fn reply(&mut self) -> Option<Message>;
    /// The sender cancelled or vanished.
    fn abort(&mut self);
}

/// Serves one transfer connection whose first message was `first` (a `FileOffer`).
pub(crate) fn serve(
    session: &mut Session,
    hub: &Arc<Hub>,
    stop: &AtomicBool,
    first: Message,
    peer_name: &str,
) -> DeskResult<()> {
    let Message::FileOffer {
        transfer_id,
        name,
        size,
        sha256,
    } = first
    else {
        return Ok(());
    };
    let offer = Offer {
        id: transfer_id,
        name,
        size,
        sha256,
    };
    let state = hub.transfers();
    let reject = |session: &mut Session, reason| {
        let sent = session.send(&Message::FileReject {
            transfer_id: offer.id.clone(),
            reason,
        });
        drain(session);
        sent
    };
    let Some(_slot) = state.claim(&offer.id) else {
        return reject(session, FileRejectReason::Busy);
    };
    let mut sink: Box<dyn Sink + '_> = if hub.consent_gate_enabled() {
        Box::new(LocalSink::new(hub, stop))
    } else {
        match AgentSink::new(hub, &offer.id) {
            Some(sink) => Box::new(sink),
            None => return reject(session, FileRejectReason::NoAnswer),
        }
    };
    if let Err(reason) = sink.offer(&offer, peer_name) {
        return reject(session, reason);
    }
    session.send(&Message::FileAccept {
        transfer_id: offer.id.clone(),
    })?;
    pump(session, sink.as_mut(), &offer.id, stop)
}

/// Reads what the peer still sends for a moment before the connection closes: closing a socket
/// with unread data resets it, and the reset can wipe out the verdict that was just sent.
fn drain(session: &mut Session) {
    let until = Instant::now() + DRAIN;
    while Instant::now() < until {
        if !matches!(session.recv_timeout(SLICE), Ok(Some(_))) {
            break;
        }
    }
}

fn pump(session: &mut Session, sink: &mut dyn Sink, id: &str, stop: &AtomicBool) -> DeskResult<()> {
    let mut last_data = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if let Some(verdict) = sink.reply() {
            let sent = session.send(&verdict);
            drain(session);
            return sent;
        }
        let incoming = match session.recv_timeout(SLICE) {
            Ok(m) => m,
            Err(e) => {
                sink.abort();
                return Err(e);
            }
        };
        let Some(msg) = incoming.filter(|m| m.transfer_id() == Some(id)) else {
            if last_data.elapsed() > IDLE_TIMEOUT {
                sink.abort();
                return session.send(&Message::FileResult {
                    transfer_id: id.to_string(),
                    ok: false,
                    error: Some("timeout".to_string()),
                });
            }
            continue;
        };
        last_data = Instant::now();
        if matches!(msg, Message::FileCancel { .. }) {
            sink.abort();
            return Ok(());
        }
        if let Some(verdict) = sink.data(&msg) {
            return session.send(&verdict);
        }
    }
    sink.abort();
    Ok(())
}

/// Attended host: the local user is asked first, the bytes are written in this process.
struct LocalSink<'a> {
    hub: &'a Hub,
    stop: &'a AtomicBool,
    receiver: Receiver,
}

impl<'a> LocalSink<'a> {
    fn new(hub: &'a Hub, stop: &'a AtomicBool) -> Self {
        let state = hub.transfers();
        let dir = lock(&state.received_dir).clone();
        let mb = state.max_upload_mb.load(Ordering::SeqCst);
        Self {
            hub,
            stop,
            receiver: Receiver::new(dir, mb),
        }
    }

    /// Blocks until the user answered, the time ran out or the host stops.
    fn ask(&self, offer: &Offer, peer_name: &str) -> Result<(), FileRejectReason> {
        let state = self.hub.transfers();
        let (tx, rx) = channel();
        lock(&state.pending).insert(offer.id.clone(), tx);
        state.emit(TransferEvent::Offer {
            transfer_id: offer.id.clone(),
            peer_name: peer_name.to_string(),
            name: offer.name.clone(),
            size: offer.size,
        });
        let started = Instant::now();
        let verdict = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(true) => break Ok(()),
                Ok(false) | Err(RecvTimeoutError::Disconnected) => {
                    break Err(FileRejectReason::Denied)
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.stop.load(Ordering::SeqCst) || started.elapsed() > CONSENT_TIMEOUT {
                        break Err(FileRejectReason::NoAnswer);
                    }
                }
            }
        };
        lock(&state.pending).remove(&offer.id);
        verdict
    }
}

impl Sink for LocalSink<'_> {
    fn offer(&mut self, offer: &Offer, peer_name: &str) -> Result<(), FileRejectReason> {
        // Limit, name and disk checks come first; nothing is shown or written for a bad offer.
        self.receiver.check_offer(&offer.name, offer.size)?;
        self.ask(offer, peer_name)?;
        self.receiver
            .accept(&offer.id, &offer.name, offer.size, offer.sha256)
    }

    fn data(&mut self, msg: &Message) -> Option<Message> {
        match self.receiver.handle(msg) {
            Handled::Failed { reply } | Handled::Saved { reply, .. } => Some(reply),
            Handled::Nothing | Handled::Progress { .. } => None,
        }
    }

    fn reply(&mut self) -> Option<Message> {
        None
    }

    fn abort(&mut self) {
        self.receiver.abort();
    }
}

/// Unattended agent: the session helper writes the file as the signed-in user.
struct AgentSink<'a> {
    hub: &'a Hub,
    id: String,
    replies: ChannelRx<Message>,
    finished: bool,
}

impl<'a> AgentSink<'a> {
    /// `None` when no helper (no signed-in user) is connected.
    fn new(hub: &'a Hub, id: &str) -> Option<Self> {
        hub.helper_connected().then_some(())?;
        let (tx, replies) = channel();
        lock(&hub.transfers().routes).insert(id.to_string(), tx);
        Some(Self {
            hub,
            id: id.to_string(),
            replies,
            finished: false,
        })
    }

    fn to_helper(&self, msg: IpcMessage) -> bool {
        self.hub.send_to_helper(&msg)
    }
}

impl Sink for AgentSink<'_> {
    fn offer(&mut self, offer: &Offer, peer_name: &str) -> Result<(), FileRejectReason> {
        let sent = self.to_helper(IpcMessage::FileIncoming {
            transfer_id: offer.id.clone(),
            peer_name: peer_name.to_string(),
        }) && self.to_helper(IpcMessage::Wire(Message::FileOffer {
            transfer_id: offer.id.clone(),
            name: offer.name.clone(),
            size: offer.size,
            sha256: offer.sha256,
        }));
        if !sent {
            return Err(FileRejectReason::NoAnswer);
        }
        match self.replies.recv_timeout(HELPER_ANSWER_TIMEOUT) {
            Ok(Message::FileAccept { .. }) => Ok(()),
            Ok(Message::FileReject { reason, .. }) => {
                self.finished = true;
                Err(reason)
            }
            _ => Err(FileRejectReason::NoAnswer),
        }
    }

    fn data(&mut self, msg: &Message) -> Option<Message> {
        self.to_helper(IpcMessage::Wire(msg.clone()));
        None
    }

    fn reply(&mut self) -> Option<Message> {
        let msg = self.replies.try_recv().ok()?;
        self.finished = true;
        Some(msg)
    }

    fn abort(&mut self) {
        if !self.finished {
            self.finished = true;
            self.to_helper(IpcMessage::Wire(Message::FileCancel {
                transfer_id: self.id.clone(),
            }));
        }
    }
}

impl Drop for AgentSink<'_> {
    fn drop(&mut self) {
        // A connection that ended without a verdict (peer vanished) must not leave a partial.
        self.abort();
        lock(&self.hub.transfers().routes).remove(&self.id);
    }
}
