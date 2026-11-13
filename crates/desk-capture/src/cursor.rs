//! # Cursor tracking
//!
//! ## Responsibility
//! DXGI duplication never contains the mouse cursor, so the viewer needs it separately: this
//! module samples the cursor position (normalised over the captured monitor, the same 0..=65535
//! range the input path uses) and renders each distinct cursor shape once to straight-alpha
//! RGBA, ready to be sent to the viewer and drawn on top of the picture.
//!
//! ## Scope Boundaries
//! - Sampling and shape rendering only; sending and drawing live elsewhere.
//! - Needs the per-monitor DPI mode of [`crate::ensure_per_monitor_dpi_aware`] so the position
//!   is in the same physical pixels as the monitor rectangle.
//!
//! ## Dependencies
//! `monitor` ([`ScreenRect`]), `windows` (cursor and GDI).

use crate::monitor::ScreenRect;
use std::collections::{HashMap, HashSet};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetObjectW, SelectObject, BITMAP,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, LoadCursorW, CURSORINFO, CURSOR_SHOWING, DI_NORMAL,
    HICON, ICONINFO, IDC_ARROW,
};

/// Cursor bitmaps larger than this (per side) are cut off; real cursors are 32..=96 px.
const MAX_SIDE: i32 = 128;
/// Normalised coordinate range shared with the input path.
const NORMALIZED_MAX: i64 = 65535;
/// Rendered shapes remembered so toggling between arrow and I-beam does not re-render.
const SHAPE_CACHE: usize = 48;

/// One cursor image: straight-alpha RGBA, top-down, with the hot spot inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub id: u32,
    pub hot_x: u16,
    pub hot_y: u16,
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

/// Where the cursor is, in the 0..=65535 range over the captured monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub x: u16,
    pub y: u16,
    pub shape_id: u32,
    /// False when the cursor is hidden or outside the captured monitor.
    pub visible: bool,
}

/// Result of one [`CursorTracker::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorUpdate {
    pub pos: CursorPos,
    /// Present the first time this shape is seen since [`CursorTracker::reset`].
    pub new_shape: Option<CursorShape>,
}

/// Maps a physical screen point to 0..=65535 over `monitor`; `None` when outside it.
/// Inverse of the mapping `desk-input` applies when injecting.
#[must_use]
pub fn normalize_to_monitor(x: i32, y: i32, monitor: ScreenRect) -> Option<(u16, u16)> {
    let axis = |v: i32, start: i32, len: i32| -> Option<u16> {
        let rel = i64::from(v) - i64::from(start);
        let span = i64::from(len.max(2)) - 1;
        if rel < 0 || rel > span {
            return None;
        }
        u16::try_from((rel * NORMALIZED_MAX + span / 2) / span).ok()
    };
    Some((
        axis(x, monitor.left, monitor.width())?,
        axis(y, monitor.top, monitor.height())?,
    ))
}

/// Which image the viewer should get for one cursor sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeChoice {
    /// The cursor Windows reports.
    Real,
    /// The standard arrow: Windows reports no usable cursor.
    Arrow,
}

/// Decides the image from the `GetCursorInfo` state. A PC without a mouse (hidden or suppressed
/// cursor, empty handle) still has a pointer the remote user drives, so it is drawn as the arrow
/// instead of vanishing. Visibility then only depends on the cursor being over the monitor.
#[must_use]
pub fn choose_shape(showing: bool, has_handle: bool) -> ShapeChoice {
    if showing && has_handle {
        ShapeChoice::Real
    } else {
        ShapeChoice::Arrow
    }
}

/// Combines the same cursor drawn over black and over white (both BGRA) into straight-alpha
/// RGBA. Pixels that invert the background (the classic I-beam) become opaque black.
#[must_use]
pub fn compose_rgba(on_black: &[u8], on_white: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(on_black.len());
    for (b, w) in on_black.chunks_exact(4).zip(on_white.chunks_exact(4)) {
        let diff: i32 = (0..3)
            .map(|c| i32::from(w[c]) - i32::from(b[c]))
            .sum::<i32>()
            / 3;
        if diff < 0 {
            out.extend_from_slice(&[0, 0, 0, 255]);
            continue;
        }
        let alpha = (255 - diff).clamp(0, 255);
        if alpha == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let un = |c: u8| u8::try_from((i32::from(c) * 255 / alpha).clamp(0, 255)).unwrap_or(255);
        // BGRA -> RGBA
        out.extend_from_slice(&[
            un(b[2]),
            un(b[1]),
            un(b[0]),
            u8::try_from(alpha).unwrap_or(255),
        ]);
    }
    out
}

/// Samples the cursor of the interactive desktop.
#[derive(Default)]
pub struct CursorTracker {
    shapes: HashMap<isize, u32>,
    sent: HashSet<u32>,
    next_id: u32,
}

