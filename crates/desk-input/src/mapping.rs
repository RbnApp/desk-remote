//! # Input coordinate and state mapping
//!
//! ## Responsibility
//! Pure functions that map viewer coordinates (0..65535 over the captured monitor) onto
//! the Windows virtual-desktop absolute range, split extended scancodes and diff mouse
//! button masks into press / release transitions.
//!
//! ## Scope Boundaries
//! - No Windows calls, so everything here is unit-testable.
//!
//! ## Dependencies
//! `desk-capture` for [`ScreenRect`].

use desk_capture::ScreenRect;

/// Normalised coordinate range used on the wire and by `SendInput` absolute mode.
pub const NORMALIZED_MAX: i64 = 65535;

/// Mouse buttons in wire order (bit 0 = left ... bit 4 = X2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

const ALL_BUTTONS: [MouseButton; 5] = [
    MouseButton::Left,
    MouseButton::Right,
    MouseButton::Middle,
    MouseButton::X1,
    MouseButton::X2,
];

impl MouseButton {
    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Maps `(x, y)` in 0..=65535 over `monitor` to the 0..=65535 range of the whole virtual
/// desktop `virtual_desktop`, as `SendInput` expects with `MOUSEEVENTF_VIRTUALDESK`.
pub fn map_to_virtual_desktop(
    x: u16,
    y: u16,
    monitor: ScreenRect,
    virtual_desktop: ScreenRect,
) -> (i32, i32) {
    let axis = |value: u16, mon_start: i32, mon_len: i32, virt_start: i32, virt_len: i32| {
        let pixel = mon_start as i64 + value as i64 * (mon_len.max(1) as i64 - 1) / NORMALIZED_MAX;
        let span = (virt_len.max(2) as i64) - 1;
        (((pixel - virt_start as i64) * NORMALIZED_MAX + span / 2) / span).clamp(0, NORMALIZED_MAX)
            as i32
    };
    (
        axis(
            x,
            monitor.left,
            monitor.width(),
            virtual_desktop.left,
            virtual_desktop.width(),
        ),
        axis(
            y,
            monitor.top,
            monitor.height(),
            virtual_desktop.top,
            virtual_desktop.height(),
        ),
    )
}

/// Splits a wire scancode into `(scan, extended)`. Extended keys arrive as `0xE0xx`.
pub fn split_scancode(scancode: u16) -> (u16, bool) {
    if scancode >> 8 == 0xE0 {
        (scancode & 0xFF, true)
    } else {
        (scancode & 0xFF, false)
    }
}

/// Button transitions needed to go from `previous` to `current` mask: `(button, pressed)`.
pub fn button_changes(previous: u8, current: u8) -> Vec<(MouseButton, bool)> {
    ALL_BUTTONS
        .iter()
        .filter_map(|&button| {
            let was = previous & button.bit() != 0;
            let now = current & button.bit() != 0;
            (was != now).then_some((button, now))
        })
        .collect()
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
    fn single_monitor_corners_map_to_full_range() {
        let m = rect(0, 0, 1920, 1080);
        assert_eq!(map_to_virtual_desktop(0, 0, m, m), (0, 0));
        assert_eq!(map_to_virtual_desktop(65535, 65535, m, m), (65535, 65535));
    }

    #[test]
    fn second_monitor_to_the_right_maps_into_upper_half() {
        let virt = rect(0, 0, 3840, 1080);
        let right = rect(1920, 0, 3840, 1080);
        let (x0, _) = map_to_virtual_desktop(0, 0, right, virt);
        let (x1, _) = map_to_virtual_desktop(65535, 0, right, virt);
        assert!((32_760..=32_790).contains(&x0), "x0 = {x0}");
        assert_eq!(x1, 65535);
    }

    #[test]
    fn monitor_left_of_primary_has_negative_origin() {
        let virt = rect(-1920, 0, 1920, 1080);
        let left = rect(-1920, 0, 0, 1080);
        assert_eq!(map_to_virtual_desktop(0, 0, left, virt).0, 0);
        let (mid, _) = map_to_virtual_desktop(65535, 0, left, virt);
        assert!((32_740..=32_790).contains(&mid), "mid = {mid}");
    }

    #[test]
    fn center_of_monitor_lands_on_its_center_pixel() {
        let virt = rect(0, 0, 3440, 1440);
        let (x, y) = map_to_virtual_desktop(32768, 32768, virt, virt);
        let px = x as i64 * 3439 / 65535;
        let py = y as i64 * 1439 / 65535;
        assert!((1719..=1721).contains(&px), "px = {px}");
        assert!((719..=721).contains(&py), "py = {py}");
    }

    #[test]
    fn scancode_splitting() {
        assert_eq!(split_scancode(0x1E), (0x1E, false));
        assert_eq!(split_scancode(0xE04B), (0x4B, true));
    }

    #[test]
    fn button_diff() {
        assert!(button_changes(0b00, 0b00).is_empty());
        assert_eq!(button_changes(0b00, 0b01), vec![(MouseButton::Left, true)]);
        assert_eq!(
            button_changes(0b101, 0b010),
            vec![
                (MouseButton::Left, false),
                (MouseButton::Right, true),
                (MouseButton::Middle, false)
            ]
        );
    }
}
