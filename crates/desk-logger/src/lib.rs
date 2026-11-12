//! Unified structured logging system for Desk-Remote.
//!
//! Exclusively configures tracing subscribers, formatting, log rotation, and sinks.

use desk_error::{DeskError, DeskResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter::EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Logging severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_filter_str(&self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Structured logger configuration contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggerConfig {
    pub level: LogLevel,
    pub log_dir: Option<PathBuf>,
    pub file_name_prefix: String,
    pub console_output: bool,
    pub json_format: bool,
}

impl Default for LoggerConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            log_dir: None,
            file_name_prefix: "desk-remote".to_string(),
            console_output: true,
            json_format: false,
        }
    }
}

/// Initializes the global tracing subscriber according to the given configuration.
///
/// Returns an optional WorkerGuard that flushes buffered logs on drop.
pub fn init(config: &LoggerConfig) -> DeskResult<Option<WorkerGuard>> {
    let env_filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::new(config.level.as_filter_str()),
    };

    let registry = tracing_subscriber::registry().with(env_filter);

    if let Some(ref dir) = config.log_dir {
        desk_paths::DeskPaths::ensure_dir(dir)?;
        let file_appender = tracing_appender::rolling::daily(dir, &config.file_name_prefix);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        if config.json_format {
            let file_layer = fmt::layer().json().with_writer(non_blocking);
            registry
                .with(file_layer)
                .try_init()
                .map_err(|e| DeskError::Other(format!("Logger init error: {}", e)))?;
        } else {
            let file_layer = fmt::layer().with_ansi(false).with_writer(non_blocking);
            registry
                .with(file_layer)
                .try_init()
                .map_err(|e| DeskError::Other(format!("Logger init error: {}", e)))?;
        }

        Ok(Some(guard))
    } else {
        let stdout_layer = fmt::layer().with_ansi(true);
        registry
            .with(stdout_layer)
            .try_init()
            .map_err(|e| DeskError::Other(format!("Logger init error: {}", e)))?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logger_config_defaults() {
        let config = LoggerConfig::default();
        assert_eq!(config.level, LogLevel::Info);
        assert_eq!(config.file_name_prefix, "desk-remote");
        assert!(config.console_output);
        assert!(!config.json_format);
    }

    #[test]
    fn test_log_level_filter() {
        assert_eq!(LogLevel::Trace.as_filter_str(), "trace");
        assert_eq!(LogLevel::Debug.as_filter_str(), "debug");
        assert_eq!(LogLevel::Info.as_filter_str(), "info");
        assert_eq!(LogLevel::Warn.as_filter_str(), "warn");
        assert_eq!(LogLevel::Error.as_filter_str(), "error");
    }

    #[test]
    fn test_file_appender_setup() {
        let temp_dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(e) => panic!("failed to create temp dir: {}", e),
        };

        let config = LoggerConfig {
            log_dir: Some(temp_dir.path().to_path_buf()),
            file_name_prefix: "test-log".to_string(),
            ..Default::default()
        };
        assert_eq!(config.file_name_prefix, "test-log");

        // Testing directory creation through DeskPaths
        let ensure_result = desk_paths::DeskPaths::ensure_dir(temp_dir.path());
        assert!(ensure_result.is_ok());
    }
}
