//! # GPU colour conversion and scaling
//!
//! ## Responsibility
//! Uses the D3D11 video processor to convert the captured BGRA texture to the encoder
//! input format (NV12 or BGRA) and to scale it to the output size, entirely on the GPU.
//!
//! ## Scope Boundaries
//! - Operates on caller-supplied textures; owns no frame pool.
//!
//! ## Dependencies
//! `desk-capture` (error context), `desk-error`.

use desk_capture::WinContext;
use desk_error::{DeskError, DeskResult};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, D3D11_BIND_RENDER_TARGET, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};

/// Video-processor based scaler / colour converter for one (source size, output size) pair.
pub struct GpuScaler {
    device: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    source_size: (u32, u32),
}

impl GpuScaler {
    pub fn new(device: &ID3D11Device, source: (u32, u32), output: (u32, u32)) -> DeskResult<Self> {
        // SAFETY: plain D3D11 video object creation on a valid device.
        unsafe {
            let video_device: ID3D11VideoDevice = device
                .cast()
                .context(DeskError::Codec, "ID3D11VideoDevice")?;
            let immediate = device
                .GetImmediateContext()
                .context(DeskError::Codec, "GetImmediateContext")?;
            let video_context: ID3D11VideoContext = immediate
                .cast()
                .context(DeskError::Codec, "ID3D11VideoContext")?;
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputWidth: source.0,
                InputHeight: source.1,
                OutputWidth: output.0,
                OutputHeight: output.1,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                ..Default::default()
            };
            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&content)
                .context(DeskError::Codec, "CreateVideoProcessorEnumerator")?;
            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .context(DeskError::Codec, "CreateVideoProcessor")?;
            Ok(Self {
                device: video_device,
                context: video_context,
                processor,
                enumerator,
                source_size: source,
            })
        }
    }

    /// Size of the source this scaler was created for.
    pub fn source_size(&self) -> (u32, u32) {
        self.source_size
    }

    /// Converts and scales `source` into `target` on the GPU.
    pub fn blit(&self, source: &ID3D11Texture2D, target: &ID3D11Texture2D) -> DeskResult<()> {
        // SAFETY: both textures belong to the device this scaler was created on; the input
        // view moved into the stream is dropped exactly once below.
        unsafe {
            let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut input_view = None;
            self.device
                .CreateVideoProcessorInputView(
                    source,
                    &self.enumerator,
                    &input_desc,
                    Some(&mut input_view),
                )
                .context(DeskError::Codec, "CreateVideoProcessorInputView")?;
            let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view = None;
            self.device
                .CreateVideoProcessorOutputView(
                    target,
                    &self.enumerator,
                    &output_desc,
                    Some(&mut output_view),
                )
                .context(DeskError::Codec, "CreateVideoProcessorOutputView")?;
            let output_view = output_view
                .ok_or_else(|| DeskError::Codec("null video processor output view".into()))?;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(input_view),
                ..Default::default()
            };
            let result = self.context.VideoProcessorBlt(
                &self.processor,
                &output_view,
                0,
                std::slice::from_ref(&stream),
            );
            drop(std::mem::ManuallyDrop::into_inner(stream.pInputSurface));
            result.context(DeskError::Codec, "VideoProcessorBlt")
        }
    }
}

/// Creates a render-target texture in `format` (NV12 for the hardware encoder, BGRA for readback).
pub fn create_target_texture(
    device: &ID3D11Device,
    size: (u32, u32),
    format: DXGI_FORMAT,
) -> DeskResult<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        ..Default::default()
    };
    let mut texture = None;
    // SAFETY: `desc` is fully initialised and `texture` is a valid out-parameter.
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context(DeskError::Codec, "CreateTexture2D(target)")?;
    }
    texture.ok_or_else(|| DeskError::Codec("CreateTexture2D returned null".into()))
}

/// Returns the pixel size of a texture.
pub fn texture_size(texture: &ID3D11Texture2D) -> (u32, u32) {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `desc` is a valid out-parameter.
    unsafe { texture.GetDesc(&mut desc) };
    (desc.Width, desc.Height)
}
