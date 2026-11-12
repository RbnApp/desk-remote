//! # Software H.264 fallback (Cisco OpenH264)
//!
//! ## Responsibility
//! Encodes with OpenH264 on the CPU when no hardware encoder is usable. The GPU still
//! scales the frame; only the scaled BGRA image is read back (staging texture) and
//! converted to I420 by OpenH264.
//!
//! ## Scope Boundaries
//! - Compiled only with the `software-fallback` feature.
//! - Much more CPU than the hardware path (about 44 % of a core at 1080p30 in the
//!   Phase 0 measurement); never selected while a hardware encoder works.
//!
//! ## Licensing
//! `openh264` is BSD-2-Clause and builds Cisco OpenH264 from source. Cisco's patent
//! licence covers only the binaries Cisco itself distributes, so shipping a self-built
//! copy needs a separate H.264 patent review before release.
//!
//! ## Dependencies
//! `openh264`, `scaler`, `settings`.

use crate::encoder::VideoEncoder;
use crate::scaler::{create_target_texture, texture_size, GpuScaler};
use crate::settings::{EncodedUnit, EncoderSettings};
use desk_capture::WinContext;
use desk_error::{DeskError, DeskResult};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use openh264::OpenH264API;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

/// CPU H.264 encoder fed through a GPU-scaled, CPU-readable BGRA copy.
pub struct SoftwareEncoder {
    encoder: Encoder,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    scaler: Option<GpuScaler>,
    scaled: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    settings: EncoderSettings,
    bgra: Vec<u8>,
    frames_since_key: u32,
}

impl SoftwareEncoder {
    pub fn new(device: &ID3D11Device, settings: EncoderSettings) -> DeskResult<Self> {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(settings.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(settings.fps as f32));
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| DeskError::Codec(format!("openh264 init: {e}")))?;
        let size = (settings.width, settings.height);
        let scaled = create_target_texture(device, size, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        let staging = create_staging(device, size)?;
        // SAFETY: immediate context of a live device.
        let context = unsafe { device.GetImmediateContext() }
            .context(DeskError::Codec, "GetImmediateContext")?;
        Ok(Self {
            encoder,
            device: device.clone(),
            context,
            scaler: None,
            scaled,
            staging,
            settings,
            bgra: vec![0; settings.width as usize * settings.height as usize * 4],
            frames_since_key: 0,
        })
    }

    fn read_back(&mut self) -> DeskResult<()> {
        let width = self.settings.width as usize;
        let height = self.settings.height as usize;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture is CPU-readable; rows are copied within the mapped pitch
        // and the subresource is unmapped before returning.
        unsafe {
            self.context.CopyResource(&self.staging, &self.scaled);
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context(DeskError::Codec, "Map(staging)")?;
            let pitch = mapped.RowPitch as usize;
            let base = mapped.pData as *const u8;
            for row in 0..height {
                let src = std::slice::from_raw_parts(base.add(row * pitch), width * 4);
                self.bgra[row * width * 4..(row + 1) * width * 4].copy_from_slice(src);
            }
            self.context.Unmap(&self.staging, 0);
        }
        Ok(())
    }
}

fn create_staging(device: &ID3D11Device, size: (u32, u32)) -> DeskResult<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        ..Default::default()
    };
    let mut texture = None;
    // SAFETY: `desc` is fully initialised and `texture` is a valid out-parameter.
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context(DeskError::Codec, "CreateTexture2D(staging)")?;
    }
    texture.ok_or_else(|| DeskError::Codec("CreateTexture2D returned null".into()))
}

impl VideoEncoder for SoftwareEncoder {
    fn name(&self) -> &str {
        "openh264"
    }

    fn encode(
        &mut self,
        frame: &ID3D11Texture2D,
        ts_us: u64,
        force_keyframe: bool,
    ) -> DeskResult<Vec<EncodedUnit>> {
        let source = texture_size(frame);
        if self.scaler.as_ref().map(GpuScaler::source_size) != Some(source) {
            self.scaler = Some(GpuScaler::new(
                &self.device,
                source,
                (self.settings.width, self.settings.height),
            )?);
        }
        if let Some(scaler) = &self.scaler {
            scaler.blit(frame, &self.scaled)?;
        }
        self.read_back()?;
        if force_keyframe || self.frames_since_key >= self.settings.gop_frames {
            self.encoder.force_intra_frame();
            self.frames_since_key = 0;
        }
        self.frames_since_key += 1;
        let yuv = YUVBuffer::from_bgra8_source(BgraSliceU8::new(
            &self.bgra,
            (self.settings.width as usize, self.settings.height as usize),
        ));
        let bitstream = self
            .encoder
            .encode(&yuv)
            .map_err(|e| DeskError::Codec(format!("openh264 encode: {e}")))?;
        let keyframe = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);
        let data = bitstream.to_vec();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![EncodedUnit {
            ts_us,
            keyframe,
            data,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desk_capture::{Capturer, Grab, MonitorSelector};

    /// Needs an interactive desktop, so it only runs with `--ignored`.
    #[test]
    #[ignore = "requires an interactive desktop session"]
    fn encodes_a_real_captured_frame() -> DeskResult<()> {
        let mut capturer = Capturer::new(MonitorSelector::Primary)?;
        let settings = EncoderSettings::for_size(1280, 720, 30);
        let mut encoder = SoftwareEncoder::new(capturer.device(), settings)?;
        let mut keyframes = 0usize;
        let mut bytes = 0usize;
        for i in 0..5u64 {
            if matches!(capturer.acquire(100)?, Grab::Lost) {
                continue;
            }
            let Some(frame) = capturer.frame() else {
                continue;
            };
            for unit in encoder.encode(frame, i * 33_000, i == 0)? {
                keyframes += usize::from(unit.keyframe);
                bytes += unit.data.len();
            }
        }
        assert!(keyframes >= 1, "no keyframe produced");
        assert!(bytes > 0, "no data produced");
        Ok(())
    }
}
