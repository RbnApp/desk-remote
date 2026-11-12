//! Translates the web UI's input events into `desk-protocol` payloads.
//!
//! The host injects raw scancodes (set 1, `0xE0xx` for extended keys), so browser
//! `KeyboardEvent.code` names are mapped here and mouse buttons become the wire bit mask.

use super::InputEvent;
use desk_protocol::InputPayload;

/// Wire mouse button bits: 0 left, 1 right, 2 middle, 3 back (X1), 4 forward (X2).
fn button_bit(web_button: u8) -> u8 {
    match web_button {
        0 => 0b0_0001,
        2 => 0b0_0010,
        1 => 0b0_0100,
        3 => 0b0_1000,
        4 => 0b1_0000,
        _ => 0,
    }
}

/// Set-1 scancode of a `KeyboardEvent.code`.
pub fn scancode(code: &str) -> Option<u32> {
    let letters = "QWERTYUIOP";
    let home = "ASDFGHJKL";
    let bottom = "ZXCVBNM";
    if let Some(letter) = code.strip_prefix("Key").and_then(|k| k.chars().next()) {
        let (row, base) = [(letters, 0x10), (home, 0x1E), (bottom, 0x2C)]
            .into_iter()
            .find(|(row, _)| row.contains(letter))?;
        return Some(base + u32::try_from(row.find(letter)?).ok()?);
    }
    if let Some(digit) = code.strip_prefix("Digit").and_then(|d| d.chars().next()) {
        return match digit {
            '1'..='9' => Some(0x02 + (digit as u32 - '1' as u32)),
            '0' => Some(0x0B),
            _ => None,
        };
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            1..=10 => Some(0x3A + n),
            11 => Some(0x57),
            12 => Some(0x58),
            _ => None,
        };
    }
    if let Some(n) = code
        .strip_prefix("Numpad")
        .and_then(|n| n.parse::<u32>().ok())
    {
        const PAD: [u32; 10] = [0x52, 0x4F, 0x50, 0x51, 0x4B, 0x4C, 0x4D, 0x47, 0x48, 0x49];
        return PAD.get(usize::try_from(n).ok()?).copied();
    }
    Some(match code {
        "Escape" => 0x01,
        "Minus" => 0x0C,
        "Equal" => 0x0D,
        "Backspace" => 0x0E,
        "Tab" => 0x0F,
        "BracketLeft" => 0x1A,
        "BracketRight" => 0x1B,
        "Enter" => 0x1C,
        "ControlLeft" => 0x1D,
        "Semicolon" => 0x27,
        "Quote" => 0x28,
        "Backquote" => 0x29,
        "ShiftLeft" => 0x2A,
        "Backslash" => 0x2B,
        "Comma" => 0x33,
        "Period" => 0x34,
        "Slash" => 0x35,
        "ShiftRight" => 0x36,
        "NumpadMultiply" => 0x37,
        "AltLeft" => 0x38,
        "Space" => 0x39,
        "CapsLock" => 0x3A,
        "NumLock" => 0x45,
        "ScrollLock" => 0x46,
        "NumpadSubtract" => 0x4A,
        "NumpadAdd" => 0x4E,
        "NumpadDecimal" => 0x53,
        "NumpadEnter" => 0xE01C,
        "ControlRight" => 0xE01D,
        "NumpadDivide" => 0xE035,
        "AltRight" => 0xE038,
        "Home" => 0xE047,
        "ArrowUp" => 0xE048,
        "PageUp" => 0xE049,
        "ArrowLeft" => 0xE04B,
        "ArrowRight" => 0xE04D,
        "End" => 0xE04F,
        "ArrowDown" => 0xE050,
        "PageDown" => 0xE051,
        "Insert" => 0xE052,
        "Delete" => 0xE053,
        "MetaLeft" => 0xE05B,
        "MetaRight" => 0xE05C,
        "ContextMenu" => 0xE05D,
        _ => return None,
    })
}

/// Tracks held mouse buttons so each wire message carries the full button state.
#[derive(Debug, Default)]
pub struct InputTranslator {
    buttons: u8,
    x: u16,
    y: u16,
    /// Scroll pixels not yet turned into a whole notch (carried between events).
    wheel_px: i32,
}

