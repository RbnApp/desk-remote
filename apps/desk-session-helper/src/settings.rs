//! "Settings" window: device name, language, start-up, updates and security.
//!
//! Content only; the frame, theme, DPI, keyboard and focus ring live in [`crate::dialog`].
//!
//! ## Responsibility
//! Shows the values the service reports in [`AgentInfo::settings`](desk_ipc::AgentInfo) and turns
//! every edit into one request: an `IpcMessage` for what the service owns (name, update policy,
//! unattended access, upload limit, service start type, view-only, pairings), or a small write
//! in this user's own folder for what only concerns this user (language, sign-in start).
//!
//! ## Scope Boundaries
//! - The helper never writes `service.json` nor the service start type: the service does, and
//!   only for messages that arrive over the verified user-helper pipe.
//! - Values are changed optimistically in the shared UI state; the service's next snapshot
//!   confirms or reverts them.

use crate::menu;
use crate::state::UiState;
use crate::ui::{apply_language, paths, shared, wide};
use desk_config::{ConfigManager, PanelConfig, MAX_DEVICE_NAME_CHARS};
use desk_i18n::{current_locale, t, Locale};
use desk_ipc::{AgentSettings, IpcMessage, ServiceStart, SettingChange, UpdateOutcome};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONWARNING, MB_YESNO};

use crate::dialog::{self, Button, Def, Entry, Item, Spec, Tone};

const TABS: u32 = 1;
const LANGUAGE: u32 = 2;
const NAME: u32 = 3;
const LOGON: u32 = 4;
const SERVICE: u32 = 5;
const AUTO_UPDATE: u32 = 6;
const CHECK_NOW: u32 = 7;
const UNATTENDED: u32 = 8;
const VIEW_ONLY: u32 = 9;
const UPLOAD: u32 = 10;
/// Remove buttons of the paired list are `PAIRED_BASE + index`.
const PAIRED_BASE: u32 = 1000;

const TAB_GENERAL: usize = 0;
const TAB_UPDATE: usize = 1;
const TAB_SECURITY: usize = 2;

/// Largest and smallest upload limit the service accepts, in MB.
const UPLOAD_MIN_MB: u32 = 1;
const UPLOAD_MAX_MB: u32 = 1024;

/// What only this window knows (the service never sees it).
#[derive(Default)]
struct Local {
    tab: usize,
    /// The last name / limit typed was refused: show the hint until a valid one is saved.
    name_refused: bool,
    upload_refused: bool,
    /// A manual update check was started and no answer arrived yet.
    checking: bool,
    /// What the sign-in start switch reads now (`None` = ask the system again).
    logon_error: bool,
}

static LOCAL: Mutex<Local> = Mutex::new(Local {
    tab: TAB_GENERAL,
    name_refused: false,
    upload_refused: false,
    checking: false,
    logon_error: false,
});

fn local() -> std::sync::MutexGuard<'static, Local> {
    LOCAL.lock().unwrap_or_else(|p| p.into_inner())
}

static DEF: Def =
    Def::new(|| t("settings.title"), build, on_button, 0).with_input(on_choice, on_edit);

fn snapshot() -> UiState {
    shared().map(|s| s.snapshot()).unwrap_or_default()
}

/// Changes the shared snapshot (and with it every window that shows it).
fn edit_info(f: impl FnOnce(&mut desk_ipc::AgentInfo)) {
    if let Some(s) = shared() {
        s.update(|ui| {
            if let Some(info) = ui.info.as_mut() {
                f(info);
            }
        });
    }
}

fn change(change: SettingChange) {
    menu::send(&IpcMessage::ChangeSetting(change));
}

/// The installed service hosts this agent (as opposed to a portable run with no service).
fn installed(settings: &AgentSettings) -> bool {
    settings.service_start != ServiceStart::NotInstalled
}

fn logon_enabled() -> bool {
    desk_service_core::logon_command().is_some()
}

