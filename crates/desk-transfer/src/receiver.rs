//! Receiving side: validates an offer and writes the file safely.

use crate::{claim_unique_path, free_space, is_safe_file_name, PARTIAL_SUFFIX};
use desk_protocol::{FileRejectReason, Message};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One megabyte, the unit of the upload limit.
pub const MB: u64 = 1024 * 1024;

/// Extra free space required on top of the file size, so a transfer never fills the disk.
const DISK_MARGIN: u64 = 16 * MB;

/// What a received message changed.
#[derive(Debug, PartialEq)]
pub enum Handled {
    /// Nothing to report (not for this transfer, or the peer cancelled it).
    Nothing,
    /// `written` bytes are on disk so far.
    Progress { written: u64 },
    /// The transfer failed; the partial file is gone. Send `reply` to the peer.
    Failed { reply: Message },
    /// The file passed the digest check and now lives at `path`. Send `reply` to the peer.
    Saved { reply: Message, path: PathBuf },
}

struct Active {
    id: String,
    name: String,
    partial: PathBuf,
    file: File,
    size: u64,
    written: u64,
    hasher: Sha256,
    expected: [u8; 32],
}

/// Receives at most one file at a time into one folder. Nothing touches the disk before
/// [`Receiver::accept`]; an unfinished transfer is deleted on abort or drop.
pub struct Receiver {
    dir: PathBuf,
    max_bytes: u64,
    active: Option<Active>,
}

impl Receiver {
    /// `max_upload_mb` is the (already clamped) upload limit in MB.
    #[must_use]
    pub fn new(dir: PathBuf, max_upload_mb: u32) -> Self {
        Self {
            dir,
            max_bytes: u64::from(max_upload_mb) * MB,
            active: None,
        }
    }

    /// True while a transfer is in progress.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    /// Decides whether an offer may be shown to the user at all. Writes nothing.
    pub fn check_offer(&self, name: &str, size: u64) -> Result<(), FileRejectReason> {
        if !is_safe_file_name(name) {
            return Err(FileRejectReason::Denied);
        }
        if size > self.max_bytes {
            return Err(FileRejectReason::TooLarge);
        }
        if self.active.is_some() {
            return Err(FileRejectReason::Busy);
        }
        let mut existing = self.dir.as_path();
        while !existing.exists() {
            match existing.parent() {
                Some(parent) => existing = parent,
                None => break,
            }
        }
        match free_space(existing) {
            Some(free) if free < size.saturating_add(DISK_MARGIN) => {
                Err(FileRejectReason::DiskFull)
            }
            _ => Ok(()),
        }
    }

