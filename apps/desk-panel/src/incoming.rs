//! Attended in-app incoming connection host for Desk-Remote panel (AnyDesk style).
//!
//! Allows another Desk-Remote panel/agent to connect to this panel PC while the panel UI is open.
//! Unattended farm PC agent rules (DAPP-80) do not apply to this user-consented, interactive host.

use desk_crypto::{DeviceIdentity, TrustStore};
use desk_paths::DeskPaths;
use desk_service_core::{ConsentEvent, HostOptions, HostRole, ServiceHost, TransferEvent};
use desk_transport::{primary_local_ip, TailscaleDetector};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{AppHandle, Emitter};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestInfo {
    pub request_id: String,
    pub peer_name: String,
    pub fingerprint: String,
    pub known: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub peer_name: String,
    pub fingerprint: String,
    /// `"view"` or `"control"`.
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingState {
    pub enabled: bool,
    pub pc_name: String,
    pub code: String,
    pub code_ttl_s: u64,
    pub addresses: Vec<String>,
    pub pending: Option<RequestInfo>,
    pub session: Option<SessionInfo>,
}

pub struct IncomingManager {
    host: Mutex<Option<ServiceHost>>,
    pc_name: String,
    paths: DeskPaths,
    loopback_only: bool,
    port: Option<u16>,
    max_upload_mb: AtomicU32,
}

impl IncomingManager {
    pub fn new(paths: DeskPaths, loopback_only: bool) -> Self {
        Self::with_port(paths, loopback_only, Some(desk_paths::DEFAULT_PORT))
    }

    pub fn with_port(paths: DeskPaths, loopback_only: bool, port: Option<u16>) -> Self {
        let pc_name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "Desk-PC".to_string());
        Self {
            host: Mutex::new(None),
            pc_name,
            paths,
            loopback_only,
            port,
            max_upload_mb: AtomicU32::new(desk_config::DEFAULT_MAX_UPLOAD_MB),
        }
    }

    /// Upload limit for files this PC receives; applies to a running host at once.
    pub fn set_max_upload_mb(&self, mb: u32) {
        self.max_upload_mb.store(mb, Ordering::SeqCst);
        if let Some(host) = lock(&self.host).as_ref() {
            host.hub().transfers().set_max_upload_mb(mb);
        }
    }

    /// The user's answer to a `transfer://offer`.
    pub fn transfer_answer(&self, transfer_id: &str, accept: bool) -> Result<(), String> {
        let guard = lock(&self.host);
        let Some(host) = guard.as_ref() else {
            return Err("incoming host not running".to_string());
        };
        if host.hub().transfers().answer(transfer_id, accept) {
            Ok(())
        } else {
            Err("no such offer".to_string())
        }
    }

    /// Returns a snapshot of the incoming connection state.
    pub fn get_state(&self) -> IncomingState {
        let guard = lock(&self.host);
        let Some(host) = guard.as_ref() else {
            return IncomingState {
                enabled: false,
                pc_name: self.pc_name.clone(),
                code: String::new(),
                code_ttl_s: 0,
                addresses: Vec::new(),
                pending: None,
                session: None,
            };
        };

        let hub = host.hub();
        let port = host.port().unwrap_or(0);
        let mut addresses = Vec::new();
        if port > 0 {
            if self.loopback_only {
                addresses.push(format!("127.0.0.1:{port}"));
            } else {
                if let Some(ip) = primary_local_ip() {
                    addresses.push(format!("{ip}:{port}"));
                }
                let ts = TailscaleDetector::detect();
                if ts.running {
                    for ip in ts.ips {
                        let addr = format!("{ip}:{port}");
                        if !addresses.contains(&addr) {
                            addresses.push(addr);
                        }
                    }
                }
            }
        }

        let (code, ttl) = hub.with_pairing(|p| {
            let active = p.active_code().map(str::to_string);
            let ttl = p.active_code_ttl_secs();
            (active, ttl)
        });

        let code = code.unwrap_or_else(|| {
            hub.with_pairing(|p| p.create_pairing_code())
                .unwrap_or_default()
        });

        let pending = hub.pending_request().map(|r| RequestInfo {
            request_id: r.request_id,
            peer_name: r.peer_name,
            fingerprint: r.fingerprint,
            known: r.known,
        });

        let session = hub.active_session().map(|s| SessionInfo {
            peer_name: s.peer_name,
            fingerprint: s.fingerprint,
            mode: s.mode,
        });

        IncomingState {
            enabled: true,
            pc_name: self.pc_name.clone(),
            code,
            code_ttl_s: ttl,
            addresses,
            pending,
            session,
        }
    }

    /// Enables or disables accepting incoming connections.
    pub fn set_enabled(&self, app: AppHandle, enabled: bool) -> Result<IncomingState, String> {
        let mut guard = lock(&self.host);
        if enabled {
            if guard.is_some() {
                drop(guard);
                return Ok(self.get_state());
            }

            let identity = DeviceIdentity::load_or_create(&self.paths.panel_identity_path())
                .map_err(|e| format!("cannot load panel identity: {e}"))?;
            let trust = TrustStore::open(&self.paths.trusted_hosts_path())
                .map_err(|e| format!("cannot open trust store: {e}"))?;

            let port = self.port.unwrap_or(desk_paths::DEFAULT_PORT);
            let mut opts =
                HostOptions::for_panel_in_app(self.paths.clone(), Some(port), self.loopback_only);
            opts.custom_identity = Some(identity);
            opts.custom_trust = Some(trust);
            opts.role = HostRole::PanelInApp;
            opts.consent_gate = true;

            let host =
                ServiceHost::start(opts).map_err(|e| format!("cannot start incoming host: {e}"))?;

            let app_clone = app.clone();
            host.hub()
                .set_consent_listener(Some(Arc::new(move |ev| match ev {
                    ConsentEvent::Request(r) => {
                        let _ = app_clone.emit(
                            "incoming://request",
                            serde_json::json!({
                                "request_id": r.request_id,
                                "peer_name": r.peer_name,
                                "fingerprint": r.fingerprint,
                                "known": r.known,
                            }),
                        );
                    }
                    ConsentEvent::Session(s) => {
                        let _ = app_clone.emit(
                            "incoming://session",
                            serde_json::json!({
                                "peer_name": s.peer_name,
                                "fingerprint": s.fingerprint,
                                "mode": s.mode,
                            }),
                        );
                    }
                    ConsentEvent::Ended { reason } => {
                        let _ = app_clone
                            .emit("incoming://ended", serde_json::json!({ "reason": reason }));
                    }
                })));

            host.hub()
                .transfers()
                .set_max_upload_mb(self.max_upload_mb.load(Ordering::SeqCst));
            let app_offers = app.clone();
            host.hub()
                .transfers()
                .set_listener(Some(Arc::new(move |ev| match ev {
                    TransferEvent::Offer {
                        transfer_id,
                        peer_name,
                        name,
                        size,
                    } => {
                        let _ = app_offers.emit(
                            "transfer://offer",
                            serde_json::json!({
                                "transfer_id": transfer_id,
                                "peer_name": peer_name,
                                "name": name,
                                "size": size,
                            }),
                        );
                    }
                })));

            // Generate initial pairing code
            let _ = host.hub().with_pairing(|p| p.create_pairing_code());
            *guard = Some(host);
            drop(guard);
            Ok(self.get_state())
        } else {
            if let Some(host) = guard.take() {
                host.stop();
                let _ = app.emit("incoming://ended", serde_json::json!({ "reason": "host" }));
            }
            drop(guard);
            Ok(self.get_state())
        }
    }

    /// Generates a fresh pairing code.
    pub fn new_code(&self) -> Result<IncomingState, String> {
        let guard = lock(&self.host);
        let Some(host) = guard.as_ref() else {
            return Err("incoming connections are disabled".to_string());
        };
        host.hub()
            .with_pairing(|p| p.create_pairing_code())
            .map_err(|e| format!("cannot create code: {e}"))?;
        drop(guard);
        Ok(self.get_state())
    }

    /// Answers a pending incoming connection request.
    pub fn answer(
        &self,
        request_id: &str,
        decision: &str,
        always_allow: bool,
    ) -> Result<(), String> {
        let guard = lock(&self.host);
        let Some(host) = guard.as_ref() else {
            return Err("incoming host not running".to_string());
        };
        host.hub()
            .answer_consent(request_id, decision, always_allow)
    }

    /// Disconnects the active incoming session.
    pub fn disconnect(&self) -> Result<(), String> {
        let guard = lock(&self.host);
        let Some(host) = guard.as_ref() else {
            return Ok(());
        };
        host.hub().disconnect_incoming();
        Ok(())
    }

    /// Changes the mode of the active incoming session.
    pub fn set_mode(&self, mode: &str) -> Result<(), String> {
        let guard = lock(&self.host);
        let Some(host) = guard.as_ref() else {
            return Err("incoming host not running".to_string());
        };
        host.hub().set_incoming_mode(mode);
        Ok(())
    }

    /// Stops the host immediately if running.
    pub fn stop(&self) {
        if let Some(host) = lock(&self.host).take() {
            host.stop();
        }
    }
}

impl Drop for IncomingManager {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn manager_defaults_to_default_port_and_detects_pc_name() {
        let paths = DeskPaths::custom(PathBuf::from(r"C:\test\sys"), PathBuf::from(r"C:\test\usr"));
        let mgr = IncomingManager::new(paths, true);
        assert_eq!(mgr.port, Some(desk_paths::DEFAULT_PORT));
        assert!(!mgr.pc_name.is_empty());

        let state = mgr.get_state();
        assert!(!state.enabled);
        assert_eq!(state.pc_name, mgr.pc_name);
        assert!(state.addresses.is_empty());
    }

    #[test]
    fn addresses_filter_loopback_in_lan_mode() {
        let paths = DeskPaths::custom(PathBuf::from(r"C:\test\sys"), PathBuf::from(r"C:\test\usr"));
        let mgr_loopback = IncomingManager::with_port(paths.clone(), true, Some(47800));
        assert!(mgr_loopback.loopback_only);

        let mgr_lan = IncomingManager::with_port(paths, false, Some(47800));
        assert!(!mgr_lan.loopback_only);
    }
}
