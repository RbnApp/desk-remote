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
//!
//! The Settings window adds input items drawn by the same frame: tabs, switches, a segmented
//! select and a single-line text box. Every interactive item is a focus *target* (Tab order =
//! top to bottom, buttons last) with the same ring, hover and keyboard rules as the buttons.

use crate::ui::{colorref, current_theme, draw_text, font, load_icon_sized, wide};
use desk_theme::ResolvedTheme;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
    DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, GetDC, GetDeviceCaps, GetMonitorInfoW,
    GetStockObject, GetTextExtentPoint32W, IntersectClipRect, InvalidateRect, MonitorFromPoint,
    ReleaseDC, RestoreDC, RoundRect, SaveDC, SelectObject, SetBkMode, SetTextColor,
    DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, HDC, HFONT, HGDIOBJ, LOGPIXELSX,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH, NULL_PEN, PAINTSTRUCT, PS_INSIDEFRAME,
    SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_BACK, VK_DELETE, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT,
    VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DrawIconEx, GetClientRect,
    GetCursorPos, GetWindowLongPtrW, KillTimer, LoadCursorW, SetCursor, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, DI_NORMAL, GWLP_USERDATA,
    HTCLIENT, IDC_ARROW, IDC_HAND, IDC_IBEAM, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_RESTORE, SW_SHOW, WINDOW_EX_STYLE, WM_CHAR, WM_CLOSE, WM_CREATE, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR,
    WM_TIMER, WS_CAPTION, WS_SYSMENU,
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
const SWITCH_W: i32 = 40;
const SWITCH_H: i32 = 22;
const TAB_H: i32 = 36;
const FIELD_H: i32 = 36;
const TOGGLE_PAD: i32 = 8;
const SECTION_GAP: i32 = 6;

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
    /// Tab strip; the dialog kind decides what the active tab shows (`id` reports the click).
    Tabs {
        id: u32,
        labels: Vec<String>,
        active: usize,
    },
    /// Group heading inside a tab.
    Section(String),
    /// Label (+ optional explanation) with a switch at the right; the whole row is the target.
    Toggle {
        id: u32,
        label: String,
        hint: Option<String>,
        on: bool,
        enabled: bool,
    },
    /// Label above a segmented control with one segment per option.
    Select {
        id: u32,
        label: String,
        options: Vec<String>,
        selected: usize,
    },
    /// Label above a single-line text box; `hint` shows below (validation, units).
    Edit {
        id: u32,
        label: String,
        value: String,
        max_chars: usize,
        numeric: bool,
        hint: Option<(String, Tone)>,
    },
    /// Rows with a text and a small action button at the right (paired panels + "remove").
    /// `empty` replaces the list when there is no entry.
    Entries { entries: Vec<Entry>, empty: String },
}

/// One row of [`Item::Entries`].
pub(crate) struct Entry {
    pub id: u32,
    pub text: String,
    pub action: String,
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
    /// A segment of an [`Item::Select`] / tab was chosen: `(id, index)`.
    pub on_choice: fn(u32, usize),
    /// A text box was committed (Enter, Tab or leaving it): `(id, text)`.
    pub on_edit: fn(u32, &str),
    /// Re-build interval in ms (0 = only on [`refresh`]).
    pub tick_ms: u32,
    hwnd: AtomicIsize,
}

fn no_choice(_: u32, _: usize) {}
fn no_edit(_: u32, _: &str) {}

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
            on_choice: no_choice,
            on_edit: no_edit,
            tick_ms,
            hwnd: AtomicIsize::new(0),
        }
    }

    /// Adds the handlers of the input items (select / tabs and text boxes).
    pub(crate) const fn with_input(
        mut self,
        on_choice: fn(u32, usize),
        on_edit: fn(u32, &str),
    ) -> Self {
        self.on_choice = on_choice;
        self.on_edit = on_edit;
        self
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
    /// Every interactive spot in Tab order (items top to bottom, then the buttons).
    targets: Vec<Target>,
}

/// What a click or Space on a target does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Button(u32),
    Toggle(u32),
    Choose(u32, usize),
    Edit(u32),
    Entry(u32),
}

/// One focusable, clickable rectangle; `item`/`sub` find it again while painting.
struct Target {
    rect: RECT,
    act: Act,
    item: usize,
    sub: usize,
}

