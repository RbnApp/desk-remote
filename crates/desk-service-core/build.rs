//! Builds the Go `desk-tailnet-helper.exe` (icon + Task Manager name included) so it can be
//! embedded into the agent executable.
//!
//! The Go build lives in `scripts/build-tailnet-helper.ps1`, the single place that stamps the
//! helper; a previously built exe is used only when that script cannot run.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
    let root = PathBuf::from(&manifest).join("../..");
    let helper_dir = root.join("apps/desk-tailnet-helper");
    let out_exe = PathBuf::from(out_dir).join("desk-tailnet-helper.exe");

    for watched in [
        "apps/desk-tailnet-helper/main.go",
        "apps/desk-tailnet-helper/panel.go",
        "apps/desk-tailnet-helper/go.mod",
        "apps/desk-tailnet-helper/go.sum",
        "scripts/build-tailnet-helper.ps1",
        "crates/desk-exe-identity/identity.json",
        "VERSION",
    ] {
        println!("cargo:rerun-if-changed=../../{watched}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    let built = helper_dir.join("desk-tailnet-helper.exe");
    let script = root.join("scripts/build-tailnet-helper.ps1");
    let stamped = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .status()
        .is_ok_and(|s| s.success());
    if (stamped || built.exists()) && std::fs::copy(&built, &out_exe).is_ok() {
        return;
    }

    if !out_exe.exists() {
        let _ = std::fs::write(&out_exe, b"");
    }
}
