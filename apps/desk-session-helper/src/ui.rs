//! Tray icon, context menu and the pairing-code window (plain Win32).
//!
//! There is deliberately no "Exit" entry and the code window only hides when closed:
//! the agent is not user-closable, and a watched PC always shows its tray indicator.

use crate::state::{Shared, WM_STATE_CHANGED};
use desk_config::{ConfigManager, PanelConfig};
use desk_i18n::{set_locale, t, Locale};
use desk_paths::{DeskPaths, APP_ICON_RESOURCE_ID};
use desk_theme::{resolve, ResolvedTheme, ThemeMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetStockObject, InvalidateRect, SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT,
    DT_CENTER, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FW_BOLD, FW_NORMAL, HFONT, HGDIOBJ,
    PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetSystemMetrics, KillTimer, LoadIconW, LoadImageW, MessageBoxW, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, SetWindowTextW,
    ShowWindow, TranslateMessage, HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR,
    MB_ICONINFORMATION, MB_OK, MSG, SM_CXSMICON, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WM_APP,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_TOOLWINDOW, WS_OVERLAPPED, WS_SYSMENU,
    WS_VISIBLE,
};

const WM_TRAY: u32 = WM_APP + 2;
const WM_QUIT_UI: u32 = WM_APP + 3;
const TRAY_ID: u32 = 1;
const BUTTON_NEW_CODE: usize = 100;
const TIMER_REFRESH: usize = 1;

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
static THEME: OnceLock<Mutex<ResolvedTheme>> = OnceLock::new();
static CODE_HWND: Mutex<isize> = Mutex::new(0);
static WAS_VIEWED: AtomicBool = AtomicBool::new(false);

pub(crate) fn shared() -> Option<&'static Arc<Shared>> {
    SHARED.get()
}

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fill(buf: &mut [u16], text: &str) {
    let w = wide(text);
    let n = w.len().min(buf.len());
    buf[..n].copy_from_slice(&w[..n]);
    if let Some(last) = buf.last_mut() {
        *last = 0;
    }
}

/// `#RRGGBB` theme token to a GDI colour (falls back to black on malformed input).
pub(crate) fn colorref(token: &str) -> COLORREF {
    let hex = token.trim_start_matches('#');
    let value = u32::from_str_radix(hex.get(..6).unwrap_or("000000"), 16).unwrap_or(0);
    let (r, g, b) = ((value >> 16) & 0xFF, (value >> 8) & 0xFF, value & 0xFF);
    COLORREF(r | (g << 8) | (b << 16))
}

fn system_is_dark() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut data = 1u32;
    let mut size = 4u32;
    // SAFETY: valid out-pointers sized for a DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && data == 0
}

fn load_theme_and_locale(paths: &DeskPaths) -> ResolvedTheme {
    let path = paths.panel_config_path();
    let cfg: PanelConfig = if path.exists() {
        ConfigManager::load_or_default(&path).unwrap_or_default()
    } else {
        PanelConfig::default()
    };
    let mode = match cfg.mode.as_str() {
        "light" => ThemeMode::Light,
        "dark" => ThemeMode::Dark,
        _ => ThemeMode::System,
    };
    let locale = match cfg.language.as_str() {
        "en" => Locale::En,
        "tr" => Locale::Tr,
        _ => os_locale(),
    };
    set_locale(locale);
    resolve(&cfg.palette, mode, system_is_dark())
}

fn os_locale() -> Locale {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    // SAFETY: buffer length matches the call.
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    let name = String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0).saturating_sub(1)]);
    Locale::from_str_loose(&name)
}

/// Tray icon: the DAPP-76 icon embedded in this executable as resource 1, loaded at the
/// size the shell wants for the notification area (16 px at 100 % scaling).
pub(crate) fn load_icon() -> HICON {
    // SAFETY: plain metrics query.
    load_icon_sized(unsafe { GetSystemMetrics(SM_CXSMICON) })
}

