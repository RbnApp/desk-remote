//! Sending side: hashes the file, offers it and streams it in chunks.

use crate::MB;
use desk_error::DeskError;
use desk_protocol::{FileRejectReason, Message, FILE_CHUNK_MAX};
use desk_transport::Session;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long the receiver may take to answer an offer (its consent prompt lasts 30 s).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(45);
/// How long the receiver may take for the final verdict after `FileDone`.
const RESULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Poll slice while waiting, so cancel stays responsive.
const POLL: Duration = Duration::from_millis(100);

/// Why a send did not complete. [`SendError::code`] is the stable string shown to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// A folder was chosen; only files can be sent.
    UnsupportedFolder,
    /// The file is over the sender's limit (found before any connection is made).
    TooLarge,
    /// The receiver refused the offer.
    Rejected(FileRejectReason),
    /// The user cancelled.
    Cancelled,
    /// The receiver reported a failure after the data was sent (`hash_mismatch`, ...).
    Failed(String),
    /// Reading the local file failed.
    Io(String),
    /// The connection broke or the receiver stopped answering.
    Network(String),
}

impl SendError {
    /// Stable snake_case code.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::UnsupportedFolder => "unsupported_folder".to_string(),
            Self::TooLarge => "too_large".to_string(),
            Self::Rejected(reason) => reason.code().to_string(),
            Self::Cancelled => "cancelled".to_string(),
            Self::Failed(code) => code.clone(),
            Self::Io(_) => "read_failed".to_string(),
            Self::Network(_) => "network".to_string(),
        }
    }

    fn net(e: DeskError) -> Self {
        Self::Network(e.to_string())
    }
}

/// A file that passed the local checks and is ready to be offered.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub transfer_id: String,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Bytes sent so far.
#[derive(Debug, Clone, Copy)]
pub struct SendProgress {
    pub sent: u64,
    pub total: u64,
}

/// A fresh id, unique per process run and practically unique across machines.
#[must_use]
pub fn new_transfer_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{nanos:x}-{:x}-{:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Size, name and limit checks plus the SHA-256 pass. Makes no connection, so a file over
/// `max_upload_mb` fails here with [`SendError::TooLarge`] before any byte leaves the PC.
pub fn prepare(
    path: &Path,
    max_upload_mb: u32,
    transfer_id: String,
    cancel: &AtomicBool,
) -> Result<Prepared, SendError> {
    let io = |e: std::io::Error| SendError::Io(e.to_string());
    let meta = std::fs::metadata(path).map_err(io)?;
    if meta.is_dir() {
        return Err(SendError::UnsupportedFolder);
    }
    let size = meta.len();
    if size > u64::from(max_upload_mb) * MB {
        return Err(SendError::TooLarge);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| SendError::Io("no file name".to_string()))?;
    let mut file = File::open(path).map_err(io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; FILE_CHUNK_MAX];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(SendError::Cancelled);
        }
        let n = file.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Prepared {
        transfer_id,
        path: path.to_path_buf(),
        name,
        size,
        sha256: hasher.finalize().into(),
    })
}

/// Offers `file` on an authenticated session and streams it once the receiver accepts.
/// `progress` is called after every chunk.
pub fn send_prepared(
    session: &mut Session,
    file: &Prepared,
    cancel: &AtomicBool,
    mut progress: impl FnMut(SendProgress),
) -> Result<(), SendError> {
    let id = &file.transfer_id;
    session
        .send(&Message::FileOffer {
            transfer_id: id.clone(),
            name: file.name.clone(),
            size: file.size,
            sha256: file.sha256,
        })
        .map_err(SendError::net)?;
    match wait_for(session, id, cancel, ANSWER_TIMEOUT)? {
        Message::FileAccept { .. } => {}
        Message::FileReject { reason, .. } => return Err(SendError::Rejected(reason)),
        _ => return Err(SendError::Network("unexpected answer".to_string())),
    }
    let outcome = stream(session, file, cancel, &mut progress);
    if matches!(outcome, Err(SendError::Cancelled)) {
        let _ = session.send(&Message::FileCancel {
            transfer_id: id.clone(),
        });
    }
    outcome
}

