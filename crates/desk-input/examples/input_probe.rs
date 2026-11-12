//! # Input injection probe
//!
//! ## Responsibility
//! Exercises the real `SendInput` path briefly: cursor placement on every monitor
//! (verified with `GetCursorPos`), held-button / held-key release on `release_all`
//! (verified with `GetAsyncKeyState`) and typing into Notepad (verified by file content).
//!
//! ## Scope Boundaries
//! - Manual diagnostics only; moves the cursor and types for a few seconds.
//!
//! ## Dependencies
//! `desk-input`, `desk-capture`, `desk-logger`.

use desk_capture::list_monitors;
use desk_error::{DeskError, DeskResult};
use desk_input::system_injector;
use desk_logger::{init, LoggerConfig};
use std::process::Command;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT, VK_XBUTTON1};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

const SCAN_CTRL: u16 = 0x1D;
const SCAN_SHIFT: u16 = 0x2A;
const SCAN_S: u16 = 0x1F;
const SCAN_ALT: u16 = 0x38;
const SCAN_F4: u16 = 0x3E;
/// "hello" in set-1 scancodes.
const HELLO: [u16; 5] = [0x23, 0x12, 0x26, 0x26, 0x18];

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: `p` is a valid out-parameter.
    let _ = unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

fn is_down(vk: i32) -> bool {
    // SAFETY: no preconditions.
    unsafe { GetAsyncKeyState(vk) < 0 }
}

fn main() -> DeskResult<()> {
    let _guard = init(&LoggerConfig::default())?;
    let mut failures = 0;

    for m in list_monitors()? {
        let mut injector = system_injector(m.rect);
        injector.mouse(32768, 32768, 0, 0)?;
        pause(100);
        let (x, y) = cursor();
        let (ex, ey) = (
            m.rect.left + m.rect.width() / 2,
            m.rect.top + m.rect.height() / 2,
        );
        let ok = (x - ex).abs() <= 2 && (y - ey).abs() <= 2;
        failures += i32::from(!ok);
        tracing::info!(monitor = m.index, expected = ?(ex, ey), actual = ?(x, y), ok, "cursor placement");
    }

    let monitors = list_monitors()?;
    let mut injector = system_injector(monitors[0].rect);
    injector.mouse(32768, 32768, 0b1000, 0)?; // X1 down
    injector.key(SCAN_SHIFT, true)?;
    pause(50);
    let held = (is_down(VK_XBUTTON1.0 as i32), is_down(VK_SHIFT.0 as i32));
    injector.release_all()?;
    pause(50);
    let released = (is_down(VK_XBUTTON1.0 as i32), is_down(VK_SHIFT.0 as i32));
    let ok = held == (true, true) && released == (false, false);
    failures += i32::from(!ok);
    tracing::info!(?held, ?released, ok, "held input released by release_all");

    let path = std::env::temp_dir().join("desk-input-probe.txt");
    std::fs::write(&path, "").map_err(|e| DeskError::Input(e.to_string()))?;
    let mut child = Command::new("notepad.exe")
        .arg(&path)
        .spawn()
        .map_err(|e| DeskError::Input(e.to_string()))?;
    pause(2500);
    for scan in HELLO {
        injector.key(scan, true)?;
        injector.key(scan, false)?;
        pause(30);
    }
    for (scan, down) in [
        (SCAN_CTRL, true),
        (SCAN_S, true),
        (SCAN_S, false),
        (SCAN_CTRL, false),
    ] {
        injector.key(scan, down)?;
        pause(30);
    }
    pause(800);
    for (scan, down) in [
        (SCAN_ALT, true),
        (SCAN_F4, true),
        (SCAN_F4, false),
        (SCAN_ALT, false),
    ] {
        injector.key(scan, down)?;
        pause(30);
    }
    pause(800);
    let _ = child.kill();
    let text = std::fs::read_to_string(&path).map_err(|e| DeskError::Input(e.to_string()))?;
    let ok = text.trim() == "hello";
    failures += i32::from(!ok);
    tracing::info!(content = %text.trim(), ok, "typed into Notepad");
    let _ = std::fs::remove_file(&path);

    if failures > 0 {
        return Err(DeskError::Input(format!(
            "{failures} probe check(s) failed"
        )));
    }
    tracing::info!("all input probe checks passed");
    Ok(())
}
