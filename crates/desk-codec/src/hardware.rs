//! # Hardware H.264 encoder (Media Foundation MFT)
//!
//! ## Responsibility
//! Picks the hardware H.264 encoder MFT that belongs to the GPU running the capture
//! device (by adapter LUID, then by vendor name), configures it for low-latency CBR
//! without B-frames, feeds it NV12 textures converted on the GPU, and returns Annex-B
//! access units. IDR frames can be forced on demand.
//!
//! ## Scope Boundaries
//! - Hardware only; the software fallback lives in `software.rs`.
//! - Does not capture and does not pace.
//!
//! ## Dependencies
//! `desk-capture` (error context), `desk-error`, `scaler`, `settings`.

use crate::encoder::VideoEncoder;
use crate::scaler::{create_target_texture, texture_size, GpuScaler};
use crate::settings::{EncodedUnit, EncoderSettings};
use desk_capture::WinContext;
use desk_error::{DeskError, DeskResult};
use windows::core::{Interface, GUID};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonLowLatency, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, ICodecAPI, IMFActivate,
    IMFAttributes, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFTransform,
    METransformHaveOutput, METransformNeedInput, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSampleExtension_CleanPoint, MFStartup, MFTEnum2, MFTEnumEx,
    MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MFSTARTUP_FULL,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT,
    MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_LOW_LATENCY, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK,
    MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::{VARIANT, VT_UI4};

/// H.264 "Main" profile value for `MF_MT_MPEG2_PROFILE`.
const H264_PROFILE_MAIN: u32 = 77;
/// Number of NV12 render targets rotated between frames.
const NV12_POOL_SIZE: usize = 3;
/// Maximum polls (250 us apart) while waiting for the MFT to produce output.
const MAX_EVENT_SPINS: u32 = 4000;

fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// PCI vendor id to the substring used in the MFT friendly name.
fn vendor_keyword(vendor_id: u32) -> Option<&'static str> {
    match vendor_id {
        0x10DE => Some("NVIDIA"),
        0x1002 | 0x1022 => Some("AMD"),
        0x8086 => Some("INTEL"),
        _ => None,
    }
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut buffer = [0u16; 128];
    let mut length = 0u32;
    // SAFETY: `buffer` and `length` are valid out-parameters.
    let ok = unsafe {
        activate
            .GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buffer, Some(&mut length))
            .is_ok()
    };
    if ok {
        String::from_utf16_lossy(&buffer[..length as usize])
    } else {
        String::from("unknown")
    }
}

/// Enumerates hardware H.264 encoder activation objects. With a `luid` the OS filters them
/// to that adapter (MFTEnum2); otherwise every hardware encoder is returned.
fn enumerate_hardware_encoders(luid: Option<u64>) -> DeskResult<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: MF fills `list` with `count` activation objects that we take ownership of
    // below and free the array with CoTaskMemFree.
    unsafe {
        match luid {
            Some(luid) => {
                let mut attributes: Option<IMFAttributes> = None;
                MFCreateAttributes(&mut attributes, 1)
                    .context(DeskError::Codec, "MFCreateAttributes")?;
                let attributes = attributes
                    .ok_or_else(|| DeskError::Codec("MFCreateAttributes returned null".into()))?;
                attributes
                    .SetUINT64(&MFT_ENUM_ADAPTER_LUID, luid)
                    .context(DeskError::Codec, "set MFT_ENUM_ADAPTER_LUID")?;
                MFTEnum2(
                    MFT_CATEGORY_VIDEO_ENCODER,
                    flags,
                    Some(&input),
                    Some(&output),
                    &attributes,
                    &mut list,
                    &mut count,
                )
                .context(DeskError::Codec, "MFTEnum2")?;
            }
            None => {
                MFTEnumEx(
                    MFT_CATEGORY_VIDEO_ENCODER,
                    flags,
                    Some(&input),
                    Some(&output),
                    &mut list,
                    &mut count,
                )
                .context(DeskError::Codec, "MFTEnumEx")?;
            }
        }
        let mut found = Vec::new();
        if !list.is_null() {
            for item in std::slice::from_raw_parts_mut(list, count as usize) {
                if let Some(activate) = item.take() {
                    found.push(activate);
                }
            }
            CoTaskMemFree(Some(list as *const _));
        }
        Ok(found)
    }
}

