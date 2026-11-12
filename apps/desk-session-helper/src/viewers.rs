//! "Connected viewers" window: who is watching this PC right now.
//!
//! Replaces the old `MessageBoxW`, which cannot carry the app icon. Same plain-Win32 style
//! and `desk-theme` colours as the About window; closing destroys it.

use crate::ui::{
    colorref, current_theme, draw_text, font, register_app_window_class, shared, wide,
};
use desk_i18n::t;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, SelectObject,
    SetBkMode, SetTextColor, DT_CENTER, DT_LEFT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FW_BOLD,
    FW_NORMAL, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, SetForegroundWindow, ShowWindow,
    HMENU, SW_SHOW, WINDOW_EX_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_PAINT, WS_CAPTION,
    WS_CHILD, WS_SYSMENU, WS_VISIBLE,
};

const BUTTON_CLOSE: usize = 300;

/// The open window (0 = none), so a second click only brings it to the front.
static VIEWERS_HWND: AtomicIsize = AtomicIsize::new(0);

fn paint(hwnd: HWND) {
    let theme = current_theme();
    let tokens = &theme.tokens;
    let viewers = shared().map(|s| s.snapshot().viewers).unwrap_or_default();
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: standard paint sequence on our own window.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let bg = CreateSolidBrush(colorref(&tokens.background));
        FillRect(hdc, &client, bg);
        let _ = DeleteObject(HGDIOBJ(bg.0));
        SetBkMode(hdc, TRANSPARENT);

        let (bold, normal) = (
            font(-20, FW_BOLD.0 as i32, false),
            font(-16, FW_NORMAL.0 as i32, false),
        );
        let at = |top: i32, bottom: i32| RECT {
            left: 24,
            top,
            right: client.right - 24,
            bottom,
        };
        SelectObject(hdc, HGDIOBJ(bold.0));
        SetTextColor(hdc, colorref(&tokens.text));
        draw_text(
            hdc,
            &t("tray.viewers"),
            at(16, 48),
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        SelectObject(hdc, HGDIOBJ(normal.0));
        if viewers.is_empty() {
            SetTextColor(hdc, colorref(&tokens.text_muted));
            draw_text(
                hdc,
                &t("tray.viewers_none"),
                at(64, 130),
                DT_CENTER | DT_WORDBREAK,
            );
        } else {
            SetTextColor(hdc, colorref(&tokens.text_muted));
            draw_text(
                hdc,
                &t("tray.viewers_some"),
                at(56, 80),
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
            SetTextColor(hdc, colorref(&tokens.text));
            draw_text(hdc, &viewers.join("\n"), at(88, 190), DT_LEFT);
        }
        let _ = DeleteObject(HGDIOBJ(bold.0));
        let _ = DeleteObject(HGDIOBJ(normal.0));
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn viewers_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 & 0xFFFF) == BUTTON_CLOSE => {
            // SAFETY: destroying our own window from its thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: destroying our own window from its thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            VIEWERS_HWND.store(0, Ordering::SeqCst);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Registers the icon-carrying window class (once, at start-up).
pub(crate) fn register_class() {
    register_app_window_class(w!("DeskRemoteViewersWindow"), Some(viewers_proc));
}

/// Opens the viewers window (or raises and refreshes it when already open).
pub fn show(_owner: HWND) {
    let existing = VIEWERS_HWND.load(Ordering::SeqCst);
    if existing != 0 {
        // SAFETY: the handle belongs to this thread's window; a stale one just fails.
        unsafe {
            let hwnd = HWND(existing as *mut _);
            let _ = InvalidateRect(Some(hwnd), None, true);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }
    // SAFETY: standard window creation on the UI thread.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let instance = module.into();
        let title = wide(&t("tray.viewers"));
        let Ok(window) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("DeskRemoteViewersWindow"),
            PCWSTR(title.as_ptr()),
            WS_CAPTION | WS_SYSMENU,
            260,
            240,
            440,
            320,
            None,
            None,
            Some(instance),
            None,
        ) else {
            tracing::warn!("viewers window creation failed");
            return;
        };
        let close = wide(&t("common.close"));
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR(close.as_ptr()),
            WS_CHILD | WS_VISIBLE,
            160,
            212,
            110,
            32,
            Some(window),
            Some(HMENU(BUTTON_CLOSE as *mut _)),
            Some(instance),
            None,
        );
        VIEWERS_HWND.store(window.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
    }
}
