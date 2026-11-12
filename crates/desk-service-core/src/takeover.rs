//! Safe hand-over from an official Tailscale client to the embedded tailnet helper.
//!
//! Order is the contract (patron decision K-581065 = B): the helper joins the tailnet and is
//! verified reachable FIRST; only then the official client is stopped and kept from starting
//! at boot. What was changed is recorded before it is changed, so `uninstall` restores it.
//! A failed verification leaves the official client untouched.

use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Windows service name of the official Tailscale client.
pub const OFFICIAL_SERVICE: &str = "Tailscale";

/// `sc` start type code for automatic start.
pub const START_AUTO: u32 = 2;
/// `sc` start type code: the service cannot start.
pub const START_DISABLED: u32 = 4;

/// What the official client looks like right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfficialState {
    pub start_type: u32,
    pub running: bool,
}

/// Service-control seam: the real one drives `sc.exe`, tests use a fake.
pub trait OfficialTailscale {
    /// `None` when the official service is not installed.
    fn query(&self) -> DeskResult<Option<OfficialState>>;
    fn stop(&self) -> DeskResult<()>;
    fn start(&self) -> DeskResult<()>;
    fn set_start_type(&self, start_type: u32) -> DeskResult<()>;
    /// Closes the tray application (best effort).
    fn close_tray(&self) -> DeskResult<()>;
}

/// Outcome, written to `tailnet-status.json` next to the helper state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TakeoverStatus {
    /// `take_over_official_tailscale` is off.
    SettingOff,
    /// No official client, or it is already stopped and disabled.
    NotNeeded,
    /// Helper connection could not be verified; the official client was NOT touched.
    Unverified,
    /// Official client stopped and disabled; original settings recorded for restore.
    TookOver,
    /// A control step failed (after verification); see `error`.
    Failed { error: String },
}

/// What `uninstall` needs to put the machine back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverRecord {
    pub start_type: u32,
    pub was_running: bool,
}

/// Runs the hand-over. `verify` proves the helper is online and reachable; it is the only gate.
pub fn take_over(
    ctl: &impl OfficialTailscale,
    enabled: bool,
    verify: impl FnOnce() -> bool,
    record_path: &Path,
) -> TakeoverStatus {
    if !enabled {
        return TakeoverStatus::SettingOff;
    }
    let state = match ctl.query() {
        Ok(Some(s)) => s,
        Ok(None) => return TakeoverStatus::NotNeeded,
        Err(e) => return fail(&e),
    };
    if !state.running && state.start_type == START_DISABLED {
        return TakeoverStatus::NotNeeded;
    }
    if !verify() {
        return TakeoverStatus::Unverified;
    }
    // Keep the first record: a second run would otherwise store "disabled" as the original.
    if !record_path.exists() {
        let record = TakeoverRecord {
            start_type: state.start_type,
            was_running: state.running,
        };
        if let Err(e) = write_record(record_path, &record) {
            return fail(&e);
        }
    }
    if let Err(e) = ctl
        .set_start_type(START_DISABLED)
        .and_then(|()| ctl.stop())
        .and_then(|()| ctl.close_tray())
    {
        return fail(&e);
    }
    TakeoverStatus::TookOver
}

/// Undoes [`take_over`]: restores the start type, restarts the client if it was running.
/// No record means nothing was taken over, so nothing is changed.
pub fn restore(ctl: &impl OfficialTailscale, record_path: &Path) -> DeskResult<bool> {
    let Some(record) = read_record(record_path)? else {
        return Ok(false);
    };
    ctl.set_start_type(record.start_type)?;
    if record.was_running {
        ctl.start()?;
    }
    std::fs::remove_file(record_path)?;
    Ok(true)
}

fn fail(e: &DeskError) -> TakeoverStatus {
    TakeoverStatus::Failed {
        error: e.to_string(),
    }
}

fn write_record(path: &Path, record: &TakeoverRecord) -> DeskResult<()> {
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| DeskError::Serialization(e.to_string()))?;
    desk_config::write_atomic(path, &json)
}

fn read_record(path: &Path) -> DeskResult<Option<TakeoverRecord>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| DeskError::Serialization(format!("takeover record: {e}")))
}

