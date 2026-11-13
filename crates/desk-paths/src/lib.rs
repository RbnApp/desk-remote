//! Canonical operating system paths and directory contracts for Desk-Remote.

use desk_error::{DeskError, DeskResult};
use std::path::{Path, PathBuf};

const APP_NAME: &str = "DeskRemote";
/// TCP port the PC agent listens on and the panel dials by default.
pub const DEFAULT_PORT: u16 = 47800;
const DEFAULT_SYSTEM_BASE: &str = r"C:\ProgramData";
/// Resource id of the application icon embedded in the service and helper executables.
pub const APP_ICON_RESOURCE_ID: u16 = 1;

/// Subcommand that turns the agent executable into the per-session helper process.
pub const HELPER_SUBCOMMAND: &str = "helper";

/// Flag after `helper` that starts the SYSTEM secure-desktop helper instead of the tray helper.
pub const SECURE_DESKTOP_FLAG: &str = "--secure-desktop";
/// File name of the panel executable (installed next to the Explorer menu extension).
pub const PANEL_EXE_NAME: &str = "desk-panel.exe";
/// Command-line flag with which Explorer entries hand the chosen paths to the panel.
pub const SHELL_SEND_FLAG: &str = "--send";
const DEFAULT_WINDOWS_DIR: &str = r"C:\Windows";
const DEFAULT_USER_BASE: &str = r"C:\Users\Default\AppData\Roaming";
const DEFAULT_PROFILES_DIR: &str = r"C:\Users";
/// File name of the viewer-panel identity, also the proof that a user runs the panel.
const PANEL_IDENTITY_FILE: &str = "panel_identity.key";
/// Sub-folder of Downloads that holds received files.
const RECEIVED_SUBDIR: &str = "Desk-Remote";
/// Machine-wide "this PC is a management panel" marker (written best effort by the panel).
const PANEL_MARKER_FILE: &str = "panel-role.marker";

/// Canonical path layout resolver.
#[derive(Debug, Clone)]
pub struct DeskPaths {
    system_root: PathBuf,
    user_root: PathBuf,
    /// Directory holding every user profile (`C:\Users`); `None` skips the machine-wide scan.
    profiles_root: Option<PathBuf>,
    /// Received files go to the user's real `Downloads\Desk-Remote` even with custom roots
    /// (portable mode); tests keep them inside their own tree.
    downloads_for_received: bool,
}

impl Default for DeskPaths {
    fn default() -> Self {
        let system_root = match std::env::var("ProgramData") {
            Ok(val) if !val.is_empty() => PathBuf::from(val).join(APP_NAME),
            _ => PathBuf::from(DEFAULT_SYSTEM_BASE).join(APP_NAME),
        };

        let user_root = match std::env::var("APPDATA") {
            Ok(val) if !val.is_empty() => PathBuf::from(val).join(APP_NAME),
            _ => PathBuf::from(DEFAULT_USER_BASE).join(APP_NAME),
        };

        let profiles_root = std::env::var_os("PUBLIC")
            .filter(|v| !v.is_empty())
            .and_then(|public| PathBuf::from(public).parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILES_DIR));

        Self {
            system_root,
            user_root,
            profiles_root: Some(profiles_root),
            downloads_for_received: true,
        }
    }
}

impl DeskPaths {
    /// Constructs a path resolver with custom roots (for hermetic testing).
    pub fn custom(system_root: impl AsRef<Path>, user_root: impl AsRef<Path>) -> Self {
        Self {
            system_root: system_root.as_ref().to_path_buf(),
            user_root: user_root.as_ref().to_path_buf(),
            profiles_root: None,
            downloads_for_received: false,
        }
    }

    /// Portable layout next to the executable: data under `dir`, received files still in the
    /// user's `Downloads\Desk-Remote` so the user finds them where every download goes.
    pub fn portable(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        let mut paths = Self::custom(dir.join("system"), dir.join("user"));
        paths.downloads_for_received = true;
        paths
    }

    /// Same as [`DeskPaths::custom`] but with a profiles directory for the panel-PC scan (tests).
    #[must_use]
    pub fn with_profiles_root(mut self, profiles_root: impl AsRef<Path>) -> Self {
        self.profiles_root = Some(profiles_root.as_ref().to_path_buf());
        self
    }

    /// Machine-wide panel-role marker (`<ProgramData>/DeskRemote/panel-role.marker`).
    pub fn panel_marker_path(&self) -> PathBuf {
        self.system_root.join(PANEL_MARKER_FILE)
    }

