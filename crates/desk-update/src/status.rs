//! The single update state every consumer renders.

use crate::release::ReleaseInfo;
use std::path::PathBuf;

/// Where an update stands. One value for the agent service, the tray and the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Nothing checked yet, or the installed version is the newest.
    NoUpdate,
    /// A check is running.
    Checking,
    /// A newer release exists and is not downloaded yet.
    Available(ReleaseInfo),
    /// The installer is being downloaded.
    Downloading {
        release: ReleaseInfo,
        received: u64,
        total: Option<u64>,
    },
    /// The installer is downloaded and its SHA-256 matches the release manifest.
    Ready { release: ReleaseInfo, path: PathBuf },
    /// The last check or download failed (nothing was changed on this PC).
    Failed { message: String },
}

impl UpdateStatus {
    /// The release this status is about, if any.
    #[must_use]
    pub fn release(&self) -> Option<&ReleaseInfo> {
        match self {
            Self::Available(r)
            | Self::Downloading { release: r, .. }
            | Self::Ready { release: r, .. } => Some(r),
            _ => None,
        }
    }
}
