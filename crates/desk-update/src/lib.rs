//! # Desk-Remote update core
//!
//! ## Responsibility
//! Finds out whether a newer GitHub Release exists for one component (agent or panel),
//! downloads its installer and proves the file is the one the release published
//! (SHA-256 from the release manifest). The result is one [`UpdateStatus`] value that every
//! consumer (agent service, tray, panel settings) renders the same way.
//!
//! ## Scope Boundaries
//! - Never runs an installer: applying an update is the consumer's job (the agent does it in
//!   `desk-service-core`). A file that failed verification is deleted and never reported ready.
//! - Each component only ever fetches its own installer (`Component::Agent` / `Panel`).
//! - No HTTP code here: all network access goes through [`Transport`], whose real
//!   implementation is `desk_transport::http`.
//!
//! ## Dependencies
//! `desk-transport` (HTTP), `desk-version` (release identity, asset names, semver),
//! `desk-error`.

mod release;
mod status;
mod updater;

pub use release::{default_feed_url, parse_manifest, parse_release, ReleaseInfo};
pub use status::UpdateStatus;
pub use updater::{Component, NetTransport, Transport, Updater};

use std::time::Duration;

/// How often the unattended agent looks for a new release (after the check at start-up).
pub const AGENT_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Largest accepted size of the release description (JSON).
pub const MAX_RELEASE_BYTES: u64 = 1024 * 1024;
/// Largest accepted size of the manifest (`SHA256SUMS.txt`).
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Largest accepted installer.
pub const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;
