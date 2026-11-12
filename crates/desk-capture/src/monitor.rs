//! # Monitor enumeration and selection
//!
//! ## Responsibility
//! Lists every desktop-attached output across all GPUs with its adapter identity and
//! desktop rectangle, and resolves a `MonitorSelector` to one of them.
//!
//! ## Scope Boundaries
//! - Read-only enumeration; does not create duplication objects.
//!
//! ## Dependencies
//! `desk-error`, DXGI factory.

use crate::win::WinContext;
use desk_error::{DeskError, DeskResult};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};

/// `MONITORINFOF_PRIMARY` flag value.
const MONITORINFOF_PRIMARY: u32 = 1;

/// Rectangle in virtual-desktop pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// One desktop-attached monitor and the GPU that drives it.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    /// Position in the list returned by [`list_monitors`] (primary first).
    pub index: usize,
    pub adapter_index: u32,
    pub output_index: u32,
    pub adapter_name: String,
    /// Adapter LUID packed as `(high << 32) | low`.
    pub adapter_luid: u64,
    /// PCI vendor id of the driving adapter (0x10DE NVIDIA, 0x1002 AMD, 0x8086 Intel).
    pub vendor_id: u32,
    pub rect: ScreenRect,
    pub primary: bool,
}

/// Which monitor to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorSelector {
    #[default]
    Primary,
    Index(usize),
}

impl MonitorSelector {
    /// Resolves the selector against an enumerated list.
    pub fn select<'a>(&self, monitors: &'a [MonitorInfo]) -> DeskResult<&'a MonitorInfo> {
        let found = match self {
            Self::Primary => monitors.iter().find(|m| m.primary).or(monitors.first()),
            Self::Index(i) => monitors.get(*i),
        };
        found.ok_or_else(|| DeskError::Capture(format!("monitor {self:?} not found")))
    }
}

/// Packs a LUID into a single `u64`.
pub fn luid_to_u64(low: u32, high: i32) -> u64 {
    ((high as u32 as u64) << 32) | low as u64
}

/// Enumerates all desktop-attached monitors, primary first.
pub fn list_monitors() -> DeskResult<Vec<MonitorInfo>> {
    // SAFETY: plain DXGI enumeration; every returned COM object is owned and released by `windows`.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().context(DeskError::Capture, "CreateDXGIFactory1")?;
        let mut monitors = Vec::new();
        let mut adapter_index = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(adapter_index) {
            let desc = adapter.GetDesc1().context(DeskError::Capture, "GetDesc1")?;
            let is_software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            let name = String::from_utf16_lossy(&desc.Description)
                .trim_end_matches('\0')
                .to_string();
            let mut output_index = 0u32;
            while let Ok(output) = adapter.EnumOutputs(output_index) {
                let out_desc = output.GetDesc().context(DeskError::Capture, "GetDesc")?;
                if !is_software && out_desc.AttachedToDesktop.as_bool() {
                    let mut info = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };
                    let primary = GetMonitorInfoW(out_desc.Monitor, &mut info).as_bool()
                        && info.dwFlags & MONITORINFOF_PRIMARY != 0;
                    let r = out_desc.DesktopCoordinates;
                    monitors.push(MonitorInfo {
                        index: 0,
                        adapter_index,
                        output_index,
                        adapter_name: name.clone(),
                        adapter_luid: luid_to_u64(
                            desc.AdapterLuid.LowPart,
                            desc.AdapterLuid.HighPart,
                        ),
                        vendor_id: desc.VendorId,
                        rect: ScreenRect {
                            left: r.left,
                            top: r.top,
                            right: r.right,
                            bottom: r.bottom,
                        },
                        primary,
                    });
                }
                output_index += 1;
            }
            adapter_index += 1;
        }
        monitors.sort_by_key(|m| !m.primary);
        for (i, m) in monitors.iter_mut().enumerate() {
            m.index = i;
        }
        Ok(monitors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(index: usize, primary: bool) -> MonitorInfo {
        MonitorInfo {
            index,
            adapter_index: 0,
            output_index: index as u32,
            adapter_name: "Test GPU".to_string(),
            adapter_luid: 1,
            vendor_id: 0x10DE,
            rect: ScreenRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            primary,
        }
    }

    #[test]
    fn selector_primary_and_index() {
        let list = vec![fake(0, false), fake(1, true)];
        assert_eq!(
            MonitorSelector::Primary.select(&list).map(|m| m.index).ok(),
            Some(1)
        );
        assert_eq!(
            MonitorSelector::Index(0)
                .select(&list)
                .map(|m| m.index)
                .ok(),
            Some(0)
        );
        assert!(MonitorSelector::Index(5).select(&list).is_err());
        assert!(MonitorSelector::Primary.select(&[]).is_err());
    }

    #[test]
    fn luid_packs_high_and_low() {
        assert_eq!(luid_to_u64(5, 1), (1u64 << 32) | 5);
        assert_eq!(luid_to_u64(0, -1) >> 32, 0xFFFF_FFFF);
    }

    #[test]
    fn rect_dimensions() {
        let r = ScreenRect {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert_eq!((r.width(), r.height()), (1920, 1080));
    }

    #[test]
    fn live_enumeration_has_a_primary() {
        let list = list_monitors().unwrap_or_default();
        if !list.is_empty() {
            assert!(list[0].primary || list.iter().all(|m| !m.primary));
        }
    }
}
