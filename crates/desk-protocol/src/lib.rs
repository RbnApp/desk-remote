//! Central binary framing, message schemas, and protocol negotiation for Desk-Remote.
//!
//! Provides the canonical [`Message`] enum, input payloads, and length-delimited
//! CRC32-validated [`FrameCodec`] used across all Desk-Remote connections.

use desk_error::{DeskError, DeskResult};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Canonical wire protocol version string.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// 4-byte framing magic bytes ("DESK").
pub const FRAME_MAGIC: [u8; 4] = *b"DESK";

/// Maximum supported frame payload size (64 MiB).
pub const MAX_FRAME_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;

/// IEEE 802.3 CRC-32 checksum calculation.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Normalized user input event payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputPayload {
    /// Normalized mouse movement, buttons, and wheel.
    ///
    /// `x` and `y` coordinates are normalized to the 0..=65535 range.
    Mouse {
        x: u16,
        y: u16,
        buttons: u8,
        wheel: i8,
    },
    /// Keyboard key state change with hardware scancode.
    Key { scancode: u32, down: bool },
}

/// Central wire message schemas for Desk-Remote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Message {
    /// Initial identity exchange upon connecting.
    Hello {
        device_id: String,
        version: String,
        peer_name: Option<String>,
    },
    /// Pairing request with one-time verification code.
    PairRequest { code: String },
    /// Result of a pairing code attempt.
    PairResult {
        success: bool,
        error: Option<String>,
    },
    /// Viewer joined session: agent starts capture pipeline.
    ViewerJoin,
    /// Viewer left session: agent pauses capture pipeline when viewer count reaches zero.
    ViewerLeave,
    /// Video stream configuration announced before frame streaming.
    VideoConfig {
        codec: String,
        width: u32,
        height: u32,
        fps: u32,
    },
    /// Video frame containing Annex B formatted NAL units.
    VideoFrame {
        ts_us: u64,
        keyframe: bool,
        data: Vec<u8>,
    },
    /// Viewer requests an immediate IDR keyframe from encoder.
    RequestKeyframe,
    /// User input forwarded from viewer to remote host.
    InputEvent(InputPayload),
    /// Heartbeat ping with timestamp in microseconds.
    Ping { ts: u64 },
    /// Heartbeat response echoing ping timestamp.
    Pong { ts: u64 },
    /// Connection health and latency status.
    Status { online: bool, latency_ms: u32 },
    /// The PC user changed what a viewer may do (agent -> viewer, informational).
    Notice { kind: NoticeKind },
    /// Sender -> receiver: proposes a file transfer (`sha256` is the digest of the whole file).
    FileOffer {
        transfer_id: String,
        name: String,
        size: u64,
        sha256: [u8; 32],
    },
    /// Receiver -> sender: the offer was accepted, chunks may follow.
    FileAccept { transfer_id: String },
    /// Receiver -> sender: the offer was refused, no byte is written.
    FileReject {
        transfer_id: String,
        reason: FileRejectReason,
    },
    /// Sender -> receiver: `data` (at most [`FILE_CHUNK_MAX`] bytes) at byte `offset`.
    FileChunk {
        transfer_id: String,
        offset: u64,
        data: Vec<u8>,
    },
    /// Sender -> receiver: every byte was sent.
    FileDone { transfer_id: String },
    /// Receiver -> sender: final verdict after the digest check (`error` is a stable code).
    FileResult {
        transfer_id: String,
        ok: bool,
        error: Option<String>,
    },
    /// Either side: abandons the transfer; the receiver deletes the partial file.
    FileCancel { transfer_id: String },
    /// Agent -> viewer: one cursor image, sent once per shape (DXGI frames never contain the
    /// cursor). `rgba` is straight-alpha, top-down, `width * height * 4` bytes.
    CursorShape {
        id: u32,
        hot_x: u16,
        hot_y: u16,
        width: u16,
        height: u16,
        rgba: Vec<u8>,
    },
    /// Agent -> viewer: cursor position over the captured monitor (0..=65535, like input) and
    /// the id of the shape to draw. Lossy: only the latest one matters.
    CursorPos {
        x: u16,
        y: u16,
        shape_id: u32,
        visible: bool,
    },
}

