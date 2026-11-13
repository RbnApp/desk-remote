//! Start/stop facade for the whole agent (independent of the Service Control Manager,
//! so `run --console` exercises exactly the code the service runs).

use crate::engine::accept_loop;
use crate::hub::Hub;
use crate::launch::LaunchMode;
use crate::supervisor::{self, SupervisorConfig};
use crate::tailnet::TailnetGuard;
use crate::tracker::SessionEvent;
use desk_config::{ConfigManager, ServiceConfig};
use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use desk_transport::{LanAnnouncer, Listener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::role::HostRole;

/// Options chosen by the binary.
pub struct HostOptions {
    pub paths: DeskPaths,
    pub mode: LaunchMode,
    /// Overrides `ServiceConfig::control_port` (0 picks a free port).
    pub port: Option<u16>,
    /// Non-canonical data root passed on to the helper (tests); `None` uses ProgramData.
    pub data_dir: Option<std::path::PathBuf>,
    /// Development/test mode: listen on 127.0.0.1 only, no mDNS announcement and no tailnet
    /// helper, so nothing is exposed to the network (and no firewall prompt appears).
    pub loopback_only: bool,
    /// Developer override (`run --console --allow-on-panel-pc`); the SCM path never sets it.
    pub allow_on_panel_pc: bool,
    /// Distinguishes the unattended agent service from the panel's in-app host.
    pub role: HostRole,
    /// Whether the interactive consent gate is enabled (farm PC agent: false, in-app host: true).
    pub consent_gate: bool,
    /// Optional custom identity (e.g. panel identity instead of system identity).
    pub custom_identity: Option<DeviceIdentity>,
    /// Optional custom trust store.
    pub custom_trust: Option<TrustStore>,
    /// Portable test mode (double-click run): the user started the agent themselves, so the
    /// helper's tray menu offers "Quit". Never set for the SYSTEM service.
    pub portable: bool,
    /// The agent checks GitHub Releases and installs newer versions itself. On for the
    /// installed service; the console mode opts in with `--self-update` (tests).
    pub self_update: bool,
}

impl HostOptions {
    /// Defaults for the background agent service / console agent.
    #[must_use]
    pub fn for_agent(paths: DeskPaths, mode: LaunchMode) -> Self {
        Self {
            paths,
            mode,
            port: None,
            data_dir: None,
            loopback_only: false,
            allow_on_panel_pc: false,
            role: HostRole::AgentService,
            consent_gate: false,
            custom_identity: None,
            custom_trust: None,
            portable: false,
            self_update: false,
        }
    }

    /// Defaults for the panel's attended in-app host.
    #[must_use]
    pub fn for_panel_in_app(paths: DeskPaths, port: Option<u16>, loopback_only: bool) -> Self {
        Self {
            paths,
            mode: LaunchMode::CurrentUser,
            port,
            data_dir: None,
            loopback_only,
            allow_on_panel_pc: true,
            role: HostRole::PanelInApp,
            consent_gate: true,
            custom_identity: None,
            custom_trust: None,
            portable: false,
            self_update: false,
        }
    }
}

/// A running agent.
pub struct ServiceHost {
    stop: Arc<AtomicBool>,
    listener: Arc<Listener>,
    hub: Arc<Hub>,
    events: Sender<SessionEvent>,
    threads: Vec<JoinHandle<()>>,
    _announcer: Option<LanAnnouncer>,
    _tailnet: Option<TailnetGuard>,
    paths: DeskPaths,
    portable: bool,
}

impl ServiceHost {
    /// Loads identity and trust, binds the viewer port and starts the worker threads.
    pub fn start(opts: HostOptions) -> DeskResult<Self> {
        let paths = opts.paths;
        if !crate::role::host_allowed_for_role(&paths, opts.role, opts.allow_on_panel_pc) {
            return Err(DeskError::Service(
                "this PC is a management panel; the PC agent host does not start here".to_string(),
            ));
        }
        let svc: ServiceConfig =
            ConfigManager::load_or_default(&paths.service_config_path()).unwrap_or_default();
        let identity = match opts.custom_identity {
            Some(id) => id,
            None => {
                DeskPaths::ensure_dir(paths.system_dir())?;
                DeviceIdentity::load_or_create(&paths.identity_key_path())?
            }
        };
        let trust = match opts.custom_trust {
            Some(t) => t,
            None => TrustStore::open(&paths.trusted_peers_path())?,
        };
        let hub = Arc::new(Hub::new_with_options(
            identity,
            PairingManager::with_trust(trust),
            opts.consent_gate,
        ));

        hub.set_announce_store(paths.service_config_path());
        hub.transfers()
            .set_max_upload_mb(desk_config::clamp_upload_mb(svc.max_upload_mb));
        hub.transfers().set_received_dir(paths.received_files_dir());

        let port = opts.port.unwrap_or(svc.control_port);
        let listener = Arc::new(if opts.loopback_only {
            Listener::bind_loopback(port)?
        } else {
            Listener::bind_any(port)?
        });
        let port = listener.local_port()?;
        hub.control().configure(port, opts.loopback_only);
        tracing::info!(port, device = %hub.identity().device_id(), "desk service listening");

        let announcer = if svc.lan_announce && !opts.loopback_only {
            match LanAnnouncer::start_auto(&hub.identity().device_id(), port, desk_version::CURRENT)
            {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!(error = %e, "LAN announcement unavailable");
                    None
                }
            }
        } else {
            None
        };
        // The tray learns of a sign-in request or a helper crash at once, not on its next click.
        let weak_hub = Arc::downgrade(&hub);
        hub.control().on_tailnet_change(move || {
            if let Some(hub) = weak_hub.upgrade() {
                hub.push_agent_info();
            }
        });
        let tailnet = if svc.tailnet_helper
            && !opts.loopback_only
            && opts.role == crate::role::HostRole::AgentService
        {
            TailnetGuard::start(
                &paths,
                port,
                svc.take_over_official_tailscale,
                opts.mode == LaunchMode::SystemService,
                Arc::clone(hub.control()),
            )
        } else {
            None
        };

        let stop = Arc::new(AtomicBool::new(false));
        let (events, event_rx) = channel();
        let mut threads = Vec::new();

        let (l, h, s) = (Arc::clone(&listener), Arc::clone(&hub), Arc::clone(&stop));
        threads.push(spawn("desk-accept", move || accept_loop(&l, &h, &s))?);

        let sup_cfg = SupervisorConfig {
            mode: opts.mode,
            helper_exe: DeskPaths::current_exe()?,
            helper_args: std::iter::once(desk_paths::HELPER_SUBCOMMAND.to_string())
                .chain(
                    opts.data_dir
                        .iter()
                        .flat_map(|d| ["--data-dir".to_string(), d.display().to_string()]),
                )
                .chain(opts.portable.then(|| "--portable".to_string()))
                .collect(),
        };
        if opts.self_update && opts.role == HostRole::AgentService {
            threads.push(crate::self_update::start(
                &hub,
                &paths,
                svc.update_feed_url.clone(),
                opts.mode,
                &stop,
            )?);
        }
        let (h, s) = (Arc::clone(&hub), Arc::clone(&stop));
        threads.push(spawn("desk-supervisor", move || {
            supervisor::run(&sup_cfg, &h, &event_rx, &s);
        })?);

        Ok(Self {
            stop,
            listener,
            hub,
            events,
            threads,
            _announcer: announcer,
            _tailnet: tailnet,
            paths,
            portable: opts.portable,
        })
    }

    /// Port viewers connect to.
    pub fn port(&self) -> DeskResult<u16> {
        self.listener.local_port()
    }

    /// Forwards an OS session notification to the supervisor.
    pub fn session_event(&self, event: SessionEvent) {
        let _ = self.events.send(event);
    }

    /// Shared state (tests, diagnostics).
    #[must_use]
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Stops all threads and the helper.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.listener.wake();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if self.portable {
            let _ = crate::tailnet::restore_official(&self.paths);
        }
        tracing::info!("desk service stopped");
    }
}

