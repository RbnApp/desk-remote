//! "About" dialog: product, version, device ID and identity fingerprint.
//!
//! Content only; the frame, theme, DPI and buttons live in [`crate::dialog`].

use crate::dialog::{self, Button, Def, Item, Spec, Tone};
use crate::ui::shared;
use desk_i18n::t;

static DEF: Def = Def::new(|| t("about.title"), build, |_| {}, 0);

fn build() -> Spec {
    let info = shared()
        .map(|s| s.snapshot())
        .and_then(|ui| ui.info)
        .unwrap_or_default();
    Spec {
        title: t("about.product"),
        subtitle: Some(t("about.version").replace("{v}", desk_version::CURRENT)),
        items: vec![
            Item::Field {
                label: t("about.device_id"),
                value: info.device_id,
            },
            Item::Field {
                label: t("about.fingerprint"),
                value: info.fingerprint,
            },
            Item::Text {
                text: t("about.hint"),
                tone: Tone::Muted,
            },
        ],
        buttons: vec![Button {
            id: dialog::CLOSE,
            label: t("common.close"),
            primary: true,
        }],
    }
}

/// Opens the About dialog (or raises it when already open).
pub fn show() {
    dialog::open(&DEF);
}
