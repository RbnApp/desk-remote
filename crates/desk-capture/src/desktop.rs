//! # Input desktop following (secure desktop capture)
//!
//! ## Responsibility
//! Lets a SYSTEM process capture and drive whichever desktop currently receives input: the
//! user's desktop, the UAC consent prompt or the lock screen (the Winlogon desktop). A thread
//! sees exactly one desktop, so each capture/input thread attaches itself here.
//!
//! ## Scope Boundaries
//! - Only the desktop switch; capture, encoding and input injection live elsewhere.
//! - Attaching succeeds only for a process with the rights to open the input desktop
//!   (SYSTEM); an ordinary user process keeps its desktop and nothing changes for it.
//!
//! ## Dependencies
//! `windows` (StationsAndDesktops).

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop,
    DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, UOI_NAME,
};

/// `GENERIC_ALL`: what the thread needs to capture and inject on the desktop it attaches to.
const DESKTOP_ALL_ACCESS: u32 = 0x1000_0000;

static FOLLOW: AtomicBool = AtomicBool::new(false);

/// Makes every new capture session attach to the input desktop first (secure-desktop helper).
pub fn set_follow_input_desktop(on: bool) {
    FOLLOW.store(on, Ordering::SeqCst);
}

/// `true` when [`set_follow_input_desktop`] was switched on in this process.
#[must_use]
pub fn follows_input_desktop() -> bool {
    FOLLOW.load(Ordering::SeqCst)
}

/// Name of the desktop that receives input right now (`Default`, `Winlogon`, ...).
#[must_use]
pub fn input_desktop_name() -> Option<String> {
    // SAFETY: plain query; the handle is closed below.
    let desktop =
        unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) }.ok()?;
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    // SAFETY: the buffer and its byte length describe `buf`.
    let queried = unsafe {
        GetUserObjectInformationW(
            windows::Win32::Foundation::HANDLE(desktop.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            std::mem::size_of_val(&buf) as u32,
            Some(&mut needed),
        )
    };
    // SAFETY: handle returned above.
    let _ = unsafe { CloseDesktop(desktop) };
    queried.ok()?;
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Attaches the calling thread to the current input desktop and returns its name.
///
/// The desktop handle stays open on purpose: Windows refuses to close a desktop a thread of
/// the process still uses, and switches are rare (UAC prompt, lock, unlock).
pub fn attach_input_desktop() -> Option<String> {
    // SAFETY: plain open; the handle is handed to SetThreadDesktop and kept.
    let desktop = unsafe {
        OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            windows::Win32::System::StationsAndDesktops::DESKTOP_ACCESS_FLAGS(DESKTOP_ALL_ACCESS),
        )
    }
    .ok()?;
    // SAFETY: valid desktop handle; fails (harmlessly) for a thread that owns windows.
    if unsafe { SetThreadDesktop(desktop) }.is_err() {
        // SAFETY: not attached, so it can be closed.
        let _ = unsafe { CloseDesktop(desktop) };
        return None;
    }
    input_desktop_name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_desktop_has_a_name() {
        // Read-only query: on a normal signed-in session the input desktop is "Default".
        if let Some(name) = input_desktop_name() {
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn follow_switch_defaults_off_and_toggles() {
        assert!(!follows_input_desktop());
        set_follow_input_desktop(true);
        assert!(follows_input_desktop());
        set_follow_input_desktop(false);
    }
}