/// Web scroll pixels that make up one wheel notch.
const PIXELS_PER_NOTCH: i32 = 100;

impl InputTranslator {
    /// Converts one UI event; `None` for keys without a known scancode.
    pub fn translate(&mut self, event: &InputEvent) -> Option<InputPayload> {
        Some(match event {
            InputEvent::MouseMove { x, y } => {
                (self.x, self.y) = (*x, *y);
                self.mouse(0)
            }
            InputEvent::MouseButton {
                button,
                pressed,
                x,
                y,
            } => {
                (self.x, self.y) = (*x, *y);
                let bit = button_bit(*button);
                if *pressed {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                }
                self.mouse(0)
            }
            // Web deltas are positive when scrolling down; the wire counts notches, positive = up.
            InputEvent::Wheel { dy, .. } => {
                self.wheel_px = self.wheel_px.saturating_add(*dy);
                let notches = (self.wheel_px / PIXELS_PER_NOTCH).clamp(-127, 127);
                self.wheel_px -= notches * PIXELS_PER_NOTCH;
                self.mouse(i8::try_from(-notches).unwrap_or(0))
            }
            InputEvent::Key { code, pressed } => InputPayload::Key {
                scancode: scancode(code)?,
                down: *pressed,
            },
        })
    }

    fn mouse(&self, wheel: i8) -> InputPayload {
        InputPayload::Mouse {
            x: self.x,
            y: self.y,
            buttons: self.buttons,
            wheel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_keys_map_to_set_1() {
        assert_eq!(scancode("KeyA"), Some(0x1E));
        assert_eq!(scancode("KeyQ"), Some(0x10));
        assert_eq!(scancode("KeyM"), Some(0x32));
        assert_eq!(scancode("Digit1"), Some(0x02));
        assert_eq!(scancode("Digit0"), Some(0x0B));
        assert_eq!(scancode("F1"), Some(0x3B));
        assert_eq!(scancode("F12"), Some(0x58));
        assert_eq!(scancode("ArrowUp"), Some(0xE048));
        assert_eq!(scancode("Numpad0"), Some(0x52));
        assert_eq!(scancode("Unidentified"), None);
    }

    #[test]
    fn buttons_accumulate_and_wheel_flips_direction() {
        let mut t = InputTranslator::default();
        let down = InputEvent::MouseButton {
            button: 0,
            pressed: true,
            x: 10,
            y: 20,
        };
        assert_eq!(
            t.translate(&down),
            Some(InputPayload::Mouse {
                x: 10,
                y: 20,
                buttons: 1,
                wheel: 0
            })
        );
        let up = InputEvent::MouseButton {
            button: 0,
            pressed: false,
            x: 10,
            y: 20,
        };
        assert!(matches!(
            t.translate(&up),
            Some(InputPayload::Mouse { buttons: 0, .. })
        ));
        // Scrolling down (positive web delta) is one notch towards the user (negative).
        assert!(matches!(
            t.translate(&InputEvent::Wheel { dy: 100 }),
            Some(InputPayload::Mouse { wheel: -1, .. })
        ));
    }

    fn wheel_of(t: &mut InputTranslator, dy: i32) -> i8 {
        match t.translate(&InputEvent::Wheel { dy }) {
            Some(InputPayload::Mouse { wheel, .. }) => wheel,
            other => panic!("not a mouse payload: {other:?}"),
        }
    }

    #[test]
    fn wheel_pixels_accumulate_into_whole_notches() {
        let mut t = InputTranslator::default();
        // 30 + 30 + 40 px is exactly one notch, delivered on the last event.
        assert_eq!(wheel_of(&mut t, 30), 0);
        assert_eq!(wheel_of(&mut t, 30), 0);
        assert_eq!(wheel_of(&mut t, 40), -1);
        // The remainder is kept: 150 px = one notch now, one more after another 50 px.
        assert_eq!(wheel_of(&mut t, 150), -1);
        assert_eq!(wheel_of(&mut t, 50), -1);
        // Upwards works the same way, and a big flick sends several notches at once.
        assert_eq!(wheel_of(&mut t, -250), 2);
    }
}
