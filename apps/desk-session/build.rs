//! Embeds the Desk-Remote icon and the Task Manager name ("Desk-Remote Session Helper") into
//! the session helper executable; the name comes from `desk-exe-identity`.

use std::path::PathBuf;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    println!("cargo:rerun-if-changed=build.rs");
    let icon = PathBuf::from(manifest).join("../desk-service/icons/icon.ico");
    desk_exe_identity::embed("session", &icon);
}
