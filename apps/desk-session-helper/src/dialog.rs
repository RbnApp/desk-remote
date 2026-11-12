//! The one standard dialog frame of the agent (About, pairing code, viewers).
//!
//! Every agent window is described by a [`Spec`] (header, body items, buttons) and drawn by
//! this module only, so width, padding, header, type scale, button row and keyboard
//! behaviour are identical everywhere. Colours come from the active `desk-theme` tokens
//! (same Ink / Petrol look as the panel's modal), sizes scale with the display DPI, and the
//! title bar follows the theme (dark title bar in dark mode).
//!
//! Buttons are drawn here too (no classic grey `BUTTON` controls): primary = filled,
//! secondary = outlined, with hover / pressed / keyboard-focus states.
//! Keys: Esc closes, Enter presses the primary button, Tab / arrows move the focus ring,
//! Space presses the focused button.

use crate::ui::{colorref, current_theme, draw_text, font, load_icon_sized, wide};
use desk_theme::ResolvedTheme;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
    DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, GetDC, GetDeviceCaps, GetMonitorInfoW,
    GetStockObject, InvalidateRect, MonitorFromPoint, ReleaseDC, RoundRect, SelectObject,
    SetBkMode, SetTextColor, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, HDC,
    HFONT, HGDIOBJ, LOGPIXELSX, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH, NULL_PEN,
    PAINTSTRUCT, PS_INSIDEFRAME, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DrawIconEx, GetClientRect,
    GetCursorPos, GetWindowLongPtrW, KillTimer, LoadCursorW, SetCursor, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, DI_NORMAL, GWLP_USERDATA,
    HTCLIENT, IDC_ARROW, IDC_HAND, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOW,
    WINDOW_EX_STYLE, WM_CLOSE, WM_CREATE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_TIMER, WS_CAPTION,
    WS_SYSMENU,
};

/// Logical (96 dpi) metrics shared by every dialog.
const CLIENT_WIDTH: i32 = 440;
const PAD: i32 = 24;
const ICON: i32 = 40;
const ICON_GAP: i32 = 14;
const HEADER_GAP: i32 = 20;
const ITEM_GAP: i32 = 14;
const FOOTER_GAP: i32 = 24;
const BOTTOM_PAD: i32 = 24;
const BUTTON_H: i32 = 36;
const BUTTON_MIN_W: i32 = 96;
const BUTTON_PAD: i32 = 20;
const BUTTON_GAP: i32 = 8;
const RADIUS: i32 = 8;
const CODE_BOX_H: i32 = 88;
const ROW_H: i32 = 36;
const ROW_GAP: i32 = 6;

const TIMER_TICK: usize = 1;
/// `WM_MOUSELEAVE` (not exported by the `windows` feature set in use).
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Button id every dialog shares: closes the window.
pub(crate) const CLOSE: u32 = 0;

/// Text colour role.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Muted,
    Danger,
}

/// One block of a dialog body (left aligned, except the code box).
pub(crate) enum Item {
    /// Wrapped paragraph.
    Text { text: String, tone: Tone },
    /// Muted label above a monospace value.
    Field { label: String, value: String },
    /// Emphasised value centred in a box (the pairing code).
    Code { text: String },
    /// One rounded row per entry (the viewers list).
    Rows(Vec<String>),
}

/// A button of the footer row (right aligned, in the given order).
pub(crate) struct Button {
    pub id: u32,
    pub label: String,
    pub primary: bool,
}

/// Everything a dialog shows right now.
pub(crate) struct Spec {
    pub title: String,
    pub subtitle: Option<String>,
    pub items: Vec<Item>,
    pub buttons: Vec<Button>,
}

/// A dialog kind: its content, its non-[`CLOSE`] button actions and its live-refresh rate.
pub(crate) struct Def {
    pub caption: fn() -> String,
    pub build: fn() -> Spec,
    pub on_button: fn(u32),
    /// Re-build interval in ms (0 = only on [`refresh`]).
    pub tick_ms: u32,
    hwnd: AtomicIsize,
}