    /// Single source for "is this PC a management panel?". A PC is a panel when the panel
    /// left the machine-wide marker, or any user profile (this one included) holds a panel
    /// identity. The profile scan keeps the answer correct for the SYSTEM service, which
    /// cannot see another user's `%APPDATA%`, and when the marker could not be written.
    pub fn is_panel_pc(&self) -> bool {
        if self.panel_marker_path().is_file() || self.panel_identity_path().is_file() {
            return true;
        }
        let Some(profiles) = &self.profiles_root else {
            return false;
        };
        let Ok(entries) = std::fs::read_dir(profiles) else {
            return false;
        };
        entries.flatten().any(|profile| {
            profile
                .path()
                .join("AppData")
                .join("Roaming")
                .join(APP_NAME)
                .join(PANEL_IDENTITY_FILE)
                .is_file()
        })
    }

    /// Records that this PC is a management panel (best effort, idempotent).
    pub fn mark_panel_pc(&self) -> DeskResult<()> {
        Self::ensure_dir(&self.system_root)?;
        std::fs::write(self.panel_marker_path(), b"desk-remote panel host\n")
            .map_err(|e| DeskError::Paths(format!("cannot write the panel marker: {e}")))
    }

    /// Machine-wide ProgramData base path for Desk-Remote.
    pub fn system_dir(&self) -> &Path {
        &self.system_root
    }

    /// User-specific AppData base path for Desk-Remote.
    pub fn user_dir(&self) -> &Path {
        &self.user_root
    }

    /// System-wide log directory (`<ProgramData>/DeskRemote/logs`).
    pub fn system_log_dir(&self) -> PathBuf {
        self.system_root.join("logs")
    }

    /// User-specific log directory (`<AppData>/DeskRemote/logs`).
    pub fn user_log_dir(&self) -> PathBuf {
        self.user_root.join("logs")
    }

    /// System configuration file path (`<ProgramData>/DeskRemote/service.json`).
    pub fn service_config_path(&self) -> PathBuf {
        self.system_root.join("service.json")
    }

    /// User configuration file path (`<AppData>/DeskRemote/panel.json`).
    pub fn panel_config_path(&self) -> PathBuf {
        self.user_root.join("panel.json")
    }

    /// Relay configuration file path (`<ProgramData>/DeskRemote/relay.json`).
    pub fn relay_config_path(&self) -> PathBuf {
        self.system_root.join("relay.json")
    }

    /// Identity keypair file path (`<ProgramData>/DeskRemote/identity.key`).
    pub fn identity_key_path(&self) -> PathBuf {
        self.system_root.join("identity.key")
    }

    /// Viewer-panel identity keypair (`<AppData>/DeskRemote/panel_identity.key`, DPAPI).
    pub fn panel_identity_path(&self) -> PathBuf {
        self.user_root.join("panel_identity.key")
    }

    /// Trusted viewer-panel list on the host (`<ProgramData>/DeskRemote/trusted_peers.dat`, DPAPI).
    pub fn trusted_peers_path(&self) -> PathBuf {
        self.system_root.join("trusted_peers.dat")
    }

    /// Trusted host list on a viewer panel (`<AppData>/DeskRemote/trusted_hosts.dat`, DPAPI).
    pub fn trusted_hosts_path(&self) -> PathBuf {
        self.user_root.join("trusted_hosts.dat")
    }