/// The same embedded icon at an explicit pixel size (the caller destroys it).
pub(crate) fn load_icon_sized(size: i32) -> HICON {
    // SAFETY: a resource load from our own module; the integer resource id is encoded as a
    // pseudo string pointer exactly as MAKEINTRESOURCE does.
    unsafe {
        let loaded = GetModuleHandleW(None).and_then(|module| {
            LoadImageW(
                Some(module.into()),
                PCWSTR(usize::from(APP_ICON_RESOURCE_ID) as *const u16),
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            )
        });
        match loaded {
            Ok(handle) => HICON(handle.0),
            Err(_) => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

fn tray_data(
    hwnd: HWND,
    flags: windows::Win32::UI::Shell::NOTIFY_ICON_DATA_FLAGS,
) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: flags,
        uCallbackMessage: WM_TRAY,
        hIcon: load_icon(),
        ..Default::default()
    }
}

fn tooltip(viewers: usize, view_only: bool) -> String {
    let base = if viewers == 0 {
        t("tray.tooltip_idle")
    } else {
        t("tray.tooltip_viewed").replace("{n}", &viewers.to_string())
    };
    if view_only {
        format!(
            "{base}
{}",
            t("tray.tooltip_view_only")
        )
    } else {
        base
    }
}

fn tray_add(hwnd: HWND) {
    let mut data = tray_data(hwnd, NIF_ICON | NIF_TIP | NIF_MESSAGE);
    fill(&mut data.szTip, &tooltip(0, false));
    // SAFETY: fully initialised structure.
    let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
}

fn tray_update(hwnd: HWND, viewers: usize, view_only: bool, balloon: bool) {
    let mut flags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
    if balloon {
        flags |= NIF_INFO;
    }
    let mut data = tray_data(hwnd, flags);
    fill(&mut data.szTip, &tooltip(viewers, view_only));
    if balloon {
        fill(&mut data.szInfoTitle, &t("tray.balloon_title"));
        fill(&mut data.szInfo, &t("tray.balloon_body"));
        data.dwInfoFlags = NIIF_INFO;
    }
    // SAFETY: fully initialised structure.
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

fn tray_remove(hwnd: HWND) {
    let data = tray_data(hwnd, NIF_ICON);
    // SAFETY: fully initialised structure.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

/// Wakes the UI thread after a state change (callable from any thread).
pub fn notify(shared: &Shared) {
    let hwnd = shared.hwnd();
    if hwnd != 0 {
        // SAFETY: posting to a window handle owned by the UI thread; a stale handle just fails.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd as *mut _)),
                WM_STATE_CHANGED,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

fn request_code() {
    if let Some(conn) = shared().and_then(|s| s.conn()) {
        let _ = conn.send(&desk_ipc::IpcMessage::PairingCodeRequest);
    }
}

fn code_hwnd() -> HWND {
    HWND(*CODE_HWND.lock().unwrap_or_else(|p| p.into_inner()) as *mut _)
}

pub(crate) fn show_code_window() {
    let hwnd = code_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    let has_valid_code = shared()
        .map(|s| s.snapshot())
        .and_then(|ui| ui.code)
        .is_some_and(|c| !c.remaining().is_zero());
    let needs_code = !has_valid_code;
    if needs_code {
        request_code();
    }
    // SAFETY: window owned by this thread.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, true);
        SetTimer(Some(hwnd), TIMER_REFRESH, 1000, None);
    }
}

pub(crate) fn show_viewers_box(owner: HWND) {
    let viewers = shared().map(|s| s.snapshot().viewers).unwrap_or_default();
    let text = if viewers.is_empty() {
        t("tray.viewers_none")
    } else {
        format!("{}\n\n{}", t("tray.viewers_some"), viewers.join("\n"))
    };
    let (text_w, title_w) = (wide(&text), wide(&t("tray.viewers")));
    // SAFETY: NUL-terminated buffers outlive the call.
    unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => show_code_window(),
                WM_RBUTTONDOWN => crate::menu::request_info(),
                WM_RBUTTONUP => crate::menu::show(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_STATE_CHANGED => {
            let ui = shared().map(|s| s.snapshot()).unwrap_or_default();
            let viewed = !ui.viewers.is_empty();
            let was = WAS_VIEWED.swap(viewed, Ordering::SeqCst);
            let view_only = ui.info.as_ref().is_some_and(|i| i.view_only);
            tray_update(hwnd, ui.viewers.len(), view_only, viewed && !was);
            let code = code_hwnd();
            if !code.0.is_null() {
                // SAFETY: window owned by this thread.
                let _ = unsafe { InvalidateRect(Some(code), None, true) };
            }
            LRESULT(0)
        }
        WM_QUIT_UI => {
            // SAFETY: destroying our own window from its thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        m if m == taskbar_created() => {
            tray_add(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            tray_remove(hwnd);
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn taskbar_created() -> u32 {
    static MSG_ID: OnceLock<u32> = OnceLock::new();
    // SAFETY: constant NUL-terminated name.
    *MSG_ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

pub(crate) fn font(height: i32, weight: i32, mono: bool) -> HFONT {
    let face = if mono { w!("Consolas") } else { w!("Segoe UI") };
    // SAFETY: plain GDI font creation.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            0,
            face,
        )
    }
}

pub(crate) fn draw_text(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    text: &str,
    mut rect: RECT,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: valid DC and buffer.
    unsafe { DrawTextW(hdc, &mut buf, &mut rect, flags) };
}

/// The resolved theme (palette + mode) for every window of this process.
pub(crate) fn current_theme() -> ResolvedTheme {
    THEME
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_else(|| resolve("ink", ThemeMode::Light, false))
}

fn paint_code(hwnd: HWND) {
    let theme = current_theme();
    let tokens = &theme.tokens;
    let ui = shared().map(|s| s.snapshot()).unwrap_or_default();
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

        let (big, normal, bold) = (
            font(-56, FW_BOLD.0 as i32, true),
            font(-16, FW_NORMAL.0 as i32, false),
            font(-20, FW_BOLD.0 as i32, false),
        );
        let at = |top: i32, bottom: i32| RECT {
            left: 16,
            top,
            right: client.right - 16,
            bottom,
        };

        SelectObject(hdc, HGDIOBJ(bold.0));
        SetTextColor(hdc, colorref(&tokens.text));
        draw_text(
            hdc,
            &t("tray.code_title"),
            at(16, 48),
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );

        let remaining = ui
            .code
            .as_ref()
            .map(|c| c.remaining())
            .filter(|r| !r.is_zero());
        SelectObject(hdc, HGDIOBJ(big.0));
        match (&ui.code, remaining) {
            (Some(code), Some(left)) => {
                SetTextColor(hdc, colorref(&tokens.primary));
                draw_text(
                    hdc,
                    &code.code,
                    at(64, 140),
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
                SelectObject(hdc, HGDIOBJ(normal.0));
                SetTextColor(hdc, colorref(&tokens.text_muted));
                let secs = left.as_secs();
                let line = t("tray.code_expires")
                    .replace("{m}", &(secs / 60).to_string())
                    .replace("{s}", &format!("{:02}", secs % 60));
                draw_text(
                    hdc,
                    &line,
                    at(144, 168),
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
            }
            _ => {
                SelectObject(hdc, HGDIOBJ(normal.0));
                SetTextColor(hdc, colorref(&tokens.danger));
                let msg = if ui.service_online {
                    t("tray.code_expired")
                } else {
                    t("tray.code_unavailable")
                };
                draw_text(hdc, &msg, at(64, 140), DT_CENTER | DT_WORDBREAK);
            }
        }
        SelectObject(hdc, HGDIOBJ(normal.0));
        SetTextColor(hdc, colorref(&tokens.text_muted));
        draw_text(
            hdc,
            &t("tray.code_hint"),
            at(176, 224),
            DT_CENTER | DT_WORDBREAK,
        );

        let _ = DeleteObject(HGDIOBJ(big.0));
        let _ = DeleteObject(HGDIOBJ(normal.0));
        let _ = DeleteObject(HGDIOBJ(bold.0));
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn code_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint_code(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            // SAFETY: window owned by this thread.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 & 0xFFFF) == BUTTON_NEW_CODE => {
            request_code();
            LRESULT(0)
        }
        WM_CLOSE => {
            // Closing only hides: the agent keeps running.
            // SAFETY: window owned by this thread.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_REFRESH);
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn create_windows(shared: &Arc<Shared>) -> windows::core::Result<()> {
    // SAFETY: standard window-class registration and creation on the UI thread.
    unsafe {
        let module = GetModuleHandleW(None)?;
        let instance = module.into();

        let tray_class = w!("DeskRemoteTrayWindow");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(tray_proc),
            hInstance: instance,
            lpszClassName: tray_class,
            ..Default::default()
        });
        let tray = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            tray_class,
            w!("Desk-Remote"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?;
        shared.set_hwnd(tray.0 as isize);

        let code_class = w!("DeskRemoteCodeWindow");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(code_proc),
            hInstance: instance,
            lpszClassName: code_class,
            ..Default::default()
        });
        let title = wide(&t("tray.code_title"));
        let code = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            code_class,
            PCWSTR(title.as_ptr()),
            WS_CAPTION | WS_SYSMENU,
            200,
            200,
            440,
            330,
            None,
            None,
            Some(instance),
            None,
        )?;
        let button_text = wide(&t("tray.code_new"));
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR(button_text.as_ptr()),
            WS_CHILD | WS_VISIBLE,
            150,
            232,
            120,
            32,
            Some(code),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                BUTTON_NEW_CODE as *mut _,
            )),
            Some(instance),
            None,
        )?;
        let _ = SetWindowTextW(code, PCWSTR(title.as_ptr()));
        let _ = GetStockObject(DEFAULT_GUI_FONT);
        *CODE_HWND.lock().unwrap_or_else(|p| p.into_inner()) = code.0 as isize;
        tray_add(tray);
    }
    Ok(())
}

/// Runs the tray + window message loop on the calling thread until the window is destroyed.
pub fn run(shared: Arc<Shared>, paths: &DeskPaths) -> windows::core::Result<()> {
    let theme = load_theme_and_locale(paths);
    let _ = THEME.set(Mutex::new(theme));
    let _ = SHARED.set(Arc::clone(&shared));
    create_windows(&shared)?;
    let mut msg = MSG::default();
    // SAFETY: standard message pump.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// Asks the UI thread to tear down the tray icon and leave its message loop.
pub fn quit(shared: &Shared) {
    let hwnd = shared.hwnd();
    if hwnd != 0 {
        // SAFETY: posting to a window owned by the UI thread.
        let _ =
            unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_QUIT_UI, WPARAM(0), LPARAM(0)) };
    }
}
