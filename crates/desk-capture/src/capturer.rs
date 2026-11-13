//! # DXGI Desktop Duplication capturer
//!
//! ## Responsibility
//! Owns the D3D11 device on the GPU that drives the chosen monitor, acquires desktop
//! frames into a persistent GPU texture, skips unchanged frames and re-opens the
//! duplication after access loss (mode change, UAC / lock screen, session switch).
//!
//! ## Scope Boundaries
//! - Never reads pixels back to the CPU.
//! - A removed or reset GPU is reported as an error; the owner rebuilds the session.
//!
//! ## Dependencies
//! `desk-error`, `monitor` (selection), `win` (error context).

use crate::monitor::{list_monitors, MonitorInfo, MonitorSelector};
use crate::win::WinContext;
use desk_error::{DeskError, DeskResult};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
    DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

/// Minimum spacing between attempts to re-open a lost duplication.
const REOPEN_INTERVAL: Duration = Duration::from_millis(250);

/// Result of one acquire attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// A new desktop image was copied into [`Capturer::frame`].
    Updated,
    /// Nothing changed (timeout or cursor-only update); no encoding needed.
    Unchanged,
    /// Duplication is unavailable (secure desktop, mode change); retrying in the background.
    Lost,
}

/// Desktop Duplication session bound to one monitor.
pub struct Capturer {
    monitor: MonitorInfo,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: Option<IDXGIOutputDuplication>,
    frame: Option<ID3D11Texture2D>,
    size: (u32, u32),
    last_reopen_attempt: Option<Instant>,
}

impl Capturer {
    /// Opens the selected monitor. Fails if no monitor matches or duplication is denied.
    pub fn new(selector: MonitorSelector) -> DeskResult<Self> {
        if crate::desktop::follows_input_desktop() {
            // Secure-desktop helper: this thread must sit on the desktop being shown.
            let _ = crate::desktop::attach_input_desktop();
        }
        let monitors = list_monitors()?;
        let monitor = selector.select(&monitors)?.clone();
        let (device, context) = create_device(&monitor)?;
        let mut capturer = Self {
            monitor,
            device,
            context,
            duplication: None,
            frame: None,
            size: (0, 0),
            last_reopen_attempt: None,
        };
        capturer.open_duplication()?;
        Ok(capturer)
    }

    pub fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// Current source size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Last captured image (valid once the first `Updated` was returned).
    pub fn frame(&self) -> Option<&ID3D11Texture2D> {
        self.frame.as_ref()
    }

    /// Re-creates the duplication object immediately (also used by tests to exercise recovery).
    pub fn reset_duplication(&mut self) {
        self.duplication = None;
        self.last_reopen_attempt = None;
    }