/// Starts COM and Media Foundation for the calling thread (idempotent).
pub(crate) fn init_media_foundation() -> DeskResult<()> {
    // SAFETY: COM / MF start-up have no preconditions; repeated calls are reference counted.
    unsafe {
        // S_FALSE / RPC_E_CHANGED_MODE only mean COM was already initialised on this thread.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).context(DeskError::Codec, "MFStartup")
    }
}

/// Friendly names of every hardware H.264 encoder MFT (diagnostics).
pub fn list_hardware_encoders() -> DeskResult<Vec<String>> {
    init_media_foundation()?;
    Ok(enumerate_hardware_encoders(None)?
        .iter()
        .map(friendly_name)
        .collect())
}

/// Name of the hardware encoder the OS associates with the adapter `luid` (diagnostics).
pub fn encoder_name_for_adapter(luid: u64) -> DeskResult<Option<String>> {
    init_media_foundation()?;
    Ok(enumerate_hardware_encoders(Some(luid))?
        .first()
        .map(friendly_name))
}

fn set_codec_value(api: &ICodecAPI, key: &GUID, value: u32) {
    let mut variant = VARIANT::default();
    // SAFETY: writing the VT_UI4 payload of a freshly default-initialised VARIANT; the
    // VARIANT is plain data for this type and is not cleared with VariantClear.
    unsafe {
        let inner = &mut variant.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
        let _ = api.SetValue(key, &variant);
    }
}

/// Hardware H.264 encoder bound to the capture GPU.
pub struct HardwareEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    scaler: Option<GpuScaler>,
    device: ID3D11Device,
    pool: Vec<ID3D11Texture2D>,
    next_target: usize,
    settings: EncoderSettings,
    provides_samples: bool,
    output_buffer_size: u32,
    pending_input: u32,
    name: String,
}

impl HardwareEncoder {
    /// Creates the encoder for the GPU identified by `adapter_luid` / `vendor_id`.
    /// Candidates are tried in OS preference order; the first one that accepts the
    /// capture device and the requested format wins.
    pub fn new(
        device: &ID3D11Device,
        adapter_luid: u64,
        vendor_id: u32,
        settings: EncoderSettings,
    ) -> DeskResult<Self> {
        init_media_foundation()?;
        let mut candidates = enumerate_hardware_encoders(Some(adapter_luid)).unwrap_or_default();
        if candidates.is_empty() {
            // Older Windows builds ignore the LUID filter: fall back to vendor-name matching.
            let keyword = vendor_keyword(vendor_id);
            candidates = enumerate_hardware_encoders(None)?
                .into_iter()
                .filter(|a| keyword.is_some_and(|k| friendly_name(a).to_uppercase().contains(k)))
                .collect();
        }
        let mut last_error = DeskError::Codec("no hardware H.264 encoder for this GPU".into());
        for activate in candidates {
            let name = friendly_name(&activate);
            match Self::configure(device, &activate, settings, name.clone()) {
                Ok(encoder) => {
                    tracing::info!(encoder = %name, adapter_luid, "hardware H.264 encoder ready");
                    return Ok(encoder);
                }
                Err(e) => {
                    tracing::warn!(encoder = %name, error = %e, "hardware encoder candidate rejected");
                    last_error = e;
                }
            }
        }
        Err(last_error)
    }

    fn configure(
        device: &ID3D11Device,
        activate: &IMFActivate,
        settings: EncoderSettings,
        name: String,
    ) -> DeskResult<Self> {
        // SAFETY: Media Foundation setup on owned objects; all out-parameters are valid.
        unsafe {
            let transform: IMFTransform = activate
                .ActivateObject()
                .context(DeskError::Codec, "ActivateObject")?;
            let attributes = transform
                .GetAttributes()
                .context(DeskError::Codec, "GetAttributes")?;
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .context(DeskError::Codec, "async unlock")?;
            attributes
                .SetUINT32(&MF_LOW_LATENCY, 1)
                .context(DeskError::Codec, "low latency")?;

            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)
                .context(DeskError::Codec, "MFCreateDXGIDeviceManager")?;
            let manager =
                manager.ok_or_else(|| DeskError::Codec("null DXGI device manager".into()))?;
            manager
                .ResetDevice(device, token)
                .context(DeskError::Codec, "ResetDevice")?;
            // Fails with E_FAIL when the MFT belongs to a different GPU than `device`.
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context(DeskError::Codec, "SET_D3D_MANAGER")?;

            let output_type = MFCreateMediaType().context(DeskError::Codec, "output type")?;
            output_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .context(DeskError::Codec, "major type")?;
            output_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .context(DeskError::Codec, "subtype")?;
            output_type
                .SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate_bps)
                .context(DeskError::Codec, "bitrate")?;
            apply_video_attributes(&output_type, &settings)?;
            output_type
                .SetUINT32(&MF_MT_MPEG2_PROFILE, H264_PROFILE_MAIN)
                .context(DeskError::Codec, "profile")?;
            transform
                .SetOutputType(0, &output_type, 0)
                .context(DeskError::Codec, "SetOutputType")?;

