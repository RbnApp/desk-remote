//! Backend used until `desk-transport` provides real sessions.

use super::{Backend, EventSink, PairError, PcInfo, PcStatus, ViewerSession};
use desk_config::PairedPc;
use desk_error::{DeskError, DeskResult};

/// Reports every paired PC as offline and refuses to pair or connect.
pub struct NoTransport;

impl Backend for NoTransport {
    fn list(&self, paired: &[PairedPc]) -> Vec<PcInfo> {
        paired
            .iter()
            .map(|pc| PcInfo {
                id: pc.id.clone(),
                name: pc.name.clone(),
                status: PcStatus::Offline,
                last_seen_min: u32::MAX,
                warn: None,
            })
            .collect()
    }

    fn pair(&self, _code: &str, _name: Option<&str>) -> Result<PairedPc, PairError> {
        Err(PairError::Network)
    }

    fn connect(&self, _pc_id: &str, _sink: EventSink) -> DeskResult<Box<dyn ViewerSession>> {
        Err(DeskError::Transport(
            "desk-transport is not available in this build".to_string(),
        ))
    }
}
