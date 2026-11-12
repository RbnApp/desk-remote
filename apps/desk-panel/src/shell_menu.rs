//! Explorer right-click entry "Send with Desk-Remote" and the hand-over of the chosen paths
//! to the (single) running panel.
//!
//! The entry lives under `HKCU` only (no administrator rights, removed per user). Windows 11
//! shows classic entries under "Show more options"; a first-level entry needs a signed package
//! (see the DAPP-87 report).
//!
//! Flow: Explorer starts `desk-panel.exe --send "<path>"` once per selected file; the
//! single-instance plugin forwards every start to the running panel, [`PickCollector`] merges
//! the burst into one list and the panel emits `transfer://pick` for the PC-picker UI.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// Event consumed by the PC-picker UI.
pub const PICK_EVENT: &str = "transfer://pick";
/// Command-line flag written into the registry command.
pub const SEND_FLAG: &str = "--send";
/// Window in which separate `--send` starts (one per selected file) are merged into one pick.
const MERGE_WINDOW: Duration = Duration::from_millis(600);
/// If the UI never claims the pick via `shell_pick_take`, it is emitted this long after load.
const UI_CLAIM_GRACE: Duration = Duration::from_millis(2500);

/// What a path handed over by Explorer turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// Existing regular file on a local drive.
    File,
    /// Existing directory (folders are not sent).
    Folder,
    /// UNC path (`\\server\share\…`); sending is refused, the UI shows a message.
    Network,
    /// Path does not exist (any more).
    Missing,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PickItem {
    pub path: String,
    pub kind: PathKind,
}

/// Payload of `transfer://pick`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PickPayload {
    /// Every path, in arrival order (ready for `transfer_send`).
    pub paths: Vec<String>,
    /// The same paths with their classification.
    pub items: Vec<PickItem>,
}

/// Registry/config state shown in Settings.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ShellMenuStatus {
    /// Explorer integration exists on this OS (Windows only).
    pub supported: bool,
    /// The entry is present in the registry.
    pub enabled: bool,
    /// The registered command points at a different executable than the running one.
    pub stale: bool,
}

/// Returns the paths after `--send` in `args` (program name excluded), or `None` without it.
pub fn parse_send_args(args: &[String]) -> Option<Vec<String>> {
    let start = args.iter().position(|a| a == SEND_FLAG)?;
    let paths: Vec<String> = args[start + 1..]
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .filter(|a| !a.is_empty())
        .cloned()
        .collect();
    Some(paths)
}

fn is_unc(path: &str) -> bool {
    let p = path.replace('/', "\\");
    p.starts_with("\\\\") && !p.starts_with("\\\\?\\") && !p.starts_with("\\\\.\\")
        || p.to_ascii_uppercase().starts_with("\\\\?\\UNC\\")
}

/// Classifies one path (existence is checked now, at delivery time).
pub fn classify(path: &str) -> PickItem {
    let kind = if is_unc(path) {
        PathKind::Network
    } else {
        match std::fs::metadata(Path::new(path)) {
            Ok(m) if m.is_dir() => PathKind::Folder,
            Ok(_) => PathKind::File,
            Err(_) => PathKind::Missing,
        }
    };
    PickItem {
        path: path.to_string(),
        kind,
    }
}

pub fn build_payload(paths: Vec<String>) -> PickPayload {
    let items = paths.iter().map(|p| classify(p)).collect();
    PickPayload { paths, items }
}

/// Merges the burst of `--send` starts into one pick and delivers it to the UI.
pub struct PickCollector {
    pending: Mutex<Vec<String>>,
    generation: AtomicU64,
    /// The UI has claimed the channel (it listens, or the grace period passed).
    ui_ready: AtomicBool,
    held: Mutex<Option<PickPayload>>,
}

