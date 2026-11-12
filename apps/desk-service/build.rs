//! Embeds the Desk-Remote icon (DAPP-76 set) as resource 1 of the executable.
//!
//! The agent is one executable (service + `helper` subcommand), so one build script and one
//! icon source serve the whole agent.

use std::path::PathBuf;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let out = std::env::var("OUT_DIR").unwrap_or_default();
    let icon = PathBuf::from(&manifest).join("icons/icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed=build.rs");
    let rc = PathBuf::from(out).join("desk-icon.rc");
    let icon_literal = icon.display().to_string().replace('\\', "/");
    if std::fs::write(&rc, format!("1 ICON \"{icon_literal}\"\n")).is_err() {
        println!("cargo:warning=cannot write the icon resource script");
        return;
    }
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_required()
        .ok();
}
