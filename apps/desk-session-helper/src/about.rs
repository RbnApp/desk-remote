//! "About" window: product, version, device ID and identity fingerprint.
//!
//! Same plain-Win32 style as the pairing-code window; colours come from the active
//! `desk-theme` palette. Closing the window destroys it, the agent keeps running.

use crate::ui::{colorref, current_theme, draw_text, font, load_icon_sized, shared, wide};
use desk_i18n::t;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, SelectObject, SetBkMode,
    SetTextColor, DT_CENTER, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, HGDIOBJ,
    PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DrawIconEx, GetClientRect,
    RegisterClassW, SetForegroundWindow, ShowWindow, DI_NORMAL, HMENU, SW_SHOW, WINDOW_EX_STYLE,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_SYSMENU,
    WS_VISIBLE,
};

const BUTTON_CLOSE: usize = 200;
const ICON_SIZE: i32 = 64;

/// The open About window (0 = none), so a second click only brings it to the front.
static ABOUT_HWND: AtomicIsize = AtomicIsize::new(0);

fn paint(hwnd: HWND) {
    let theme = current_theme();
    let tokens = &theme.tokens;
    let info = shared()
        .map(|s| s.snapshot())
        .and_then(|ui| ui.info)
        .unwrap_or_default();
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

        let icon = load_icon_sized(ICON_SIZE);
        let _ = DrawIconEx(hdc, 24, 20, icon, ICON_SIZE, ICON_SIZE, 0, None, DI_NORMAL);
        let _ = DestroyIcon(icon);

        let (title, normal, mono) = (
            font(-24, FW_BOLD.0 as i32, false),
            font(-16, FW_NORMAL.0 as i32, false),
            font(-15, FW_NORMAL.0 as i32, true),
        );
        let at = |left: i32, top: i32, bottom: i32| RECT {
            left,
            top,
            right: client.right - 24,
            bottom,
        };
        let row = |label: &str, value: &str, top: i32, mono_value: bool| {
            SelectObject(hdc, HGDIOBJ(normal.0));
            SetTextColor(hdc, colorref(&tokens.text_muted));
            draw_text(
                hdc,
                label,
                at(24, top, top + 22),
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
            SelectObject(hdc, HGDIOBJ(if mono_value { mono.0 } else { normal.0 }));
            SetTextColor(hdc, colorref(&tokens.text));
            draw_text(
                hdc,
                value,
                at(24, top + 22, top + 46),
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
        };

        SelectObject(hdc, HGDIOBJ(title.0));
        SetTextColor(hdc, colorref(&tokens.text));
        draw_text(
            hdc,
            &t("about.product"),
            at(104, 24, 56),
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        SelectObject(hdc, HGDIOBJ(normal.0));
        SetTextColor(hdc, colorref(&tokens.text_muted));
        let version = t("about.version").replace("{v}", desk_version::CURRENT);
        draw_text(
            hdc,
            &version,
            at(104, 56, 80),
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );

        row(&t("about.device_id"), &info.device_id, 108, true);
        row(&t("about.fingerprint"), &info.fingerprint, 164, true);
        SelectObject(hdc, HGDIOBJ(normal.0));
        SetTextColor(hdc, colorref(&tokens.text_muted));
        draw_text(
            hdc,
            &t("about.hint"),
            at(24, 218, 244),
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );

        let _ = DeleteObject(HGDIOBJ(title.0));
        let _ = DeleteObject(HGDIOBJ(normal.0));
        let _ = DeleteObject(HGDIOBJ(mono.0));
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn about_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
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
            ABOUT_HWND.store(0, Ordering::SeqCst);
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Opens the About window (or raises it when already open).
pub fn show(_owner: HWND) {
    let existing = ABOUT_HWND.load(Ordering::SeqCst);
    if existing != 0 {
        // SAFETY: the handle belongs to this thread's window; a stale one just fails.
        unsafe {
            let _ = SetForegroundWindow(HWND(existing as *mut _));
        }
        return;
    }
    // SAFETY: standard window-class registration and creation on the UI thread.
    let created = unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let instance = module.into();
        let class = w!("DeskRemoteAboutWindow");
        // Registering twice just fails harmlessly; the first registration stays.
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(about_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        });
        let title = wide(&t("about.title"));
        let Ok(window) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(title.as_ptr()),
            WS_CAPTION | WS_SYSMENU,
            240,
            220,
            480,
            360,
            None,
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        let close = wide(&t("common.close"));
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR(close.as_ptr()),
            WS_CHILD | WS_VISIBLE,
            180,
            252,
            110,
            32,
            Some(window),
            Some(HMENU(BUTTON_CLOSE as *mut _)),
            Some(instance),
            None,
        );
        ABOUT_HWND.store(window.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
        window
    };
    let _ = created;
}