impl Drop for ServiceHost {
    fn drop(&mut self) {
        if self.portable {
            let _ = crate::tailnet::restore_official(&self.paths);
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> DeskResult<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .map_err(|e| DeskError::Service(format!("cannot start thread {name}: {e}")))
}

static CONSOLE_STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn console_ctrl(_kind: u32) -> windows::core::BOOL {
    CONSOLE_STOP.store(true, Ordering::SeqCst);
    windows::core::BOOL(1)
}

/// Name of the per-session stop event used by console (test) mode. The binary has no console
/// window, so `desk-service stop` signals this event instead of Ctrl+C.
const STOP_EVENT: windows::core::PCWSTR = windows::core::w!(r"Local\DeskRemote-ConsoleStop");

/// Whether a `run --console` agent of this user session is already running (single-instance
/// check for the portable double-click mode).
#[must_use]
pub fn console_agent_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenEventW, SYNCHRONIZATION_SYNCHRONIZE};
    // SAFETY: plain Win32 calls on a handle owned by this function.
    unsafe {
        match OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, STOP_EVENT) {
            Ok(event) => {
                let _ = CloseHandle(event);
                true
            }
            Err(_) => false,
        }
    }
}

/// Asks a running `run --console` instance of the same user session to stop. Returns `false`
/// when none is running.
#[must_use]
pub fn request_console_stop() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    // SAFETY: plain Win32 calls on a handle owned by this function.
    unsafe {
        let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, STOP_EVENT) else {
            return false;
        };
        let ok = SetEvent(event).is_ok();
        let _ = CloseHandle(event);
        ok
    }
}

/// Blocks until Ctrl+C / console close / [`request_console_stop`], or until `max` elapsed
/// (console mode only).
pub fn wait_for_console_stop(max: Option<std::time::Duration>) {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateEventW, OpenEventW, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE,
    };
    // SAFETY: the handler only touches an atomic flag.
    let _ =
        unsafe { windows::Win32::System::Console::SetConsoleCtrlHandler(Some(console_ctrl), true) };
    // SAFETY: the event handle is closed before returning.
    let security = crate::elevate::StopEventSecurity::new().ok();
    let event = unsafe {
        CreateEventW(
            security.as_ref().map(|s| s.attributes()),
            true,
            false,
            STOP_EVENT,
        )
    }
    .ok()
    .or_else(|| unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, STOP_EVENT).ok() });
    let started = std::time::Instant::now();
    while !CONSOLE_STOP.load(Ordering::SeqCst) {
        if max.is_some_and(|m| started.elapsed() >= m) {
            break;
        }
        match event {
            // SAFETY: valid event handle created above.
            Some(e) if unsafe { WaitForSingleObject(e, 100) } == WAIT_OBJECT_0 => break,
            Some(_) => {}
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    if let Some(e) = event {
        // SAFETY: closing the handle created above.
        let _ = unsafe { CloseHandle(e) };
    }
}
