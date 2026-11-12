//! "Connected viewers" dialog: who is watching this PC right now.
//!
//! Content only; the frame, theme, DPI and buttons live in [`crate::dialog`].

use crate::dialog::{self, Button, Def, Item, Spec};
use crate::ui::shared;
use desk_i18n::t;

static DEF: Def = Def::new(|| t("tray.viewers"), build, |_| {}, 0);

fn build() -> Spec {
    let viewers = shared().map(|s| s.snapshot().viewers).unwrap_or_default();
    let (subtitle, items) = if viewers.is_empty() {
        (t("tray.viewers_none"), Vec::new())
    } else {
        (t("tray.viewers_some"), vec![Item::Rows(viewers)])
    };
    Spec {
        title: t("tray.viewers"),
        subtitle: Some(subtitle),
        items,
        buttons: vec![Button {
            id: dialog::CLOSE,
            label: t("common.close"),
            primary: true,
        }],
    }
}

/// Opens the viewers dialog (or refreshes and raises it when already open).
pub fn show() {
    dialog::open(&DEF);
}

/// Re-reads the list of an open dialog (called on every state change).
pub fn refresh() {
    dialog::refresh(&DEF);
}