/// Largest payload of one [`Message::FileChunk`] (256 KiB).
pub const FILE_CHUNK_MAX: usize = 256 * 1024;

/// Why a receiver refused a [`Message::FileOffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileRejectReason {
    /// The file is larger than the receiver's upload limit.
    TooLarge,
    /// The PC user declined (or the name is not acceptable).
    Denied,
    /// Another transfer is running on this PC.
    Busy,
    /// Not enough free disk space.
    DiskFull,
    /// Nobody answered in time (or nobody is signed in to receive).
    NoAnswer,
}

impl FileRejectReason {
    /// Stable snake_case code shown to the UI.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::Denied => "denied",
            Self::Busy => "busy",
            Self::DiskFull => "disk_full",
            Self::NoAnswer => "no_answer",
        }
    }
}

impl Message {
    /// The transfer a file message belongs to; `None` for every other message.
    #[must_use]
    pub fn transfer_id(&self) -> Option<&str> {
        match self {
            Self::FileOffer { transfer_id, .. }
            | Self::FileAccept { transfer_id }
            | Self::FileReject { transfer_id, .. }
            | Self::FileChunk { transfer_id, .. }
            | Self::FileDone { transfer_id }
            | Self::FileResult { transfer_id, .. }
            | Self::FileCancel { transfer_id } => Some(transfer_id),
            _ => None,
        }
    }
}

/// What a [`Message::Notice`] tells the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoticeKind {
    /// The PC user disconnected every viewer; the connection closes right after.
    DisconnectedByHost,
    /// Remote input is blocked on the PC (watch only).
    ViewOnlyOn,
    /// Remote input is allowed again.
    ViewOnlyOff,
}

/// Length-delimited and CRC32-validated framing codec.
///
/// Frame layout on wire:
/// - Magic (4 bytes): `[b'D', b'E', b'S', b'K']`
/// - Length (4 bytes, BE): length of serialized payload
/// - CRC32 (4 bytes, BE): CRC32 of payload bytes
/// - Payload (N bytes): `postcard` binary-serialized [`Message`] (byte buffers such as
///   `VideoFrame::data` are carried raw, not as number arrays)
#[derive(Debug, Default, Clone)]
pub struct FrameCodec;

impl FrameCodec {
    /// Encodes a [`Message`] into a framed byte buffer.
    pub fn encode(msg: &Message) -> DeskResult<Vec<u8>> {
        Self::encode_value(msg)
    }