impl PickCollector {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            ui_ready: AtomicBool::new(false),
            held: Mutex::new(None),
        })
    }

    /// Adds paths; after [`MERGE_WINDOW`] without further input the merged pick is delivered.
    pub fn push(self: &Arc<Self>, app: &AppHandle, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        {
            let mut pending = lock(&self.pending);
            for p in paths {
                if !pending.contains(&p) {
                    pending.push(p);
                }
            }
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let this = Arc::clone(self);
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(MERGE_WINDOW);
            if this.generation.load(Ordering::SeqCst) != generation {
                return; // a newer start extended the burst
            }
            let paths = std::mem::take(&mut *lock(&this.pending));
            if !paths.is_empty() {
                this.deliver(&app, build_payload(paths));
            }
        });
    }

    fn deliver(&self, app: &AppHandle, payload: PickPayload) {
        bring_to_front(app);
        tracing::info!(count = payload.paths.len(), "explorer pick");
        for item in &payload.items {
            tracing::info!(path = %item.path, kind = ?item.kind, "explorer pick item");
        }
        if self.ui_ready.load(Ordering::SeqCst) {
            if let Err(e) = app.emit(PICK_EVENT, &payload) {
                tracing::warn!(error = %e, "emit transfer://pick failed");
            }
        } else {
            let mut held = lock(&self.held);
            match held.as_mut() {
                Some(h) => {
                    h.paths.extend(payload.paths);
                    h.items.extend(payload.items);
                }
                None => *held = Some(payload),
            }
        }
    }

    /// The UI registered its listener: later picks are emitted live; returns a pick that
    /// arrived while the panel was still starting.
    pub fn claim(&self) -> Option<PickPayload> {
        self.ui_ready.store(true, Ordering::SeqCst);
        lock(&self.held).take()
    }

    /// Fallback for a UI that never claims: emit the held pick once the page has loaded.
    pub fn claim_after_grace(self: &Arc<Self>, app: &AppHandle) {
        let this = Arc::clone(self);
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(UI_CLAIM_GRACE);
            if this.ui_ready.swap(true, Ordering::SeqCst) {
                return;
            }
            if let Some(payload) = lock(&this.held).take() {
                let _ = app.emit(PICK_EVENT, &payload);
            }
        });
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Shows, un-minimizes and focuses the main window.
pub fn bring_to_front(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// ---------------------------------------------------------------------------------------
// Registry (HKCU only). `reg.exe` through `hidden_command`: no console window, no new crate.
// ---------------------------------------------------------------------------------------

/// Verb key; the `*` class covers every file type.
const VERB_KEY: &str = r"HKCU\Software\Classes\*\shell\DeskRemoteSend";

/// Command line stored in the registry for `exe`.
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" {SEND_FLAG} \"%1\"", exe.display())
}

/// Executable named by a registry command line (`"<exe>" --send "%1"`).
pub fn exe_of_command(command: &str) -> Option<PathBuf> {
    let rest = command.trim().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(PathBuf::from(&rest[..end]))
}

/// Menu text for a locale tag such as `tr`, `tr-TR`, `en-US`.
pub fn label_for(locale_tag: &str) -> String {
    desk_i18n::translate(
        "shell_menu.send",
        desk_i18n::Locale::from_str_loose(locale_tag),
    )
}

#[cfg(windows)]
mod os {
    use super::{command_line, exe_of_command, label_for, ShellMenuStatus, VERB_KEY};
    use std::path::{Path, PathBuf};