    /// The user accepted: re-checks the offer and creates the `.partial` file.
    pub fn accept(
        &mut self,
        transfer_id: &str,
        name: &str,
        size: u64,
        sha256: [u8; 32],
    ) -> Result<(), FileRejectReason> {
        self.check_offer(name, size)?;
        std::fs::create_dir_all(&self.dir).map_err(|_| FileRejectReason::DiskFull)?;
        let partial = self.dir.join(format!(
            ".{}-{}{PARTIAL_SUFFIX}",
            std::process::id(),
            sanitize_id(transfer_id)
        ));
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|_| FileRejectReason::DiskFull)?;
        self.active = Some(Active {
            id: transfer_id.to_string(),
            name: name.to_string(),
            partial,
            file,
            size,
            written: 0,
            hasher: Sha256::new(),
            expected: sha256,
        });
        Ok(())
    }

    /// Feeds `FileChunk`, `FileDone` and `FileCancel` messages; everything else is ignored.
    pub fn handle(&mut self, msg: &Message) -> Handled {
        match msg {
            Message::FileChunk {
                transfer_id,
                offset,
                data,
            } if self.is_current(transfer_id) => self.on_chunk(*offset, data),
            Message::FileDone { transfer_id } if self.is_current(transfer_id) => self.on_done(),
            Message::FileCancel { transfer_id } if self.is_current(transfer_id) => {
                self.abort();
                Handled::Nothing
            }
            _ => Handled::Nothing,
        }
    }

    /// Drops the running transfer and deletes its partial file.
    pub fn abort(&mut self) {
        if let Some(active) = self.active.take() {
            drop(active.file);
            remove_quietly(&active.partial);
        }
    }

    fn is_current(&self, id: &str) -> bool {
        self.active.as_ref().is_some_and(|a| a.id == id)
    }

    fn fail(&mut self, code: &str) -> Handled {
        let id = self
            .active
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default();
        self.abort();
        Handled::Failed {
            reply: failure(&id, code),
        }
    }

    fn on_chunk(&mut self, offset: u64, data: &[u8]) -> Handled {
        let Some(active) = self.active.as_mut() else {
            return Handled::Nothing;
        };
        let len = data.len() as u64;
        if offset != active.written {
            return self.fail("bad_offset");
        }
        // More bytes than offered: cut the transfer and delete what was written.
        if active.written.saturating_add(len) > active.size {
            return self.fail("too_much_data");
        }
        if active.file.write_all(data).is_err() {
            return self.fail("disk_full");
        }
        active.hasher.update(data);
        active.written += len;
        Handled::Progress {
            written: active.written,
        }
    }

    fn on_done(&mut self) -> Handled {
        let Some(active) = self.active.take() else {
            return Handled::Nothing;
        };
        let Active {
            id,
            name,
            partial,
            mut file,
            size,
            written,
            hasher,
            expected,
        } = active;
        let flushed = file.flush().and_then(|()| file.sync_all()).is_ok();
        drop(file);
        let code = if written != size {
            Some("incomplete")
        } else if hasher.finalize().as_slice() != expected.as_slice() {
            Some("hash_mismatch")
        } else if !flushed {
            Some("disk_full")
        } else {
            None
        };
        if let Some(code) = code {
            remove_quietly(&partial);
            return Handled::Failed {
                reply: failure(&id, code),
            };
        }
        // The name is claimed atomically, so a concurrent transfer cannot take it and the
        // rename below only ever replaces our own empty placeholder (DAPP-105 L1).
        let Ok(target) = claim_unique_path(&self.dir, &name) else {
            remove_quietly(&partial);
            return Handled::Failed {
                reply: failure(&id, "disk_full"),
            };
        };
        if std::fs::rename(&partial, &target).is_err() {
            remove_quietly(&target);
            remove_quietly(&partial);
            return Handled::Failed {
                reply: failure(&id, "disk_full"),
            };
        }
        Handled::Saved {
            reply: Message::FileResult {
                transfer_id: id,
                ok: true,
                error: None,
            },
            path: target,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.abort();
    }
}

fn failure(id: &str, code: &str) -> Message {
    Message::FileResult {
        transfer_id: id.to_string(),
        ok: false,
        error: Some(code.to_string()),
    }
}

/// Transfer ids come from the peer; only keep characters that are safe in a file name.
fn sanitize_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(48)
        .collect()
}

