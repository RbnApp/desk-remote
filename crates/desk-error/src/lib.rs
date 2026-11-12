//! Common error types and error codes for Desk-Remote.

use thiserror::Error;

/// Central result type for Desk-Remote operations.
pub type DeskResult<T> = Result<T, DeskError>;

/// Unified error enum for all Desk-Remote subsystems.
#[derive(Debug, Error)]
pub enum DeskError {
    /// Input/Output failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON / format serialization or deserialization failure.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Configuration parsing, validation, or persistence failure.
    #[error("Configuration error: {0}")]
    Config(String),

    /// Canonical path resolution failure.
    #[error("Path error: {0}")]
    Paths(String),

    /// Theme resolution or validation failure.
    #[error("Theme error: {0}")]
    Theme(String),

    /// Localization lookup or catalog failure.
    #[error("Localization error: {0}")]
    I18n(String),

    /// Network transport, WebRTC, signaling, or connection failure.
    #[error("Transport error: {0}")]
    Transport(String),

    /// Binary framing, packet decoding, or protocol mismatch.
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Key generation, signature validation, or decryption failure.
    #[error("Crypto error: {0}")]
    Crypto(String),

    /// Named Pipe IPC connection or message failure.
    #[error("IPC error: {0}")]
    Ipc(String),

    /// Windows service lifecycle or session dispatcher failure.
    #[error("Service error: {0}")]
    Service(String),

    /// Screen capture initialization or frame grabbing failure.
    #[error("Capture error: {0}")]
    Capture(String),

    /// Video encoding or decoding failure.
    #[error("Codec error: {0}")]
    Codec(String),

    /// Input event translation or injection failure.
    #[error("Input error: {0}")]
    Input(String),

    /// General unclassified error.
    #[error("Internal error: {0}")]
    Other(String),
}

impl DeskError {
    /// Returns the canonical alphanumeric error code for the error variant.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Io(_) => "ERR_IO",
            Self::Serialization(_) => "ERR_SERIALIZATION",
            Self::Config(_) => "ERR_CONFIG",
            Self::Paths(_) => "ERR_PATHS",
            Self::Theme(_) => "ERR_THEME",
            Self::I18n(_) => "ERR_I18N",
            Self::Transport(_) => "ERR_TRANSPORT",
            Self::Protocol(_) => "ERR_PROTOCOL",
            Self::Crypto(_) => "ERR_CRYPTO",
            Self::Ipc(_) => "ERR_IPC",
            Self::Service(_) => "ERR_SERVICE",
            Self::Capture(_) => "ERR_CAPTURE",
            Self::Codec(_) => "ERR_CODEC",
            Self::Input(_) => "ERR_INPUT",
            Self::Other(_) => "ERR_OTHER",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        let err = DeskError::Config("missing required field".to_string());
        assert_eq!(err.error_code(), "ERR_CONFIG");
        assert_eq!(
            err.to_string(),
            "Configuration error: missing required field"
        );

        let theme_err = DeskError::Theme("invalid token".to_string());
        assert_eq!(theme_err.error_code(), "ERR_THEME");

        let io_err = DeskError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        assert_eq!(io_err.error_code(), "ERR_IO");
    }

    #[test]
    fn test_io_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let desk_err: DeskError = io_err.into();
        assert_eq!(desk_err.error_code(), "ERR_IO");
    }
}
