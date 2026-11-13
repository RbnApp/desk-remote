//! Logon start for the portable (double-click) agent (DAPP-119).
//!
//! The installed agent starts through its Windows service (automatic, delayed). The portable
//! agent has no service, so it registers itself under the current user's `Run` key once; after
//! a restart or sign-in it comes back with its tray, exactly as it was started.

use desk_config::{ConfigManager, HelperConfig};
use desk_error::{DeskError, DeskResult};
use desk_paths::DeskPaths;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "DeskRemoteAgent";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The command the logon entry runs: the portable agent with its own data folder. It carries
/// explicit arguments, so no administrator prompt appears at every sign-in.
#[must_use]
pub fn portable_run_command(exe: &Path, data_dir: &Path) -> String {
    format!(
        "\"{}\" run --console --portable --data-dir \"{}\"",
        exe.display(),
        data_dir.display()
    )
}

/// Adds (or refreshes) the logon entry for the current user. Idempotent. Does nothing when the
/// user switched "start at sign-in" off in Settings.
pub fn register_portable_autostart(
    paths: &DeskPaths,
    exe: &Path,
    data_dir: &Path,
) -> DeskResult<()> {
    let wanted = ConfigManager::load_or_default::<HelperConfig>(&paths.helper_config_path())
        .map_or(true, |c| c.logon_start);
    if !wanted {
        return Ok(());
    }
    set_logon_command(&portable_run_command(exe, data_dir))
}

/// The command of the logon entry, `None` when the agent does not start at sign-in.
#[must_use]
pub fn logon_command() -> Option<String> {
    let key = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    let mut size = 0u32;
    // SAFETY: the first call only asks for the byte length; strings are NUL-terminated.
    let probe = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if probe.0 != 0 || size < 2 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: `buf` holds `size` bytes as reported by the probe above.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if read.0 != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Removes the logon entry of the current user; `Ok` also when there was none.
pub fn remove_logon_start() -> DeskResult<()> {
    let key = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    // SAFETY: NUL-terminated strings that outlive the call.
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
        )
    };
    // 2 = the value was not there.
    if status.0 == 0 || status.0 == 2 {
        Ok(())
    } else {
        Err(DeskError::Service(format!(
            "cannot remove the logon start (code {})",
            status.0
        )))
    }
}

/// Writes the logon entry with the given command (the one `portable_run_command` builds, or one
/// that was removed before).
pub fn set_logon_command(command: &str) -> DeskResult<()> {
    let command = wide(command);
    let key = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    // SAFETY: all strings are NUL-terminated and outlive the call; the byte length covers the
    // UTF-16 data including the terminator.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(command.as_ptr().cast()),
            (command.len() * 2) as u32,
        )
    };
    if status.0 == 0 {
        Ok(())
    } else {
        Err(DeskError::Service(format!(
            "cannot register the logon start (code {})",
            status.0
        )))
    }
}

/// Turns the portable agent's sign-in start on or off (the Settings switch). Off remembers the
/// removed command in `helper.json` and keeps the agent from registering itself again; on puts
/// that command back, or `fallback` when none was kept.
pub fn set_logon_start(paths: &DeskPaths, on: bool, fallback: &str) -> DeskResult<()> {
    let path = paths.helper_config_path();
    if on {
        let kept = ConfigManager::update::<HelperConfig, _>(&path, |c| c.logon_start = true)?;
        let command = if kept.logon_command.trim().is_empty() {
            fallback
        } else {
            kept.logon_command.as_str()
        };
        set_logon_command(command)
    } else {
        let current = logon_command();
        ConfigManager::update::<HelperConfig, _>(&path, |c| {
            c.logon_start = false;
            if let Some(command) = &current {
                c.logon_command.clone_from(command);
            }
        })?;
        remove_logon_start()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_both_paths_and_keeps_the_portable_flags() {
        let cmd = portable_run_command(
            Path::new(r"C:\Tools\My Agent\DeskRemote-Agent.exe"),
            Path::new(r"C:\Tools\My Agent\DeskRemote-data"),
        );
        assert_eq!(
            cmd,
            r#""C:\Tools\My Agent\DeskRemote-Agent.exe" run --console --portable --data-dir "C:\Tools\My Agent\DeskRemote-data""#
        );
    }
}
