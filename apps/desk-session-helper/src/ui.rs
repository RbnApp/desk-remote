//! Tray icon, theme/locale loading and the Win32 message loop (plain Win32).
//!
//! The tray menu offers "Close Desk-Remote" in both modes through one code path: the portable
//! agent stops directly; the installed service asks for administrator rights (UAC) first, so
//! the close is always a deliberate, authenticated act. The windows themselves live in `dialog`.

use crate::state::{Notice, Shared, WM_STATE_CHANGED};
use desk_config::{ConfigManager, PanelConfig};
use desk_i18n::{set_locale, t, Locale};
use desk_paths::{DeskPaths, APP_ICON_RESOURCE_ID};
use desk_theme::{resolve, ResolvedTheme, ThemeMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DrawTextW, HFONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW, GetSystemMetrics,
    GetWindowThreadProcessId, LoadIconW, LoadImageW, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, TranslateMessage, HICON, IDI_APPLICATION, IMAGE_ICON,
    LR_DEFAULTCOLOR, MSG, MSGFLT_ALLOW, SM_CXICON, SM_CXSMICON, SW_SHOWNORMAL, WM_APP, WM_DESTROY,
    WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WNDCLASSEXW, WNDPROC, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

/// `NIN_BALLOONUSERCLICK`: the user clicked the balloon (legacy tray callback, low word of lParam).
const NIN_BALLOONUSERCLICK: u32 = 0x0405;
const WM_TRAY: u32 = WM_APP + 2;
const WM_QUIT_UI: u32 = WM_APP + 3;
const WM_SHOW_CODE: u32 = WM_APP + 4;
const WM_HIDE_TRAY: u32 = WM_APP + 5;
const TRAY_CLASS: PCWSTR = w!("DeskRemoteTrayWindow");
const TRAY_ID: u32 = 1;

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
static THEME: OnceLock<Mutex<ResolvedTheme>> = OnceLock::new();
static WAS_VIEWED: AtomicBool = AtomicBool::new(false);
/// The user closed the tray: the icon stays gone for this helper's life while capture, input and
/// the service link keep running (DAPP-110). A new logon starts a new helper with a fresh tray.
static TRAY_HIDDEN: AtomicBool = AtomicBool::new(false);
/// File the visible balloon opens in Explorer when clicked (`None` for informational balloons).
static BALLOON_TARGET: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// `explorer.exe` argument line that selects `file`; the path is quoted so spaces survive.
fn explorer_select_args(file: &std::path::Path) -> String {
    format!("/select,\"{}\"", file.display())
}

/// Opens Explorer on the folder of `file` with the file selected.
fn reveal_in_explorer(file: &std::path::Path) {
    let args = wide(&explorer_select_args(file));
    // SAFETY: NUL-terminated buffers that outlive the call; no window owner needed.
    let _ = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("explorer.exe"),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
}

/// Posts "open the pairing-code window" to the running helper's tray window. The caller is the
/// freshly double-clicked process, which owns the foreground right, so it hands that right to
/// the helper; otherwise Windows would leave the window behind the others.
pub(crate) fn request_show_code() -> bool {
    // SAFETY: lookup by class name and plain message/permission calls; no pointers are retained.
    unsafe {
        let Ok(hwnd) = FindWindowW(TRAY_CLASS, PCWSTR::null()) else {
            return false;
        };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        PostMessageW(Some(hwnd), WM_SHOW_CODE, WPARAM(0), LPARAM(0)).is_ok()
    }
}

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

/// Registers a window class that carries the Desk-Remote icon (large `hIcon` for Alt+Tab /
/// the taskbar, small `hIconSm` for the title bar), both loaded from embedded resource 1 at
/// the system's current icon metrics so they stay sharp at 125 / 150 % scaling. Every window
/// of this agent registers through here so none falls back to the default Windows icon.
/// The icons belong to the class for the life of the process (loaded once per class).
pub(crate) fn register_app_window_class(name: PCWSTR, proc: WNDPROC) {
    // SAFETY: plain metrics queries and a class registration on the UI thread; registering an
    // existing class name just fails harmlessly.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: module.into(),
            lpszClassName: name,
            hIcon: load_icon_sized(GetSystemMetrics(SM_CXICON)),
            hIconSm: load_icon_sized(GetSystemMetrics(SM_CXSMICON)),
            ..Default::default()
        });
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
    if TRAY_HIDDEN.load(Ordering::SeqCst) {
        return;
    }
    let mut data = tray_data(hwnd, NIF_ICON | NIF_TIP | NIF_MESSAGE);
    fill(&mut data.szTip, &tooltip(0, false));
    // SAFETY: fully initialised structure.
    let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
}

