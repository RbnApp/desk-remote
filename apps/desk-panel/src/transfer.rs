//! Sending files from the panel to a paired PC.
//!
//! Every file is one transfer: `transfer_send` returns the ids at once and the work runs on a
//! thread per file. Transfers to the same PC run one after another (the receiver accepts one
//! at a time); different PCs run in parallel. Progress and the verdict reach the UI as the
//! `transfer://progress` and `transfer://done` events.

use crate::backend::{Backend, PairError};
use desk_config::PairedPc;
use desk_transfer::{new_transfer_id, prepare, send_prepared, SendError};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Progress events are throttled to this interval (the last one is always sent).
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Delivers `(event name, payload)` to the UI.
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// One row of `transfer_inspect`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Name, size and kind of each path; paths that cannot be read are left out.
pub fn inspect(paths: &[String]) -> Vec<FileInfo> {
    paths
        .iter()
        .filter_map(|p| {
            let meta = std::fs::metadata(p).ok()?;
            let name = Path::new(p).file_name()?.to_string_lossy().into_owned();
            Some(FileInfo {
                path: p.clone(),
                name,
                size: if meta.is_dir() { 0 } else { meta.len() },
                is_dir: meta.is_dir(),
            })
        })
        .collect()
}

/// Running sends: a queue lock per PC and a cancel flag per transfer.
#[derive(Default)]
pub struct TransferManager {
    queues: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl TransferManager {
    /// Starts one transfer per path towards `pc` and returns their ids in path order.
    pub fn send(
        self: &Arc<Self>,
        backend: &Arc<dyn Backend>,
        pc: &PairedPc,
        paths: Vec<String>,
        max_upload_mb: u32,
        emit: &Emit,
    ) -> Vec<String> {
        let queue = Arc::clone(lock(&self.queues).entry(pc.id.clone()).or_default());
        let mut ids = Vec::with_capacity(paths.len());
        for path in paths {
            let id = new_transfer_id();
            let cancel = Arc::new(AtomicBool::new(false));
            lock(&self.cancels).insert(id.clone(), Arc::clone(&cancel));
            ids.push(id.clone());
            let job = Job {
                id,
                path,
                pc: pc.clone(),
                max_upload_mb,
                cancel,
                backend: Arc::clone(backend),
                emit: Arc::clone(emit),
            };
            let (queue, manager) = (Arc::clone(&queue), Arc::clone(self));
            let spawned = std::thread::Builder::new()
                .name("file-send".to_string())
                .spawn(move || {
                    let _turn = lock(&queue);
                    let id = job.id.clone();
                    job.run();
                    lock(&manager.cancels).remove(&id);
                });
            if let Err(e) = spawned {
                tracing::error!(error = %e, "cannot start the file sender");
            }
        }
        ids
    }

    /// Stops a queued or running transfer; unknown ids are ignored.
    pub fn cancel(&self, transfer_id: &str) {
        if let Some(flag) = lock(&self.cancels).get(transfer_id) {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

struct Job {
    id: String,
    path: String,
    pc: PairedPc,
    max_upload_mb: u32,
    cancel: Arc<AtomicBool>,
    backend: Arc<dyn Backend>,
    emit: Emit,
}

impl Job {
    fn run(self) {
        let name = Path::new(&self.path)
            .file_name()
            .map_or_else(|| self.path.clone(), |n| n.to_string_lossy().into_owned());
        let result = self.transfer(&name);
        let payload = match result {
            Ok(()) => json!({"transfer_id": self.id, "name": name, "ok": true}),
            Err(code) => {
                json!({"transfer_id": self.id, "name": name, "ok": false, "error": code})
            }
        };
        (self.emit)("transfer://done", payload);
    }

    /// The error value is the stable code shown in the UI.
    fn transfer(&self, name: &str) -> Result<(), String> {
        let file = prepare(
            Path::new(&self.path),
            self.max_upload_mb,
            self.id.clone(),
            &self.cancel,
        )
        .map_err(|e| e.code())?;
        let mut session = self
            .backend
            .open_session(&self.pc)
            .map_err(|e: PairError| e.code().to_string())?;
        let total = file.size;
        let progress = |sent: u64| {
            (self.emit)(
                "transfer://progress",
                json!({"transfer_id": self.id, "name": name, "sent": sent, "total": total}),
            );
        };
        progress(0);
        let mut last = Instant::now();
        send_prepared(&mut session, &file, &self.cancel, |p| {
            if p.sent >= total || last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                progress(p.sent);
            }
        })
        .map_err(|e: SendError| e.code())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn inspect_reports_files_and_folders_and_skips_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        std::fs::write(&file, vec![0u8; 1234]).unwrap();
        let rows = inspect(&[
            file.to_string_lossy().into_owned(),
            dir.path().to_string_lossy().into_owned(),
            dir.path().join("gone").to_string_lossy().into_owned(),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].name.as_str(), rows[0].size, rows[0].is_dir),
            ("a.bin", 1234, false)
        );
        assert!(rows[1].is_dir);
    }
}
