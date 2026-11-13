//! Remembers the panel window: size, position and maximized state (DAPP-108).
//!
//! The first start (no record) opens large and centered. A record that no longer fits any
//! connected monitor is pulled back onto a visible one, so the window never opens off screen.

use crate::commands::AppState;
use desk_config::{ConfigManager, WindowState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Manager, PhysicalPosition, PhysicalSize, Window, WindowEvent};

/// First window height as a share of the work area. The owner's reference is a Chrome Remote
/// Desktop window (about half the width, 70% of the height of a 34" ultrawide), "a bit bigger".
const FIRST_START_HEIGHT_PERCENT: i32 = 78;
/// First window width per height (16:10: a 16:9 remote screen plus the sidebar).
const FIRST_START_ASPECT: (i32, i32) = (16, 10);
/// The first window never takes more than this share of the work area's width.
const FIRST_START_MAX_WIDTH_PERCENT: i32 = 90;
/// At least this much of the window must lie on a monitor for a record to count as visible.
const MIN_VISIBLE_W: i32 = 160;
const MIN_VISIBLE_H: i32 = 80;
/// Quiet time after the last move/resize before the record is written.
const SAVE_DELAY: Duration = Duration::from_millis(600);

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn overlap(self, other: Rect) -> (i32, i32) {
        let w = (self.x + self.w).min(other.x + other.w) - self.x.max(other.x);
        let h = (self.y + self.h).min(other.y + other.h) - self.y.max(other.y);
        (w.max(0), h.max(0))
    }
}

fn centered(area: Rect, w: i32, h: i32) -> Rect {
    Rect {
        x: area.x + (area.w - w) / 2,
        y: area.y + (area.h - h) / 2,
        w,
        h,
    }
}

/// Where the window opens and whether it starts maximized.
///
/// `areas` are the monitors' work areas; `primary` is the one used when there is no usable record.
#[must_use]
pub fn initial_placement(
    saved: Option<WindowState>,
    areas: &[Rect],
    primary: Rect,
) -> (Rect, bool) {
    let Some(saved) = saved else {
        let h = primary.h * FIRST_START_HEIGHT_PERCENT / 100;
        let w = (h * FIRST_START_ASPECT.0 / FIRST_START_ASPECT.1)
            .min(primary.w * FIRST_START_MAX_WIDTH_PERCENT / 100);
        return (centered(primary, w, h), false);
    };
    let rect = Rect {
        x: saved.x,
        y: saved.y,
        w: i32::try_from(saved.width).unwrap_or(i32::MAX / 4),
        h: i32::try_from(saved.height).unwrap_or(i32::MAX / 4),
    };
    let best = areas.iter().copied().max_by_key(|a| {
        let (w, h) = rect.overlap(*a);
        i64::from(w) * i64::from(h)
    });
    if let Some(area) = best {
        let (ow, oh) = rect.overlap(area);
        if ow >= MIN_VISIBLE_W.min(rect.w) && oh >= MIN_VISIBLE_H.min(rect.h) {
            // Shrink to the monitor if it got smaller, then slide fully inside it.
            let (w, h) = (rect.w.min(area.w), rect.h.min(area.h));
            let x = rect.x.clamp(area.x, area.x + area.w - w);
            let y = rect.y.clamp(area.y, area.y + area.h - h);
            return (Rect { x, y, w, h }, saved.maximized);
        }
    }
    // The monitor it lived on is gone: keep the size, center on the primary one.
    let (w, h) = (rect.w.min(primary.w), rect.h.min(primary.h));
    (centered(primary, w, h), saved.maximized)
}

fn area_of(m: &tauri::Monitor) -> Rect {
    let a = m.work_area();
    Rect {
        x: a.position.x,
        y: a.position.y,
        w: i32::try_from(a.size.width).unwrap_or(i32::MAX / 4),
        h: i32::try_from(a.size.height).unwrap_or(i32::MAX / 4),
    }
}

/// Applies the remembered (or first-start) placement. Call before the window is shown.
pub fn restore(app: &tauri::App) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let areas: Vec<Rect> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(area_of)
        .collect();
    let primary = window
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| area_of(&m))
        .or_else(|| areas.first().copied());
    let Some(primary) = primary else { return };
    let saved = app
        .state::<AppState>()
        .config
        .lock()
        .ok()
        .and_then(|c| c.window);
    let (rect, maximized) = initial_placement(saved, &areas, primary);
    let w = u32::try_from(rect.w).unwrap_or(1);
    let h = u32::try_from(rect.h).unwrap_or(1);
    if let Err(error) = window.set_size(PhysicalSize::new(w, h)) {
        tracing::warn!(%error, "could not restore the window size");
    }
    if let Err(error) = window.set_position(PhysicalPosition::new(rect.x, rect.y)) {
        tracing::warn!(%error, "could not restore the window position");
    }
    if maximized {
        let _ = window.maximize();
    }
}

