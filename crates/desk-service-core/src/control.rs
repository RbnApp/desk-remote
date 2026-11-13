//! State behind the tray menu: the "watch only" switch, the listening port, the tailnet
//! report and the [`AgentInfo`] snapshot the session helper renders.
//!
//! The network is queried here (service side) and nowhere else: the helper only receives
//! the finished snapshot over IPC.

use desk_crypto::DeviceIdentity;
use desk_ipc::{AgentInfo, AgentSettings, TailnetState};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Address advertised when the listener is bound to loopback only.
const LOOPBACK_IP: &str = "127.0.0.1";

#[derive(Default)]
struct TailnetReport {
    state: TailnetState,
    ips: Vec<String>,
    auth_url: Option<String>,
    takeover_needs_admin: bool,
}

/// Outcome of the latest update check, for the Settings window.
#[derive(Default)]
struct UpdateReport {
    latest: String,
    checked_at: u64,
}

/// Name used when the user set none and Windows reports none.
const FALLBACK_NAME: &str = "Desk-PC";

/// Runtime switches and facts shared by the hub, the host and the tailnet supervisor.
#[derive(Default)]
pub struct Control {
    view_only: AtomicBool,
    port: AtomicU16,
    loopback_only: AtomicBool,
    tailnet: Mutex<TailnetReport>,
    /// Name the user gave this PC; empty means the Windows computer name.
    device_name: Mutex<String>,
    /// Inverted on purpose: the default (`false`) is today's behavior, updates on.
    manual_updates_only: AtomicBool,
    /// This process is the installed Windows service (the start type can be changed).
    service_mode: AtomicBool,
    update: Mutex<UpdateReport>,
    /// Called (outside the lock) when the tailnet state changes, so the tray learns at once.
    on_tailnet_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Control {
    /// Records what the listener really bound (called once after binding).
    pub fn configure(&self, port: u16, loopback_only: bool) {
        self.port.store(port, Ordering::SeqCst);
        self.loopback_only.store(loopback_only, Ordering::SeqCst);
    }

    /// The name announced to panels and shown in the tray: the user's choice, else the Windows
    /// computer name.
    #[must_use]
    pub fn device_name(&self) -> String {
        let custom = lock(&self.device_name).clone();
        if !custom.is_empty() {
            return custom;
        }
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| FALLBACK_NAME.to_string())
    }

    /// Stores the name the user chose (`""` goes back to the computer name).
    pub fn set_device_name(&self, name: &str) {
        *lock(&self.device_name) = name.to_string();
    }

    /// Whether the agent looks for and installs updates by itself.
    #[must_use]
    pub fn auto_update(&self) -> bool {
        !self.manual_updates_only.load(Ordering::SeqCst)
    }

    pub fn set_auto_update(&self, enabled: bool) {
        self.manual_updates_only.store(!enabled, Ordering::SeqCst);
    }

    /// Marks this process as the installed Windows service.
    pub fn set_service_mode(&self, service: bool) {
        self.service_mode.store(service, Ordering::SeqCst);
    }

    #[must_use]
    pub fn service_mode(&self) -> bool {
        self.service_mode.load(Ordering::SeqCst)
    }

    /// Remembers that an update check just finished and which version is the newest.
    pub fn record_update_check(&self, latest: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut report = lock(&self.update);
        report.latest = latest.to_string();
        report.checked_at = now;
    }

    /// True while remote input is blocked. Always `false` after a service restart.
    #[must_use]
    pub fn view_only(&self) -> bool {
        self.view_only.load(Ordering::SeqCst)
    }

    /// Switches watch-only mode; returns whether the value changed.
    pub fn set_view_only(&self, enabled: bool) -> bool {
        self.view_only.swap(enabled, Ordering::SeqCst) != enabled
    }

    /// Stores the tailnet helper's report (`status` as printed by the helper).
    pub fn set_tailnet_report(&self, status: &str, ips: &[String], auth_url: Option<&str>) {
        let state = match status {
            "listening" | "running" if !ips.is_empty() => TailnetState::Connected,
            "needs_auth" => TailnetState::NeedsLogin,
            "error" => TailnetState::Failed,
            _ => TailnetState::Off,
        };
        let changed = {
            let mut report = lock(&self.tailnet);
            let changed = report.state != state || report.auth_url.as_deref() != auth_url;
            Self::store_report(&mut report, state, ips, auth_url);
            changed
        };
        if changed {
            let hook = lock(&self.on_tailnet_change).clone();
            if let Some(hook) = hook {
                hook();
            }
        }
    }

    /// Registers what runs when the tailnet state changes (the hub pushes a fresh snapshot).
    pub fn on_tailnet_change(&self, hook: impl Fn() + Send + Sync + 'static) {
        *lock(&self.on_tailnet_change) = Some(Arc::new(hook));
    }

