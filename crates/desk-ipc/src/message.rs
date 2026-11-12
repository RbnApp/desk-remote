//! Typed messages exchanged over the service <-> helper pipe.

use desk_protocol::Message;
use serde::{Deserialize, Serialize};

/// One IPC message. Media and input reuse the wire [`Message`] so frames are never
/// re-encoded between the helper, the service and the viewer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IpcMessage {
    /// Helper -> service: first message after connecting.
    HelperHello { session_id: u32, pid: u32 },
    /// Carries `ViewerJoin`/`ViewerLeave`/`RequestKeyframe`/`InputEvent` (service -> helper)
    /// and `VideoConfig`/`VideoFrame` (helper -> service).
    Wire(Message),
    /// Helper -> service: the tray user asked for a pairing code.
    PairingCodeRequest,
    /// Service -> helper: the one-time code and its validity in seconds (`0` = refused).
    PairingCode { code: String, ttl_secs: u32 },
    /// Service -> helper: the device IDs of the currently connected viewers.
    Viewers { device_ids: Vec<String> },
    /// Helper -> service: the input desktop is the lock screen / secure desktop (or not).
    DesktopLocked { locked: bool },
    /// Helper -> service: no monitor is attached, nothing can be captured.
    NoDisplay,
    /// Service -> helper: exit cleanly.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use desk_error::DeskResult;
    use desk_protocol::FrameCodec;

    #[test]
    fn roundtrip_through_the_shared_frame_codec() -> DeskResult<()> {
        let messages = vec![
            IpcMessage::HelperHello {
                session_id: 3,
                pid: 99,
            },
            IpcMessage::Wire(Message::VideoFrame {
                ts_us: 5,
                keyframe: true,
                data: vec![1, 2, 3],
            }),
            IpcMessage::PairingCode {
                code: "ABCD-2345".to_string(),
                ttl_secs: 300,
            },
            IpcMessage::Viewers {
                device_ids: vec!["a".to_string()],
            },
            IpcMessage::DesktopLocked { locked: true },
            IpcMessage::Shutdown,
        ];
        for original in messages {
            let frame = FrameCodec::encode_value(&original)?;
            let decoded: Option<(IpcMessage, usize)> = FrameCodec::decode_value(&frame)?;
            assert_eq!(decoded, Some((original, frame.len())));
        }
        Ok(())
    }
}