impl Def {
    pub(crate) const fn new(
        caption: fn() -> String,
        build: fn() -> Spec,
        on_button: fn(u32),
        tick_ms: u32,
    ) -> Self {
        Self {
            caption,
            build,
            on_button,
            tick_ms,
            hwnd: AtomicIsize::new(0),
        }
    }
}

/// Computed rectangles (client pixels).
#[derive(Default)]
struct Layout {
    height: i32,
    icon: RECT,
    title: RECT,
    subtitle: RECT,
    items: Vec<RECT>,
    buttons: Vec<RECT>,
}

struct State {
    def: &'static Def,
    dpi: i32,
    spec: Spec,
    layout: Layout,
    hover: Option<usize>,
    pressed: Option<usize>,
    focus: usize,
    keyboard: bool,
    tracking: bool,
}

/// Display DPI. `DESK_DIALOG_DPI` overrides it so the 150 % layout can be checked on a 100 %
/// display (test hook; unset in normal use).
fn display_dpi() -> i32 {
    if let Some(v) = std::env::var("DESK_DIALOG_DPI")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|v| (96..=384).contains(v))
    {
        return v;
    }
    // SAFETY: screen DC obtained and released in place.
    unsafe {
        let dc = GetDC(None);
        let dpi = GetDeviceCaps(Some(dc), LOGPIXELSX);
        ReleaseDC(None, dc);
        if dpi >= 96 {
            dpi
        } else {
            96
        }
    }
}

impl State {
    fn px(&self, v: i32) -> i32 {
        (v * self.dpi + 48) / 96
    }
}

struct Fonts {
    title: HFONT,
    body: HFONT,
    small: HFONT,
    button: HFONT,
    mono: HFONT,
    code: HFONT,
}

impl Fonts {
    fn new(st: &State) -> Self {
        let h = |v: i32| -st.px(v);
        Self {
            title: font(h(19), FW_BOLD.0 as i32, false),
            body: font(h(15), FW_NORMAL.0 as i32, false),
            small: font(h(13), FW_NORMAL.0 as i32, false),
            button: font(h(15), FW_SEMIBOLD.0 as i32, false),
            mono: font(h(16), FW_NORMAL.0 as i32, true),
            code: font(h(40), FW_BOLD.0 as i32, true),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [
            self.title,
            self.body,
            self.small,
            self.button,
            self.mono,
            self.code,
        ] {
            // SAFETY: fonts created in `Fonts::new`, deleted once here.
            let _ = unsafe { DeleteObject(HGDIOBJ(f.0)) };
        }
    }
}

fn measure(hdc: HDC, f: HFONT, text: &str, width: i32, wrap: bool) -> (i32, i32) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: 0,
    };
    let mut flags = DT_CALCRECT | DT_NOPREFIX;
    flags |= if wrap { DT_WORDBREAK } else { DT_SINGLELINE };
    // SAFETY: valid DC and buffer; the font is selected only for the call.
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(f.0));
        windows::Win32::Graphics::Gdi::DrawTextW(hdc, &mut buf, &mut r, flags);
        SelectObject(hdc, old);
    }
    (r.right - r.left, r.bottom - r.top)
}

fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