    /// Folder received files land in: `<Downloads>\Desk-Remote`. Resolved in the
    /// calling user's context (the session helper or the panel, never the SYSTEM service).
    /// Uses SHGetKnownFolderPath(FOLDERID_Downloads) on Windows (supports redirected Downloads),
    /// falling back to USERPROFILE\Downloads.
    /// Custom (test) roots keep it inside their own tree.
    #[must_use]
    pub fn received_files_dir(&self) -> PathBuf {
        if !self.downloads_for_received {
            return self.user_root.join("received");
        }
        #[cfg(windows)]
        {
            if let Some(dir) = downloads_dir() {
                return dir.join(RECEIVED_SUBDIR);
            }
        }
        let profile = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty());
        match profile {
            Some(profile) => PathBuf::from(profile)
                .join("Downloads")
                .join(RECEIVED_SUBDIR),
            None => self.user_root.join("received"),
        }
    }

    /// Ensures that the specified directory exists, creating parent directories if needed.
    pub fn ensure_dir(dir: &Path) -> DeskResult<()> {
        std::fs::create_dir_all(dir).map_err(|e| {
            DeskError::Paths(format!(
                "Failed to create directory {}: {}",
                dir.display(),
                e
            ))
        })
    }

    /// Path of the running executable (the agent is one binary: service and `helper` subcommand).
    pub fn current_exe() -> DeskResult<PathBuf> {
        std::env::current_exe()
            .map_err(|e| DeskError::Paths(format!("cannot resolve current executable: {e}")))
    }

    /// Path of an executable that ships next to the running binary (for example the
    /// session helper next to the service).
    pub fn sibling_exe(file_name: &str) -> DeskResult<PathBuf> {
        let exe = std::env::current_exe()
            .map_err(|e| DeskError::Paths(format!("cannot resolve current executable: {e}")))?;
        let dir = exe
            .parent()
            .ok_or_else(|| DeskError::Paths("current executable has no parent".to_string()))?;
        Ok(dir.join(file_name))
    }

    /// Full path of a Windows system tool (`%SystemRoot%\System32\<name>`), so a planted
    /// executable earlier on `PATH` or in the working directory can never be started instead.
    #[must_use]
    pub fn system_tool(file_name: &str) -> PathBuf {
        let root = match std::env::var_os("SystemRoot") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => PathBuf::from(DEFAULT_WINDOWS_DIR),
        };
        root.join("System32").join(file_name)
    }

    /// Full path where the embedded tailnet helper is extracted: `<system>/bin/desk-tailnet-helper.exe`.
    #[must_use]
    pub fn tailnet_helper_path(&self) -> PathBuf {
        self.system_root.join("bin").join("desk-tailnet-helper.exe")
    }

    /// Where the agent service keeps a downloaded installer: `<system>/updates`. The system
    /// folder is writable by SYSTEM and Administrators only, so a verified installer cannot be
    /// swapped by a normal user before it runs.
    #[must_use]
    pub fn agent_update_dir(&self) -> PathBuf {
        self.system_root.join("updates")
    }

    /// Where the panel keeps a downloaded installer: `<AppData>/DeskRemote/updates`.
    #[must_use]
    pub fn panel_update_dir(&self) -> PathBuf {
        self.user_root.join("updates")
    }

    /// Node state of the panel's own tailnet node (`<AppData>/DeskRemote/tailscale`).
    #[must_use]
    pub fn panel_tailnet_dir(&self) -> PathBuf {
        self.user_root.join("tailscale")
    }

    /// Where the panel extracts its copy of the embedded tailnet helper
    /// (`<AppData>/DeskRemote/bin/desk-tailnet-helper.exe`).
    #[must_use]
    pub fn panel_tailnet_helper_path(&self) -> PathBuf {
        self.user_root.join("bin").join("desk-tailnet-helper.exe")
    }

    /// Record of what the tailnet hand-over changed on the official Tailscale client
    /// (`<system>/takeover/official-tailscale.json`). The folder is admin-only
    /// ([`restrict_to_admins`]); a user-writable record would let any user cancel the restore.
    #[must_use]
    pub fn takeover_record_path(&self) -> PathBuf {
        self.system_root
            .join("takeover")
            .join("official-tailscale.json")
    }

    /// Creates the machine-wide data folder and locks it down: SYSTEM and Administrators
    /// have full control, ordinary users may only read and list (`service.json`, logs).
    /// Without this, `C:\ProgramData\DeskRemote` inherits "Users can create and delete files"
    /// and any user could replace the identity, the trust list, the limits or the logs.
    pub fn secure_system_dir(&self) -> DeskResult<()> {
        Self::ensure_dir(&self.system_root)?;
        apply_acl(
            &self.system_root,
            &[ACE_SYSTEM_DIR, ACE_ADMINS_DIR, ACE_USERS_READ_DIR],
        )
    }

    /// Windows Named Pipe name for the central service.
    pub fn pipe_service() -> &'static str {
        r"\\.\pipe\DeskRemote-Service"
    }

    /// Windows Named Pipe name for the SYSTEM helper that serves a session's secure desktop
    /// (UAC prompt, lock screen).
    pub fn pipe_secure_helper(session_id: u32) -> String {
        format!(r"\\.\pipe\DeskRemote-SecureHelper-{}", session_id)
    }

    /// Windows Named Pipe name for an interactive session helper.
    pub fn pipe_session_helper(session_id: u32) -> String {
        format!(r"\\.\pipe\DeskRemote-Helper-{}", session_id)
    }
}

#[cfg(windows)]
fn downloads_dir() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
    // SAFETY: calls SHGetKnownFolderPath and frees the allocated buffer with CoTaskMemFree.
    unsafe {
        let path_ptr =
            SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let path_str = path_ptr.to_string().ok()?;
        CoTaskMemFree(Some(path_ptr.as_ptr().cast()));
        if path_str.is_empty() {
            None
        } else {
            Some(PathBuf::from(path_str))
        }
    }
}

