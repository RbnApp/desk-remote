//! Single source of the name every Desk-Remote executable shows in Task Manager (DAPP-121).
//!
//! Task Manager lists a child process under its app with the executable's `FileDescription`
//! and the executable's icon. `identity.json` is the one place that says what each executable
//! is called; build scripts embed it ([`embed`], feature `embed`), the Go helper's build script
//! reads the same file, and the service registers its display name from it.

use serde::Deserialize;

const RAW: &str = include_str!("../identity.json");

/// One executable's identity.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ExeIdentity {
    /// File name of the executable.
    pub file: String,
    /// Task Manager name: what the executable does.
    pub description: String,
}

#[derive(Deserialize)]
struct Catalog {
    product: String,
    copyright: String,
    exes: std::collections::BTreeMap<String, ExeIdentity>,
}

fn catalog() -> Option<Catalog> {
    serde_json::from_str(RAW).ok()
}

/// Product name shared by every executable (`Desk-Remote`).
#[must_use]
pub fn product() -> String {
    catalog().map(|c| c.product).unwrap_or_default()
}

/// Identity of the executable `key` (`panel`, `agent`, `tailnet`, `relay`).
#[must_use]
pub fn exe(key: &str) -> Option<ExeIdentity> {
    catalog()?.exes.remove(key)
}

/// Task Manager name of the executable `key`; empty for an unknown key.
#[must_use]
pub fn description(key: &str) -> String {
    exe(key).map(|e| e.description).unwrap_or_default()
}

/// Every executable key with its identity, in key order.
#[must_use]
pub fn all() -> Vec<(String, ExeIdentity)> {
    catalog()
        .map(|c| c.exes.into_iter().collect())
        .unwrap_or_default()
}

/// Windows `VERSIONINFO` resource script for the executable `key`, version from `VERSION`.
#[must_use]
pub fn version_script(key: &str) -> Option<String> {
    let catalog = catalog()?;
    let identity = catalog.exes.get(key)?;
    let v = desk_version::SemVer::current()?;
    let (major, minor, patch) = (v.major, v.minor, v.patch);
    Some(format!(
        "1 VERSIONINFO\nFILEVERSION {major},{minor},{patch},0\nPRODUCTVERSION {major},{minor},{patch},0\n\
         FILEOS 0x40004\nFILETYPE 1\n{{\n BLOCK \"StringFileInfo\"\n {{\n  BLOCK \"040904B0\"\n  {{\n\
         \x20  VALUE \"ProductName\", \"{product}\"\n   VALUE \"FileDescription\", \"{desc}\"\n\
         \x20  VALUE \"InternalName\", \"{file}\"\n   VALUE \"OriginalFilename\", \"{file}\"\n\
         \x20  VALUE \"FileVersion\", \"{version}\"\n   VALUE \"ProductVersion\", \"{version}\"\n\
         \x20  VALUE \"LegalCopyright\", \"{copyright}\"\n  }}\n }}\n BLOCK \"VarFileInfo\"\n {{\n\
         \x20 VALUE \"Translation\", 0x0409, 0x04B0\n }}\n}}\n",
        product = catalog.product,
        desc = identity.description,
        file = identity.file,
        version = desk_version::CURRENT,
        copyright = catalog.copyright,
    ))
}

/// Embeds the icon (resource 1) and the version resource of the executable `key` into the
/// binary being built. Call from a `build.rs`.
// Cargo build-script directives are printed to stdout by protocol; this runs only at build time.
#[cfg(feature = "embed")]
#[allow(clippy::print_stdout)]
pub fn embed(key: &str, icon: &std::path::Path) {
    let out = std::env::var("OUT_DIR").unwrap_or_default();
    let rc = std::path::PathBuf::from(out).join(format!("desk-{key}.rc"));
    println!("cargo:rerun-if-changed={}", icon.display());
    println!(
        "cargo:rerun-if-changed={}",
        concat!(env!("CARGO_MANIFEST_DIR"), "/identity.json")
    );
    println!("cargo:rerun-if-changed=../../VERSION");
    let Some(version) = version_script(key) else {
        println!("cargo:warning=no executable identity for '{key}'");
        return;
    };
    let icon_literal = icon.display().to_string().replace('\\', "/");
    if std::fs::write(&rc, format!("1 ICON \"{icon_literal}\"\n{version}")).is_err() {
        println!("cargo:warning=cannot write the resource script");
        return;
    }
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_required()
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_executable_has_a_distinct_description() {
        let all = all();
        assert_eq!(all.len(), 4);
        let mut seen: Vec<String> = all.iter().map(|(_, e)| e.description.clone()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 4, "descriptions must differ");
        assert!(all
            .iter()
            .all(|(_, e)| e.description.starts_with("Desk-Remote")));
    }

    #[test]
    fn panel_description_matches_the_tauri_product_name() {
        let conf = include_str!("../../../apps/desk-panel/tauri.conf.json");
        assert!(conf.contains(&format!("\"productName\": \"{}\"", description("panel"))));
    }

    #[test]
    fn version_script_carries_name_and_version() {
        let rc = version_script("tailnet").unwrap_or_default();
        assert!(rc.contains("FileDescription\", \"Desk-Remote Tailnet Helper"));
        assert!(rc.contains(&format!("FileVersion\", \"{}", desk_version::CURRENT)));
        assert!(version_script("nope").is_none());
    }
}
