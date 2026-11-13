//! Tray context menu: agent info (click to copy), pairing code, viewers, "paired panels"
//! (remove a pairing), "disconnect all viewers", "block remote control", About and "Close".
//!
//! The helper never queries the network: every address shown here comes from the
//! [`AgentInfo`](desk_ipc::AgentInfo) snapshot the service pushes over IPC. There is
//! deliberately no "Stop agent" entry: "Close" ends a portable console agent the user started
//! and otherwise hides only the tray icon, never stopping the installed service nor the helper
//! (DAPP-104 M-09, DAPP-110).

use crate::state::UiState;
use crate::ui::{shared, wide};
use desk_i18n::t;
use desk_ipc::{AgentInfo, IpcMessage, PairedPeerInfo, TailnetState};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MessageBoxW, SetForegroundWindow,
    TrackPopupMenu, IDYES, MB_ICONQUESTION, MB_YESNO, MF_CHECKED, MF_GRAYED, MF_POPUP,
    MF_SEPARATOR, MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

/// Clipboard format id of UTF-16 text (`CF_UNICODETEXT`).
const CF_UNICODETEXT: u32 = 13;

const ID_COPY_NAME: usize = 1;
const ID_COPY_LOCAL: usize = 2;
const ID_COPY_TAILNET: usize = 3;
const ID_SHOW_CODE: usize = 4;
const ID_VIEWERS: usize = 5;
const ID_DISCONNECT_ALL: usize = 6;
const ID_VIEW_ONLY: usize = 7;
const ID_ABOUT: usize = 8;
const ID_QUIT: usize = 9;
const ID_UPDATE: usize = 10;
/// First id of the "paired panels" entries; entry `i` is `ID_PAIRED_BASE + i`.
const ID_PAIRED_BASE: usize = 100;
/// Most paired panels the submenu lists.
const PAIRED_MAX: usize = 50;

fn send(msg: &IpcMessage) {
    if let Some(conn) = shared().and_then(|s| s.conn()) {
        let _ = conn.send(msg);
    }
}

/// Asks the service for a fresh snapshot (called when the right button goes down, so the
/// answer is there by the time the menu opens).
pub fn request_info() {
    send(&IpcMessage::AgentInfoRequest);
}

