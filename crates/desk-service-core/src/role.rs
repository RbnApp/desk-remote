//! Role guard: a PC is either the watcher (management panel) or the watched (PC agent),
//! never both. The decision itself lives in `DeskPaths::is_panel_pc`; this module only
//! applies it to the agent's install and run paths.

use desk_error::{DeskError, DeskResult};
use desk_i18n::{translate, Locale};
use desk_paths::DeskPaths;
use windows_service::service::ServiceAccess;
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const REFUSED_KEY: &str = "role.panel_pc_no_agent";

/// True when the agent host may run: not a panel PC, or the developer override is set.
#[must_use]
pub fn host_allowed(paths: &DeskPaths, allow_on_panel_pc: bool) -> bool {
    allow_on_panel_pc || !paths.is_panel_pc()
}

/// Refuses to install the agent on a panel PC. Needs no administrator rights, so it runs
/// before anything touches the Service Control Manager.
pub fn refuse_install_on_panel_pc(paths: &DeskPaths) -> DeskResult<()> {
    if host_allowed(paths, false) {
        return Ok(());
    }
    Err(DeskError::Service(format!(
        "{} / {}",
        translate(REFUSED_KEY, Locale::En),
        translate(REFUSED_KEY, Locale::Tr)
    )))
}

/// True when the agent service is registered on this PC (any state). Readable by a
/// standard user; used by the panel to warn about a misplaced agent.
#[must_use]
pub fn agent_service_installed() -> bool {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|m| m.open_service(crate::SERVICE_NAME, ServiceAccess::QUERY_STATUS))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_paths(name: &str) -> (DeskPaths, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("desk-role-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = DeskPaths::custom(dir.join("system"), dir.join("user"));
        (paths, dir)
    }

    #[test]
    fn plain_pc_allows_the_agent_and_install() {
        let (paths, dir) = panel_paths("plain");
        assert!(host_allowed(&paths, false));
        assert!(refuse_install_on_panel_pc(&paths).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn panel_pc_refuses_install_with_both_languages() -> DeskResult<()> {
        let (paths, dir) = panel_paths("install");
        paths.mark_panel_pc()?;
        let msg = match refuse_install_on_panel_pc(&paths) {
            Err(e) => e.to_string(),
            Ok(()) => String::new(),
        };
        assert!(msg.contains("PC agent is not installed here"), "{msg}");
        assert!(msg.contains("PC ajanı buraya kurulmaz"), "{msg}");
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }

    #[test]
    fn panel_pc_blocks_host_unless_developer_flag() -> DeskResult<()> {
        let (paths, dir) = panel_paths("run");
        paths.mark_panel_pc()?;
        assert!(!host_allowed(&paths, false));
        assert!(host_allowed(&paths, true));
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }
}
