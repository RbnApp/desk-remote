//! Keeps one session helper alive for the console session and feeds its IPC into the hub.

use crate::hub::Hub;
use crate::launch::{active_console_session, launch_helper, HelperProcess, LaunchMode};
use crate::tracker::{Backoff, SessionEvent, SessionTracker};
use desk_ipc::{pipe_access_sddl, ExpectedClient, IpcMessage, IpcServer};
use desk_paths::DeskPaths;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(250);
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// Settings for the supervisor loop.
pub struct SupervisorConfig {
    pub mode: LaunchMode,
    pub helper_exe: PathBuf,
    pub helper_args: Vec<String>,
}

struct Running {
    process: HelperProcess,
    pipe_name: String,
    started: Instant,
    reader_stop: Arc<AtomicBool>,
}

/// Runs until `stop`; `events` carries SCM session notifications.
pub fn run(
    cfg: &SupervisorConfig,
    hub: &Arc<Hub>,
    events: &Receiver<SessionEvent>,
    stop: &Arc<AtomicBool>,
) {
    let mut tracker = SessionTracker::with_console(active_console_session());
    let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(5));
    let mut running: Option<Running> = None;
    let mut next_attempt = Instant::now();
    hub.set_session_locked(tracker.locked());

    while !stop.load(Ordering::SeqCst) {
        let mut changed = false;
        while let Ok(ev) = events.try_recv() {
            tracing::info!(?ev, "session event");
            changed |= tracker.apply(ev);
        }
        // The console may also move without a notification we saw; ask the OS.
        if cfg.mode == LaunchMode::SystemService {
            let os_console = active_console_session();
            if tracker.target() != Some(os_console) && os_console != crate::tracker::NO_SESSION {
                changed |= tracker.apply(SessionEvent::ConsoleConnect(os_console));
            }
        }
        hub.set_session_locked(tracker.locked());

        let wanted = match cfg.mode {
            LaunchMode::CurrentUser => Some(crate::launch::current_process_session()),
            LaunchMode::SystemService => tracker.target(),
        };

        if let Some(run) = running.as_mut() {
            let wrong_session = wanted != Some(run.process.session_id());
            if wrong_session || !run.process.is_running() {
                let crashed = !wrong_session;
                tracing::warn!(crashed, "stopping session helper");
                stop_running(running.take());
                if crashed {
                    let delay = backoff.next_delay();
                    next_attempt = Instant::now() + delay;
                } else {
                    // Session change: restart at once and forget old crash history.
                    backoff.reset();
                    next_attempt = Instant::now();
                }
            } else if run.started.elapsed() > STABLE_AFTER {
                backoff.reset();
            }
        }
        let _ = changed;

        if running.is_none() && Instant::now() >= next_attempt {
            if let Some(session) = wanted {
                match start(cfg, hub, session) {
                    Ok(r) => running = Some(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot start session helper");
                        next_attempt = Instant::now() + backoff.next_delay();
                    }
                }
            }
        }
        std::thread::sleep(TICK);
    }
    stop_running(running.take());
}

fn start(cfg: &SupervisorConfig, hub: &Arc<Hub>, session: u32) -> desk_error::DeskResult<Running> {
    let pipe_name = DeskPaths::pipe_session_helper(session);
    // The pipe must exist (with its DACL) before the helper starts, never the reverse.
    let user_sid = match cfg.mode {
        LaunchMode::CurrentUser => desk_ipc::current_user_sid()?,
        LaunchMode::SystemService => session_user_sid(session)?,
    };
    let server = IpcServer::bind(&pipe_name, &pipe_access_sddl(&user_sid))?;
    let process = launch_helper(cfg.mode, &cfg.helper_exe, &cfg.helper_args, session)?;
    tracing::info!(pid = process.pid(), session, "session helper started");
    let reader_stop = Arc::new(AtomicBool::new(false));
    let hub = Arc::clone(hub);
    let stop = Arc::clone(&reader_stop);
    let expected = ExpectedHelper {
        pid: process.pid(),
        image: cfg.helper_exe.clone(),
        user_sid,
    };
    std::thread::Builder::new()
        .name("desk-helper-ipc".to_string())
        .spawn(move || helper_link(&server, &hub, &stop, &expected))
        .map_err(|e| desk_error::DeskError::Service(format!("ipc thread: {e}")))?;
    Ok(Running {
        process,
        pipe_name,
        started: Instant::now(),
        reader_stop,
    })
}

