//! # Desk-Remote Codec - Hardware H.264 Encoding Engine
//!
//! ## Responsibility
//! Encodes captured GPU frames to low-latency H.264 (Annex-B) on the same GPU that runs
//! the capture, scales on the GPU, forces IDR frames on request, and runs the
//! viewer-gated capture-to-encoder pipeline.
//!
//! ## Scope Boundaries
//! - Encoder side only (decoding happens in the viewer).
//! - Software encoding is an opt-in feature (`software-fallback`), used only when no
//!   hardware encoder works.
//! - No networking: output goes to an [`EncodedSink`].
//!
//! ## Dependencies
//! `desk-capture` (device, frames, pacing), `desk-error`, `tracing`.

#![cfg(windows)]

mod encoder;
mod hardware;
mod pipeline;
mod scaler;
mod settings;
#[cfg(feature = "software-fallback")]
mod software;

pub use encoder::{create_encoder, VideoEncoder};
pub use hardware::{encoder_name_for_adapter, list_hardware_encoders, HardwareEncoder};
pub use pipeline::{EncodedSink, MediaPipeline, PipelineConfig, PipelineStats};
pub use settings::{default_bitrate, fit_dimensions, EncodedUnit, EncoderSettings};
