//! Embeds the Desk-Remote icon and the Task Manager name (DAPP-121) into the relay executable.

use std::path::PathBuf;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    println!("cargo:rerun-if-changed=build.rs");
    desk_exe_identity::embed(
        "relay",
        &PathBuf::from(manifest).join("../desk-service/icons/icon.ico"),
    );
}