impl CursorTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets which shapes the viewer already has (call when a viewer joins).
    pub fn reset(&mut self) {
        self.sent.clear();
    }

    /// Current cursor over `monitor`, with its shape the first time it is seen. `None` when the
    /// cursor cannot be read (secure desktop, locked screen).
    pub fn poll(&mut self, monitor: ScreenRect) -> Option<CursorUpdate> {
        let mut info = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `info.cbSize` is set; the structure is a valid out-parameter.
        unsafe { GetCursorInfo(&mut info) }.ok()?;
        let showing = info.flags.0 & CURSOR_SHOWING.0 != 0;
        let inside = normalize_to_monitor(info.ptScreenPos.x, info.ptScreenPos.y, monitor);
        let (x, y) = inside.unwrap_or((0, 0));
        let cursor = match choose_shape(showing, !info.hCursor.is_invalid()) {
            ShapeChoice::Real => HICON(info.hCursor.0),
            // SAFETY: shared system cursor; never destroyed.
            ShapeChoice::Arrow => match unsafe { LoadCursorW(None, IDC_ARROW) } {
                Ok(arrow) => HICON(arrow.0),
                Err(_) => {
                    return Some(CursorUpdate {
                        pos: CursorPos {
                            x,
                            y,
                            shape_id: 0,
                            visible: false,
                        },
                        new_shape: None,
                    })
                }
            },
        };
        let handle = cursor.0 as isize;
        let id = if let Some(id) = self.shapes.get(&handle) {
            *id
        } else {
            if self.shapes.len() >= SHAPE_CACHE {
                self.shapes.clear();
                self.sent.clear();
            }
            self.next_id += 1;
            self.shapes.insert(handle, self.next_id);
            self.next_id
        };
        let new_shape = if self.sent.insert(id) {
            // A cursor that cannot be rendered is retried when it changes, not every tick.
            render_shape(id, cursor)
        } else {
            None
        };
        Some(CursorUpdate {
            pos: CursorPos {
                x,
                y,
                shape_id: id,
                visible: inside.is_some(),
            },
            new_shape,
        })
    }
}

/// Releases the GDI objects of one render pass.
struct Gdi {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        // SAFETY: objects created in `render_over`, released exactly once.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Draws `cursor` over a solid `background` byte (0 = black, 255 = white); returns BGRA pixels.
fn render_over(cursor: HICON, width: i32, height: i32, background: u8) -> Option<Vec<u8>> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    // SAFETY: standard off-screen DIB; every object is released by `Gdi::drop`.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            let _ = DeleteDC(dc);
            return None;
        };
        let previous = SelectObject(dc, bitmap.into());
        let gdi = Gdi {
            dc,
            bitmap,
            previous,
        };
        if bits.is_null() {
            return None;
        }
        let len = usize::try_from(width * height * 4).ok()?;
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), len);
        pixels.fill(background);
        DrawIconEx(gdi.dc, 0, 0, cursor, width, height, 0, None, DI_NORMAL).ok()?;
        Some(pixels.to_vec())
    }
}

/// Size in pixels of a cursor from its bitmaps (monochrome cursors stack AND and XOR masks).
fn cursor_size(info: &ICONINFO) -> Option<(i32, i32)> {
    let color = !info.hbmColor.is_invalid();
    let source = if color { info.hbmColor } else { info.hbmMask };
    let mut bitmap = BITMAP::default();
    // SAFETY: valid bitmap handle and an out-parameter of the stated size.
    let got = unsafe {
        GetObjectW(
            source.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some((&raw mut bitmap).cast()),
        )
    };
    if got == 0 {
        return None;
    }
    let height = if color {
        bitmap.bmHeight
    } else {
        bitmap.bmHeight / 2
    };
    (bitmap.bmWidth > 0 && height > 0).then_some((bitmap.bmWidth, height))
}

