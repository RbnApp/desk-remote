//! Logon start for the portable (double-click) agent (DAPP-119).
//!
//! The installed agent starts through its Windows service (automatic, delayed). The portable
//! agent has no service, so it registers itself under the current user's `Run` key once; after
//! a restart or sign-in it comes back with its tray, exactly as it was started.

use desk_error::{DeskError, DeskResult};
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

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

/// Adds (or refreshes) the logon entry for the current user. Idempotent.
pub fn register_portable_autostart(exe: &Path, data_dir: &Path) -> DeskResult<()> {
    let command = wide(&portable_run_command(exe, data_dir));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_both_paths_and_keeps_the_portable_flags() {
        let cmd = portable_run_command(
            Path::new(r"C:\Tools\My Agent\DeskRemote-Ajan.exe"),
            Path::new(r"C:\Tools\My Agent\DeskRemote-data"),
        );
        assert_eq!(
            cmd,
            r#""C:\Tools\My Agent\DeskRemote-Ajan.exe" run --console --portable --data-dir "C:\Tools\My Agent\DeskRemote-data""#
        );
    }
}