/// Text for "how long ago" (minutes, hours, days), or the "never" text.
fn ago(epoch_secs: u64) -> String {
    if epoch_secs == 0 {
        return t("settings.never");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(epoch_secs);
    let (key, n) = match secs {
        0..=59 => return t("settings.just_now"),
        60..=3599 => ("settings.minutes_ago", secs / 60),
        3600..=86399 => ("settings.hours_ago", secs / 3600),
        _ => ("settings.days_ago", secs / 86400),
    };
    t(key).replace("{n}", &n.to_string())
}

fn update_text(ui: &UiState, local: &Local) -> Option<(String, Tone)> {
    if let Some((outcome, version)) = &ui.update_result {
        let key = match outcome {
            UpdateOutcome::UpToDate => "tray.update_uptodate",
            UpdateOutcome::Installing => "tray.update_installing",
            UpdateOutcome::Deferred => "tray.update_deferred",
            UpdateOutcome::Failed => "tray.update_failed",
        };
        let tone = if *outcome == UpdateOutcome::Failed {
            Tone::Danger
        } else {
            Tone::Muted
        };
        return Some((t(key).replace("{v}", version), tone));
    }
    local
        .checking
        .then(|| (t("settings.checking"), Tone::Muted))
}

fn general(ui: &UiState, s: &AgentSettings, local: &Local) -> Vec<Item> {
    let name = if s.device_name.is_empty() {
        ui.info
            .as_ref()
            .map(|i| i.pc_name.clone())
            .unwrap_or_default()
    } else {
        s.device_name.clone()
    };
    let installed = installed(s);
    vec![
        Item::Section(t("settings.section_device")),
        Item::Edit {
            id: NAME,
            label: t("settings.device_name"),
            value: name,
            max_chars: MAX_DEVICE_NAME_CHARS,
            numeric: false,
            hint: Some(if local.name_refused {
                (t("settings.device_name_blank"), Tone::Danger)
            } else {
                (t("settings.device_name_hint"), Tone::Muted)
            }),
        },
        Item::Select {
            id: LANGUAGE,
            label: t("settings.language"),
            options: vec!["Türkçe".to_string(), "English".to_string()],
            selected: usize::from(current_locale() != Locale::Tr),
        },
        Item::Section(t("settings.section_start")),
        Item::Toggle {
            id: LOGON,
            label: t("settings.logon_start"),
            hint: Some(if installed {
                t("settings.logon_start_service")
            } else if local.logon_error {
                t("settings.logon_start_failed")
            } else {
                t("settings.logon_start_hint")
            }),
            on: installed || logon_enabled(),
            enabled: !installed,
        },
        Item::Toggle {
            id: SERVICE,
            label: t("settings.service_start"),
            hint: Some(match s.service_start {
                ServiceStart::NotInstalled => t("settings.service_start_portable"),
                ServiceStart::Manual => t("settings.service_start_manual"),
                ServiceStart::Automatic => t("settings.service_start_hint"),
            }),
            on: s.service_start == ServiceStart::Automatic,
            enabled: installed,
        },
    ]
}

fn updates(ui: &UiState, s: &AgentSettings, local: &Local) -> Vec<Item> {
    let mut items = vec![
        Item::Section(t("settings.section_version")),
        Item::Field {
            label: t("settings.installed_version"),
            value: desk_version::CURRENT.to_string(),
        },
        Item::Field {
            label: t("settings.latest_version"),
            value: if s.latest_version.is_empty() {
                t("settings.unknown")
            } else {
                s.latest_version.clone()
            },
        },
        Item::Field {
            label: t("settings.last_check"),
            value: ago(s.last_update_check),
        },
    ];
    if let Some((text, tone)) = update_text(ui, local) {
        items.push(Item::Text { text, tone });
    }
    items.push(Item::Toggle {
        id: AUTO_UPDATE,
        label: t("settings.auto_update"),
        hint: Some(t("settings.auto_update_hint")),
        on: s.auto_update,
        enabled: true,
    });
    items
}

fn security(ui: &UiState, s: &AgentSettings, local: &Local) -> Vec<Item> {
    let info = ui.info.clone().unwrap_or_default();
    let entries = info
        .paired
        .iter()
        .enumerate()
        .map(|(i, peer)| Entry {
            id: PAIRED_BASE + u32::try_from(i).unwrap_or(0),
            text: menu::peer_label(peer),
            action: t("settings.remove"),
        })
        .collect();
    vec![
        Item::Section(t("settings.section_access")),
        Item::Toggle {
            id: UNATTENDED,
            label: t("settings.unattended"),
            hint: Some(t("settings.unattended_hint")),
            on: s.allow_unattended,
            enabled: true,
        },
        Item::Toggle {
            id: VIEW_ONLY,
            label: t("settings.view_only"),
            hint: Some(t("settings.view_only_hint")),
            on: info.view_only,
            enabled: ui.service_online,
        },
        Item::Edit {
            id: UPLOAD,
            label: t("settings.upload_limit"),
            value: s.max_upload_mb.to_string(),
            max_chars: 4,
            numeric: true,
            hint: Some(if local.upload_refused {
                (
                    t("settings.upload_limit_range")
                        .replace("{min}", &UPLOAD_MIN_MB.to_string())
                        .replace("{max}", &UPLOAD_MAX_MB.to_string()),
                    Tone::Danger,
                )
            } else {
                (t("settings.upload_limit_hint"), Tone::Muted)
            }),
        },
        Item::Section(t("settings.section_paired")),
        Item::Entries {
            entries,
            empty: t("tray.paired_none"),
        },
    ]
}

fn build() -> Spec {
    let ui = snapshot();
    let settings = ui
        .info
        .as_ref()
        .map(|i| i.settings.clone())
        .unwrap_or_default();
    let local = local();
    let tab = local.tab;
    let mut items = vec![Item::Tabs {
        id: TABS,
        labels: vec![
            t("settings.tab_general"),
            t("settings.tab_update"),
            t("settings.tab_security"),
        ],
        active: tab,
    }];
    items.extend(match tab {
        TAB_UPDATE => updates(&ui, &settings, &local),
        TAB_SECURITY => security(&ui, &settings, &local),
        _ => general(&ui, &settings, &local),
    });
    let mut buttons = Vec::new();
    if tab == TAB_UPDATE {
        buttons.push(Button {
            id: CHECK_NOW,
            label: t("settings.check_now"),
            primary: false,
        });
    }
    buttons.push(Button {
        id: dialog::CLOSE,
        label: t("common.close"),
        primary: true,
    });
    Spec {
        title: t("settings.heading"),
        subtitle: Some(t("about.version").replace("{v}", desk_version::CURRENT)),
        items,
        buttons,
    }
}

fn warn_yes_no(text: &str, title: &str) -> bool {
    let (body, title) = (wide(text), wide(title));
    let owner = shared().map(|s| HWND(s.hwnd() as *mut _));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let answer = unsafe {
        MessageBoxW(
            owner,
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONWARNING,
        )
    };
    answer == IDYES
}

fn current_settings() -> AgentSettings {
    snapshot().info.map(|i| i.settings).unwrap_or_default()
}

fn on_button(id: u32) {
    let ui = snapshot();
    let settings = current_settings();
    match id {
        LOGON => toggle_logon(),
        SERVICE => {
            let to = if settings.service_start == ServiceStart::Automatic {
                if !warn_yes_no(&t("settings.service_start_confirm"), &t("settings.title")) {
                    return;
                }
                ServiceStart::Manual
            } else {
                ServiceStart::Automatic
            };
            edit_info(|i| i.settings.service_start = to);
            change(SettingChange::ServiceStart(to));
        }
        AUTO_UPDATE => {
            let on = !settings.auto_update;
            edit_info(|i| i.settings.auto_update = on);
            change(SettingChange::AutoUpdate(on));
        }
        UNATTENDED => {
            let on = !settings.allow_unattended;
            edit_info(|i| i.settings.allow_unattended = on);
            change(SettingChange::AllowUnattended(on));
        }
        VIEW_ONLY => {
            let enabled = !ui.info.as_ref().is_some_and(|i| i.view_only);
            edit_info(|i| i.view_only = enabled);
            menu::send(&IpcMessage::SetViewOnly { enabled });
        }
        CHECK_NOW => {
            local().checking = true;
            if let Some(s) = shared() {
                s.update(|ui| ui.update_result = None);
            }
            menu::send(&IpcMessage::UpdateRequest);
        }
        n if n >= PAIRED_BASE => {
            let peer = ui
                .info
                .as_ref()
                .and_then(|i| i.paired.get((n - PAIRED_BASE) as usize));
            if let (Some(peer), Some(s)) = (peer, shared()) {
                menu::confirm_and_unpair(HWND(s.hwnd() as *mut _), peer);
            }
        }
        _ => {}
    }
}

/// Switches the portable agent's sign-in start (the `Run` entry) on or off.
fn toggle_logon() {
    let Some(paths) = paths() else { return };
    let on = !logon_enabled();
    let fallback = match (
        desk_paths::DeskPaths::current_exe(),
        paths.user_dir().parent(),
    ) {
        (Ok(exe), Some(dir)) => desk_service_core::portable_run_command(&exe, dir),
        _ => String::new(),
    };
    let done = desk_service_core::set_logon_start(paths, on, &fallback);
    if let Err(e) = &done {
        tracing::warn!(error = %e, "cannot change the sign-in start");
    }
    local().logon_error = done.is_err();
}

fn on_choice(id: u32, index: usize) {
    match id {
        TABS => local().tab = index.min(TAB_SECURITY),
        LANGUAGE => set_language(if index == 0 { Locale::Tr } else { Locale::En }),
        _ => {}
    }
}

/// Saves the language for the tray, the windows and the panel, and switches this process now.
fn set_language(locale: Locale) {
    if let Some(paths) = paths() {
        let code = if locale == Locale::Tr { "tr" } else { "en" };
        let saved = ConfigManager::update::<PanelConfig, _>(&paths.panel_config_path(), |c| {
            c.language = code.to_string();
        });
        if let Err(e) = saved {
            tracing::warn!(error = %e, "cannot save the language");
        }
    }
    apply_language(locale);
}

fn on_edit(id: u32, text: &str) {
    match id {
        NAME => match desk_config::clean_device_name(text) {
            Some(name) => {
                local().name_refused = false;
                edit_info(|i| {
                    i.settings.device_name.clone_from(&name);
                    i.pc_name.clone_from(&name);
                });
                change(SettingChange::DeviceName(name));
            }
            None => local().name_refused = true,
        },
        UPLOAD => match text.trim().parse::<u32>() {
            Ok(mb) if (UPLOAD_MIN_MB..=UPLOAD_MAX_MB).contains(&mb) => {
                local().upload_refused = false;
                edit_info(|i| i.settings.max_upload_mb = mb);
                change(SettingChange::MaxUploadMb(mb));
            }
            _ => local().upload_refused = true,
        },
        _ => {}
    }
}

/// Opens the Settings window (or raises it when already open).
pub fn show() {
    dialog::open(&DEF);
}

/// Re-reads the open window after a state change.
pub fn refresh() {
    dialog::refresh(&DEF);
}

/// Settings is on screen: the update result shows there instead of as a balloon.
pub fn is_open() -> bool {
    dialog::is_open(&DEF)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Building a tab holds the local-state lock; no tab may take it a second time (that hung
    /// the whole window on the Updates tab once).
    #[test]
    fn every_tab_builds_without_deadlock() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for tab in [TAB_GENERAL, TAB_UPDATE, TAB_SECURITY] {
                local().tab = tab;
                let _ = build();
            }
            local().tab = TAB_GENERAL;
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok(),
            "building a tab hung"
        );
    }

    #[test]
    fn ago_has_a_text_for_every_range() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let all = [
            ago(0),
            ago(now),
            ago(now - 300),
            ago(now - 7200),
            ago(now - 3 * 86400),
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "each range reads differently");
            }
        }
    }

    #[test]
    fn upload_range_matches_what_the_service_accepts() {
        assert_eq!((UPLOAD_MIN_MB, UPLOAD_MAX_MB), (1, 1024));
    }
}