/// Win32 `CREATE_NO_WINDOW` process-creation flag: the child never gets a console window.
/// The single definition for every process the product starts.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Builds a [`std::process::Command`] that can never show a console window (Windows:
/// `CREATE_NO_WINDOW`; other platforms: plain command). EVERY process the product starts
/// must be created through this function.
#[must_use]
pub fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    hide_window(&mut cmd);
    cmd
}

/// Applies the no-console flag to an existing command (use when a command is built elsewhere).
pub fn hide_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `icacls` grant strings with well-known SIDs (language neutral).
const ACE_SYSTEM_DIR: &str = "*S-1-5-18:(OI)(CI)F";
const ACE_ADMINS_DIR: &str = "*S-1-5-32-544:(OI)(CI)F";
const ACE_USERS_READ_DIR: &str = "*S-1-5-32-545:(OI)(CI)RX";
const ACE_SYSTEM_FILE: &str = "*S-1-5-18:F";
const ACE_ADMINS_FILE: &str = "*S-1-5-32-544:F";

/// Replaces the ACL of `target` (inheritance cut) with exactly `grants`.
fn apply_acl(target: &Path, grants: &[&str]) -> DeskResult<()> {
    let mut cmd = hidden_command(DeskPaths::system_tool("icacls.exe"));
    cmd.arg(target).arg("/inheritance:r");
    for grant in grants {
        cmd.args(["/grant:r", grant]);
    }
    let out = cmd.output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(DeskError::Paths(format!(
            "icacls failed for {}: {}",
            target.display(),
            String::from_utf8_lossy(&out.stdout).trim()
        )))
    }
}

/// Folder ACL: full control for SYSTEM and Administrators only, inherited by new children
/// (secrets such as the tailnet auth key and the takeover record live in such folders).
pub fn restrict_to_admins(dir: &Path) -> DeskResult<()> {
    apply_acl(dir, &[ACE_SYSTEM_DIR, ACE_ADMINS_DIR])
}

/// Folder ACL for a per-user (portable) run: the given user SID and SYSTEM only.
pub fn restrict_to_user(dir: &Path, user_sid: &str) -> DeskResult<()> {
    let grant = format!("*{user_sid}:(OI)(CI)F");
    apply_acl(dir, &[ACE_SYSTEM_DIR, &grant])
}

/// File ACL: full control for SYSTEM and Administrators only.
pub fn restrict_file_to_admins(file: &Path) -> DeskResult<()> {
    apply_acl(file, &[ACE_SYSTEM_FILE, ACE_ADMINS_FILE])
}