    /// Encodes any serde value (for example the IPC message enum) with the same frame layout.
    pub fn encode_value<T: Serialize>(msg: &T) -> DeskResult<Vec<u8>> {
        let payload = postcard::to_stdvec(msg)
            .map_err(|e| DeskError::Protocol(format!("Failed to serialize message: {e}")))?;

        if payload.len() > MAX_FRAME_PAYLOAD_SIZE {
            return Err(DeskError::Protocol(format!(
                "Payload size {} exceeds limit of {} bytes",
                payload.len(),
                MAX_FRAME_PAYLOAD_SIZE
            )));
        }

        let checksum = crc32(&payload);
        let len_u32 = u32::try_from(payload.len())
            .map_err(|_| DeskError::Protocol("Payload length overflow".to_string()))?;

        let mut frame = Vec::with_capacity(12 + payload.len());
        frame.extend_from_slice(&FRAME_MAGIC);
        frame.extend_from_slice(&len_u32.to_be_bytes());
        frame.extend_from_slice(&checksum.to_be_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    /// Attempts to decode one [`Message`] from a continuous byte buffer.
    ///
    /// Returns `Ok(Some((message, consumed_bytes)))` on success, `Ok(None)` if more
    /// bytes are required, or `Err(DeskError)` on framing or integrity errors.
    pub fn decode(buf: &[u8]) -> DeskResult<Option<(Message, usize)>> {
        Self::decode_value(buf)
    }

    /// Decodes one serde value of type `T` from a continuous byte buffer (see [`Self::decode`]).
    pub fn decode_value<T: DeserializeOwned>(buf: &[u8]) -> DeskResult<Option<(T, usize)>> {
        if buf.len() < 12 {
            return Ok(None);
        }

        // Validate magic bytes
        if buf[0..4] != FRAME_MAGIC {
            return Err(DeskError::Protocol(format!(
                "Invalid frame magic: {:02x?}",
                &buf[0..4]
            )));
        }

        let len_bytes: [u8; 4] = buf[4..8]
            .try_into()
            .map_err(|_| DeskError::Protocol("Slice conversion failed".to_string()))?;
        let len = usize::try_from(u32::from_be_bytes(len_bytes))
            .map_err(|_| DeskError::Protocol("Length conversion failed".to_string()))?;

        if len > MAX_FRAME_PAYLOAD_SIZE {
            return Err(DeskError::Protocol(format!(
                "Incoming frame size {len} exceeds maximum allowed {MAX_FRAME_PAYLOAD_SIZE}"
            )));
        }

        let total_frame_len = 12 + len;
        if buf.len() < total_frame_len {
            return Ok(None);
        }

        let crc_bytes: [u8; 4] = buf[8..12]
            .try_into()
            .map_err(|_| DeskError::Protocol("Slice conversion failed".to_string()))?;
        let expected_crc = u32::from_be_bytes(crc_bytes);

        let payload = &buf[12..total_frame_len];
        let actual_crc = crc32(payload);
        if actual_crc != expected_crc {
            return Err(DeskError::Protocol(format!(
                "CRC32 mismatch: expected {expected_crc:#010x}, calculated {actual_crc:#010x}"
            )));
        }

        let message = postcard::from_bytes(payload)
            .map_err(|e| DeskError::Protocol(format!("Failed to parse message payload: {e}")))?;

        Ok(Some((message, total_frame_len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_known_vector() {
        let input = b"123456789";
        assert_eq!(crc32(input), 0xCBF4_3926);
    }

    #[test]
    fn test_message_roundtrip_hello() -> DeskResult<()> {
        let msg = Message::Hello {
            device_id: "agent-test-1234".to_string(),
            version: PROTOCOL_VERSION.to_string(),
            peer_name: None,
        };

        let encoded = FrameCodec::encode(&msg)?;
        let decoded = FrameCodec::decode(&encoded)?;

        assert_eq!(decoded, Some((msg, encoded.len())));

        let msg_with_name = Message::Hello {
            device_id: "agent-test-1234".to_string(),
            version: PROTOCOL_VERSION.to_string(),
            peer_name: Some("Laptop-Alice".to_string()),
        };
        let encoded = FrameCodec::encode(&msg_with_name)?;
        let decoded = FrameCodec::decode(&encoded)?;
        assert_eq!(decoded, Some((msg_with_name, encoded.len())));
        Ok(())
    }

    #[test]
    fn test_message_roundtrip_all_variants() -> DeskResult<()> {
        let messages = vec![
            Message::PairRequest {
                code: "ABCD-2345".to_string(),
            },
            Message::PairResult {
                success: true,
                error: None,
            },
            Message::ViewerJoin,
            Message::ViewerLeave,
            Message::VideoConfig {
                codec: "h264".to_string(),
                width: 1920,
                height: 1080,
                fps: 60,
            },
            Message::VideoFrame {
                ts_us: 1_234_567,
                keyframe: true,
                data: vec![0, 0, 0, 1, 0x67, 0x42, 0x00],
            },
            Message::RequestKeyframe,
            Message::InputEvent(InputPayload::Mouse {
                x: 32768,
                y: 32768,
                buttons: 1,
                wheel: 0,
            }),
            Message::InputEvent(InputPayload::Key {
                scancode: 30,
                down: true,
            }),
            Message::Ping { ts: 987_654_321 },
            Message::Pong { ts: 987_654_321 },
            Message::Status {
                online: true,
                latency_ms: 12,
            },
            Message::Notice {
                kind: NoticeKind::DisconnectedByHost,
            },
            Message::Notice {
                kind: NoticeKind::ViewOnlyOn,
            },
            Message::FileOffer {
                transfer_id: "t1".to_string(),
                name: "a.txt".to_string(),
                size: 1024,
                sha256: [7; 32],
            },
            Message::FileAccept {
                transfer_id: "t1".to_string(),
            },
            Message::FileReject {
                transfer_id: "t1".to_string(),
                reason: FileRejectReason::TooLarge,
            },
            Message::FileChunk {
                transfer_id: "t1".to_string(),
                offset: 262_144,
                data: vec![1, 2, 3],
            },
            Message::FileDone {
                transfer_id: "t1".to_string(),
            },
            Message::FileResult {
                transfer_id: "t1".to_string(),
                ok: false,
                error: Some("hash_mismatch".to_string()),
            },
            Message::FileCancel {
                transfer_id: "t1".to_string(),
            },
        ];

        for original in messages {
            let encoded = FrameCodec::encode(&original)?;
            let (decoded, consumed) = match FrameCodec::decode(&encoded)? {
                Some(pair) => pair,
                None => return Err(DeskError::Protocol("Unexpected None".to_string())),
            };
            assert_eq!(decoded, original);
            assert_eq!(consumed, encoded.len());
        }
        Ok(())
    }

    #[test]
    fn full_size_file_chunk_stays_raw_and_fits_one_frame() -> DeskResult<()> {
        let msg = Message::FileChunk {
            transfer_id: "t-0123456789abcdef".to_string(),
            offset: u64::from(u32::MAX) + 5,
            data: vec![0xAB; FILE_CHUNK_MAX],
        };
        let encoded = FrameCodec::encode(&msg)?;
        assert!(encoded.len() < FILE_CHUNK_MAX + 64, "{}", encoded.len());
        assert_eq!(FrameCodec::decode(&encoded)?, Some((msg, encoded.len())));
        Ok(())
    }

    #[test]
    fn reject_codes_and_transfer_ids() {
        assert_eq!(FileRejectReason::TooLarge.code(), "too_large");
        assert_eq!(FileRejectReason::DiskFull.code(), "disk_full");
        let done = Message::FileDone {
            transfer_id: "x".to_string(),
        };
        assert_eq!(done.transfer_id(), Some("x"));
        assert_eq!(Message::ViewerJoin.transfer_id(), None);
    }

    #[test]
    fn test_corrupted_crc_rejected() -> DeskResult<()> {
        let msg = Message::Ping { ts: 100 };
        let mut encoded = FrameCodec::encode(&msg)?;
        let last_idx = encoded.len() - 1;
        encoded[last_idx] ^= 0xFF;

        let res = FrameCodec::decode(&encoded);
        assert!(res.is_err());
        Ok(())
    }

    #[test]
    fn test_partial_buffer_returns_none() -> DeskResult<()> {
        let msg = Message::Ping { ts: 100 };
        let encoded = FrameCodec::encode(&msg)?;

        let partial = &encoded[..encoded.len() / 2];
        assert_eq!(FrameCodec::decode(partial)?, None);
        Ok(())
    }

    #[test]
    fn test_video_frame_binary_vs_json_size() -> DeskResult<()> {
        // Pseudo-random 1 MiB payload (worst case for JSON number arrays).
        let mut state = 0x1234_5678u32;
        let data: Vec<u8> = (0..1024 * 1024)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect();
        let msg = Message::VideoFrame {
            ts_us: 1,
            keyframe: true,
            data,
        };
        let binary = FrameCodec::encode(&msg)?.len();
        let json = serde_json::to_vec(&msg)
            .map_err(|e| DeskError::Protocol(e.to_string()))?
            .len();
        // Machine-readable line captured as report evidence (`--nocapture`).
        #[allow(clippy::print_stdout)]
        {
            println!("VIDEOFRAME_1MIB json_bytes={json} binary_bytes={binary}");
        }
        assert!(
            binary < 1024 * 1024 + 64,
            "binary overhead too large: {binary}"
        );
        assert!(json > 2 * binary);
        Ok(())
    }
}
