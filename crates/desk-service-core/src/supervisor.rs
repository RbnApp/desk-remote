//! Keeps one session helper alive for the console session and feeds its IPC into the hub.

use crate::hub::Hub;
use crate::launch::{active_console_session, launch_helper, HelperProcess, LaunchMode};
use crate::tracker::{Backoff, SessionEvent, SessionTracker};
use desk_ipc::{pipe_access_sddl, IpcMessage, IpcServer};
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
    let expected_pid = process.pid();
    std::thread::Builder::new()
        .name("desk-helper-ipc".to_string())
        .spawn(move || helper_link(&server, &hub, &stop, expected_pid))
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

/// Accepts the helper connection (verifying who connected) and pumps its messages.
fn helper_link(server: &IpcServer, hub: &Arc<Hub>, stop: &Arc<AtomicBool>, expected_pid: u32) {
    let conn = loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match server.accept(Duration::from_millis(500)) {
            Ok(Some(c)) => match c.client_process_id() {
                Ok(pid) if pid == expected_pid => break c,
                other => {
                    tracing::warn!(?other, expected_pid, "rejected unexpected IPC client");
                    c.close();
                }
            },
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
