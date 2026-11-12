//! Centralized configuration management with schema validation and atomic persistence.

pub mod model;

use desk_error::{DeskError, DeskResult};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

pub use model::{
    clamp_upload_mb, PairedPc, PanelConfig, RelayConfig, ServiceConfig, DEFAULT_MAX_UPLOAD_MB,
    MAX_UPLOAD_MB_CEILING,
};

/// Writes string content to a file atomically via a temporary swap file.
pub fn write_atomic(path: &Path, content: &str) -> DeskResult<()> {
    if let Some(parent) = path.parent() {
        desk_paths::DeskPaths::ensure_dir(parent)?;
    }

    let tmp_path = path.with_extension("tmp");

    {
        let mut file = File::create(&tmp_path).map_err(|e| {
            DeskError::Config(format!(
                "Failed to create temp config {}: {}",
                tmp_path.display(),
                e
            ))
        })?;

        file.write_all(content.as_bytes()).map_err(|e| {
            DeskError::Config(format!(
                "Failed to write temp config {}: {}",
                tmp_path.display(),
                e
            ))
        })?;

        file.sync_all().map_err(|e| {
            DeskError::Config(format!(
                "Failed to flush temp config {}: {}",
                tmp_path.display(),
                e
            ))
        })?;
    }

    // On Windows, rename fails if destination exists in some scenarios, so we remove dest if present.
    if path.exists() {
        let _ = fs::remove_file(path);
    }

    fs::rename(&tmp_path, path).map_err(|e| {
        DeskError::Config(format!(
            "Failed to rename temp config {} to {}: {}",
            tmp_path.display(),
            path.display(),
            e
        ))
    })?;

    Ok(())
}

/// Generic configuration manager supporting atomic persistence and schema validation.
pub struct ConfigManager;

impl ConfigManager {
    /// Loads configuration from path or writes defaults if file does not exist.
    pub fn load_or_default<T>(path: &Path) -> DeskResult<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        if !path.exists() {
            let default_cfg = T::default();
            Self::save(path, &default_cfg)?;
            return Ok(default_cfg);
        }

        let content = fs::read_to_string(path).map_err(|e| {
            DeskError::Config(format!(
                "Failed to read config file {}: {}",
                path.display(),
                e
            ))
        })?;

        let parsed: T = serde_json::from_str(&content).map_err(|e| {
            DeskError::Config(format!("Invalid JSON schema in {}: {}", path.display(), e))
        })?;

        Ok(parsed)
    }

    /// Atomically persists configuration to disk in formatted JSON.
    pub fn save<T>(path: &Path, config: &T) -> DeskResult<()>
    where
        T: Serialize,
    {
        let json_str = serde_json::to_string_pretty(config)
            .map_err(|e| DeskError::Serialization(format!("Config serialization failed: {}", e)))?;

        write_atomic(path, &json_str)
    }

    /// Loads, mutates via closure, and persists configuration atomically.
    pub fn update<T, F>(path: &Path, mut f: F) -> DeskResult<T>
    where
        T: Serialize + DeserializeOwned + Default,
        F: FnMut(&mut T),
    {
        let mut cfg = Self::load_or_default::<T>(path)?;
        f(&mut cfg);
        Self::save(path, &cfg)?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panel_config_defaults_and_sanitize() {
        let mut cfg = PanelConfig::default();
        assert_eq!(cfg.palette, "ink");
        assert_eq!(cfg.mode, "system");

        cfg.palette = "invalid_palette".to_string();
        cfg.mode = "invalid_mode".to_string();
        cfg.sanitize();

        assert_eq!(cfg.palette, "ink");
        assert_eq!(cfg.mode, "system");

        cfg.palette = "petrol".to_string();
        cfg.mode = "dark".to_string();
        cfg.sanitize();

        assert_eq!(cfg.palette, "petrol");
        assert_eq!(cfg.mode, "dark");
    }

    #[test]
    fn upload_limit_defaults_to_100_mb_and_is_clamped() {
        assert_eq!(PanelConfig::default().max_upload_mb, 100);
        assert_eq!(ServiceConfig::default().max_upload_mb, 100);
        assert_eq!(clamp_upload_mb(0), 100);
        assert_eq!(clamp_upload_mb(1), 1);
        assert_eq!(clamp_upload_mb(1024), 1024);
        assert_eq!(clamp_upload_mb(5000), 1024);
        // An older file without the key keeps working and gets the default.
        let old: PanelConfig = serde_json::from_str(r#"{"palette":"ink"}"#).unwrap_or_default();
        assert_eq!(old.max_upload_mb, 100);
        let mut cfg = PanelConfig {
            max_upload_mb: 9999,
            ..PanelConfig::default()
        };
        cfg.sanitize();
        assert_eq!(cfg.max_upload_mb, 1024);
    }

    #[test]
    fn test_atomic_write_and_load() {
        let temp_dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(e) => panic!("failed to create temp dir: {}", e),
        };

        let config_file = temp_dir.path().join("panel.json");

        // Load or default (should create with defaults)
        let loaded: PanelConfig = match ConfigManager::load_or_default(&config_file) {
            Ok(cfg) => cfg,
            Err(e) => panic!("failed to load config: {}", e),
        };
        assert_eq!(loaded, PanelConfig::default());
        assert!(config_file.exists());

        // Update config
        let updated = ConfigManager::update(&config_file, |cfg: &mut PanelConfig| {
            cfg.palette = "petrol".to_string();
            cfg.mode = "dark".to_string();
        });
        assert!(updated.is_ok());

        // Verify saved content
        let reloaded: PanelConfig = match ConfigManager::load_or_default(&config_file) {
            Ok(cfg) => cfg,
            Err(e) => panic!("failed to reload config: {}", e),
        };
        assert_eq!(reloaded.palette, "petrol");
        assert_eq!(reloaded.mode, "dark");
    }
}