    fn store_report(
        report: &mut TailnetReport,
        state: TailnetState,
        ips: &[String],
        auth_url: Option<&str>,
    ) {
        report.state = state;
        report.ips = if state == TailnetState::Connected {
            ips.to_vec()
        } else {
            Vec::new()
        };
        report.auth_url = auth_url.map(ToString::to_string);
    }

    /// Records whether taking over official Tailscale requires administrator privileges.
    pub fn set_takeover_needs_admin(&self, needs_admin: bool) {
        let mut report = lock(&self.tailnet);
        report.takeover_needs_admin = needs_admin;
    }

    /// The values this control owns; the hub adds the ones it owns (lock rule, upload limit,
    /// service start type).
    fn settings(&self) -> AgentSettings {
        let report = lock(&self.update);
        AgentSettings {
            device_name: lock(&self.device_name).clone(),
            auto_update: self.auto_update(),
            latest_version: report.latest.clone(),
            last_update_check: report.checked_at,
            ..AgentSettings::default()
        }
    }

    /// Builds the snapshot for the tray menu.
    #[must_use]
    pub fn snapshot(
        &self,
        identity: &DeviceIdentity,
        viewers: usize,
        unavailable: bool,
        paired: Vec<desk_ipc::PairedPeerInfo>,
    ) -> AgentInfo {
        let loopback = self.loopback_only.load(Ordering::SeqCst);
        let report = lock(&self.tailnet);
        AgentInfo {
            pc_name: self.device_name(),
            local_ip: if loopback {
                Some(LOOPBACK_IP.to_string())
            } else {
                desk_transport::primary_local_ip().map(|ip| ip.to_string())
            },
            port: self.port.load(Ordering::SeqCst),
            tailnet: report.state,
            tailnet_ips: report.ips.clone(),
            tailnet_auth_url: report.auth_url.clone(),
            takeover_needs_admin: report.takeover_needs_admin,
            device_id: identity.device_id(),
            fingerprint: identity.fingerprint(),
            viewers: u32::try_from(viewers).unwrap_or(u32::MAX),
            unavailable,
            view_only: self.view_only(),
            paired,
            settings: self.settings(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desk_error::DeskResult;

    #[test]
    fn device_name_is_the_users_choice_else_the_computer_name() {
        let control = Control::default();
        let default_name = control.device_name();
        assert!(!default_name.is_empty());
        control.set_device_name("Farm PC");
        assert_eq!(control.device_name(), "Farm PC");
        control.set_device_name("");
        assert_eq!(control.device_name(), default_name);
    }

    #[test]
    fn auto_update_is_on_until_switched_off() {
        let control = Control::default();
        assert!(control.auto_update());
        control.set_auto_update(false);
        assert!(!control.auto_update());
        control.record_update_check("0.1.3");
        let report = lock(&control.update);
        assert_eq!(report.latest, "0.1.3");
        assert!(report.checked_at > 0);
    }

    #[test]
    fn view_only_reports_changes_and_starts_off() {
        let control = Control::default();
        assert!(!control.view_only());
        assert!(control.set_view_only(true));
        assert!(!control.set_view_only(true), "no change the second time");
        assert!(control.view_only());
    }

    #[test]
    fn tailnet_report_maps_helper_status_to_menu_state() -> DeskResult<()> {
        let control = Control::default();
        let identity = DeviceIdentity::generate()?;
        let ips = vec!["100.64.0.7".to_string()];
        let state = |c: &Control| c.snapshot(&identity, 0, false, Vec::new()).tailnet;
        assert_eq!(state(&control), TailnetState::Off, "nothing reported yet");
        control.set_tailnet_report("listening", &ips, None);
        assert_eq!(state(&control), TailnetState::Connected);
        assert_eq!(
            control
                .snapshot(&identity, 0, false, Vec::new())
                .tailnet_ips,
            ips
        );
        control.set_tailnet_report("needs_auth", &[], Some("https://login.example.com"));
        assert_eq!(state(&control), TailnetState::NeedsLogin);
        assert_eq!(
            control
                .snapshot(&identity, 0, false, Vec::new())
                .tailnet_auth_url
                .as_deref(),
            Some("https://login.example.com")
        );
        assert!(control
            .snapshot(&identity, 0, false, Vec::new())
            .tailnet_ips
            .is_empty());
        control.set_tailnet_report("error", &ips, None);
        assert_eq!(state(&control), TailnetState::Failed);
        control.set_tailnet_report("starting", &ips, None);
        assert_eq!(state(&control), TailnetState::Off);
        Ok(())
    }

    #[test]
    fn loopback_mode_advertises_loopback_with_the_bound_port() -> DeskResult<()> {
        let control = Control::default();
        control.configure(51234, true);
        let info = control.snapshot(&DeviceIdentity::generate()?, 3, true, Vec::new());
        assert_eq!(info.local_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(
            (info.port, info.viewers, info.unavailable),
            (51234, 3, true)
        );
        Ok(())
    }
}