fn compute_layout(hdc: HDC, fonts: &Fonts, st: &State) -> Layout {
    let width = st.px(CLIENT_WIDTH);
    let pad = st.px(PAD);
    let text_left = pad + st.px(ICON) + st.px(ICON_GAP);
    let text_w = width - pad - text_left;
    let mut lay = Layout::default();

    // Header: icon on the left, title (+ subtitle) vertically centred next to it.
    let (_, title_h) = measure(hdc, fonts.title, &st.spec.title, text_w, false);
    let sub_h = st
        .spec
        .subtitle
        .as_deref()
        .map_or(0, |s| measure(hdc, fonts.small, s, text_w, true).1);
    let block_h = title_h + if sub_h > 0 { st.px(2) + sub_h } else { 0 };
    let icon = st.px(ICON);
    let header_h = icon.max(block_h);
    let text_top = pad + (header_h - block_h) / 2;
    lay.icon = rect(
        pad,
        pad + (header_h - icon) / 2,
        pad + icon,
        pad + (header_h - icon) / 2 + icon,
    );
    lay.title = rect(text_left, text_top, width - pad, text_top + title_h);
    lay.subtitle = rect(
        text_left,
        lay.title.bottom + st.px(2),
        width - pad,
        lay.title.bottom + st.px(2) + sub_h,
    );
    let mut y = pad + header_h;

    // Body.
    let body_w = width - 2 * pad;
    for item in &st.spec.items {
        y += if lay.items.is_empty() {
            st.px(HEADER_GAP)
        } else {
            st.px(ITEM_GAP)
        };
        let h = match item {
            Item::Text { text, .. } => measure(hdc, fonts.body, text, body_w, true).1,
            Item::Field { label, value } => {
                measure(hdc, fonts.small, label, body_w, false).1
                    + st.px(4)
                    + measure(hdc, fonts.mono, value, body_w, false).1
            }
            Item::Code { .. } => st.px(CODE_BOX_H),
            Item::Rows(rows) => {
                let n = i32::try_from(rows.len()).unwrap_or(1).max(1);
                n * st.px(ROW_H) + (n - 1) * st.px(ROW_GAP)
            }
        };
        lay.items.push(rect(pad, y, width - pad, y + h));
        y += h;
    }

    // Footer: buttons right aligned, spec order left to right.
    y += st.px(FOOTER_GAP);
    let (bh, gap) = (st.px(BUTTON_H), st.px(BUTTON_GAP));
    let mut right = width - pad;
    let mut buttons = Vec::new();
    for b in st.spec.buttons.iter().rev() {
        let bw = (measure(hdc, fonts.button, &b.label, width, false).0 + 2 * st.px(BUTTON_PAD))
            .max(st.px(BUTTON_MIN_W));
        buttons.push(rect(right - bw, y, right, y + bh));
        right -= bw + gap;
    }
    buttons.reverse();
    lay.buttons = buttons;
    lay.height = y + bh + st.px(BOTTOM_PAD);
    lay
}

fn style() -> windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE {
    WS_CAPTION | WS_SYSMENU
}

