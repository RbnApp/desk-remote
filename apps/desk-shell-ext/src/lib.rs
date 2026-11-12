//! Windows 11 first-level Explorer entry "Send with Desk-Remote".
//!
//! Windows 11 lists classic registry verbs only under "Show more options". A verb in the
//! top-level menu must be an `IExplorerCommand` COM class declared by a (sparse) package
//! identity (`packaging/AppxManifest.xml.in`). This DLL is that class: it only collects the
//! selected paths and starts `desk-panel.exe --send <paths>` next to it; the panel's
//! single-instance hand-over and the PC picker do the rest (`apps/desk-panel/src/shell_menu.rs`).

#[cfg(windows)]
mod command;

#[cfg(windows)]
pub use command::{DllCanUnloadNow, DllGetClassObject, CLSID_DESK_REMOTE_SEND};

/// Menu text for a BCP-47 locale name such as `tr-TR` (same catalog as the classic entry).
#[must_use]
pub fn menu_title(locale_name: &str) -> String {
    desk_i18n::translate(
        "shell_menu.send",
        desk_i18n::Locale::from_str_loose(locale_name),
    )
}

/// Argument list for the panel: `--send <path>...`.
#[must_use]
pub fn panel_args(paths: &[String]) -> Vec<String> {
    let mut args = Vec::with_capacity(paths.len() + 1);
    args.push(desk_paths::SHELL_SEND_FLAG.to_string());
    args.extend(paths.iter().filter(|p| !p.is_empty()).cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_follows_locale() {
        assert_eq!(menu_title("tr-TR"), "Desk-Remote ile gönder");
        assert_eq!(menu_title("en-US"), "Send with Desk-Remote");
    }

    #[test]
    fn args_start_with_the_send_flag_and_skip_empty_paths() {
        let args = panel_args(&[r"C:\a b\c.txt".to_string(), String::new()]);
        assert_eq!(
            args,
            vec!["--send".to_string(), r"C:\a b\c.txt".to_string()]
        );
    }
}