fn session_user_sid(session: u32) -> desk_error::DeskResult<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
    let mut token = HANDLE::default();
    // SAFETY: valid out-parameter; requires SYSTEM.
    unsafe { WTSQueryUserToken(session, &mut token) }.map_err(|e| {
        desk_error::DeskError::Service(format!("no user in session {session}: {e}"))
    })?;
    let sid = desk_ipc::user_sid_of_token(token);
    // SAFETY: token owned here.
    let _ = unsafe { CloseHandle(token) };
    sid
}

/// Who the one accepted pipe client must be: the helper this supervisor just started.
struct ExpectedHelper {
    pid: u32,
    image: PathBuf,
    user_sid: String,
}

/// Accepts the helper connection (verifying who connected) and pumps its messages. Nothing is
/// sent to a client before it passed [`desk_ipc::IpcConnection::verify_client`], so a
/// look-alike process of the same user never sees the pairing code (DAPP-103 H-04 / M-10).
fn helper_link(
    server: &IpcServer,
    hub: &Arc<Hub>,
    stop: &Arc<AtomicBool>,
    expected: &ExpectedHelper,
) {
    let conn = loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match server.accept(Duration::from_millis(500)) {
            Ok(Some(c)) => {
                let verdict = c.verify_client(&ExpectedClient {
                    pid: expected.pid,
                    image: &expected.image,
                    user_sid: &expected.user_sid,
                });
                match verdict {
                    Ok(()) => break c,
                    Err(e) => {
                        tracing::warn!(error = %e, expected_pid = expected.pid, "rejected unexpected IPC client");
                        c.close();
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "ipc accept failed");
                return;
            }
        }
    };
    let token = hub.attach_helper(conn.clone());
    while !stop.load(Ordering::SeqCst) {
        match conn.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(IpcMessage::Shutdown)) => break,
            Ok(Some(msg)) => hub.on_helper_message(msg),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    hub.detach_helper(token);
    conn.close();
}

fn stop_running(running: Option<Running>) {
    if let Some(mut run) = running {
        run.reader_stop.store(true, Ordering::SeqCst);
        let _ = run.pipe_name;
        if run.process.is_running() {
            run.process.terminate();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use desk_crypto::{DeviceIdentity, PairingManager};
    use std::sync::mpsc::channel;
    use std::sync::Mutex;

    /// Both tests bind the same per-session pipe name, so they must not overlap.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// Runs the real supervisor loop (current-user mode) for `run_for` with a stand-in helper
    /// that appends one line to a marker file per start, then runs `tail`. Returns the starts.
    fn starts_within(tail: &str, run_for: Duration) -> usize {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
        let work = tempfile::tempdir().unwrap();
        let marker = work.path().join("starts.txt");
        // A script avoids argument-quoting differences between Rust and cmd.exe.
        let script = work.path().join("helper.cmd");
        std::fs::write(
            &script,
            format!(
                "@echo off\r\necho x>>\"{}\"\r\n{tail}\r\n",
                marker.display()
            ),
        )
        .unwrap();
        let cfg = SupervisorConfig {
            mode: LaunchMode::CurrentUser,
            helper_exe: script,
            helper_args: Vec::new(),
        };
        let hub = Arc::new(Hub::new(
            DeviceIdentity::generate().unwrap(),
            PairingManager::new(),
        ));
        let (_tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || run(&cfg, &hub, &rx, &stop))
        };
        std::thread::sleep(run_for);
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
        std::fs::read_to_string(&marker).map_or(0, |t| t.lines().count())
    }

    #[test]
    fn a_helper_that_keeps_running_is_not_started_again() {
        // The "tray closed by the user" case: the helper lives on (only its icon is gone).
        assert_eq!(
            starts_within("ping -n 8 127.0.0.1 >nul", Duration::from_secs(4)),
            1
        );
    }

    #[test]
    fn a_helper_that_exits_unexpectedly_is_restarted_with_backoff() {
        let starts = starts_within("exit 1", Duration::from_secs(4));
        assert!(
            starts >= 2,
            "crashed helper must be restarted, starts={starts}"
        );
        assert!(starts <= 6, "restarts must back off, starts={starts}");
    }
}