fn remove_quietly(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, "cannot delete partial transfer file");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn digest(data: &[u8]) -> [u8; 32] {
        Sha256::digest(data).into()
    }

    fn chunk(id: &str, offset: u64, data: &[u8]) -> Message {
        Message::FileChunk {
            transfer_id: id.to_string(),
            offset,
            data: data.to_vec(),
        }
    }

    fn done(id: &str) -> Message {
        Message::FileDone {
            transfer_id: id.to_string(),
        }
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn error_of(handled: Handled) -> String {
        match handled {
            Handled::Failed {
                reply:
                    Message::FileResult {
                        ok: false,
                        error: Some(e),
                        ..
                    },
            } => e,
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_full_transfer_is_verified_and_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = Receiver::new(dir.path().join("in"), 1);
        let data = b"hello world";
        assert_eq!(rx.check_offer("a.txt", data.len() as u64), Ok(()));
        // Nothing exists before the user accepted.
        assert!(!dir.path().join("in").exists());
        assert_eq!(
            rx.accept("t", "a.txt", data.len() as u64, digest(data)),
            Ok(())
        );
        assert!(rx.is_busy());
        assert_eq!(
            rx.handle(&chunk("t", 0, &data[..5])),
            Handled::Progress { written: 5 }
        );
        assert_eq!(
            rx.handle(&chunk("t", 5, &data[5..])),
            Handled::Progress { written: 11 }
        );
        let Handled::Saved { reply, path } = rx.handle(&done("t")) else {
            panic!("expected Saved");
        };
        assert!(matches!(reply, Message::FileResult { ok: true, .. }));
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(listing(&dir.path().join("in")), vec!["a.txt"]);
        assert!(!rx.is_busy());
    }

    #[test]
    fn a_name_clash_gets_a_numbered_copy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let mut rx = Receiver::new(dir.path().to_path_buf(), 1);
        assert_eq!(rx.accept("t", "a.txt", 1, digest(b"x")), Ok(()));
        rx.handle(&chunk("t", 0, b"x"));
        assert!(matches!(rx.handle(&done("t")), Handled::Saved { .. }));
        assert_eq!(listing(dir.path()), vec!["a (1).txt", "a.txt"]);
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn bad_offers_are_refused_without_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("in");
        let mut rx = Receiver::new(target.clone(), 100);
        assert_eq!(
            rx.accept("t", "big.bin", 100 * MB + 1, [0; 32]),
            Err(FileRejectReason::TooLarge)
        );
        assert_eq!(rx.check_offer("big.bin", 100 * MB), Ok(()));
        for bad in [r"..\..\x.txt", "a:b", "CON", "../x"] {
            assert_eq!(
                rx.accept("t", bad, 1, [0; 32]),
                Err(FileRejectReason::Denied),
                "{bad}"
            );
        }
        assert!(!target.exists());
        assert!(!rx.is_busy());
    }

    #[test]
    fn a_second_offer_while_busy_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = Receiver::new(dir.path().to_path_buf(), 1);
        assert_eq!(rx.accept("t1", "a", 1, [0; 32]), Ok(()));
        assert_eq!(rx.check_offer("b", 1), Err(FileRejectReason::Busy));
    }

    #[test]
    fn extra_bytes_cut_the_transfer_and_delete_the_partial() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = Receiver::new(dir.path().to_path_buf(), 1);
        assert_eq!(rx.accept("t", "a.bin", 4, digest(b"abcd")), Ok(()));
        assert_eq!(
            rx.handle(&chunk("t", 0, b"abc")),
            Handled::Progress { written: 3 }
        );
        assert_eq!(error_of(rx.handle(&chunk("t", 3, b"def"))), "too_much_data");
        assert!(listing(dir.path()).is_empty(), "{:?}", listing(dir.path()));
    }

    #[test]
    fn a_wrong_digest_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = Receiver::new(dir.path().to_path_buf(), 1);
        assert_eq!(rx.accept("t", "a.bin", 3, digest(b"zzz")), Ok(()));
        rx.handle(&chunk("t", 0, b"abc"));
        assert_eq!(error_of(rx.handle(&done("t"))), "hash_mismatch");
        assert!(listing(dir.path()).is_empty());
    }

    #[test]
    fn cancel_drop_and_gaps_leave_no_partial() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = Receiver::new(dir.path().to_path_buf(), 1);
        assert_eq!(rx.accept("t", "a.bin", 10, [0; 32]), Ok(()));
        rx.handle(&chunk("t", 0, b"abc"));
        assert_eq!(listing(dir.path()).len(), 1, "partial exists mid transfer");
        rx.handle(&Message::FileCancel {
            transfer_id: "t".to_string(),
        });
        assert!(listing(dir.path()).is_empty());
        // A chunk that skips ahead is refused.
        assert_eq!(rx.accept("u", "a.bin", 10, [0; 32]), Ok(()));
        assert_eq!(error_of(rx.handle(&chunk("u", 5, b"x"))), "bad_offset");
        assert!(listing(dir.path()).is_empty());
        // Dropping a running receiver (connection lost) cleans up too.
        assert_eq!(rx.accept("v", "a.bin", 10, [0; 32]), Ok(()));
        drop(rx);
        assert!(listing(dir.path()).is_empty());
    }

    #[test]
    fn messages_of_another_transfer_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut rx = Receiver::new(dir.path().to_path_buf(), 1);
        assert_eq!(rx.accept("t", "a.bin", 3, digest(b"abc")), Ok(()));
        assert_eq!(rx.handle(&chunk("other", 0, b"abc")), Handled::Nothing);
        assert_eq!(rx.handle(&done("other")), Handled::Nothing);
        assert!(rx.is_busy());
    }
}