fn stream(
    session: &mut Session,
    file: &Prepared,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(SendProgress),
) -> Result<(), SendError> {
    let id = &file.transfer_id;
    let mut src = File::open(&file.path).map_err(|e| SendError::Io(e.to_string()))?;
    let mut buf = vec![0u8; FILE_CHUNK_MAX];
    let mut sent = 0u64;
    while sent < file.size {
        if cancel.load(Ordering::SeqCst) {
            return Err(SendError::Cancelled);
        }
        // The receiver may abort at any time (cancel, disk full, limit exceeded).
        if let Some(msg) = session
            .recv_timeout(Duration::from_millis(1))
            .map_err(SendError::net)?
        {
            if let Some(err) = abort_reason(&msg, id) {
                return Err(err);
            }
        }
        let want = usize::try_from((file.size - sent).min(FILE_CHUNK_MAX as u64))
            .unwrap_or(FILE_CHUNK_MAX);
        let n = src
            .read(&mut buf[..want])
            .map_err(|e| SendError::Io(e.to_string()))?;
        if n == 0 {
            return Err(SendError::Io("file changed while sending".to_string()));
        }
        session
            .send(&Message::FileChunk {
                transfer_id: id.clone(),
                offset: sent,
                data: buf[..n].to_vec(),
            })
            .map_err(SendError::net)?;
        sent += n as u64;
        progress(SendProgress {
            sent,
            total: file.size,
        });
    }
    session
        .send(&Message::FileDone {
            transfer_id: id.clone(),
        })
        .map_err(SendError::net)?;
    match wait_for(session, id, cancel, RESULT_TIMEOUT)? {
        Message::FileResult { ok: true, .. } => Ok(()),
        Message::FileResult { error, .. } => Err(SendError::Failed(
            error.unwrap_or_else(|| "failed".to_string()),
        )),
        Message::FileReject { reason, .. } => Err(SendError::Rejected(reason)),
        _ => Err(SendError::Network("unexpected answer".to_string())),
    }
}

/// The error a mid-transfer message from the receiver means, if any.
fn abort_reason(msg: &Message, id: &str) -> Option<SendError> {
    if msg.transfer_id() != Some(id) {
        return None;
    }
    match msg {
        Message::FileReject { reason, .. } => Some(SendError::Rejected(*reason)),
        Message::FileCancel { .. } => Some(SendError::Failed("cancelled_by_peer".to_string())),
        Message::FileResult {
            ok: false, error, ..
        } => Some(SendError::Failed(
            error.clone().unwrap_or_else(|| "failed".to_string()),
        )),
        _ => None,
    }
}

/// Waits for the next message of transfer `id`, skipping unrelated traffic.
fn wait_for(
    session: &mut Session,
    id: &str,
    cancel: &AtomicBool,
    limit: Duration,
) -> Result<Message, SendError> {
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(SendError::Cancelled);
        }
        if started.elapsed() > limit {
            return Err(SendError::Rejected(FileRejectReason::NoAnswer));
        }
        let msg = session.recv_timeout(POLL).map_err(SendError::net)?;
        if let Some(msg) = msg.filter(|m| m.transfer_id() == Some(id)) {
            return Ok(msg);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        assert_ne!(new_transfer_id(), new_transfer_id());
    }

    #[test]
    fn prepare_checks_folder_limit_and_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let id = || "t".to_string();
        assert_eq!(
            prepare(dir.path(), 1, id(), &cancel).map(|_| ()),
            Err(SendError::UnsupportedFolder)
        );
        let file = dir.path().join("a.bin");
        let data = vec![7u8; (MB + 1) as usize];
        std::fs::write(&file, &data).unwrap();
        assert_eq!(
            prepare(&file, 1, id(), &cancel).map(|_| ()),
            Err(SendError::TooLarge)
        );
        let ok = prepare(&file, 2, id(), &cancel).unwrap();
        assert_eq!(ok.size, MB + 1);
        assert_eq!(ok.name, "a.bin");
        let expected: [u8; 32] = Sha256::digest(&data).into();
        assert_eq!(ok.sha256, expected);
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            prepare(&file, 2, id(), &cancel).map(|_| ()),
            Err(SendError::Cancelled)
        );
    }

    #[test]
    fn codes_are_stable() {
        assert_eq!(
            SendError::Rejected(FileRejectReason::TooLarge).code(),
            "too_large"
        );
        assert_eq!(SendError::UnsupportedFolder.code(), "unsupported_folder");
        assert_eq!(SendError::Network("x".into()).code(), "network");
    }
}
