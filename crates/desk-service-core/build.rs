//! Builds or copies the Go `desk-tailnet-helper.exe` so it can be embedded into the agent executable.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
    let helper_dir = PathBuf::from(&manifest).join("../../apps/desk-tailnet-helper");
    let out_exe = PathBuf::from(out_dir).join("desk-tailnet-helper.exe");

    println!("cargo:rerun-if-changed=../../apps/desk-tailnet-helper/main.go");
    println!("cargo:rerun-if-changed=../../apps/desk-tailnet-helper/go.mod");
    println!("cargo:rerun-if-changed=../../apps/desk-tailnet-helper/go.sum");
    println!("cargo:rerun-if-changed=build.rs");

    let prebuilt = helper_dir.join("desk-tailnet-helper.exe");
    if prebuilt.exists() && std::fs::copy(&prebuilt, &out_exe).is_ok() {
        return;
    }

    let status = Command::new("go")
        .args(["build", "-ldflags=-s -w", "-o"])
        .arg(&out_exe)
        .arg(".")
        .current_dir(&helper_dir)
        .status();

    if let Ok(s) = status {
        if s.success() {
            return;
        }
    }

    if !out_exe.exists() {
        let _ = std::fs::write(&out_exe, b"");
    }
}
