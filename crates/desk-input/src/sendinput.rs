//! # SendInput backend
//!
//! ## Responsibility
//! Implements [`InputBackend`] with the Win32 `SendInput` API: absolute mouse moves over
//! the virtual desktop, button / wheel events and scancode keyboard events.
//!
//! ## Scope Boundaries
//! - Make the process per-monitor DPI aware at creation so pixel math matches DXGI.
//! - UIPI applies: input into higher-integrity windows is dropped by the OS.
//!
//! ## Dependencies
//! `injector` (trait and events), `desk-capture` (error context), `desk-error`.

use crate::injector::{InputBackend, InputInjector, RawInput};
use crate::mapping::MouseButton;
use desk_capture::ScreenRect;
use desk_error::{DeskError, DeskResult};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

/// Injector bound to the real Windows input queue.
pub type SystemInjector = InputInjector<SendInputBackend>;

/// `SendInput` backend.
pub struct SendInputBackend;

impl SendInputBackend {
    /// Creates the backend and makes the process per-monitor DPI aware (idempotent).
    pub fn new() -> Self {
        // SAFETY: process-wide DPI mode; failure only means it was already set.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        Self
    }
}

impl Default for SendInputBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn to_input(event: RawInput) -> INPUT {
    match event {
        RawInput::MouseMove { x, y } => mouse_input(
            x,
            y,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        ),
        RawInput::MouseButton { button, down } => {
            let (flags, data) = match (button, down) {
                (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                (MouseButton::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                (MouseButton::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                (MouseButton::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                (MouseButton::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2),
            };
            mouse_input(0, 0, data, flags)
        }
        RawInput::Wheel { delta } => mouse_input(0, 0, delta as u32, MOUSEEVENTF_WHEEL),
        RawInput::Key {
            scan,
            extended,
            down,
        } => {
            let mut flags = KEYEVENTF_SCANCODE;
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: scan,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
    }
}

impl InputBackend for SendInputBackend {
    fn send(&mut self, events: &[RawInput]) -> DeskResult<()> {
        let inputs: Vec<INPUT> = events.iter().copied().map(to_input).collect();
        // SAFETY: `inputs` is a valid slice of fully initialised INPUT structures.
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if (sent as usize) < inputs.len() {
            return Err(DeskError::Input(format!(
                "SendInput injected {sent} of {} events (blocked by UIPI or secure desktop?)",
                inputs.len()
            )));
        }
        Ok(())
    }

    fn virtual_desktop(&self) -> ScreenRect {
        // SAFETY: GetSystemMetrics has no preconditions.
        unsafe {
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
            ScreenRect {
                left,
                top,
                right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
                bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        }
    }
}

/// Creates an injector on the real input queue for the given captured monitor.
pub fn system_injector(monitor: ScreenRect) -> SystemInjector {
    InputInjector::new(SendInputBackend::new(), monitor)
}
