//! # Windows error context helper
//!
//! ## Responsibility
//! Converts `windows::core::Error` into the unified `DeskError` with a short context
//! string, so every media crate maps native failures the same way.
//!
//! ## Scope Boundaries
//! - Only error conversion; no Windows API calls.
//!
//! ## Dependencies
//! `desk-error`.

use desk_error::{DeskError, DeskResult};

/// Adds context to a native Windows result and maps it into a `DeskError` variant.
pub trait WinContext<T> {
    /// `make` is a `DeskError` variant constructor, e.g. `DeskError::Codec`.
    fn context(self, make: fn(String) -> DeskError, what: &str) -> DeskResult<T>;
}

impl<T> WinContext<T> for windows::core::Result<T> {
    fn context(self, make: fn(String) -> DeskError, what: &str) -> DeskResult<T> {
        self.map_err(|e| make(format!("{what}: {e}")))
    }
}