            let input_type = MFCreateMediaType().context(DeskError::Codec, "input type")?;
            input_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .context(DeskError::Codec, "major type")?;
            input_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .context(DeskError::Codec, "subtype")?;
            apply_video_attributes(&input_type, &settings)?;
            transform
                .SetInputType(0, &input_type, 0)
                .context(DeskError::Codec, "SetInputType")?;

            let codec_api = transform.cast::<ICodecAPI>().ok();
            if let Some(api) = &codec_api {
                set_codec_value(api, &CODECAPI_AVEncCommonRateControlMode, 0); // CBR
                set_codec_value(api, &CODECAPI_AVEncCommonMeanBitRate, settings.bitrate_bps);
                set_codec_value(api, &CODECAPI_AVEncMPVGOPSize, settings.gop_frames);
                set_codec_value(api, &CODECAPI_AVEncCommonLowLatency, 1);
                set_codec_value(api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            }

            let stream_info = transform
                .GetOutputStreamInfo(0)
                .context(DeskError::Codec, "GetOutputStreamInfo")?;
            let provides_samples =
                stream_info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

            transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .context(DeskError::Codec, "flush")?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .context(DeskError::Codec, "begin streaming")?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .context(DeskError::Codec, "start of stream")?;
            let events: IMFMediaEventGenerator = transform
                .cast()
                .context(DeskError::Codec, "IMFMediaEventGenerator")?;

            let mut pool = Vec::with_capacity(NV12_POOL_SIZE);
            for _ in 0..NV12_POOL_SIZE {
                pool.push(create_target_texture(
                    device,
                    (settings.width, settings.height),
                    DXGI_FORMAT_NV12,
                )?);
            }
            Ok(Self {
                transform,
                events,
                codec_api,
                scaler: None,
                device: device.clone(),
                pool,
                next_target: 0,
                settings,
                provides_samples,
                output_buffer_size: stream_info.cbSize.max(1 << 20),
                pending_input: 0,
                name,
            })
        }
    }

    /// Pulls one output sample, if the MFT has one ready.
    fn pull_output(&mut self) -> DeskResult<Option<EncodedUnit>> {
        // SAFETY: the output sample either comes from the MFT or is allocated here; both
        // pSample and pEvents are taken back out of the ManuallyDrop exactly once.
        unsafe {
            let mut buffer = MFT_OUTPUT_DATA_BUFFER::default();
            if !self.provides_samples {
                let sample = MFCreateSample().context(DeskError::Codec, "MFCreateSample")?;
                let memory = MFCreateMemoryBuffer(self.output_buffer_size)
                    .context(DeskError::Codec, "MFCreateMemoryBuffer")?;
                sample
                    .AddBuffer(&memory)
                    .context(DeskError::Codec, "AddBuffer")?;
                buffer.pSample = std::mem::ManuallyDrop::new(Some(sample));
            }
            let mut status = 0u32;
            let mut buffers = [buffer];
            let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
            let sample = std::mem::ManuallyDrop::take(&mut buffers[0].pSample);
            let _events = std::mem::ManuallyDrop::take(&mut buffers[0].pEvents);
            if let Err(e) = result {
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(None);
                }
                return Err(DeskError::Codec(format!("ProcessOutput: {e}")));
            }
            let Some(sample) = sample else {
                return Ok(None);
            };
            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let ts_us = (sample.GetSampleTime().unwrap_or(0).max(0) / 10) as u64;
            let contiguous = sample
                .ConvertToContiguousBuffer()
                .context(DeskError::Codec, "ConvertToContiguousBuffer")?;
            let mut data_ptr = std::ptr::null_mut();
            let mut length = 0u32;
            contiguous
                .Lock(&mut data_ptr, None, Some(&mut length))
                .context(DeskError::Codec, "Lock")?;
            let data = std::slice::from_raw_parts(data_ptr, length as usize).to_vec();
            let _ = contiguous.Unlock();
            Ok(Some(EncodedUnit {
                ts_us,
                keyframe,
                data,
            }))
        }
    }

    /// Runs the async-MFT event loop: feed the sample on NeedInput, collect HaveOutput.
    fn drive(
        &mut self,
        sample: &windows::Win32::Media::MediaFoundation::IMFSample,
    ) -> DeskResult<Vec<EncodedUnit>> {
        let mut outputs = Vec::new();
        let mut fed = false;
        let mut spins = 0u32;
        loop {
            if fed && !outputs.is_empty() {
                break;
            }
            if !fed && self.pending_input > 0 {
                self.pending_input -= 1;
                // SAFETY: valid sample; the MFT requested input earlier.
                unsafe { self.transform.ProcessInput(0, sample, 0) }
                    .context(DeskError::Codec, "ProcessInput")?;
                fed = true;
                continue;
            }
            let flags = if fed || self.pending_input > 0 {
                MF_EVENT_FLAG_NO_WAIT
            } else {
                MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)
            };
            // SAFETY: event generator of the live MFT.
            match unsafe { self.events.GetEvent(flags) } {
                Ok(event) => {
                    // SAFETY: event object returned by GetEvent.
                    let kind =
                        unsafe { event.GetType() }.context(DeskError::Codec, "event type")?;
                    if kind == METransformNeedInput.0 as u32 {
                        if fed {
                            self.pending_input += 1;
                        } else {
                            // SAFETY: valid sample; the MFT requested input.
                            unsafe { self.transform.ProcessInput(0, sample, 0) }
                                .context(DeskError::Codec, "ProcessInput")?;
                            fed = true;
                        }
                    } else if kind == METransformHaveOutput.0 as u32 {
                        if let Some(unit) = self.pull_output()? {
                            outputs.push(unit);
                        }
                    }
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    spins += 1;
                    if spins > MAX_EVENT_SPINS {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_micros(250));
                }
                Err(e) => return Err(DeskError::Codec(format!("GetEvent: {e}"))),
            }
        }
        Ok(outputs)
    }
}