/// The text box that has the keyboard: its live text and caret (in characters).
struct Editing {
    id: u32,
    orig: String,
    text: String,
    caret: usize,
}

struct State {
    def: &'static Def,
    dpi: i32,
    spec: Spec,
    layout: Layout,
    /// Hover / pressed / focus are indexes into `layout.targets`.
    hover: Option<usize>,
    pressed: Option<usize>,
    focus: usize,
    keyboard: bool,
    tracking: bool,
    editing: Option<Editing>,
    /// First half of a UTF-16 pair typed into a text box.
    high_surrogate: Option<u16>,
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

    /// Target index of footer button `i` (the buttons are the last targets).
    fn button_target(&self, i: usize) -> usize {
        self.layout.targets.len() - self.spec.buttons.len() + i
    }

    fn target_of(&self, item: usize, sub: usize) -> Option<usize> {
        self.layout
            .targets
            .iter()
            .position(|t| t.item == item && t.sub == sub)
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
    // Empty text measures like one space (same height, no width) and never reaches DrawTextW empty.
    let mut buf: Vec<u16> = if text.is_empty() {
        vec![u16::from(b' ')]
    } else {
        text.encode_utf16().collect()
    };
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
    (
        if text.is_empty() { 0 } else { r.right - r.left },
        r.bottom - r.top,
    )
}

/// `measure` with the default font, for the empty-text regression test.
#[cfg(test)]
pub(crate) fn measure_for_test(hdc: HDC, text: &str) -> (i32, i32) {
    measure(hdc, HFONT::default(), text, 200, false)
}

/// Width of `text` in `f` (no wrapping; trailing spaces count, unlike `DT_CALCRECT`).
fn text_width(hdc: HDC, f: HFONT, text: &str) -> i32 {
    let buf: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    // SAFETY: valid DC and buffer; the font is selected only for the call.
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(f.0));
        let _ = GetTextExtentPoint32W(hdc, &buf, &mut size);
        SelectObject(hdc, old);
    }
    size.cx
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
    for (index, item) in st.spec.items.iter().enumerate() {
        y += if lay.items.is_empty() {
            st.px(HEADER_GAP)
        } else {
            st.px(ITEM_GAP)
        };
        let h = match item {
            Item::Tabs { id, labels, .. } => {
                let n = i32::try_from(labels.len()).unwrap_or(1).max(1);
                for sub in 0..labels.len() {
                    let k = i32::try_from(sub).unwrap_or(0);
                    lay.targets.push(Target {
                        rect: rect(
                            pad + body_w * k / n,
                            y,
                            pad + body_w * (k + 1) / n,
                            y + st.px(TAB_H),
                        ),
                        act: Act::Choose(*id, sub),
                        item: index,
                        sub,
                    });
                }
                st.px(TAB_H)
            }
            Item::Section(text) => {
                measure(hdc, fonts.button, text, body_w, false).1 + st.px(SECTION_GAP)
            }
            Item::Toggle {
                id, label, hint, ..
            } => {
                let text_w = body_w - st.px(SWITCH_W) - 3 * st.px(TOGGLE_PAD) - st.px(8);
                let mut inner = measure(hdc, fonts.body, label, text_w, true).1;
                if let Some(hint) = hint {
                    inner += st.px(2) + measure(hdc, fonts.small, hint, text_w, true).1;
                }
                let h = inner + 2 * st.px(TOGGLE_PAD + 2);
                lay.targets.push(Target {
                    rect: rect(pad, y, width - pad, y + h),
                    act: Act::Toggle(*id),
                    item: index,
                    sub: 0,
                });
                h
            }
            Item::Select {
                id, label, options, ..
            } => {
                let lh = measure(hdc, fonts.small, label, body_w, false).1 + st.px(4);
                let n = i32::try_from(options.len()).unwrap_or(1).max(1);
                let gap = st.px(BUTTON_GAP);
                let seg = (body_w - (n - 1) * gap) / n;
                for sub in 0..options.len() {
                    let k = i32::try_from(sub).unwrap_or(0);
                    let left = pad + k * (seg + gap);
                    lay.targets.push(Target {
                        rect: rect(left, y + lh, left + seg, y + lh + st.px(FIELD_H)),
                        act: Act::Choose(*id, sub),
                        item: index,
                        sub,
                    });
                }
                lh + st.px(FIELD_H)
            }
            Item::Edit {
                id, label, hint, ..
            } => {
                let lh = measure(hdc, fonts.small, label, body_w, false).1 + st.px(4);
                lay.targets.push(Target {
                    rect: rect(pad, y + lh, width - pad, y + lh + st.px(FIELD_H)),
                    act: Act::Edit(*id),
                    item: index,
                    sub: 0,
                });
                let hint_h = hint.as_ref().map_or(0, |(text, _)| {
                    st.px(4) + measure(hdc, fonts.small, text, body_w, true).1
                });
                lh + st.px(FIELD_H) + hint_h
            }
            Item::Entries { entries, .. } => {
                let (rh, gap) = (st.px(ROW_H), st.px(ROW_GAP));
                let n = i32::try_from(entries.len()).unwrap_or(1).max(1);
                for (sub, entry) in entries.iter().enumerate() {
                    let k = i32::try_from(sub).unwrap_or(0);
                    let top = y + k * (rh + gap);
                    let bw =
                        measure(hdc, fonts.small, &entry.action, width, false).0 + 2 * st.px(12);
                    let bh = rh - 2 * st.px(4);
                    lay.targets.push(Target {
                        rect: rect(
                            width - pad - st.px(6) - bw,
                            top + st.px(4),
                            width - pad - st.px(6),
                            top + st.px(4) + bh,
                        ),
                        act: Act::Entry(entry.id),
                        item: index,
                        sub,
                    });
                }
                n * rh + (n - 1) * gap
            }
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
    for (i, (b, r)) in st.spec.buttons.iter().zip(&buttons).enumerate() {
        lay.targets.push(Target {
            rect: *r,
            act: Act::Button(b.id),
            item: usize::MAX,
            sub: i,
        });
    }
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
    let fonts = Fonts::new(st);
    // SAFETY: screen DC obtained and released in place.
    unsafe {
        let dc = GetDC(None);
        st.layout = compute_layout(dc, &fonts, st);
        ReleaseDC(None, dc);
    }
    st.focus = st.focus.min(st.layout.targets.len().saturating_sub(1));
    let editing = st.editing.as_ref().map(|e| e.id);
    if editing.is_some_and(|id| !st.layout.targets.iter().any(|t| t.act == Act::Edit(id))) {
        st.editing = None;
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

/// Keyboard focus ring around `r`.
fn ring(hdc: HDC, st: &State, theme: &ResolvedTheme, r: RECT) {
    let g = st.px(3);
    round(
        hdc,
        rect(r.left - g, r.top - g, r.right + g, r.bottom + g),
        st.px(RADIUS) + g,
        None,
        Some((&theme.tokens.focus_ring, st.px(2))),
    );
}

fn fill_rect(hdc: HDC, r: RECT, token: &str) {
    // SAFETY: brush created, used and deleted in place.
    unsafe {
        let brush = CreateSolidBrush(ink(token));
        FillRect(hdc, &r, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

/// A button-looking target: primary = filled, otherwise outlined; hover / pressed / focus ring.
fn draw_face(
    hdc: HDC,
    f: HFONT,
    st: &State,
    theme: &ResolvedTheme,
    (target, r, label, primary): (usize, RECT, &str, bool),
) {
    let t = &theme.tokens;
    let down = st.pressed == Some(target) && st.hover == Some(target);
    let over = st.hover == Some(target);
    let one = st.px(1).max(1);
    let (fill, border, text) = if primary {
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
    if st.keyboard && st.focus == target {
        ring(hdc, st, theme, r);
    }
    round(hdc, r, st.px(RADIUS), Some(fill), Some((border, one)));
    text_at(
        hdc,
        f,
        text,
        label,
        r,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER,
    );
}

fn draw_button(hdc: HDC, fonts: &Fonts, st: &State, theme: &ResolvedTheme, i: usize) {
    let (Some(b), Some(&r)) = (st.spec.buttons.get(i), st.layout.buttons.get(i)) else {
        return;
    };
    draw_face(
        hdc,
        fonts.button,
        st,
        theme,
        (st.button_target(i), r, &b.label, b.primary),
    );
}

/// Switch (track + knob) of a toggle row, vertically centred at the right of `row`.
fn draw_switch(hdc: HDC, st: &State, theme: &ResolvedTheme, row: RECT, on: bool, enabled: bool) {
    let t = &theme.tokens;
    let (w, h) = (st.px(SWITCH_W), st.px(SWITCH_H));
    let right = row.right - st.px(TOGGLE_PAD + 4);
    let top = row.top + (row.bottom - row.top - h) / 2;
    let track = rect(right - w, top, right, top + h);
    let (fill, knob) = if !enabled {
        (&t.disabled_bg, &t.disabled_text)
    } else if on {
        (&t.success, &t.surface)
    } else {
        (&t.text_muted, &t.surface)
    };
    round(hdc, track, h / 2, Some(fill), None);
    let d = h - 2 * st.px(3);
    let x = if on {
        track.right - st.px(3) - d
    } else {
        track.left + st.px(3)
    };
    round(
        hdc,
        rect(x, top + st.px(3), x + d, top + st.px(3) + d),
        d / 2,
        Some(knob),
        None,
    );
}

/// Text box: border (ring colour while it has the keyboard), text, caret.
fn draw_edit(
    hdc: HDC,
    fonts: &Fonts,
    st: &State,
    theme: &ResolvedTheme,
    (target, value): (usize, &str),
) {
    let t = &theme.tokens;
    let r = st.layout.targets[target].rect;
    let editing = st
        .editing
        .as_ref()
        .filter(|e| st.layout.targets[target].act == Act::Edit(e.id));
    let stroke = if editing.is_some() {
        (&t.focus_ring, st.px(2))
    } else {
        (&t.border, st.px(1).max(1))
    };
    round(
        hdc,
        r,
        st.px(RADIUS),
        Some(&t.surface),
        Some((stroke.0.as_str(), stroke.1)),
    );
    let inner = rect(r.left + st.px(12), r.top, r.right - st.px(12), r.bottom);
    let shown = editing.map_or(value, |e| e.text.as_str());
    let caret_x = editing.map(|e| {
        let head: String = e.text.chars().take(e.caret).collect();
        text_width(hdc, fonts.body, &head)
    });
    let scroll = caret_x.map_or(0, |x| (x - (inner.right - inner.left) + st.px(2)).max(0));
    // SAFETY: the clip region is saved and restored around the draws.
    unsafe {
        let saved = SaveDC(hdc);
        IntersectClipRect(hdc, inner.left, inner.top, inner.right, inner.bottom);
        text_at(
            hdc,
            fonts.body,
            &t.text,
            shown,
            rect(
                inner.left - scroll,
                inner.top,
                inner.right + st.px(400),
                inner.bottom,
            ),
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        if let Some(x) = caret_x {
            let h = st.px(18);
            let mid = (r.top + r.bottom) / 2;
            fill_rect(
                hdc,
                rect(
                    inner.left + x - scroll,
                    mid - h / 2,
                    inner.left + x - scroll + st.px(2).max(1),
                    mid + h / 2,
                ),
                &t.text,
            );
        }
        let _ = RestoreDC(hdc, saved);
    }
}

/// Body of the input items (tabs, switches, select, text box, entries).
fn paint_input(
    hdc: HDC,
    fonts: &Fonts,
    st: &State,
    theme: &ResolvedTheme,
    (index, item, r): (usize, &Item, RECT),
) {
    let t = &theme.tokens;
    let one = st.px(1).max(1);
    match item {
        Item::Tabs { labels, active, .. } => {
            fill_rect(
                hdc,
                rect(r.left, r.bottom - one, r.right, r.bottom),
                &t.border,
            );
            for (sub, label) in labels.iter().enumerate() {
                let Some(ti) = st.target_of(index, sub) else {
                    continue;
                };
                let tr = st.layout.targets[ti].rect;
                if st.hover == Some(ti) {
                    fill_rect(hdc, tr, &t.surface_hover);
                }
                let on = sub == *active;
                text_at(
                    hdc,
                    fonts.button,
                    if on { &t.text } else { &t.text_muted },
                    label,
                    tr,
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
                if on {
                    fill_rect(
                        hdc,
                        rect(tr.left, tr.bottom - st.px(3), tr.right, tr.bottom),
                        &t.primary,
                    );
                }
                if st.keyboard && st.focus == ti {
                    ring(hdc, st, theme, tr);
                }
            }
        }
        Item::Section(text) => {
            text_at(hdc, fonts.button, &t.text, text, r, DT_LEFT | DT_SINGLELINE);
            fill_rect(
                hdc,
                rect(r.left, r.bottom - one, r.right, r.bottom),
                &t.border,
            );
        }
        Item::Toggle {
            label,
            hint,
            on,
            enabled,
            ..
        } => {
            let Some(ti) = st.target_of(index, 0) else {
                return;
            };
            let fill = if *enabled && st.hover == Some(ti) {
                &t.surface_hover
            } else {
                &t.surface_alt
            };
            if st.keyboard && st.focus == ti {
                ring(hdc, st, theme, r);
            }
            round(hdc, r, st.px(RADIUS), Some(fill), None);
            let left = r.left + st.px(TOGGLE_PAD + 4);
            let right = r.right - st.px(SWITCH_W) - 2 * st.px(TOGGLE_PAD) - st.px(8);
            let top = r.top + st.px(TOGGLE_PAD + 2);
            let lh = measure(hdc, fonts.body, label, right - left, true).1;
            let (main, soft) = if *enabled {
                (&t.text, &t.text_muted)
            } else {
                (&t.disabled_text, &t.disabled_text)
            };
            text_at(
                hdc,
                fonts.body,
                main,
                label,
                rect(left, top, right, top + lh),
                DT_LEFT | DT_WORDBREAK,
            );
            if let Some(hint) = hint {
                let y = top + lh + st.px(2);
                text_at(
                    hdc,
                    fonts.small,
                    soft,
                    hint,
                    rect(left, y, right, r.bottom),
                    DT_LEFT | DT_WORDBREAK,
                );
            }
            draw_switch(hdc, st, theme, r, *on, *enabled);
        }
        Item::Select {
            label,
            options,
            selected,
            ..
        } => {
            let lh = measure(hdc, fonts.small, label, r.right - r.left, false).1;
            text_at(
                hdc,
                fonts.small,
                &t.text_muted,
                label,
                rect(r.left, r.top, r.right, r.top + lh),
                DT_LEFT | DT_SINGLELINE,
            );
            for (sub, option) in options.iter().enumerate() {
                if let Some(ti) = st.target_of(index, sub) {
                    let tr = st.layout.targets[ti].rect;
                    draw_face(
                        hdc,
                        fonts.button,
                        st,
                        theme,
                        (ti, tr, option, sub == *selected),
                    );
                }
            }
        }
        Item::Edit {
            label, value, hint, ..
        } => {
            let lh = measure(hdc, fonts.small, label, r.right - r.left, false).1;
            text_at(
                hdc,
                fonts.small,
                &t.text_muted,
                label,
                rect(r.left, r.top, r.right, r.top + lh),
                DT_LEFT | DT_SINGLELINE,
            );
            let Some(ti) = st.target_of(index, 0) else {
                return;
            };
            let br = st.layout.targets[ti].rect;
            if st.keyboard && st.focus == ti && st.editing.is_none() {
                ring(hdc, st, theme, br);
            }
            draw_edit(hdc, fonts, st, theme, (ti, value));
            if let Some((text, tone)) = hint {
                text_at(
                    hdc,
                    fonts.small,
                    tone_color(theme, *tone),
                    text,
                    rect(r.left, br.bottom + st.px(4), r.right, r.bottom),
                    DT_LEFT | DT_WORDBREAK,
                );
            }
        }
        Item::Entries { entries, empty } => {
            let (rh, gap) = (st.px(ROW_H), st.px(ROW_GAP));
            if entries.is_empty() {
                round(hdc, r, st.px(RADIUS), Some(&t.surface_alt), None);
                text_at(
                    hdc,
                    fonts.body,
                    &t.text_muted,
                    empty,
                    rect(r.left + st.px(14), r.top, r.right - st.px(12), r.bottom),
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
            }
            for (sub, entry) in entries.iter().enumerate() {
                let top = r.top + i32::try_from(sub).unwrap_or(0) * (rh + gap);
                let row = rect(r.left, top, r.right, top + rh);
                round(hdc, row, st.px(RADIUS), Some(&t.surface_alt), None);
                let Some(ti) = st.target_of(index, sub) else {
                    continue;
                };
                let action = st.layout.targets[ti].rect;
                text_at(
                    hdc,
                    fonts.body,
                    &t.text,
                    &entry.text,
                    rect(
                        row.left + st.px(14),
                        row.top,
                        action.left - st.px(8),
                        row.bottom,
                    ),
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
                draw_face(
                    hdc,
                    fonts.small,
                    st,
                    theme,
                    (ti, action, &entry.action, false),
                );
            }
        }
        _ => {}
    }
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
    for (index, (item, &r)) in st.spec.items.iter().zip(&st.layout.items).enumerate() {
        paint_input(hdc, &fonts, st, theme, (index, item, r));
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
            Item::Tabs { .. }
            | Item::Section(_)
            | Item::Toggle { .. }
            | Item::Select { .. }
            | Item::Edit { .. }
            | Item::Entries { .. } => {}
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
        .targets
        .iter()
        .position(|t| x >= t.rect.left && x < t.rect.right && y >= t.rect.top && y < t.rect.bottom)
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

/// Runs what a click or Space on target `i` means, then redraws with the new content.
fn activate_target(hwnd: HWND, i: usize) {
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    let Some(target) = st.layout.targets.get(i) else {
        return;
    };
    let (act, def) = (target.act, st.def);
    match act {
        Act::Button(id) => activate(hwnd, id),
        Act::Toggle(id) | Act::Entry(id) => {
            (def.on_button)(id);
            refresh_hwnd(hwnd);
        }
        Act::Choose(id, index) => {
            (def.on_choice)(id, index);
            refresh_hwnd(hwnd);
        }
        Act::Edit(_) => set_focus(hwnd, i),
    }
}

/// Byte offset of the `n`-th character of `text` (its end when `n` is past it).
fn byte_at(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(i, _)| i)
}

fn begin_edit(st: &mut State, id: u32) {
    let value = st.spec.items.iter().find_map(|item| match item {
        Item::Edit { id: i, value, .. } if *i == id => Some(value.clone()),
        _ => None,
    });
    if let Some(value) = value {
        st.editing = Some(Editing {
            id,
            caret: value.chars().count(),
            orig: value.clone(),
            text: value,
        });
    }
}

/// Stops editing; a changed text goes to the dialog kind, which may refuse it (blank name).
fn commit_edit(hwnd: HWND) {
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    let Some(done) = st.editing.take() else {
        return;
    };
    if done.text != done.orig {
        (st.def.on_edit)(done.id, &done.text);
    }
    refresh_hwnd(hwnd);
}

/// Moves the focus to target `index`; leaving a text box commits it, entering one starts it.
fn set_focus(hwnd: HWND, index: usize) {
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    if st.focus == index && st.editing.is_some() {
        return;
    }
    if st.editing.is_some() {
        commit_edit(hwnd);
    }
    // SAFETY: as above; the commit may have rebuilt the state, so it is read again.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    st.focus = index.min(st.layout.targets.len().saturating_sub(1));
    if let Some(Act::Edit(id)) = st.layout.targets.get(st.focus).map(|t| t.act) {
        begin_edit(st, id);
    }
    redraw(hwnd);
}

fn move_focus(hwnd: HWND, step: isize) {
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    let n = st.layout.targets.len() as isize;
    if n == 0 {
        return;
    }
    let next = ((st.focus as isize + step).rem_euclid(n)) as usize;
    st.keyboard = true;
    set_focus(hwnd, next);
}

/// Keys of a focused text box; `false` when the key is not one of its own.
fn edit_key(hwnd: HWND, key: u16) -> bool {
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return false;
    };
    if st.editing.is_none() {
        return false;
    }
    if key == VK_RETURN.0 {
        commit_edit(hwnd);
        // Keep typing: the box starts again from the saved value.
        // SAFETY: as above.
        let focus = unsafe { state(hwnd) }.map(|s| s.focus);
        if let Some(focus) = focus {
            set_focus(hwnd, focus);
        }
        return true;
    }
    let Some(e) = st.editing.as_mut() else {
        return false;
    };
    let len = e.text.chars().count();
    if key == VK_LEFT.0 {
        e.caret = e.caret.saturating_sub(1);
    } else if key == VK_RIGHT.0 {
        e.caret = (e.caret + 1).min(len);
    } else if key == VK_HOME.0 {
        e.caret = 0;
    } else if key == VK_END.0 {
        e.caret = len;
    } else if key == VK_BACK.0 {
        if e.caret > 0 {
            e.caret -= 1;
            e.text.remove(byte_at(&e.text, e.caret));
        }
    } else if key == VK_DELETE.0 {
        if e.caret < len {
            e.text.remove(byte_at(&e.text, e.caret));
        }
    } else {
        return false;
    }
    redraw(hwnd);
    true
}

fn on_key(hwnd: HWND, key: u16) {
    if key == VK_ESCAPE.0 {
        activate(hwnd, CLOSE);
    } else if edit_key(hwnd, key) {
    } else if key == VK_RETURN.0 {
        // SAFETY: UI-thread window created by `open`.
        let primary = unsafe { state(hwnd) }.and_then(|s| {
            s.spec
                .buttons
                .iter()
                .rposition(|b| b.primary)
                .map(|p| s.button_target(p))
        });
        if let Some(i) = primary {
            activate_target(hwnd, i);
        }
    } else if key == VK_SPACE.0 {
        // SAFETY: UI-thread window created by `open`.
        let focus = unsafe { state(hwnd) }.map(|s| s.focus);
        if let Some(i) = focus {
            activate_target(hwnd, i);
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

/// A typed character (UTF-16 unit) for the focused text box.
fn on_char(hwnd: HWND, unit: u16) {
    // SAFETY: UI-thread window created by `open`.
    let Some(st) = (unsafe { state(hwnd) }) else {
        return;
    };
    let Some(id) = st.editing.as_ref().map(|e| e.id) else {
        return;
    };
    let (max, numeric) = st
        .spec
        .items
        .iter()
        .find_map(|item| match item {
            Item::Edit {
                id: i,
                max_chars,
                numeric,
                ..
            } if *i == id => Some((*max_chars, *numeric)),
            _ => None,
        })
        .unwrap_or((0, false));
    let typed = if (0xD800..0xDC00).contains(&unit) {
        st.high_surrogate = Some(unit);
        return;
    } else if (0xDC00..0xE000).contains(&unit) {
        st.high_surrogate
            .take()
            .and_then(|high| char::decode_utf16([high, unit]).next())
            .and_then(Result::ok)
    } else {
        char::from_u32(u32::from(unit))
    };
    let Some(c) = typed.filter(|c| !c.is_control() && (!numeric || c.is_ascii_digit())) else {
        return;
    };
    let Some(e) = st.editing.as_mut() else {
        return;
    };
    if e.text.chars().count() >= max {
        return;
    }
    e.text.insert(byte_at(&e.text, e.caret), c);
    e.caret += 1;
    redraw(hwnd);
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
                // SAFETY: capture released on button-up.
                unsafe { SetCapture(hwnd) };
                set_focus(hwnd, i);
            } else {
                // A click on empty space ends the text box (and saves it).
                commit_edit(hwnd);
            }
            redraw(hwnd);
        }
        WM_LBUTTONUP => {
            let pressed = st.pressed.take();
            let is_edit = pressed
                .and_then(|i| st.layout.targets.get(i))
                .is_some_and(|t| matches!(t.act, Act::Edit(_)));
            // SAFETY: releases our own capture.
            let _ = unsafe { ReleaseCapture() };
            redraw(hwnd);
            if let (Some(i), false) = (pressed.filter(|p| Some(*p) == over), is_edit) {
                activate_target(hwnd, i);
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
        let id = match hit(st, at.x, at.y).and_then(|i| st.layout.targets.get(i)) {
            Some(t) if matches!(t.act, Act::Edit(_)) => IDC_IBEAM,
            Some(_) => IDC_HAND,
            None => IDC_ARROW,
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
                    editing: None,
                    high_surrogate: None,
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
        WM_CHAR => {
            on_char(hwnd, wp.0 as u16);
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

/// Whether this dialog is on screen right now.
pub(crate) fn is_open(def: &'static Def) -> bool {
    def.hwnd.load(Ordering::SeqCst) != 0
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
        let hwnd = HWND(existing as *mut _);
        // SAFETY: the handle belongs to this thread's window; a stale one just fails.
        // SW_RESTORE un-minimizes (SetForegroundWindow alone leaves an iconic window iconic).
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
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
        let primary = st.spec.buttons.iter().rposition(|b| b.primary).unwrap_or(0);
        st.focus = st.button_target(primary);
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