/// Reads the live window; `None` while it is minimized or fullscreen (nothing worth keeping).
fn capture(window: &Window, previous: Option<WindowState>) -> Option<WindowState> {
    if window.is_minimized().unwrap_or(false) || window.is_fullscreen().unwrap_or(false) {
        return None;
    }
    let read = |maximized: bool| {
        let (pos, size) = (window.outer_position().ok()?, window.inner_size().ok()?);
        (size.width > 0 && size.height > 0).then_some(WindowState {
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
            maximized,
        })
    };
    if window.is_maximized().unwrap_or(false) {
        // Keep the normal rectangle so restoring from maximized returns to it.
        return previous
            .map(|p| WindowState {
                maximized: true,
                ..p
            })
            .or_else(|| read(true));
    }
    read(false)
}

fn persist(window: &Window) {
    let state = window.state::<AppState>();
    let Ok(mut cfg) = state.config.lock() else {
        return;
    };
    let Some(now) = capture(window, cfg.window) else {
        return;
    };
    if cfg.window == Some(now) {
        return;
    }
    cfg.window = Some(now);
    if let Err(error) = ConfigManager::save(&state.config_path, &*cfg) {
        tracing::warn!(%error, "could not save the window state");
    }
}

/// Window event hook: saves after moves/resizes settle, and at once on close.
pub fn watcher() -> impl Fn(&Window, &WindowEvent) + Send + Sync + 'static {
    let generation = Arc::new(AtomicU64::new(0));
    move |window, event| {
        if window.label() != "main" {
            return;
        }
        match event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
                let (window, generation) = (window.clone(), Arc::clone(&generation));
                std::thread::spawn(move || {
                    std::thread::sleep(SAVE_DELAY);
                    if generation.load(Ordering::SeqCst) == mine {
                        persist(&window);
                    }
                });
            }
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => persist(window),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1040,
    };

    fn state(x: i32, y: i32, width: u32, height: u32, maximized: bool) -> WindowState {
        WindowState {
            x,
            y,
            width,
            height,
            maximized,
        }
    }

    #[test]
    fn first_start_is_a_bit_bigger_than_the_reference_and_centered() {
        let (r, max) = initial_placement(None, &[SCREEN], SCREEN);
        assert_eq!((r.w, r.h), (1297, 811));
        assert_eq!((r.x, r.y), (311, 114));
        assert!(!max);
    }

    #[test]
    fn first_start_on_an_ultrawide_follows_the_height_and_a_narrow_screen_caps_the_width() {
        let wide = Rect {
            x: 0,
            y: 0,
            w: 3440,
            h: 1392,
        };
        let (r, _) = initial_placement(None, &[wide], wide);
        assert_eq!((r.w, r.h), (1736, 1085));
        let narrow = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 1024,
        };
        let (r, _) = initial_placement(None, &[narrow], narrow);
        assert_eq!((r.w, r.h), (1152, 798));
    }

    #[test]
    fn saved_rect_and_maximized_are_kept() {
        let (r, max) = initial_placement(Some(state(200, 100, 1000, 700, true)), &[SCREEN], SCREEN);
        assert_eq!(
            r,
            Rect {
                x: 200,
                y: 100,
                w: 1000,
                h: 700
            }
        );
        assert!(max);
    }

    #[test]
    fn rect_on_a_missing_monitor_comes_back_onto_the_primary() {
        let (r, _) = initial_placement(Some(state(2400, 300, 1000, 700, false)), &[SCREEN], SCREEN);
        assert_eq!(
            r,
            Rect {
                x: 460,
                y: 170,
                w: 1000,
                h: 700
            }
        );
    }

    #[test]
    fn hand_written_far_off_screen_values_are_safe() {
        let (r, _) = initial_placement(
            Some(state(-30000, -30000, 800, 600, false)),
            &[SCREEN],
            SCREEN,
        );
        assert!(r.x >= 0 && r.y >= 0 && r.x + r.w <= SCREEN.w && r.y + r.h <= SCREEN.h);
    }

    #[test]
    fn rect_hanging_over_an_edge_slides_inside() {
        let (r, _) = initial_placement(Some(state(1700, 900, 900, 600, false)), &[SCREEN], SCREEN);
        assert!(r.x + r.w <= SCREEN.w && r.y + r.h <= SCREEN.h);
    }

    #[test]
    fn oversized_record_shrinks_to_the_monitor() {
        let (r, _) = initial_placement(Some(state(0, 0, 5000, 4000, false)), &[SCREEN], SCREEN);
        assert_eq!((r.w, r.h), (1920, 1040));
    }
}