fn apply_video_attributes(
    media_type: &windows::Win32::Media::MediaFoundation::IMFMediaType,
    settings: &EncoderSettings,
) -> DeskResult<()> {
    // SAFETY: plain attribute setters on an owned media type.
    unsafe {
        media_type
            .SetUINT64(&MF_MT_FRAME_SIZE, pack(settings.width, settings.height))
            .context(DeskError::Codec, "frame size")?;
        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, pack(settings.fps, 1))
            .context(DeskError::Codec, "frame rate")?;
        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
            .context(DeskError::Codec, "aspect ratio")?;
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .context(DeskError::Codec, "interlace")?;
    }
    Ok(())
}

impl VideoEncoder for HardwareEncoder {
    fn name(&self) -> &str {
        &self.name
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
        let target = self.pool[self.next_target].clone();
        self.next_target = (self.next_target + 1) % self.pool.len();
        if let Some(scaler) = &self.scaler {
            scaler.blit(frame, &target)?;
        }
        // SAFETY: NV12 render target on the encoder's own device.
        let sample = unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &target, 0, false)
                .context(DeskError::Codec, "MFCreateDXGISurfaceBuffer")?;
            let sample = MFCreateSample().context(DeskError::Codec, "MFCreateSample")?;
            sample
                .AddBuffer(&buffer)
                .context(DeskError::Codec, "AddBuffer")?;
            sample
                .SetSampleTime(ts_us as i64 * 10)
                .context(DeskError::Codec, "SetSampleTime")?;
            sample
                .SetSampleDuration(10_000_000 / self.settings.fps.max(1) as i64)
                .context(DeskError::Codec, "SetSampleDuration")?;
            sample
        };
        if force_keyframe {
            if let Some(api) = &self.codec_api {
                set_codec_value(api, &CODECAPI_AVEncVideoForceKeyFrame, 1);
            }
        }
        self.drive(&sample)
    }
}