fn render_shape(id: u32, cursor: HICON) -> Option<CursorShape> {
    let mut icon = ICONINFO::default();
    // SAFETY: valid out-parameter; the two bitmaps it returns are deleted below.
    unsafe { GetIconInfo(cursor, &mut icon) }.ok()?;
    let size = cursor_size(&icon);
    let (hot_x, hot_y) = (icon.xHotspot, icon.yHotspot);
    // SAFETY: the bitmaps belong to us after GetIconInfo.
    unsafe {
        if !icon.hbmColor.is_invalid() {
            let _ = DeleteObject(icon.hbmColor.into());
        }
        if !icon.hbmMask.is_invalid() {
            let _ = DeleteObject(icon.hbmMask.into());
        }
    }
    let (width, height) = size?;
    let (width, height) = (width.min(MAX_SIDE), height.min(MAX_SIDE));
    let on_black = render_over(cursor, width, height, 0)?;
    let on_white = render_over(cursor, width, height, 255)?;
    let (w16, h16) = (u16::try_from(width).ok()?, u16::try_from(height).ok()?);
    Some(CursorShape {
        id,
        hot_x: u16::try_from(hot_x).unwrap_or(0).min(w16),
        hot_y: u16::try_from(hot_y).unwrap_or(0).min(h16),
        width: w16,
        height: h16,
        rgba: compose_rgba(&on_black, &on_white),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn corners_and_center_normalise_to_the_wire_range() {
        let m = rect(0, 0, 3440, 1440);
        assert_eq!(normalize_to_monitor(0, 0, m), Some((0, 0)));
        assert_eq!(normalize_to_monitor(3439, 1439, m), Some((65535, 65535)));
        let (cx, cy) = normalize_to_monitor(1720, 720, m).unwrap_or_default();
        assert!((32_760..=32_790).contains(&cx), "{cx}");
        assert!((32_760..=32_790).contains(&cy), "{cy}");
    }

    #[test]
    fn secondary_monitor_with_negative_origin_is_relative_to_its_own_corner() {
        let left = rect(-1920, -200, 0, 880);
        assert_eq!(normalize_to_monitor(-1920, -200, left), Some((0, 0)));
        assert_eq!(normalize_to_monitor(-1, 879, left), Some((65535, 65535)));
        assert_eq!(
            normalize_to_monitor(0, 0, left),
            None,
            "right of the monitor"
        );
        assert_eq!(
            normalize_to_monitor(-1921, 0, left),
            None,
            "left of the monitor"
        );
    }

    #[test]
    fn opaque_transparent_half_and_inverting_pixels() {
        // BGRA pairs: opaque red, transparent, 50 % white, inverting.
        let black = [
            0, 0, 255, 255, 0, 0, 0, 0, 128, 128, 128, 255, 255, 255, 255, 255,
        ];
        let white = [
            0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
        ];
        let out = compose_rgba(&black, &white);
        assert_eq!(&out[0..4], &[255, 0, 0, 255], "opaque red");
        assert_eq!(&out[4..8], &[0, 0, 0, 0], "transparent");
        assert_eq!(out[11], 128, "half transparent keeps ~50 % alpha");
        assert_eq!(
            &out[12..16],
            &[0, 0, 0, 255],
            "inverting pixel becomes opaque black"
        );
    }

    #[test]
    fn shape_choice_real_when_showing_arrow_otherwise() {
        assert_eq!(choose_shape(true, true), ShapeChoice::Real);
        assert_eq!(choose_shape(false, true), ShapeChoice::Arrow, "suppressed");
        assert_eq!(choose_shape(false, false), ShapeChoice::Arrow, "no flags");
        assert_eq!(
            choose_shape(true, false),
            ShapeChoice::Arrow,
            "empty handle"
        );
    }

    #[test]
    fn outside_the_monitor_stays_hidden() {
        assert_eq!(normalize_to_monitor(5000, 5, rect(0, 0, 1920, 1080)), None);
    }

    /// Moves the real pointer, so it is opt-in: `cargo test -p desk-capture -- --ignored`.
    #[test]
    #[ignore = "moves the physical pointer"]
    fn position_follows_the_pointer_without_a_mouse() {
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        let mut tracker = CursorTracker::new();
        let m = rect(0, 0, 100_000, 100_000);
        for (px, py) in [(300, 200), (640, 480)] {
            if unsafe { SetCursorPos(px, py) }.is_err() {
                return;
            }
            let Some(u) = tracker.poll(m) else { return };
            assert_eq!(
                (u.pos.x, u.pos.y),
                normalize_to_monitor(px, py, m).unwrap_or_default()
            );
            assert!(u.pos.visible);
        }
    }

    #[test]
    fn the_real_cursor_renders_with_a_hot_spot_inside_it() {
        let mut tracker = CursorTracker::new();
        let Some(update) = tracker.poll(rect(0, 0, 100_000, 100_000)) else {
            return; // no interactive desktop (service session)
        };
        if let Some(shape) = update.new_shape {
            assert_eq!(
                shape.rgba.len(),
                usize::from(shape.width) * usize::from(shape.height) * 4
            );
            assert!(shape.hot_x <= shape.width && shape.hot_y <= shape.height);
            assert!(shape.rgba.chunks_exact(4).any(|p| p[3] > 0), "not blank");
        }
    }

    /// DAPP-107: the standard arrow must render without a frame - the border pixels of the image
    /// stay fully transparent, and only the arrow itself is opaque.
    #[test]
    fn the_arrow_renders_without_a_box_around_it() {
        // SAFETY: shared system cursor; never destroyed.
        let Ok(arrow) = (unsafe { LoadCursorW(None, IDC_ARROW) }) else {
            return; // no interactive desktop
        };
        let Some(shape) = render_shape(1, HICON(arrow.0)) else {
            return;
        };
        let (w, h) = (usize::from(shape.width), usize::from(shape.height));
        let alpha = |x: usize, y: usize| shape.rgba[(y * w + x) * 4 + 3];
        for i in 0..w {
            assert_eq!(alpha(i, h - 1), 0, "bottom row pixel {i}");
        }
        for i in 0..h {
            assert_eq!(alpha(w - 1, i), 0, "right column pixel {i}");
        }
        let opaque = shape.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(
            opaque < w * h / 2,
            "a frame/fill covers the image: {opaque}"
        );
    }
}
