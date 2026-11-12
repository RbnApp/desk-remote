//! Embeds the Desk-Remote icon (DAPP-76 set) as resource 1 of the executable.
//!
//! Shared by `desk-session-helper` and `desk-service` (`build = ...` in the service's
//! `Cargo.toml`), so both executables carry the same icon from one source file.

use std::path::PathBuf;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let out = std::env::var("OUT_DIR").unwrap_or_default();
    let icon = PathBuf::from(&manifest).join("../desk-session-helper/icons/icon.ico");
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
