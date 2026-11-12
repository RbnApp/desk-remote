//! Receives files for the signed-in user: the service forwards a transfer over the pipe, the
//! helper writes it with this user's rights into `Downloads\Desk-Remote` and shows a balloon.

use crate::state::{Notice, Shared};
use desk_i18n::t;
use desk_ipc::{IpcConnection, IpcMessage};
use desk_protocol::Message;
use desk_transfer::{Handled, Receiver};
use std::path::PathBuf;

/// The agent PC is unattended, so an offer is accepted when it passes the checks (limit,
/// name, disk space); the user learns about the file from the tray balloon.
pub struct FileInbox {
    receiver: Receiver,
    conn: IpcConnection,
    /// Sender name announced by the service for the transfer that is about to be offered.
    announced: Option<(String, String)>,
    current_peer: String,
    current_name: String,
}

impl FileInbox {
    pub fn new(dir: PathBuf, max_upload_mb: u32, conn: IpcConnection) -> Self {
        Self {
            receiver: Receiver::new(dir, desk_config::clamp_upload_mb(max_upload_mb)),
            conn,
            announced: None,
            current_peer: String::new(),
            current_name: String::new(),
        }
    }

    /// Remembers who sends the offer that follows.
    pub fn announce(&mut self, transfer_id: String, peer_name: String) {
        self.announced = Some((transfer_id, peer_name));
    }

    /// Handles one `File*` message forwarded by the service.
    pub fn handle(&mut self, msg: &Message, shared: &Shared) {
        match msg {
            Message::FileOffer {
                transfer_id,
                name,
                size,
                sha256,
            } => {
                let reply = match self.receiver.accept(transfer_id, name, *size, *sha256) {
                    Ok(()) => {
                        self.current_name.clone_from(name);
                        self.current_peer = match self.announced.take() {
                            Some((id, peer)) if id == *transfer_id => peer,
                            _ => String::new(),
                        };
                        Message::FileAccept {
                            transfer_id: transfer_id.clone(),
                        }
                    }
                    Err(reason) => Message::FileReject {
                        transfer_id: transfer_id.clone(),
                        reason,
                    },
                };
                self.send(reply);
            }
            other => match self.receiver.handle(other) {
                Handled::Saved { reply, .. } => {
                    self.send(reply);
                    let body = t("tray.file_body")
                        .replace("{peer}", &self.current_peer)
                        .replace("{name}", &self.current_name);
                    shared.update(|ui| {
                        ui.pending_notice = Some(Notice {
                            title: t("tray.file_title"),
                            body,
                        });
                    });
                }
                Handled::Failed { reply } => self.send(reply),
                Handled::Nothing | Handled::Progress { .. } => {}
            },
        }
    }

    /// The service connection ended: nothing half written stays behind.
    pub fn abort(&mut self) {
        self.receiver.abort();
    }

    fn send(&self, msg: Message) {
        if let Err(e) = self.conn.send(&IpcMessage::Wire(msg)) {
            tracing::debug!(error = %e, "cannot answer the service");
        }
    }
}