    fn reg(args: &[&str]) -> Result<String, String> {
        let out = desk_paths::hidden_command(desk_paths::DeskPaths::system_tool("reg.exe"))
            .args(args)
            .output()
            .map_err(|e| format!("reg.exe: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    /// Value of `(Default)` under `key`, if present.
    fn default_value(key: &str) -> Option<String> {
        let out = reg(&["query", key, "/ve"]).ok()?;
        let line = out.lines().find(|l| l.contains("REG_SZ"))?;
        Some(line.split("REG_SZ").nth(1)?.trim().to_string())
    }

    /// OS display language tag (for the menu text when the panel language is "auto").
    fn os_locale_tag() -> String {
        reg(&[
            "query",
            r"HKCU\Control Panel\International",
            "/v",
            "LocaleName",
        ])
        .ok()
        .and_then(|o| {
            o.lines()
                .find(|l| l.contains("REG_SZ"))
                .and_then(|l| l.split("REG_SZ").nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_default()
    }

    pub fn status(running_exe: &Path) -> ShellMenuStatus {
        let command = default_value(&format!(r"{VERB_KEY}\command"));
        let registered: Option<PathBuf> = command.as_deref().and_then(exe_of_command);
        ShellMenuStatus {
            supported: true,
            enabled: command.is_some(),
            stale: registered.is_some_and(|p| {
                !p.to_string_lossy()
                    .eq_ignore_ascii_case(&running_exe.to_string_lossy())
            }),
        }
    }

    pub fn register(key: &str, exe: &Path, language: &str) -> Result<(), String> {
        let tag = match language {
            "tr" | "en" => language.to_string(),
            _ => os_locale_tag(),
        };
        let label = label_for(&tag);
        let icon = format!("\"{}\",0", exe.display());
        let command = command_line(exe);
        let cmd_key = format!(r"{key}\command");
        reg(&["add", key, "/ve", "/d", &label, "/f"])?;
        reg(&["add", key, "/v", "Icon", "/d", &icon, "/f"])?;
        reg(&["add", key, "/v", "MultiSelectModel", "/d", "Player", "/f"])?;
        reg(&["add", &cmd_key, "/ve", "/d", &command, "/f"])?;
        Ok(())
    }

    pub fn unregister(key: &str) -> Result<(), String> {
        if reg(&["query", key]).is_err() {
            return Ok(()); // already gone
        }
        reg(&["delete", key, "/f"]).map(|_| ())
    }
}

#[cfg(not(windows))]
mod os {
    use super::ShellMenuStatus;
    use std::path::Path;

    pub fn status(_running_exe: &Path) -> ShellMenuStatus {
        ShellMenuStatus {
            supported: false,
            enabled: false,
            stale: false,
        }
    }
    pub fn register(_key: &str, _exe: &Path, _language: &str) -> Result<(), String> {
        Err("Explorer integration is Windows-only".to_string())
    }
    pub fn unregister(_key: &str) -> Result<(), String> {
        Ok(())
    }
}

fn running_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot resolve the panel executable: {e}"))
}

/// Current registry state.
pub fn status() -> ShellMenuStatus {
    match running_exe() {
        Ok(exe) => os::status(&exe),
        Err(_) => os::status(Path::new("")),
    }
}

/// Writes (or refreshes) the entry for the running executable.
pub fn enable(language: &str) -> Result<(), String> {
    os::register(VERB_KEY, &running_exe()?, language)
}

/// Removes the entry. This is the single uninstall hook for the installer package (Wave 4):
/// the installer calls `desk-panel.exe --remove-shell-menu` (see [`handle_cli_removal`]).
pub fn disable() -> Result<(), String> {
    os::unregister(VERB_KEY)
}

/// Makes the registry match the saved preference; runs at panel start.
pub fn sync(enabled: bool, language: &str) {
    let result = if enabled {
        let s = status();
        if s.enabled && !s.stale {
            return;
        }
        enable(language)
    } else {
        if !status().enabled {
            return;
        }
        disable()
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "explorer menu sync failed");
    }
}

/// Flag used by the installer's uninstall step.
pub const REMOVE_FLAG: &str = "--remove-shell-menu";

/// `desk-panel.exe --remove-shell-menu`: removes the entry and tells the caller to exit.
pub fn handle_cli_removal(args: &[String]) -> bool {
    if !args.iter().any(|a| a == REMOVE_FLAG) {
        return false;
    }
    if let Err(e) = disable() {
        tracing::warn!(error = %e, "explorer menu removal failed");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_send_paths() {
        assert_eq!(parse_send_args(&s(&["p.exe"])), None);
        assert_eq!(
            parse_send_args(&s(&["p.exe", "--send", r"C:\a b\c.txt", r"D:\d.bin"])),
            Some(s(&[r"C:\a b\c.txt", r"D:\d.bin"]))
        );
        assert_eq!(
            parse_send_args(&s(&["p.exe", "--send", "", "--other"])),
            Some(vec![])
        );
    }

    #[test]
    fn classifies_paths() -> std::io::Result<()> {
        let dir = std::env::temp_dir();
        let file = dir.join("desk-remote-pick-test.txt");
        std::fs::write(&file, b"x")?;
        assert_eq!(classify(&file.to_string_lossy()).kind, PathKind::File);
        assert_eq!(classify(&dir.to_string_lossy()).kind, PathKind::Folder);
        assert_eq!(
            classify(r"C:\definitely\missing\file.zzz").kind,
            PathKind::Missing
        );
        assert_eq!(classify(r"\\server\share\f.txt").kind, PathKind::Network);
        assert_eq!(
            classify(r"\\?\UNC\server\share\f.txt").kind,
            PathKind::Network
        );
        assert_ne!(classify(r"\\?\C:\Windows").kind, PathKind::Network);
        let _ = std::fs::remove_file(file);
        Ok(())
    }

    #[test]
    fn command_line_round_trips() {
        let exe = Path::new(r"C:\Program Files\Desk-Remote\desk-panel.exe");
        let cmd = command_line(exe);
        assert_eq!(
            cmd,
            r#""C:\Program Files\Desk-Remote\desk-panel.exe" --send "%1""#
        );
        assert_eq!(exe_of_command(&cmd).as_deref(), Some(exe));
        assert_eq!(exe_of_command("no quotes"), None);
    }

    #[test]
    fn label_follows_locale() {
        assert_eq!(label_for("tr-TR"), "Desk-Remote ile gönder");
        assert_eq!(label_for("en-US"), "Send with Desk-Remote");
        assert_eq!(label_for(""), "Send with Desk-Remote");
    }

    #[cfg(windows)]
    #[test]
    fn registry_round_trip_on_a_scratch_key() -> Result<(), String> {
        let key = r"HKCU\Software\Classes\*\shell\DeskRemoteSendUnitTest";
        let exe = Path::new(r"C:\Test Dir\desk-panel.exe");
        os::register(key, exe, "tr")?;
        os::register(key, exe, "tr")?; // idempotent
        assert!(os::unregister(key).is_ok());
        assert!(os::unregister(key).is_ok()); // already gone
        Ok(())
    }
}