/// Real controller: `sc.exe` (numeric output parsed, so it works in every Windows language).
pub struct ScTailscale;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

impl ScTailscale {
    fn sc(args: &[&str]) -> DeskResult<(bool, String)> {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new(DeskPaths::system_tool("sc.exe"))
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ))
    }
}

/// Number that follows `key` and a colon, e.g. `STATE : 4  RUNNING` gives 4.
fn number_after(text: &str, key: &str) -> Option<u32> {
    text.lines()
        .find(|l| l.contains(key))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

impl OfficialTailscale for ScTailscale {
    fn query(&self) -> DeskResult<Option<OfficialState>> {
        let (ok, config) = Self::sc(&["qc", OFFICIAL_SERVICE])?;
        // Error 1060 means the service does not exist.
        let Some(start_type) = number_after(&config, "START_TYPE").filter(|_| ok) else {
            return Ok(None);
        };
        let (_, status) = Self::sc(&["query", OFFICIAL_SERVICE])?;
        Ok(Some(OfficialState {
            start_type,
            running: number_after(&status, "STATE") == Some(4),
        }))
    }

    fn stop(&self) -> DeskResult<()> {
        // 1062 (not started) is fine: the goal is "not running".
        let (ok, out) = Self::sc(&["stop", OFFICIAL_SERVICE])?;
        if ok || out.contains("1062") {
            Ok(())
        } else {
            Err(DeskError::Service(format!("sc stop: {}", out.trim())))
        }
    }

    fn start(&self) -> DeskResult<()> {
        // 1056: already running.
        let (ok, out) = Self::sc(&["start", OFFICIAL_SERVICE])?;
        if ok || out.contains("1056") {
            Ok(())
        } else {
            Err(DeskError::Service(format!("sc start: {}", out.trim())))
        }
    }

    fn set_start_type(&self, start_type: u32) -> DeskResult<()> {
        let word = match start_type {
            START_DISABLED => "disabled",
            START_AUTO => "auto",
            3 => "demand",
            _ => "delayed-auto",
        };
        let (ok, out) = Self::sc(&["config", OFFICIAL_SERVICE, "start=", word])?;
        if ok {
            Ok(())
        } else {
            Err(DeskError::Service(format!("sc config: {}", out.trim())))
        }
    }

    fn close_tray(&self) -> DeskResult<()> {
        use std::os::windows::process::CommandExt;
        // Exit code 128 means "not running", which is fine.
        let _ = std::process::Command::new(DeskPaths::system_tool("taskkill.exe"))
            .args(["/F", "/IM", "tailscale-ipn.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        state: RefCell<Option<OfficialState>>,
        calls: RefCell<Vec<String>>,
        fail_stop: bool,
    }

    impl Fake {
        fn new(state: Option<OfficialState>) -> Self {
            Self {
                state: RefCell::new(state),
                calls: RefCell::new(Vec::new()),
                fail_stop: false,
            }
        }
        fn running_auto() -> Self {
            Self::new(Some(OfficialState {
                start_type: START_AUTO,
                running: true,
            }))
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl OfficialTailscale for Fake {
        fn query(&self) -> DeskResult<Option<OfficialState>> {
            Ok(*self.state.borrow())
        }
        fn stop(&self) -> DeskResult<()> {
            self.calls.borrow_mut().push("stop".into());
            if self.fail_stop {
                return Err(DeskError::Service("denied".into()));
            }
            if let Some(s) = self.state.borrow_mut().as_mut() {
                s.running = false;
            }
            Ok(())
        }
        fn start(&self) -> DeskResult<()> {
            self.calls.borrow_mut().push("start".into());
            Ok(())
        }
        fn set_start_type(&self, t: u32) -> DeskResult<()> {
            self.calls.borrow_mut().push(format!("start_type={t}"));
            if let Some(s) = self.state.borrow_mut().as_mut() {
                s.start_type = t;
            }
            Ok(())
        }
        fn close_tray(&self) -> DeskResult<()> {
            self.calls.borrow_mut().push("close_tray".into());
            Ok(())
        }
    }

    fn tmp() -> tempfile::TempDir {
        ok(tempfile::tempdir())
    }

    fn ok<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
        match r {
            Ok(v) => v,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    fn some<T>(o: Option<T>) -> T {
        match o {
            Some(v) => v,
            None => panic!("unexpected None"),
        }
    }

    fn rec(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("takeover.json")
    }

    #[test]
    fn unverified_helper_never_touches_official_client() {
        let dir = tmp();
        let fake = Fake::running_auto();
        let status = take_over(&fake, true, || false, &rec(&dir));
        assert_eq!(status, TakeoverStatus::Unverified);
        assert!(
            fake.calls().is_empty(),
            "no control call: {:?}",
            fake.calls()
        );
        assert!(!rec(&dir).exists());
    }

    #[test]
    fn verification_runs_before_any_stop() {
        let dir = tmp();
        let fake = Fake::running_auto();
        let seen = RefCell::new(None);
        let status = take_over(
            &fake,
            true,
            || {
                *seen.borrow_mut() = Some(fake.calls());
                true
            },
            &rec(&dir),
        );
        assert_eq!(status, TakeoverStatus::TookOver);
        assert_eq!(
            seen.into_inner(),
            Some(vec![]),
            "nothing changed at verify time"
        );
        assert_eq!(
            fake.calls(),
            ["start_type=4", "stop", "close_tray"],
            "disable boot start, stop, close tray"
        );
    }

    #[test]
    fn setting_off_does_nothing_and_does_not_even_verify() {
        let dir = tmp();
        let fake = Fake::running_auto();
        let status = take_over(&fake, false, || panic!("must not verify"), &rec(&dir));
        assert_eq!(status, TakeoverStatus::SettingOff);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn not_installed_or_already_disabled_is_not_needed() {
        let dir = tmp();
        let none = Fake::new(None);
        assert_eq!(
            take_over(&none, true, || panic!("no verify"), &rec(&dir)),
            TakeoverStatus::NotNeeded
        );
        let off = Fake::new(Some(OfficialState {
            start_type: START_DISABLED,
            running: false,
        }));
        assert_eq!(
            take_over(&off, true, || panic!("no verify"), &rec(&dir)),
            TakeoverStatus::NotNeeded
        );
        assert!(none.calls().is_empty() && off.calls().is_empty());
    }

    #[test]
    fn restore_puts_back_start_type_and_running_state() {
        let dir = tmp();
        let fake = Fake::running_auto();
        take_over(&fake, true, || true, &rec(&dir));
        let state = some(*fake.state.borrow());
        assert_eq!((state.start_type, state.running), (START_DISABLED, false));

        assert!(ok(restore(&fake, &rec(&dir))));
        let state = some(*fake.state.borrow());
        assert_eq!(state.start_type, START_AUTO);
        assert_eq!(fake.calls().last().map(String::as_str), Some("start"));
        assert!(!rec(&dir).exists());
        assert!(!ok(restore(&fake, &rec(&dir))), "second restore is a no-op");
    }

    #[test]
    fn second_takeover_keeps_the_original_record() {
        let dir = tmp();
        let fake = Fake::running_auto();
        take_over(&fake, true, || true, &rec(&dir));
        // The official client came back (started by hand) and is taken over again.
        *fake.state.borrow_mut() = Some(OfficialState {
            start_type: START_DISABLED,
            running: true,
        });
        take_over(&fake, true, || true, &rec(&dir));
        let record = some(ok(read_record(&rec(&dir))));
        assert_eq!(record.start_type, START_AUTO);
        assert!(record.was_running);
    }

    #[test]
    fn failed_stop_is_reported_not_swallowed() {
        let dir = tmp();
        let mut fake = Fake::running_auto();
        fake.fail_stop = true;
        let status = take_over(&fake, true, || true, &rec(&dir));
        assert!(matches!(status, TakeoverStatus::Failed { .. }));
        assert!(
            rec(&dir).exists(),
            "record kept so uninstall can still undo"
        );
    }

    #[test]
    fn sc_output_is_parsed_numerically() {
        let qc = "        TYPE               : 10  WIN32_OWN_PROCESS\n        START_TYPE         : 2   AUTO_START\n";
        assert_eq!(number_after(qc, "START_TYPE"), Some(2));
        let q = "        STATE              : 4  RUNNING\n";
        assert_eq!(number_after(q, "STATE"), Some(4));
        assert_eq!(number_after("", "STATE"), None);
    }
}