fn tray_update(hwnd: HWND, viewers: usize, view_only: bool, balloon: Option<&Notice>) {
    if TRAY_HIDDEN.load(Ordering::SeqCst) {
        return;
    }
    let mut flags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
    if balloon.is_some() {
        flags |= NIF_INFO;
    }
    let mut data = tray_data(hwnd, flags);
    fill(&mut data.szTip, &tooltip(viewers, view_only));
    if let Some(notice) = balloon {
        if let Ok(mut target) = BALLOON_TARGET.lock() {
            target.clone_from(&notice.open);
        }
        fill(&mut data.szInfoTitle, &notice.title);
        fill(&mut data.szInfo, &notice.body);
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

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => crate::code::show(),
                WM_RBUTTONDOWN => crate::menu::request_info(),
                WM_RBUTTONUP => crate::menu::show(hwnd),
                NIN_BALLOONUSERCLICK => {
                    let target = BALLOON_TARGET.lock().ok().and_then(|mut t| t.take());
                    if let Some(file) = target {
                        reveal_in_explorer(&file);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_STATE_CHANGED => {
            let ui = shared().map(|s| s.snapshot()).unwrap_or_default();
            let viewed = !ui.viewers.is_empty();
            let was = WAS_VIEWED.swap(viewed, Ordering::SeqCst);
            let view_only = ui.info.as_ref().is_some_and(|i| i.view_only);
            let viewed_now = (viewed && !was).then(|| Notice {
                title: t("tray.balloon_title"),
                body: t("tray.balloon_body"),
                open: None,
            });
            let balloon = shared().and_then(|s| s.take_notice()).or(viewed_now);
            tray_update(hwnd, ui.viewers.len(), view_only, balloon.as_ref());
            crate::code::refresh();
            crate::viewers::refresh();
            LRESULT(0)
        }
        WM_SHOW_CODE => {
            crate::code::show();
            LRESULT(0)
        }
        WM_HIDE_TRAY => {
            // Only the icon goes; the process (screen, input, service link) must keep running,
            // otherwise the service supervisor would see a dead helper and start it again.
            TRAY_HIDDEN.store(true, Ordering::SeqCst);
            tray_remove(hwnd);
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

fn create_windows(shared: &Arc<Shared>) -> windows::core::Result<()> {
    // SAFETY: standard window-class registration and creation on the UI thread.
    unsafe {
        let module = GetModuleHandleW(None)?;
        let instance = module.into();

        register_app_window_class(TRAY_CLASS, Some(tray_proc));
        let tray = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            TRAY_CLASS,
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
        // An elevated agent's tray window must still hear the normal-integrity second
        // double-click ("show the pairing code").
        let _ = ChangeWindowMessageFilterEx(tray, WM_SHOW_CODE, MSGFLT_ALLOW, None);

        crate::dialog::register_class();
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

/// Removes the tray icon but keeps the helper running (remote screen and input stay available).
pub fn hide_tray(shared: &Shared) {
    let hwnd = shared.hwnd();
    if hwnd != 0 {
        // SAFETY: posting to a window owned by the UI thread.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd as *mut _)),
                WM_HIDE_TRAY,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_selects_the_received_file_even_with_spaces() {
        let args = explorer_select_args(std::path::Path::new(
            r"C:\Users\a b\Downloads\Desk-Remote\x y.txt",
        ));
        assert_eq!(
            args,
            r#"/select,"C:\Users\a b\Downloads\Desk-Remote\x y.txt""#
        );
    }
}
