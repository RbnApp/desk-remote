//! # Encoder abstraction and selection
//!
//! ## Responsibility
//! Defines the one trait every H.264 backend implements and chooses the backend:
//! hardware MFT first; the software fallback only when no hardware encoder works and
//! the caller allowed it.
//!
//! ## Scope Boundaries
//! - Selection policy only; the backends live in `hardware.rs` and `software.rs`.
//!
//! ## Dependencies
//! `desk-error`, `hardware`, `software` (feature `software-fallback`).

use crate::hardware::HardwareEncoder;
use crate::settings::{EncodedUnit, EncoderSettings};
use desk_error::{DeskError, DeskResult};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};

/// A GPU-texture-in, Annex-B-out H.264 encoder.
pub trait VideoEncoder {
    /// Human readable backend name (MFT friendly name or `openh264`).
    fn name(&self) -> &str;

    /// Encodes one BGRA texture. `ts_us` is returned unchanged on the produced unit.
    /// Output may lag by zero or more frames; an empty vector is normal.
    fn encode(
        &mut self,
        frame: &ID3D11Texture2D,
        ts_us: u64,
        force_keyframe: bool,
    ) -> DeskResult<Vec<EncodedUnit>>;
}

/// Creates the best available encoder for the GPU that owns `device`.
pub fn create_encoder(
    device: &ID3D11Device,
    adapter_luid: u64,
    vendor_id: u32,
    settings: EncoderSettings,
    allow_software: bool,
) -> DeskResult<Box<dyn VideoEncoder>> {
    match HardwareEncoder::new(device, adapter_luid, vendor_id, settings) {
        Ok(encoder) => Ok(Box::new(encoder)),
        Err(hardware_error) => software_fallback(device, settings, allow_software, hardware_error),
    }
}

#[cfg(feature = "software-fallback")]
fn software_fallback(
    device: &ID3D11Device,
    settings: EncoderSettings,
    allow_software: bool,
    hardware_error: DeskError,
) -> DeskResult<Box<dyn VideoEncoder>> {
    if !allow_software {
        return Err(hardware_error);
    }
    tracing::warn!(error = %hardware_error, "no hardware encoder, using software H.264");
    Ok(Box::new(crate::software::SoftwareEncoder::new(
        device, settings,
    )?))
}

#[cfg(not(feature = "software-fallback"))]
fn software_fallback(
    _device: &ID3D11Device,
    _settings: EncoderSettings,
    allow_software: bool,
    hardware_error: DeskError,
) -> DeskResult<Box<dyn VideoEncoder>> {
    if allow_software {
        tracing::warn!("software fallback requested but the `software-fallback` feature is off");
    }
    Err(hardware_error)
}
