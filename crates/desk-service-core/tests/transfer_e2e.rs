//! End-to-end file transfer: a real panel-style host (consent gate on) on 127.0.0.1 and the
//! real sender from `desk-transfer` over an authenticated Noise session.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_error::DeskResult;
use desk_paths::DeskPaths;
use desk_protocol::{FileRejectReason, Message};
use desk_service_core::{HostOptions, ServiceHost, TransferEvent};
use desk_transfer::{new_transfer_id, prepare, send_prepared, SendError, MB};
use desk_transport::{connect, PeerTarget, Session};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Rig {
    host: ServiceHost,
    port: u16,
    inbox: PathBuf,
    offers: Arc<Mutex<Vec<TransferEvent>>>,
    work: tempfile::TempDir,
}

impl Rig {
    /// `answer`: what the (simulated) user clicks when an offer is shown; `None` = never.
    fn start(limit_mb: u32, answer: Option<bool>) -> DeskResult<Self> {
        let work = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
        let paths = DeskPaths::custom(work.path().join("system"), work.path().join("user"));
        paths.mark_panel_pc()?;
        let mut opts = HostOptions::for_panel_in_app(paths.clone(), Some(0), true);
        opts.custom_identity = Some(DeviceIdentity::generate()?);
        opts.custom_trust = Some(TrustStore::open(&paths.trusted_hosts_path())?);
        let host = ServiceHost::start(opts)?;
        let port = host.port()?;
        let offers = Arc::new(Mutex::new(Vec::new()));
        let state = host.hub().transfers();
        state.set_max_upload_mb(limit_mb);
        let (hub_offers, hub) = (Arc::clone(&offers), Arc::clone(host.hub()));
        state.set_listener(Some(Arc::new(move |event| {
            let TransferEvent::Offer { transfer_id, .. } = &event;
            let id = transfer_id.clone();
            hub_offers.lock().unwrap().push(event);
            if let Some(accept) = answer {
                let hub = Arc::clone(&hub);
                std::thread::spawn(move || {
                    // The listener runs before the host starts waiting; retry until it does.
                    for _ in 0..50 {
                        if hub.transfers().answer(&id, accept) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                });
            }
        })));
        Ok(Self {
            inbox: paths.received_files_dir(),
            host,
            port,
            offers,
            work,
        })
    }

    fn client(&self) -> DeskResult<Session> {
        let code = self
            .host
            .hub()
            .with_pairing(PairingManager::create_pairing_code)?;
        let mut trust = TrustStore::open(&self.work.path().join("client-trust.json"))?;
        let mut session = connect(PeerTarget::loopback(self.port))?;
        session.authenticate_as_client(&DeviceIdentity::generate()?, &mut trust, Some(&code))?;
        Ok(session)
    }

    fn file(&self, name: &str, data: &[u8]) -> PathBuf {
        let dir = self.work.path().join("src");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    /// Sends `path` with the real sender; `sender_limit_mb` is the panel-side limit.
    fn send(&self, path: &Path, sender_limit_mb: u32) -> Result<(), SendError> {
        let never = AtomicBool::new(false);
        let file = prepare(path, sender_limit_mb, new_transfer_id(), &never)?;
        let mut session = self
            .client()
            .map_err(|e| SendError::Network(e.to_string()))?;
        send_prepared(&mut session, &file, &never, |_| {})
    }

    fn inbox_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.inbox)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn sha(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Raw offer by a hand-written sender (to exercise hostile behaviour).
fn raw_offer(session: &mut Session, id: &str, name: &str, size: u64, hash: [u8; 32]) -> Message {
    session
        .send(&Message::FileOffer {
            transfer_id: id.to_string(),
            name: name.to_string(),
            size,
            sha256: hash,
        })
        .unwrap();
    loop {
        let msg = session.recv_timeout(Duration::from_secs(5)).unwrap();
        match msg {
            Some(m) if m.transfer_id() == Some(id) => return m,
            Some(_) => {}
            None => panic!("no answer to the offer"),
        }
    }
}

#[test]
fn files_arrive_with_matching_sha_across_sizes() {
    let rig = Rig::start(100, Some(true)).unwrap();
    // 1 KB, a size that is not a multiple of the chunk size, and several chunks.
    for (name, len) in [
        ("one.bin", 1024usize),
        ("odd.bin", 600_000),
        ("big.bin", 5 * MB as usize + 17),
    ] {
        let data = pattern(len);
        rig.send(&rig.file(name, &data), 100).unwrap();
        assert_eq!(
            sha(&std::fs::read(rig.inbox.join(name)).unwrap()),
            sha(&data),
            "{name}"
        );
    }
    assert_eq!(rig.inbox_names(), vec!["big.bin", "odd.bin", "one.bin"]);
    // The same name again is stored next to the first copy.
    rig.send(&rig.file("one.bin", b"second"), 100).unwrap();
    assert_eq!(
        rig.inbox_names(),
        vec!["big.bin", "odd.bin", "one (1).bin", "one.bin"]
    );
    let offers = rig.offers.lock().unwrap();
    assert_eq!(offers.len(), 4);
}

#[test]
fn the_limit_boundary_holds_with_large_files() {
    let rig = Rig::start(100, Some(true)).unwrap();
    for (name, len) in [
        ("fifty.bin", 50 * MB as usize),
        ("limit.bin", 100 * MB as usize),
    ] {
        let data = pattern(len);
        rig.send(&rig.file(name, &data), 100).unwrap();
        let got = std::fs::read(rig.inbox.join(name)).unwrap();
        println!(
            "[SHA] {name} {} bytes sha256={:02x?}",
            got.len(),
            &sha(&got)[..8]
        );
        assert_eq!(sha(&got), sha(&data), "{name}");
    }
    // One byte over the limit never leaves the sender.
    let over = rig.file("over.bin", &pattern(100 * MB as usize + 1));
    assert_eq!(rig.send(&over, 100), Err(SendError::TooLarge));
    println!("[INBOX] {:?}", rig.inbox_names());
    assert_eq!(rig.inbox_names(), vec!["fifty.bin", "limit.bin"]);
}

#[test]
fn the_sender_stops_a_too_large_file_before_connecting() {
    let rig = Rig::start(100, Some(true)).unwrap();
    let path = rig.file("over.bin", &pattern(MB as usize + 1));
    assert_eq!(rig.send(&path, 1), Err(SendError::TooLarge));
    assert!(
        rig.offers.lock().unwrap().is_empty(),
        "nothing reached the host"
    );
    assert!(rig.inbox_names().is_empty());
}

#[test]
fn the_receiver_refuses_a_too_large_offer_and_writes_nothing() {
    // Sender allows 2 MB, the receiving host only 1 MB.
    let rig = Rig::start(1, Some(true)).unwrap();
    let path = rig.file("two.bin", &pattern(2 * MB as usize - 5));
    assert_eq!(
        rig.send(&path, 2),
        Err(SendError::Rejected(FileRejectReason::TooLarge))
    );
    assert!(
        rig.offers.lock().unwrap().is_empty(),
        "the user is not even asked"
    );
    assert!(!rig.inbox.exists());
}

#[test]
fn a_fake_sender_offering_more_than_the_limit_is_rejected() {
    let rig = Rig::start(100, Some(true)).unwrap();
    let mut session = rig.client().unwrap();
    let answer = raw_offer(&mut session, "fake-1", "huge.bin", 100 * MB + 1, [0; 32]);
    assert_eq!(
        answer,
        Message::FileReject {
            transfer_id: "fake-1".to_string(),
            reason: FileRejectReason::TooLarge
        }
    );
    assert!(!rig.inbox.exists());
}

#[test]
fn nothing_is_written_before_accept_and_a_refusal_writes_nothing() {
    let rig = Rig::start(100, None).unwrap();
    let mut session = rig.client().unwrap();
    session
        .send(&Message::FileOffer {
            transfer_id: "p1".to_string(),
            name: "wait.txt".to_string(),
            size: 4,
            sha256: sha(b"data"),
        })
        .unwrap();
    // The user has not answered: the offer is visible, no file or folder exists.
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(rig.offers.lock().unwrap().len(), 1);
    assert!(!rig.inbox.exists());
    // Data sent without an Accept is ignored by the host.
    session
        .send(&Message::FileChunk {
            transfer_id: "p1".to_string(),
            offset: 0,
            data: b"data".to_vec(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(!rig.inbox.exists());
    assert!(rig.host.hub().transfers().answer("p1", false));
    let verdict = loop {
        match session.recv_timeout(Duration::from_secs(5)).unwrap() {
            Some(m) if m.transfer_id() == Some("p1") => break m,
            Some(_) => {}
            None => panic!("no verdict"),
        }
    };
    assert_eq!(
        verdict,
        Message::FileReject {
            transfer_id: "p1".to_string(),
            reason: FileRejectReason::Denied
        }
    );
    assert!(!rig.inbox.exists());
}

#[test]
fn hostile_names_are_refused_without_asking_or_writing() {
    let rig = Rig::start(100, Some(true)).unwrap();
    for (i, name) in [r"..\..\x.txt", "a:b", "CON", "../x", "dir/x.txt"]
        .iter()
        .enumerate()
    {
        let mut session = rig.client().unwrap();
        let answer = raw_offer(&mut session, &format!("bad-{i}"), name, 1, sha(b"x"));
        assert_eq!(
            answer,
            Message::FileReject {
                transfer_id: format!("bad-{i}"),
                reason: FileRejectReason::Denied
            },
            "{name}"
        );
    }
    assert!(rig.offers.lock().unwrap().is_empty());
    assert!(!rig.inbox.exists());
}

#[test]
fn sending_more_bytes_than_offered_cuts_the_transfer() {
    let rig = Rig::start(100, Some(true)).unwrap();
    let mut session = rig.client().unwrap();
    let answer = raw_offer(&mut session, "over-1", "short.bin", 4, sha(b"abcd"));
    assert!(matches!(answer, Message::FileAccept { .. }));
    session
        .send(&Message::FileChunk {
            transfer_id: "over-1".to_string(),
            offset: 0,
            data: b"abcdefgh".to_vec(),
        })
        .unwrap();
    let verdict = loop {
        match session.recv_timeout(Duration::from_secs(5)).unwrap() {
            Some(m) if m.transfer_id() == Some("over-1") => break m,
            Some(_) => {}
            None => panic!("no verdict"),
        }
    };
    assert_eq!(
        verdict,
        Message::FileResult {
            transfer_id: "over-1".to_string(),
            ok: false,
            error: Some("too_much_data".to_string())
        }
    );
    assert!(rig.inbox_names().is_empty(), "{:?}", rig.inbox_names());
}

#[test]
fn a_wrong_hash_is_reported_and_leaves_no_file() {
    let rig = Rig::start(100, Some(true)).unwrap();
    let mut session = rig.client().unwrap();
    let answer = raw_offer(&mut session, "h-1", "bad.bin", 4, sha(b"zzzz"));
    assert!(matches!(answer, Message::FileAccept { .. }));
    for msg in [
        Message::FileChunk {
            transfer_id: "h-1".to_string(),
            offset: 0,
            data: b"abcd".to_vec(),
        },
        Message::FileDone {
            transfer_id: "h-1".to_string(),
        },
    ] {
        session.send(&msg).unwrap();
    }
    let verdict = loop {
        match session.recv_timeout(Duration::from_secs(5)).unwrap() {
            Some(m) if m.transfer_id() == Some("h-1") => break m,
            Some(_) => {}
            None => panic!("no verdict"),
        }
    };
    assert!(matches!(
        verdict,
        Message::FileResult { ok: false, error: Some(ref e), .. } if e == "hash_mismatch"
    ));
    assert!(rig.inbox_names().is_empty());
}

#[test]
fn cancelling_in_the_middle_leaves_no_partial_file() {
    let rig = Rig::start(100, Some(true)).unwrap();
    let data = pattern(20 * MB as usize);
    let path = rig.file("cancel.bin", &data);
    let cancel = AtomicBool::new(false);
    let file = prepare(&path, 100, new_transfer_id(), &cancel).unwrap();
    let mut session = rig.client().unwrap();
    let mut seen_partial = false;
    let result = send_prepared(&mut session, &file, &cancel, |p| {
        if p.sent >= 2 * MB && !cancel.load(Ordering::SeqCst) {
            seen_partial = rig.inbox_names().iter().any(|n| n.ends_with(".partial"));
            cancel.store(true, Ordering::SeqCst);
        }
    });
    assert_eq!(result, Err(SendError::Cancelled));
    assert!(
        seen_partial,
        "a .partial file exists while the transfer runs"
    );
    std::thread::sleep(Duration::from_millis(600));
    assert!(rig.inbox_names().is_empty(), "{:?}", rig.inbox_names());
}

#[test]
fn a_dropped_connection_removes_the_partial_file() {
    let rig = Rig::start(100, Some(true)).unwrap();
    let mut session = rig.client().unwrap();
    let answer = raw_offer(&mut session, "d-1", "gone.bin", 10, sha(b"0123456789"));
    assert!(matches!(answer, Message::FileAccept { .. }));
    session
        .send(&Message::FileChunk {
            transfer_id: "d-1".to_string(),
            offset: 0,
            data: b"0123".to_vec(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.inbox_names().len(), 1);
    drop(session);
    std::thread::sleep(Duration::from_millis(800));
    assert!(rig.inbox_names().is_empty(), "{:?}", rig.inbox_names());
}

#[test]
fn only_one_transfer_runs_at_a_time() {
    let rig = Rig::start(100, None).unwrap();
    let mut first = rig.client().unwrap();
    first
        .send(&Message::FileOffer {
            transfer_id: "one".to_string(),
            name: "a.txt".to_string(),
            size: 1,
            sha256: sha(b"a"),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(600)); // the first offer waits for its answer
    let mut second = rig.client().unwrap();
    let answer = raw_offer(&mut second, "two", "b.txt", 1, sha(b"b"));
    assert_eq!(
        answer,
        Message::FileReject {
            transfer_id: "two".to_string(),
            reason: FileRejectReason::Busy
        }
    );
}

/// Agent mode: the host has no consent gate and forwards the transfer to the session helper,
/// which writes the file. The helper end here is a pipe peer driving the real `Receiver`.
mod agent {
    use super::*;
    use desk_ipc::{pipe_access_sddl, IpcClient, IpcMessage, IpcServer};
    use desk_transfer::{Handled, Receiver};

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(900);

    struct AgentRig {
        rig: Rig,
        helper: std::thread::JoinHandle<Vec<String>>,
        stop: Arc<AtomicBool>,
        dir: PathBuf,
    }

    fn start_agent(limit_mb: u32, with_helper: bool) -> AgentRig {
        let work = tempfile::tempdir().unwrap();
        let paths = DeskPaths::custom(work.path().join("system"), work.path().join("user"));
        paths.mark_panel_pc().unwrap();
        let mut opts = HostOptions::for_panel_in_app(paths.clone(), Some(0), true);
        opts.custom_identity = Some(DeviceIdentity::generate().unwrap());
        opts.custom_trust = Some(TrustStore::open(&paths.trusted_hosts_path()).unwrap());
        opts.consent_gate = false; // unattended agent behaviour
        let host = ServiceHost::start(opts).unwrap();
        let port = host.port().unwrap();
        let dir = work.path().join("helper-inbox");
        let stop = Arc::new(AtomicBool::new(false));
        let helper = if with_helper {
            let name = format!(
                r"\\.\pipe\DeskRemote-xfer-helper-{}",
                NEXT.fetch_add(1, Ordering::SeqCst)
            );
            let server = IpcServer::bind(
                &name,
                &pipe_access_sddl(&desk_ipc::current_user_sid().unwrap()),
            )
            .unwrap();
            let connect_name = name.clone();
            let client = std::thread::spawn(move || {
                IpcClient::connect(&connect_name, Duration::from_secs(5))
            });
            let service_end = server.accept(Duration::from_secs(5)).unwrap().unwrap();
            let helper_end = client.join().unwrap().unwrap();
            host.hub().attach_helper(service_end.clone());
            // The supervisor normally pumps helper messages into the hub; do the same here.
            let (hub, pump_stop) = (Arc::clone(host.hub()), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !pump_stop.load(Ordering::SeqCst) {
                    if let Ok(Some(msg)) = service_end.recv_timeout(Duration::from_millis(50)) {
                        hub.on_helper_message(msg);
                    }
                }
            });
            let (dir, stop) = (dir.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut receiver = Receiver::new(dir, limit_mb);
                let mut announced = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    let Ok(Some(msg)) = helper_end.recv_timeout(Duration::from_millis(50)) else {
                        continue;
                    };
                    match msg {
                        IpcMessage::FileIncoming { peer_name, .. } => announced.push(peer_name),
                        IpcMessage::Wire(Message::FileOffer {
                            transfer_id,
                            name,
                            size,
                            sha256,
                        }) => {
                            let reply = match receiver.accept(&transfer_id, &name, size, sha256) {
                                Ok(()) => Message::FileAccept { transfer_id },
                                Err(reason) => Message::FileReject {
                                    transfer_id,
                                    reason,
                                },
                            };
                            helper_end.send(&IpcMessage::Wire(reply)).unwrap();
                        }
                        IpcMessage::Wire(other) => match receiver.handle(&other) {
                            Handled::Saved { reply, .. } | Handled::Failed { reply } => {
                                helper_end.send(&IpcMessage::Wire(reply)).unwrap();
                            }
                            Handled::Nothing | Handled::Progress { .. } => {}
                        },
                        _ => {}
                    }
                }
                announced
            })
        } else {
            std::thread::spawn(Vec::new)
        };
        let rig = Rig {
            inbox: dir.clone(),
            host,
            port,
            offers: Arc::new(Mutex::new(Vec::new())),
            work,
        };
        AgentRig {
            rig,
            helper,
            stop,
            dir,
        }
    }

    impl AgentRig {
        fn finish(self) -> Vec<String> {
            self.stop.store(true, Ordering::SeqCst);
            self.helper.join().unwrap()
        }
    }

    #[test]
    fn the_helper_receives_the_file_without_any_prompt() {
        let agent = start_agent(100, true);
        let data = pattern(3 * MB as usize + 5);
        agent
            .rig
            .send(&agent.rig.file("agent.bin", &data), 100)
            .unwrap();
        assert_eq!(
            sha(&std::fs::read(agent.dir.join("agent.bin")).unwrap()),
            sha(&data)
        );
        assert!(
            agent.rig.offers.lock().unwrap().is_empty(),
            "no panel prompt on an agent"
        );
        let announced = agent.finish();
        assert_eq!(announced.len(), 1, "the helper learned who sends the file");
    }

    #[test]
    fn the_helper_enforces_the_limit_and_names() {
        let agent = start_agent(1, true);
        let path = agent.rig.file("two.bin", &pattern(2 * MB as usize));
        assert_eq!(
            agent.rig.send(&path, 5),
            Err(SendError::Rejected(FileRejectReason::TooLarge))
        );
        let mut session = agent.rig.client().unwrap();
        let answer = raw_offer(&mut session, "n-1", r"..\..\x.txt", 1, sha(b"x"));
        assert!(matches!(
            answer,
            Message::FileReject {
                reason: FileRejectReason::Denied,
                ..
            }
        ));
        assert!(!agent.dir.exists());
        agent.finish();
    }

    #[test]
    fn a_cancelled_agent_transfer_leaves_no_partial() {
        let agent = start_agent(100, true);
        let path = agent.rig.file("c.bin", &pattern(20 * MB as usize));
        let cancel = AtomicBool::new(false);
        let file = prepare(&path, 100, new_transfer_id(), &cancel).unwrap();
        let mut session = agent.rig.client().unwrap();
        let result = send_prepared(&mut session, &file, &cancel, |p| {
            if p.sent >= 2 * MB {
                cancel.store(true, Ordering::SeqCst);
            }
        });
        assert_eq!(result, Err(SendError::Cancelled));
        std::thread::sleep(Duration::from_millis(800));
        let left: Vec<_> = std::fs::read_dir(&agent.dir)
            .map(|rd| rd.filter_map(Result::ok).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
        agent.finish();
    }

    #[test]
    fn without_a_signed_in_user_the_offer_gets_no_answer() {
        let agent = start_agent(100, false);
        let path = agent.rig.file("x.bin", b"x");
        assert_eq!(
            agent.rig.send(&path, 100),
            Err(SendError::Rejected(FileRejectReason::NoAnswer))
        );
        agent.finish();
    }
}
