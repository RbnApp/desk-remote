//! Typed messages exchanged over the service <-> helper pipe.

use crate::info::AgentInfo;
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
    /// Helper -> service: send a fresh [`AgentInfo`] (sent when the tray menu is about to open).
    AgentInfoRequest,
    /// Service -> helper: the agent snapshot for the tray menu (on attach, on change, on request).
    AgentInfo(AgentInfo),
    /// Helper -> service: the tray user asked to drop every viewer now.
    DisconnectAllRequest,
    /// Helper -> service: block (`true`) or allow remote input; resets when the service restarts.
    SetViewOnly { enabled: bool },
    /// Service -> helper: who sends the file offered next (`Wire(FileOffer)` follows).
    FileIncoming {
        transfer_id: String,
        peer_name: String,
    },
    /// Service -> helper: exit cleanly.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::TailnetState;
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
            IpcMessage::AgentInfoRequest,
            IpcMessage::AgentInfo(AgentInfo {
                pc_name: "FARM-01".to_string(),
                local_ip: Some("192.168.1.20".to_string()),
                port: 47800,
                tailnet: TailnetState::Connected,
                tailnet_ips: vec!["100.64.0.7".to_string()],
                device_id: "desk-0123456789abcdef".to_string(),
                fingerprint: "0123-4567".to_string(),
                viewers: 2,
                unavailable: false,
                view_only: true,
            }),
            IpcMessage::FileIncoming {
                transfer_id: "t1".to_string(),
                peer_name: "Laptop".to_string(),
            },
            IpcMessage::Wire(Message::FileChunk {
                transfer_id: "t1".to_string(),
                offset: 0,
                data: vec![9; 1000],
            }),
            IpcMessage::DisconnectAllRequest,
            IpcMessage::SetViewOnly { enabled: true },
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
