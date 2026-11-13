//! Embeds the Desk-Remote icon (DAPP-76 set) and the Task Manager name (DAPP-121) into the
//! executable.
//!
//! The agent is one executable (service + `helper` subcommand), so one build script and one
//! icon source serve the whole agent; the name comes from `desk-exe-identity`.

use std::path::PathBuf;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    println!("cargo:rerun-if-changed=build.rs");
    desk_exe_identity::embed("agent", &PathBuf::from(manifest).join("icons/icon.ico"));
}