/// Convenience function returning the default canonical paths.
pub fn canonical() -> DeskPaths {
    DeskPaths::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acl_lines(dir: &Path) -> Vec<String> {
        let out = hidden_command(DeskPaths::system_tool("icacls.exe"))
            .arg(dir)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let text = out.replace(&dir.display().to_string(), "");
        text.lines()
            .filter(|l| l.contains(":("))
            .map(|l| l.trim().to_string())
            .collect()
    }

    /// DAPP-103 M-07: the data folder ends up SYSTEM/Administrators full + Users read-only,
    /// and a child created before the call loses its inherited user access too.
    #[test]
    fn secure_system_dir_locks_the_data_folder_and_its_children() -> DeskResult<()> {
        let tmp = tempfile::tempdir().map_err(DeskError::Io)?;
        let root = tmp.path().join("DeskRemote");
        let paths = DeskPaths::custom(&root, tmp.path().join("user"));
        DeskPaths::ensure_dir(&root)?;
        let child = root.join("service.json");
        std::fs::write(&child, "{}")?;
        let before = acl_lines(&root);
        assert!(
            before.len() > 3,
            "fixture must start with broader access: {before:?}"
        );
        paths.secure_system_dir()?;
        let after = acl_lines(&root);
        let child_after = acl_lines(&child);
        assert_eq!(after.len(), 3, "{after:?}");
        let has = |lines: &[String], names: &[&str]| {
            lines.iter().any(|l| names.iter().any(|n| l.contains(n)))
        };
        assert!(has(&after, &["SYSTEM", "S-1-5-18"]));
        assert!(has(&after, &["Administrators", "S-1-5-32-544"]));
        let users: Vec<_> = after
            .iter()
            .filter(|l| l.contains("Users") || l.contains("S-1-5-32-545"))
            .collect();
        assert_eq!(users.len(), 1, "{after:?}");
        assert!(
            !users[0].contains("(F)") && !users[0].contains("(M)") && !users[0].contains("(W)")
        );
        assert_eq!(
            child_after.len(),
            3,
            "child must follow the folder: {child_after:?}"
        );
        // Idempotent: a second run leaves the same ACL.
        paths.secure_system_dir()?;
        assert_eq!(acl_lines(&root), after);
        Ok(())
    }

    #[test]
    fn takeover_record_lives_in_its_own_folder() {
        let paths = DeskPaths::custom(PathBuf::from("sys"), PathBuf::from("usr"));
        assert_eq!(
            paths.takeover_record_path(),
            PathBuf::from("sys")
                .join("takeover")
                .join("official-tailscale.json")
        );
    }

    #[test]
    fn test_custom_paths_and_structure() {
        let root = PathBuf::from(r"C:\test_data");
        let user = PathBuf::from(r"C:\test_user");
        let paths = DeskPaths::custom(&root, &user);

        assert_eq!(paths.system_dir(), &root);
        assert_eq!(paths.user_dir(), &user);
        assert_eq!(paths.system_log_dir(), root.join("logs"));
        assert_eq!(paths.user_log_dir(), user.join("logs"));
        assert_eq!(paths.service_config_path(), root.join("service.json"));
        assert_eq!(paths.panel_config_path(), user.join("panel.json"));
        assert_eq!(paths.relay_config_path(), root.join("relay.json"));
        assert_eq!(paths.identity_key_path(), root.join("identity.key"));
    }

    #[test]
    fn panel_pc_detection_uses_marker_own_identity_and_other_profiles() -> DeskResult<()> {
        let dir = std::env::temp_dir().join(format!("desk-paths-role-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fresh = || {
            DeskPaths::custom(dir.join("system"), dir.join("me"))
                .with_profiles_root(dir.join("Users"))
        };
        std::fs::create_dir_all(dir.join("Users"))?;
        assert!(!fresh().is_panel_pc());

        let other = dir
            .join("Users")
            .join("bob")
            .join("AppData")
            .join("Roaming")
            .join(APP_NAME);
        std::fs::create_dir_all(&other)?;
        std::fs::write(other.join(PANEL_IDENTITY_FILE), b"x")?;
        assert!(fresh().is_panel_pc(), "another profile's identity counts");
        std::fs::remove_dir_all(dir.join("Users").join("bob"))?;
        assert!(!fresh().is_panel_pc());

        fresh().mark_panel_pc()?;
        fresh().mark_panel_pc()?;
        assert!(fresh().is_panel_pc(), "marker counts");
        std::fs::remove_file(fresh().panel_marker_path())?;

        std::fs::create_dir_all(dir.join("me"))?;
        std::fs::write(fresh().panel_identity_path(), b"x")?;
        assert!(fresh().is_panel_pc(), "own identity counts");
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn custom_roots_never_scan_real_profiles() {
        let paths = DeskPaths::custom(r"Z:\no-such\system", r"Z:\no-such\user");
        assert!(!paths.is_panel_pc());
    }

    #[test]
    fn system_tools_resolve_to_an_absolute_system32_path() {
        let sc = DeskPaths::system_tool("sc.exe");
        assert!(sc.is_absolute(), "{sc:?}");
        assert!(sc.ends_with(r"System32\sc.exe"), "{sc:?}");
        assert!(sc.exists(), "{sc:?}");
    }

    #[test]
    fn received_files_land_in_downloads_for_real_roots_and_in_the_tree_for_test_roots() {
        let custom = DeskPaths::custom(r"Z:\s", r"Z:\u");
        assert_eq!(custom.received_files_dir(), PathBuf::from(r"Z:\u\received"));
        let real = DeskPaths::default().received_files_dir();
        assert!(real.ends_with(r"Downloads\Desk-Remote") || real.ends_with("received"));
        let portable = DeskPaths::portable(r"Z:\p");
        assert!(portable
            .received_files_dir()
            .ends_with(r"Downloads\Desk-Remote"));
        assert!(portable.system_log_dir().starts_with(r"Z:\p\system"));
    }

    #[test]
    fn test_named_pipe_names() {
        assert_eq!(DeskPaths::pipe_service(), r"\\.\pipe\DeskRemote-Service");
        assert_eq!(
            DeskPaths::pipe_session_helper(1),
            r"\\.\pipe\DeskRemote-Helper-1"
        );
        assert_eq!(
            DeskPaths::pipe_session_helper(42),
            r"\\.\pipe\DeskRemote-Helper-42"
        );
    }
}