/// Rebuilds the spec, re-measures and resizes the window when the content height changed.
fn relayout(hwnd: HWND, st: &mut State) {
    st.spec = (st.def.build)();
    st.focus = st.focus.min(st.spec.buttons.len().saturating_sub(1));
    let fonts = Fonts::new(st);
    // SAFETY: screen DC obtained and released in place.
    unsafe {
        let dc = GetDC(None);
        st.layout = compute_layout(dc, &fonts, st);
        ReleaseDC(None, dc);
    }
    let mut outer = rect(0, 0, st.px(CLIENT_WIDTH), st.layout.height);
    // SAFETY: plain window metrics and resize of our own window.
    unsafe {
        let _ = AdjustWindowRectEx(&mut outer, style(), false, WINDOW_EX_STYLE(0));
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            outer.right - outer.left,
            outer.bottom - outer.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn ink(tokens: &str) -> COLORREF {
    colorref(tokens)
}

/// Rounded rectangle; `fill`/`stroke` are theme tokens (`None` = not drawn).
fn round(hdc: HDC, r: RECT, radius: i32, fill: Option<&str>, stroke: Option<(&str, i32)>) {
    // SAFETY: GDI objects are created, selected and deleted within this call.
    unsafe {
        let brush = fill.map(|c| CreateSolidBrush(ink(c)));
        let pen = stroke.map(|(c, w)| CreatePen(PS_INSIDEFRAME, w, ink(c)));
        let old_brush = SelectObject(
            hdc,
            brush.map_or_else(|| GetStockObject(NULL_BRUSH), |b| HGDIOBJ(b.0)),
        );
        let old_pen = SelectObject(
            hdc,
            pen.map_or_else(|| GetStockObject(NULL_PEN), |p| HGDIOBJ(p.0)),
        );
        let _ = RoundRect(
            hdc,
            r.left,
            r.top,
            r.right,
            r.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
        if let Some(b) = brush {
            let _ = DeleteObject(HGDIOBJ(b.0));
        }
        if let Some(p) = pen {
            let _ = DeleteObject(HGDIOBJ(p.0));
        }
    }
}

fn text_at(hdc: HDC, f: HFONT, color: &str, text: &str, r: RECT, flags: DRAW_TEXT_FORMAT) {
    // SAFETY: valid DC; the font stays selected for the following draws.
    unsafe {
        SelectObject(hdc, HGDIOBJ(f.0));
        SetTextColor(hdc, ink(color));
    }
    draw_text(hdc, text, r, flags | DT_NOPREFIX);
}

fn tone_color(theme: &ResolvedTheme, tone: Tone) -> &str {
    match tone {
        Tone::Muted => &theme.tokens.text_muted,
        Tone::Danger => &theme.tokens.danger,
    }
}

fn draw_button(hdc: HDC, fonts: &Fonts, st: &State, theme: &ResolvedTheme, i: usize) {
    let (Some(b), Some(&r)) = (st.spec.buttons.get(i), st.layout.buttons.get(i)) else {
        return;
    };
    let t = &theme.tokens;
    let down = st.pressed == Some(i) && st.hover == Some(i);
    let over = st.hover == Some(i);
    let one = st.px(1).max(1);
    let (fill, border, label) = if b.primary {
        let fill = if down {
            &t.primary_pressed
        } else if over {
            &t.primary_hover
        } else {
            &t.primary
        };
        (fill, fill, &t.on_primary)
    } else {
        let fill = if down {
            &t.surface_pressed
        } else if over {
            &t.surface_hover
        } else {
            &t.surface
        };
        (fill, &t.border, &t.text)
    };
    if st.keyboard && st.focus == i {
        let g = st.px(3);
        let ring = rect(r.left - g, r.top - g, r.right + g, r.bottom + g);
        round(
            hdc,
            ring,
            st.px(RADIUS) + g,
            None,
            Some((&t.focus_ring, st.px(2))),
        );
    }
    round(hdc, r, st.px(RADIUS), Some(fill), Some((border, one)));
    text_at(
        hdc,
        fonts.button,
        label,
        &b.label,
        r,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER,
    );
}

fn paint_into(hdc: HDC, size: (i32, i32), st: &State, theme: &ResolvedTheme) {
    let t = &theme.tokens;
    let fonts = Fonts::new(st);
    // SAFETY: GDI calls on a DC owned by the paint sequence.
    unsafe {
        let bg = CreateSolidBrush(ink(&t.surface));
        FillRect(hdc, &rect(0, 0, size.0, size.1), bg);
        let _ = DeleteObject(HGDIOBJ(bg.0));
        SetBkMode(hdc, TRANSPARENT);
        let icon = load_icon_sized(st.px(ICON));
        let _ = DrawIconEx(
            hdc,
            st.layout.icon.left,
            st.layout.icon.top,
            icon,
            st.px(ICON),
            st.px(ICON),
            0,
            None,
            DI_NORMAL,
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon);
    }
    text_at(
        hdc,
        fonts.title,
        &t.text,
        &st.spec.title,
        st.layout.title,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
    );
    if let Some(sub) = &st.spec.subtitle {
        text_at(
            hdc,
            fonts.small,
            &t.text_muted,
            sub,
            st.layout.subtitle,
            DT_LEFT | DT_WORDBREAK,
        );
    }
    for (item, &r) in st.spec.items.iter().zip(&st.layout.items) {
        match item {
            Item::Text { text, tone } => {
                text_at(
                    hdc,
                    fonts.body,
                    tone_color(theme, *tone),
                    text,
                    r,
                    DT_LEFT | DT_WORDBREAK,
                );
            }
            Item::Field { label, value } => {
                let lh = measure(hdc, fonts.small, label, r.right - r.left, false).1;
                let top = rect(r.left, r.top, r.right, r.top + lh);
                text_at(
                    hdc,
                    fonts.small,
                    &t.text_muted,
                    label,
                    top,
                    DT_LEFT | DT_SINGLELINE,
                );
                let low = rect(r.left, r.top + lh + st.px(4), r.right, r.bottom);
                text_at(
                    hdc,
                    fonts.mono,
                    &t.text,
                    value,
                    low,
                    DT_LEFT | DT_SINGLELINE,
                );
            }
            Item::Code { text } => {
                round(
                    hdc,
                    r,
                    st.px(RADIUS + 2),
                    Some(&t.surface_alt),
                    Some((&t.border, st.px(1).max(1))),
                );
                text_at(
                    hdc,
                    fonts.code,
                    &t.primary,
                    text,
                    r,
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
            }
            Item::Rows(rows) => {
                let (rh, gap) = (st.px(ROW_H), st.px(ROW_GAP));
                for (n, row) in rows.iter().enumerate() {
                    let top = r.top + i32::try_from(n).unwrap_or(0) * (rh + gap);
                    let rr = rect(r.left, top, r.right, top + rh);
                    round(hdc, rr, st.px(RADIUS), Some(&t.surface_alt), None);
                    let dot = st.px(4);
                    let (cx, cy) = (rr.left + st.px(14), rr.top + rh / 2);
                    round(
                        hdc,
                        rect(cx - dot, cy - dot, cx + dot, cy + dot),
                        dot,
                        Some(&t.success),
                        None,
                    );
                    let txt = rect(cx + st.px(14), rr.top, rr.right - st.px(12), rr.bottom);
                    text_at(
                        hdc,
                        fonts.body,
                        &t.text,
                        row,
                        txt,
                        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );
                }
            }
        }
    }
    for i in 0..st.spec.buttons.len() {
        draw_button(hdc, &fonts, st, theme, i);
    }
}

fn paint(hwnd: HWND, st: &State) {
    let theme = current_theme();
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: standard double-buffered paint sequence on our own window.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let size = (client.right, client.bottom);
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, size.0.max(1), size.1.max(1));
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        paint_into(mem, size, st, &theme);
        let _ = BitBlt(hdc, 0, 0, size.0, size.1, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn hit(st: &State, x: i32, y: i32) -> Option<usize> {
    st.layout
        .buttons
        .iter()
        .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

/// # Safety
/// Only valid on the UI thread for windows created by [`open`].
unsafe fn state<'a>(hwnd: HWND) -> Option<&'a mut State> {
    // SAFETY: the pointer is set in WM_NCCREATE and cleared in WM_NCDESTROY.
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State).as_mut() }
}

fn redraw(hwnd: HWND) {
    // SAFETY: invalidating our own window.
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
}

/// Runs a button: [`CLOSE`] destroys the window, anything else goes to the dialog kind.
fn activate(hwnd: HWND, id: u32) {
    // SAFETY: state read before any destroy below.
    let def = unsafe { state(hwnd) }.map(|s| s.def);
    if id == CLOSE {
        // SAFETY: destroying our own window from its thread.
        let _ = unsafe { DestroyWindow(hwnd) };
    } else if let Some(def) = def {
        (def.on_button)(id);
    }
}

fn button_id(hwnd: HWND, index: Option<usize>) -> Option<u32> {
    // SAFETY: UI-thread window created by `open`.
    let st = unsafe { state(hwnd) }?;
    st.spec.buttons.get(index?).map(|b| b.id)
}

fn move_focus(hwnd: HWND, step: isize) {
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    let n = st.spec.buttons.len() as isize;
    if n == 0 {
        return;
    }
    st.focus = ((st.focus as isize + step).rem_euclid(n)) as usize;
    st.keyboard = true;
    redraw(hwnd);
}

fn on_key(hwnd: HWND, key: u16) {
    if key == VK_ESCAPE.0 {
        activate(hwnd, CLOSE);
    } else if key == VK_RETURN.0 {
        // SAFETY: UI-thread window created by `open`.
        let primary =
            unsafe { state(hwnd) }.and_then(|s| s.spec.buttons.iter().rposition(|b| b.primary));
        if let Some(id) = button_id(hwnd, primary) {
            activate(hwnd, id);
        }
    } else if key == VK_SPACE.0 {
        // SAFETY: UI-thread window created by `open`.
        let focus = unsafe { state(hwnd) }.map(|s| s.focus);
        if let Some(id) = button_id(hwnd, focus) {
            activate(hwnd, id);
        }
    } else if key == VK_TAB.0 {
        // SAFETY: plain key-state query.
        let back = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0;
        move_focus(hwnd, if back { -1 } else { 1 });
    } else if key == VK_LEFT.0 {
        move_focus(hwnd, -1);
    } else if key == VK_RIGHT.0 {
        move_focus(hwnd, 1);
    }
}

fn on_mouse(hwnd: HWND, msg: u32, lp: LPARAM) {
    let (x, y) = (i32::from(lp.0 as i16), i32::from((lp.0 >> 16) as i16));
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    let over = hit(st, x, y);
    match msg {
        WM_MOUSEMOVE => {
            if !st.tracking {
                st.tracking = true;
                let mut ev = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: fully initialised structure.
                let _ = unsafe { TrackMouseEvent(&mut ev) };
            }
            if st.hover != over {
                st.hover = over;
                redraw(hwnd);
            }
        }
        WM_LBUTTONDOWN => {
            st.pressed = over;
            st.keyboard = false;
            if let Some(i) = over {
                st.focus = i;
                // SAFETY: capture released on button-up.
                unsafe { SetCapture(hwnd) };
            }
            redraw(hwnd);
        }
        WM_LBUTTONUP => {
            let pressed = st.pressed.take();
            // SAFETY: releases our own capture.
            let _ = unsafe { ReleaseCapture() };
            redraw(hwnd);
            if pressed.is_some() && pressed == over {
                if let Some(id) = button_id(hwnd, over) {
                    activate(hwnd, id);
                }
            }
        }
        _ => {}
    }
}

fn on_cursor(hwnd: HWND) -> bool {
    let mut at = POINT::default();
    // SAFETY: cursor query and conversion on our own window.
    unsafe {
        let _ = GetCursorPos(&mut at);
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut at);
        let Some(st) = state(hwnd) else {
            return false;
        };
        let id = if hit(st, at.x, at.y).is_some() {
            IDC_HAND
        } else {
            IDC_ARROW
        };
        if let Ok(cursor) = LoadCursorW(None, id) {
            SetCursor(Some(cursor));
        }
    }
    true
}

unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: `lp` points at the CREATESTRUCTW whose parameter is the `&'static Def`.
            let def = unsafe {
                ((*(lp.0 as *const CREATESTRUCTW)).lpCreateParams as *const Def).as_ref()
            };
            if let Some(def) = def {
                let st = Box::new(State {
                    def,
                    dpi: display_dpi(),
                    spec: (def.build)(),
                    layout: Layout::default(),
                    hover: None,
                    pressed: None,
                    focus: 0,
                    keyboard: false,
                    tracking: false,
                });
                // SAFETY: ownership moves into the window; freed in WM_NCDESTROY.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize) };
            }
            // SAFETY: default handling continues creation.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_CREATE => LRESULT(0),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // SAFETY: UI-thread window created by `open`.
            if let Some(st) = unsafe { state(hwnd) } {
                paint(hwnd, st);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            refresh_hwnd(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            on_key(hwnd, wp.0 as u16);
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
            on_mouse(hwnd, msg, lp);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            // SAFETY: UI-thread window created by `open`.
            if let Some(st) = unsafe { state(hwnd) } {
                st.tracking = false;
                st.hover = None;
                redraw(hwnd);
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            if on_cursor(hwnd) {
                LRESULT(1)
            } else {
                // SAFETY: default handling.
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
        }
        WM_CLOSE => {
            activate(hwnd, CLOSE);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: reclaims the box stored in WM_NCCREATE exactly once.
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !ptr.is_null() {
                    let st = Box::from_raw(ptr);
                    st.def.hwnd.store(0, Ordering::SeqCst);
                    let _ = KillTimer(Some(hwnd), TIMER_TICK);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Registers the one dialog window class (once, at start-up).
pub(crate) fn register_class() {
    crate::ui::register_app_window_class(w!("DeskRemoteDialog"), Some(dialog_proc));
}

fn refresh_hwnd(hwnd: HWND) {
    // SAFETY: UI-thread window created by `open`.
    if let Some(st) = unsafe { state(hwnd) } {
        relayout(hwnd, st);
        redraw(hwnd);
    }
}

/// Re-reads the content of an open dialog (no-op when it is closed).
pub(crate) fn refresh(def: &'static Def) {
    let existing = def.hwnd.load(Ordering::SeqCst);
    if existing != 0 {
        refresh_hwnd(HWND(existing as *mut _));
    }
}

/// Themes the title bar: dark mode flag plus the theme's own caption colours (Windows 11).
fn theme_title_bar(hwnd: HWND, theme: &ResolvedTheme) {
    let dark = i32::from(theme.is_dark);
    let colors = [
        (DWMWA_CAPTION_COLOR, ink(&theme.tokens.surface)),
        (DWMWA_TEXT_COLOR, ink(&theme.tokens.text)),
        (DWMWA_BORDER_COLOR, ink(&theme.tokens.border)),
    ];
    // SAFETY: plain attribute calls on our own window; unsupported ones just fail.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        for (attr, color) in colors {
            let _ = DwmSetWindowAttribute(
                hwnd,
                attr,
                (&color as *const COLORREF).cast(),
                std::mem::size_of::<COLORREF>() as u32,
            );
        }
    }
}

/// Centres the window on the work area of the monitor under the cursor.
fn center(hwnd: HWND) {
    let mut at = POINT::default();
    let mut win = RECT::default();
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: plain metrics queries and a move of our own window.
    unsafe {
        let _ = GetCursorPos(&mut at);
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(monitor, &mut info);
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut win);
        let work = info.rcWork;
        let x = work.left + (work.right - work.left - (win.right - win.left)) / 2;
        let y = work.top + (work.bottom - work.top - (win.bottom - win.top)) / 2;
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE,
        );
    }
}

/// Opens the dialog (or refreshes and raises it when it is already open).
pub(crate) fn open(def: &'static Def) {
    let existing = def.hwnd.load(Ordering::SeqCst);
    if existing != 0 {
        refresh(def);
        // SAFETY: the handle belongs to this thread's window; a stale one just fails.
        let _ = unsafe { SetForegroundWindow(HWND(existing as *mut _)) };
        return;
    }
    let caption = wide(&(def.caption)());
    // SAFETY: window creation on the UI thread; `def` is 'static so the pointer outlives it.
    let created = unsafe {
        let Ok(module) = windows::Win32::System::LibraryLoader::GetModuleHandleW(None) else {
            return;
        };
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("DeskRemoteDialog"),
            PCWSTR(caption.as_ptr()),
            style(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            Some((def as *const Def).cast()),
        )
    };
    let Ok(hwnd) = created else {
        tracing::warn!("dialog window creation failed");
        return;
    };
    def.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
    theme_title_bar(hwnd, &current_theme());
    refresh_hwnd(hwnd);
    // The default (primary) button holds the initial focus ring position.
    // SAFETY: UI-thread window created above.
    if let Some(st) = unsafe { state(hwnd) } {
        st.focus = st.spec.buttons.iter().rposition(|b| b.primary).unwrap_or(0);
    }
    center(hwnd);
    // SAFETY: showing and focusing our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if def.tick_ms > 0 {
            SetTimer(Some(hwnd), TIMER_TICK, def.tick_ms, None);
        }
    }
}
