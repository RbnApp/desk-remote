//! The panel's own tailnet node (process-wide).
//!
//! The panel never uses the official Tailscale client. It runs the embedded helper in `panel`
//! mode with its own state folder (`<AppData>/DeskRemote/tailscale`); an official client on this PC
//! is neither read nor changed. Only tailnet addresses are dialed through the node; LAN and
//! loopback targets keep the plain TCP path.

use desk_error::DeskResult;
use desk_paths::DeskPaths;
use desk_transport::{
    connect_timeout, PanelTailnet, PanelTailnetConfig, PanelTailnetSnapshot, PeerTarget, Session,
};
use std::sync::OnceLock;
use std::time::Duration;

static NODE: OnceLock<Option<PanelTailnet>> = OnceLock::new();

/// Tailnet host name of this panel: `deskremote-panel-<pc>`, DNS-label safe.
fn node_name() -> String {
    let pc: String = std::env::var("COMPUTERNAME")
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(40)
        .collect();
    if pc.is_empty() {
        "deskremote-panel".to_string()
    } else {
        format!("deskremote-panel-{}", pc.to_ascii_lowercase())
    }
}

fn build() -> Option<PanelTailnet> {
    let paths = DeskPaths::default();
    let helper = desk_service_core::ensure_panel_helper_extracted(&paths)
        .map_err(|e| tracing::warn!(error = %e, "panel tailnet helper unavailable"))
        .ok()?;
    PanelTailnet::new(PanelTailnetConfig {
        helper,
        state_dir: paths.panel_tailnet_dir(),
        hostname: node_name(),
        control_url: None,
    })
    .map_err(|e| tracing::warn!(error = %e, "panel tailnet node not created"))
    .ok()
}

fn node() -> Option<&'static PanelTailnet> {
    NODE.get_or_init(build).as_ref()
}

/// True once a login exists in the node's state folder, so a plain start never opens a browser.
#[cfg(not(feature = "demo"))]
fn has_login() -> bool {
    std::fs::read_dir(DeskPaths::default().panel_tailnet_dir())
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// Brings the node up in the background when a login already exists.
#[cfg(not(feature = "demo"))]
pub fn start_if_signed_in() {
    if has_login() {
        std::thread::spawn(|| {
            if let Some(n) = node() {
                if let Err(e) = n.start() {
                    tracing::warn!(error = %e, "panel tailnet start failed");
                }
            }
        });
    }
}

/// Current node state; `Off` when the helper is unavailable.
pub fn snapshot() -> PanelTailnetSnapshot {
    node().map(PanelTailnet::snapshot).unwrap_or_default()
}

/// Starts the node if needed and returns the sign-in URL (`None`: already online).
pub fn begin_sign_in() -> DeskResult<Option<String>> {
    match node() {
        Some(n) => n.begin_sign_in(),
        None => Err(desk_error::DeskError::Transport(
            "tailnet helper unavailable".to_string(),
        )),
    }
}

/// Signs the node out and forgets the login.
pub fn sign_out() -> DeskResult<()> {
    match node() {
        Some(n) => n.sign_out(),
        None => Ok(()),
    }
}

/// Connects to a peer: tailnet targets through the node, everything else over plain TCP.
pub fn dial(target: &PeerTarget, timeout: Duration) -> DeskResult<Session> {
    match (target, node()) {
        (PeerTarget::Tailscale { .. }, Some(n)) => n.connect(target, timeout),
        _ => connect_timeout(target, timeout),
    }
}
