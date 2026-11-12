//! State behind the tray menu: the "watch only" switch, the listening port, the tailnet
//! report and the [`AgentInfo`] snapshot the session helper renders.
//!
//! The network is queried here (service side) and nowhere else: the helper only receives
//! the finished snapshot over IPC.

use desk_crypto::DeviceIdentity;
use desk_ipc::{AgentInfo, TailnetState};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Address advertised when the listener is bound to loopback only.
const LOOPBACK_IP: &str = "127.0.0.1";

#[derive(Default)]
struct TailnetReport {
    state: TailnetState,
    ips: Vec<String>,
}

/// Runtime switches and facts shared by the hub, the host and the tailnet supervisor.
#[derive(Default)]
pub struct Control {
    view_only: AtomicBool,
    port: AtomicU16,
    loopback_only: AtomicBool,
    tailnet: Mutex<TailnetReport>,
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

    /// True while remote input is blocked. Always `false` after a service restart.
    #[must_use]
    pub fn view_only(&self) -> bool {
        self.view_only.load(Ordering::SeqCst)
    }

    /// Switches watch-only mode; returns whether the value changed.
    pub fn set_view_only(&self, enabled: bool) -> bool {
        self.view_only.swap(enabled, Ordering::SeqCst) != enabled
    }

    /// Stores the tailnet helper's first report (`status` as printed by the helper).
    pub fn set_tailnet_report(&self, status: &str, ips: &[String]) {
        let state = match status {
            "listening" | "running" if !ips.is_empty() => TailnetState::Connected,
            "needs_auth" => TailnetState::NeedsLogin,
            _ => TailnetState::Off,
        };
        let mut report = lock(&self.tailnet);
        report.state = state;
        report.ips = if state == TailnetState::Connected {
            ips.to_vec()
        } else {
            Vec::new()
        };
    }

    /// Builds the snapshot for the tray menu.
    #[must_use]
    pub fn snapshot(
        &self,
        identity: &DeviceIdentity,
        viewers: usize,
        unavailable: bool,
    ) -> AgentInfo {
        let loopback = self.loopback_only.load(Ordering::SeqCst);
        let report = lock(&self.tailnet);
        AgentInfo {
            pc_name: std::env::var("COMPUTERNAME").unwrap_or_default(),
            local_ip: if loopback {
                Some(LOOPBACK_IP.to_string())
            } else {
                desk_transport::primary_local_ip()
            },
            port: self.port.load(Ordering::SeqCst),
            tailnet: report.state,
            tailnet_ips: report.ips.clone(),
            device_id: identity.device_id(),
            fingerprint: identity.fingerprint(),
            viewers: u32::try_from(viewers).unwrap_or(u32::MAX),
            unavailable,
            view_only: self.view_only(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desk_error::DeskResult;

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
        let state = |c: &Control| c.snapshot(&identity, 0, false).tailnet;
        assert_eq!(state(&control), TailnetState::Off, "nothing reported yet");
        control.set_tailnet_report("listening", &ips);
        assert_eq!(state(&control), TailnetState::Connected);
        assert_eq!(control.snapshot(&identity, 0, false).tailnet_ips, ips);
        control.set_tailnet_report("needs_auth", &[]);
        assert_eq!(state(&control), TailnetState::NeedsLogin);
        assert!(control.snapshot(&identity, 0, false).tailnet_ips.is_empty());
        control.set_tailnet_report("error", &ips);
        assert_eq!(state(&control), TailnetState::Off);
        Ok(())
    }

    #[test]
    fn loopback_mode_advertises_loopback_with_the_bound_port() -> DeskResult<()> {
        let control = Control::default();
        control.configure(51234, true);
        let info = control.snapshot(&DeviceIdentity::generate()?, 3, true);
        assert_eq!(info.local_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(
            (info.port, info.viewers, info.unavailable),
            (51234, 3, true)
        );
        Ok(())
    }
}
