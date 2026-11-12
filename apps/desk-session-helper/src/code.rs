//! Pairing-code dialog: the code, its remaining validity and "new code".
//!
//! Content only; the frame, theme, DPI and buttons live in [`crate::dialog`]. The dialog
//! re-reads the state every second so the countdown runs.

use crate::dialog::{self, Button, Def, Item, Spec, Tone};
use crate::ui::shared;
use desk_i18n::t;

const BUTTON_NEW_CODE: u32 = 1;

static DEF: Def = Def::new(|| t("tray.code_title"), build, on_button, 1000);

fn request_code() {
    if let Some(conn) = shared().and_then(|s| s.conn()) {
        let _ = conn.send(&desk_ipc::IpcMessage::PairingCodeRequest);
    }
}

fn on_button(id: u32) {
    if id == BUTTON_NEW_CODE {
        request_code();
    }
}

fn build() -> Spec {
    let ui = shared().map(|s| s.snapshot()).unwrap_or_default();
    let valid = ui
        .code
        .as_ref()
        .and_then(|c| Some((c, c.remaining())).filter(|(_, left)| !left.is_zero()));
    let items = match valid {
        Some((code, left)) => {
            let secs = left.as_secs();
            vec![
                Item::Code {
                    text: code.code.clone(),
                },
                Item::Text {
                    text: t("tray.code_expires")
                        .replace("{m}", &(secs / 60).to_string())
                        .replace("{s}", &format!("{:02}", secs % 60)),
                    tone: Tone::Muted,
                },
            ]
        }
        None => vec![Item::Text {
            text: if ui.service_online {
                t("tray.code_expired")
            } else {
                t("tray.code_unavailable")
            },
            tone: Tone::Danger,
        }],
    };
    Spec {
        title: t("tray.code_title"),
        subtitle: Some(t("tray.code_hint")),
        items,
        buttons: vec![
            Button {
                id: BUTTON_NEW_CODE,
                label: t("tray.code_new"),
                primary: false,
            },
            Button {
                id: dialog::CLOSE,
                label: t("common.close"),
                primary: true,
            },
        ],
    }
}

/// Opens the code dialog, asking the service for a fresh code when none is valid.
pub(crate) fn show() {
    let has_valid_code = shared()
        .map(|s| s.snapshot())
        .and_then(|ui| ui.code)
        .is_some_and(|c| !c.remaining().is_zero());
    if !has_valid_code {
        request_code();
    }
    dialog::open(&DEF);
}

/// Re-reads an open dialog (called on every state change).
pub(crate) fn refresh() {
    dialog::refresh(&DEF);
}