/// `ip:port`, the form a viewer can paste straight into its address field.
fn endpoint(ip: &str, port: u16) -> String {
    if ip.contains(':') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

/// Tailnet address to offer: the first IPv4 one, else whatever came first.
fn tailnet_ip(info: &AgentInfo) -> Option<&str> {
    info.tailnet_ips
        .iter()
        .find(|ip| !ip.contains(':'))
        .or_else(|| info.tailnet_ips.first())
        .map(String::as_str)
}

fn status_line(ui: &UiState) -> String {
    match (&ui.info, ui.service_online) {
        (Some(info), true) if info.unavailable => t("tray.status_locked"),
        (Some(info), true) if info.viewers > 0 => {
            t("tray.status_online").replace("{n}", &info.viewers.to_string())
        }
        (Some(_), true) => t("tray.status_waiting"),
        _ => t("tray.status_offline"),
    }
}

fn tailnet_line(info: Option<&AgentInfo>) -> (String, bool) {
    match info {
        Some(i) if i.tailnet == TailnetState::Connected => match tailnet_ip(i) {
            Some(ip) => (t("tray.tailnet_connected").replace("{ip}", ip), true),
            None => (t("tray.tailnet_off"), false),
        },
        Some(i) if i.tailnet == TailnetState::NeedsLogin => (t("tray.tailnet_login"), true),
        Some(i) if i.tailnet == TailnetState::Failed => (t("tray.tailnet_error"), false),
        _ => (t("tray.tailnet_off"), false),
    }
}

/// Opens a URL in the user's default browser (never creates a console window).
pub fn open_browser(url: &str) {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        tracing::warn!("ignoring non-http browser request");
        return;
    }
    let operation = wide("open");
    let wide_url = wide(url);
    // SAFETY: ShellExecuteW is called with valid wide strings for default browser launch.
    unsafe {
        let _ = windows::Win32::UI::Shell::ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(wide_url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

/// Puts `text` on the clipboard as Unicode text.
fn copy_to_clipboard(owner: HWND, text: &str) {
    let data = wide(text);
    let bytes = data.len() * std::mem::size_of::<u16>();
    // SAFETY: the clipboard is opened and closed on this thread; the global block is sized
    // for exactly `data`, filled while locked and handed over to the system on success.
    unsafe {
        if let Err(e) = OpenClipboard(Some(owner)) {
            tracing::warn!(error = %e, "clipboard cannot be opened");
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
            let target = GlobalLock(block).cast::<u16>();
            if target.is_null() {
                tracing::warn!("clipboard block cannot be locked");
            } else {
                std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
                let _ = GlobalUnlock(block);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(block.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

fn append(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    id: usize,
    text: &str,
    enabled: bool,
    checked: bool,
) {
    let text = wide(text);
    let mut flags = MF_STRING;
    if !enabled {
        flags |= MF_GRAYED;
    }
    if checked {
        flags |= MF_CHECKED;
    }
    // SAFETY: NUL-terminated text outlives the call; the menu belongs to this thread.
    let _ = unsafe { AppendMenuW(menu, flags, id, PCWSTR(text.as_ptr())) };
}

fn separator(menu: windows::Win32::UI::WindowsAndMessaging::HMENU) {
    // SAFETY: menu belongs to this thread.
    let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
}

/// Name shown for a paired panel (its last announced name, else a short device id).
fn peer_label(peer: &PairedPeerInfo) -> String {
    if peer.name.is_empty() {
        let short: String = peer.device_id.chars().take(14).collect();
        t("tray.paired_unnamed").replace("{id}", &short)
    } else {
        peer.name.clone()
    }
}

/// Adds the "Paired panels" submenu: one entry per paired panel, click = remove its pairing.
fn append_paired(menu: windows::Win32::UI::WindowsAndMessaging::HMENU, info: Option<&AgentInfo>) {
    let peers = info.map(|i| i.paired.as_slice()).unwrap_or_default();
    // SAFETY: the submenu belongs to this thread and is destroyed with its parent menu.
    let Ok(sub) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    if peers.is_empty() {
        append(sub, 0, &t("tray.paired_none"), false, false);
    }
    for (i, peer) in peers.iter().take(PAIRED_MAX).enumerate() {
        append(sub, ID_PAIRED_BASE + i, &peer_label(peer), true, false);
    }
    let title = wide(&t("tray.paired"));
    // SAFETY: NUL-terminated text outlives the call; for MF_POPUP the id is the submenu handle.
    let _ = unsafe {
        AppendMenuW(
            menu,
            MF_STRING | MF_POPUP,
            sub.0 as usize,
            PCWSTR(title.as_ptr()),
        )
    };
}

/// Asks the PC's user to confirm, then tells the service to forget the panel.
fn confirm_and_unpair(hwnd: HWND, peer: &PairedPeerInfo) {
    let body = wide(&t("tray.paired_remove_body").replace("{name}", &peer_label(peer)));
    let title = wide(&t("tray.paired_remove_title"));
    // SAFETY: both strings are NUL-terminated and outlive the call; `hwnd` is our window.
    let answer = unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    if answer == IDYES {
        tracing::info!("pairing removal confirmed in the tray");
        send(&IpcMessage::UnpairRequest {
            device_id: peer.device_id.clone(),
        });
    }
}

/// Opens the menu at the cursor and runs the chosen entry.
pub fn show(hwnd: HWND) {
    let ui = shared().map(|s| s.snapshot()).unwrap_or_default();
    let info = ui.info.clone();
    // SAFETY: the menu is created, used and destroyed on this thread.
    let picked = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        append(menu, 0, &status_line(&ui), false, false);
        if let Some(i) = &info {
            append(
                menu,
                ID_COPY_NAME,
                &t("tray.pc_name").replace("{name}", &i.pc_name),
                true,
                false,
            );
            match &i.local_ip {
                Some(ip) => append(
                    menu,
                    ID_COPY_LOCAL,
                    &t("tray.local_ip").replace("{ip}", ip),
                    true,
                    false,
                ),
                None => append(menu, 0, &t("tray.local_ip_none"), false, false),
            }
            let (line, copyable) = tailnet_line(Some(i));
            append(menu, ID_COPY_TAILNET, &line, copyable, false);
            if i.takeover_needs_admin {
                append(menu, 0, &t("tray.tailnet_needs_admin"), false, false);
            }
        }
        separator(menu);
        append(menu, ID_SHOW_CODE, &t("tray.show_code"), true, false);
        append(menu, ID_VIEWERS, &t("tray.viewers"), true, false);
        append_paired(menu, info.as_ref());
        separator(menu);
        append(
            menu,
            ID_DISCONNECT_ALL,
            &t("tray.disconnect_all"),
            !ui.viewers.is_empty(),
            false,
        );
        append(
            menu,
            ID_VIEW_ONLY,
            &t("tray.view_only"),
            ui.service_online,
            info.as_ref().is_some_and(|i| i.view_only),
        );
        separator(menu);
        append(
            menu,
            0,
            &t("tray.version").replace("{v}", desk_version::CURRENT),
            false,
            false,
        );
        append(
            menu,
            ID_UPDATE,
            &t("tray.update_check"),
            ui.service_online,
            false,
        );
        append(menu, ID_ABOUT, &t("tray.about"), true, false);
        // Portable and installed-service mode share this one entry (and one code path).
        append(menu, ID_QUIT, &t("tray.quit"), true, false);

        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        usize::try_from(picked.0).unwrap_or(0)
    };
    run(hwnd, picked, info.as_ref());
}

fn run(hwnd: HWND, picked: usize, info: Option<&AgentInfo>) {
    tracing::info!(picked, "tray menu entry chosen");
    match picked {
        ID_COPY_NAME => {
            if let Some(i) = info {
                copy_to_clipboard(hwnd, &i.pc_name);
            }
        }
        ID_COPY_LOCAL => {
            if let Some((i, ip)) = info.and_then(|i| i.local_ip.as_deref().map(|ip| (i, ip))) {
                copy_to_clipboard(hwnd, &endpoint(ip, i.port));
            }
        }
        ID_COPY_TAILNET => {
            if let Some(i) = info {
                if i.tailnet == TailnetState::NeedsLogin {
                    if let Some(url) = &i.tailnet_auth_url {
                        open_browser(url);
                    }
                } else if let Some((_, ip)) = tailnet_ip(i).map(|ip| (i, ip)) {
                    copy_to_clipboard(hwnd, &endpoint(ip, i.port));
                }
            }
        }
        ID_SHOW_CODE => crate::code::show(),
        ID_VIEWERS => crate::viewers::show(),
        ID_DISCONNECT_ALL => send(&IpcMessage::DisconnectAllRequest),
        ID_VIEW_ONLY => send(&IpcMessage::SetViewOnly {
            enabled: !info.is_some_and(|i| i.view_only),
        }),
        ID_UPDATE => send(&IpcMessage::UpdateRequest),
        ID_ABOUT => crate::about::show(),
        // Console agent: stopped directly. Installed service: only the tray icon goes; this helper
        // keeps capturing and injecting, so the supervisor has nothing to restart (DAPP-110).
        ID_QUIT => {
            if desk_service_core::quit_agent() == desk_service_core::StopOutcome::TrayOnly {
                if let Some(s) = shared() {
                    crate::ui::hide_tray(s);
                }
            }
        }
        id if id >= ID_PAIRED_BASE => {
            if let Some(peer) = info.and_then(|i| i.paired.get(id - ID_PAIRED_BASE)) {
                confirm_and_unpair(hwnd, peer);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> AgentInfo {
        AgentInfo {
            port: 47800,
            tailnet: TailnetState::Connected,
            tailnet_ips: vec!["fd7a::1".to_string(), "100.64.0.7".to_string()],
            ..AgentInfo::default()
        }
    }

    #[test]
    fn endpoint_is_ip_colon_port_and_brackets_ipv6() {
        assert_eq!(endpoint("192.168.1.20", 47800), "192.168.1.20:47800");
        assert_eq!(endpoint("fd7a::1", 47800), "[fd7a::1]:47800");
    }

    #[test]
    fn tailnet_ip_prefers_ipv4() {
        assert_eq!(tailnet_ip(&info()), Some("100.64.0.7"));
        let mut only_v6 = info();
        only_v6.tailnet_ips = vec!["fd7a::1".to_string()];
        assert_eq!(tailnet_ip(&only_v6), Some("fd7a::1"));
        assert_eq!(tailnet_ip(&AgentInfo::default()), None);
    }

    #[test]
    fn tailnet_line_is_clickable_for_connected_and_login() {
        assert!(tailnet_line(Some(&info())).1);
        let mut login = info();
        login.tailnet = TailnetState::NeedsLogin;
        assert!(tailnet_line(Some(&login)).1);
        assert!(!tailnet_line(None).1);
    }

    #[test]
    fn status_line_follows_service_lock_and_viewers() {
        let mut ui = UiState {
            service_online: true,
            info: Some(info()),
            ..UiState::default()
        };
        let waiting = status_line(&ui);
        if let Some(i) = ui.info.as_mut() {
            i.viewers = 2;
        }
        let online = status_line(&ui);
        if let Some(i) = ui.info.as_mut() {
            i.unavailable = true;
        }
        let locked = status_line(&ui);
        ui.service_online = false;
        let offline = status_line(&ui);
        let all = [&waiting, &online, &locked, &offline];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "each state has its own text");
            }
        }
        assert!(online.contains('2'));
    }
}