    /// Waits up to `timeout_ms` for a new frame and copies it into [`Self::frame`].
    pub fn acquire(&mut self, timeout_ms: u32) -> DeskResult<Grab> {
        if self.duplication.is_none() && !self.try_reopen()? {
            return Ok(Grab::Lost);
        }
        let Some(duplication) = self.duplication.clone() else {
            return Ok(Grab::Lost);
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: `info` and `resource` outlive the call; the frame is released below.
        let acquired =
            unsafe { duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) };
        if let Err(e) = acquired {
            let code = e.code();
            if code == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(Grab::Unchanged);
            }
            if code == DXGI_ERROR_ACCESS_LOST {
                tracing::debug!("desktop duplication access lost, will re-open");
                self.duplication = None;
                return Ok(Grab::Lost);
            }
            return Err(map_device_error(code, "AcquireNextFrame", &e));
        }
        // LastPresentTime == 0 means only the mouse pointer moved.
        let presented = info.LastPresentTime != 0;
        let copied = match (presented, resource) {
            (true, Some(resource)) => self.copy_frame(&resource),
            _ => Ok(false),
        };
        // SAFETY: matches the successful AcquireNextFrame above.
        let released = unsafe { duplication.ReleaseFrame() };
        if let Err(e) = released {
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                self.duplication = None;
                return Ok(Grab::Lost);
            }
            return Err(map_device_error(e.code(), "ReleaseFrame", &e));
        }
        Ok(if copied? {
            Grab::Updated
        } else {
            Grab::Unchanged
        })
    }

    fn copy_frame(&mut self, resource: &IDXGIResource) -> DeskResult<bool> {
        let source: ID3D11Texture2D = resource
            .cast()
            .context(DeskError::Capture, "frame is not a texture")?;
        let frame = self.ensure_frame_texture(&source)?;
        // SAFETY: both textures belong to this device and have identical size and format.
        unsafe { self.context.CopyResource(&frame, &source) };
        Ok(true)
    }

    /// Keeps a persistent copy target with the same format and size as the duplicated image.
    fn ensure_frame_texture(&mut self, source: &ID3D11Texture2D) -> DeskResult<ID3D11Texture2D> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a valid out-parameter.
        unsafe { source.GetDesc(&mut desc) };
        if let Some(existing) = &self.frame {
            let mut current = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: `current` is a valid out-parameter.
            unsafe { existing.GetDesc(&mut current) };
            if current.Width == desc.Width
                && current.Height == desc.Height
                && current.Format == desc.Format
            {
                return Ok(existing.clone());
            }
        }
        desc.Usage = D3D11_USAGE_DEFAULT;
        // The D3D11 video processor needs a render-target capable input texture.
        desc.BindFlags = (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32;
        desc.CPUAccessFlags = 0;
        desc.MiscFlags = 0;
        let mut texture = None;
        // SAFETY: `desc` is fully initialised and `texture` is a valid out-parameter.
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .context(DeskError::Capture, "CreateTexture2D(frame)")?;
        }
        let texture =
            texture.ok_or_else(|| DeskError::Capture("CreateTexture2D returned null".into()))?;
        self.size = (desc.Width, desc.Height);
        self.frame = Some(texture.clone());
        Ok(texture)
    }

    /// Rate-limited re-open; `Ok(false)` means "not available yet".
    fn try_reopen(&mut self) -> DeskResult<bool> {
        if let Some(last) = self.last_reopen_attempt {
            if last.elapsed() < REOPEN_INTERVAL {
                return Ok(false);
            }
        }
        self.last_reopen_attempt = Some(Instant::now());
        match self.open_duplication() {
            Ok(()) => {
                tracing::info!(
                    width = self.size.0,
                    height = self.size.1,
                    "desktop duplication re-opened"
                );
                Ok(true)
            }
            Err(e) if is_transient(&e) => {
                tracing::debug!(error = %e, "desktop duplication not available yet");
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn open_duplication(&mut self) -> DeskResult<()> {
        // SAFETY: plain DXGI calls on owned COM objects.
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().context(DeskError::Capture, "CreateDXGIFactory1")?;
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapters1(self.monitor.adapter_index)
                .context(DeskError::Capture, "EnumAdapters1")?;
            let output = adapter
                .EnumOutputs(self.monitor.output_index)
                .context(DeskError::Capture, "EnumOutputs")?;
            let output1: IDXGIOutput1 = output
                .cast()
                .context(DeskError::Capture, "IDXGIOutput1 unsupported")?;
            let duplication = output1
                .DuplicateOutput(&self.device)
                .map_err(|e| classify_duplicate_error(e.code(), "DuplicateOutput", &e))?;
            let desc = duplication.GetDesc();
            let size = (desc.ModeDesc.Width, desc.ModeDesc.Height);
            if size != self.size {
                // New mode: drop the old copy target; it is re-created on the next frame.
                self.frame = None;
                self.size = size;
            }
            self.duplication = Some(duplication);
        }
        Ok(())
    }
}

fn create_device(monitor: &MonitorInfo) -> DeskResult<(ID3D11Device, ID3D11DeviceContext)> {
    // SAFETY: plain D3D11/DXGI creation with owned out-parameters.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().context(DeskError::Capture, "CreateDXGIFactory1")?;
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapters1(monitor.adapter_index)
            .context(DeskError::Capture, "EnumAdapters1")?;
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .context(DeskError::Capture, "D3D11CreateDevice")?;
        let device = device.ok_or_else(|| DeskError::Capture("no D3D11 device".into()))?;
        let context = context.ok_or_else(|| DeskError::Capture("no D3D11 context".into()))?;
        // The encoder runs on its own thread against the same device.
        if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
            let _ = multithread.SetMultithreadProtected(true);
        }
        Ok((device, context))
    }
}

const TRANSIENT_PREFIX: &str = "transient: ";

/// Access denied / lost / session switch while opening means "try again later".
fn classify_duplicate_error(
    code: windows::core::HRESULT,
    what: &str,
    error: &windows::core::Error,
) -> DeskError {
    if code == E_ACCESSDENIED
        || code == DXGI_ERROR_ACCESS_LOST
        || code == DXGI_ERROR_SESSION_DISCONNECTED
    {
        DeskError::Capture(format!("{TRANSIENT_PREFIX}{what}: {error}"))
    } else {
        map_device_error(code, what, error)
    }
}

fn is_transient(error: &DeskError) -> bool {
    matches!(error, DeskError::Capture(m) if m.starts_with(TRANSIENT_PREFIX))
}

fn map_device_error(
    code: windows::core::HRESULT,
    what: &str,
    error: &windows::core::Error,
) -> DeskError {
    if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
        DeskError::Capture(format!("GPU device lost during {what}: {error}"))
    } else {
        DeskError::Capture(format!("{what}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an interactive desktop, so it only runs with `--ignored`.
    #[test]
    #[ignore = "requires an interactive desktop session"]
    fn duplication_recovers_after_reset() -> DeskResult<()> {
        let mut capturer = Capturer::new(MonitorSelector::Primary)?;
        let first = capturer.acquire(500)?;
        assert!(!matches!(first, Grab::Lost), "no frame before reset");
        capturer.reset_duplication();
        let mut recovered = false;
        for _ in 0..20 {
            if !matches!(capturer.acquire(200)?, Grab::Lost) {
                recovered = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(recovered, "capture did not recover within 2 s");
        Ok(())
    }
}
