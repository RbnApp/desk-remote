//! # Input injector with stuck-key protection
//!
//! ## Responsibility
//! Converts remote mouse / keyboard events into backend [`RawInput`] events while
//! tracking which keys and buttons are held, so [`InputInjector::release_all`] (also run
//! on drop) can lift everything when a viewer disconnects.
//!
//! ## Scope Boundaries
//! - Platform calls are behind [`InputBackend`]; this module is unit-testable with a
//!   recording backend.
//!
//! ## Dependencies
//! `mapping`, `desk-capture` ([`ScreenRect`]), `desk-error`.

use crate::mapping::{button_changes, map_to_virtual_desktop, split_scancode, MouseButton};
use desk_capture::ScreenRect;
use desk_error::DeskResult;
use std::collections::BTreeSet;

/// Windows wheel units per notch (`WHEEL_DELTA`).
pub const WHEEL_DELTA: i16 = 120;

/// Converts the wire wheel value (whole notches, positive = up) into `WHEEL_DELTA` units.
/// `i8::MAX * 120` always fits an `i16`, so this cannot overflow.
#[must_use]
pub fn wheel_units(notches: i8) -> i16 {
    i16::from(notches) * WHEEL_DELTA
}

/// One low-level event for the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawInput {
    /// Absolute move, 0..=65535 over the whole virtual desktop.
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// Wheel rotation in `WHEEL_DELTA` units (120 = one notch, positive = away from user).
    Wheel {
        delta: i32,
    },
    Key {
        scan: u16,
        extended: bool,
        down: bool,
    },
}

/// Platform sink for [`RawInput`].
pub trait InputBackend {
    fn send(&mut self, events: &[RawInput]) -> DeskResult<()>;
    /// Bounds of the whole virtual desktop in pixels.
    fn virtual_desktop(&self) -> ScreenRect;
}

/// Stateful injector for one remote viewer session.
pub struct InputInjector<B: InputBackend> {
    backend: B,
    monitor: ScreenRect,
    buttons: u8,
    held_keys: BTreeSet<u16>,
}

impl<B: InputBackend> InputInjector<B> {
    /// `monitor` is the desktop rectangle of the captured monitor.
    pub fn new(backend: B, monitor: ScreenRect) -> Self {
        Self {
            backend,
            monitor,
            buttons: 0,
            held_keys: BTreeSet::new(),
        }
    }

    /// Changes the captured monitor (e.g. after a monitor switch).
    pub fn set_monitor(&mut self, monitor: ScreenRect) {
        self.monitor = monitor;
    }

    pub fn held_key_count(&self) -> usize {
        self.held_keys.len()
    }

    pub fn held_buttons(&self) -> u8 {
        self.buttons
    }

    /// Mouse event: `x`/`y` in 0..=65535 over the captured monitor, `buttons` bit mask
    /// (bit 0 left, 1 right, 2 middle, 3 X1, 4 X2), `wheel` in `WHEEL_DELTA` units.
    pub fn mouse(&mut self, x: u16, y: u16, buttons: u8, wheel: i16) -> DeskResult<()> {
        let virtual_desktop = self.backend.virtual_desktop();
        let (vx, vy) = map_to_virtual_desktop(x, y, self.monitor, virtual_desktop);
        let mut events = vec![RawInput::MouseMove { x: vx, y: vy }];
        for (button, down) in button_changes(self.buttons, buttons & 0b1_1111) {
            events.push(RawInput::MouseButton { button, down });
        }
        if wheel != 0 {
            events.push(RawInput::Wheel {
                delta: wheel as i32,
            });
        }
        self.backend.send(&events)?;
        self.buttons = buttons & 0b1_1111;
        Ok(())
    }

    /// Keyboard event by set-1 scancode (`0xE0xx` for extended keys).
    pub fn key(&mut self, scancode: u16, down: bool) -> DeskResult<()> {
        let (scan, extended) = split_scancode(scancode);
        if scan == 0 {
            return Ok(());
        }
        self.backend.send(&[RawInput::Key {
            scan,
            extended,
            down,
        }])?;
        if down {
            self.held_keys.insert(scancode);
        } else {
            self.held_keys.remove(&scancode);
        }
        Ok(())
    }

    /// Releases every held key and mouse button (connection lost / viewer left).
    pub fn release_all(&mut self) -> DeskResult<()> {
        let mut events = Vec::new();
        for (button, _) in button_changes(self.buttons, 0) {
            events.push(RawInput::MouseButton {
                button,
                down: false,
            });
        }
        for &scancode in self.held_keys.iter().rev() {
            let (scan, extended) = split_scancode(scancode);
            events.push(RawInput::Key {
                scan,
                extended,
                down: false,
            });
        }
        if events.is_empty() {
            return Ok(());
        }
        let released = events.len();
        let result = self.backend.send(&events);
        self.buttons = 0;
        self.held_keys.clear();
        tracing::info!(released, "released held input on session end");
        result
    }
}

impl<B: InputBackend> Drop for InputInjector<B> {
    fn drop(&mut self) {
        if let Err(e) = self.release_all() {
            tracing::warn!(error = %e, "could not release held input on drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Recorder(Rc<RefCell<Vec<RawInput>>>);

    impl InputBackend for Recorder {
        fn send(&mut self, events: &[RawInput]) -> DeskResult<()> {
            self.0.borrow_mut().extend_from_slice(events);
            Ok(())
        }

        fn virtual_desktop(&self) -> ScreenRect {
            screen()
        }
    }

    fn screen() -> ScreenRect {
        ScreenRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    fn injector() -> (InputInjector<Recorder>, Recorder) {
        let recorder = Recorder::default();
        (InputInjector::new(recorder.clone(), screen()), recorder)
    }

    #[test]
    fn mouse_emits_move_then_button_edges() -> DeskResult<()> {
        let (mut inj, rec) = injector();
        inj.mouse(100, 200, 0b01, 0)?;
        inj.mouse(100, 200, 0b01, 0)?; // held, no repeat press
        inj.mouse(100, 200, 0b00, 120)?;
        let events = rec.0.borrow();
        let presses = events
            .iter()
            .filter(|e| matches!(e, RawInput::MouseButton { down: true, .. }))
            .count();
        let releases = events
            .iter()
            .filter(|e| matches!(e, RawInput::MouseButton { down: false, .. }))
            .count();
        assert_eq!((presses, releases), (1, 1));
        assert!(events.contains(&RawInput::Wheel { delta: 120 }));
        assert!(matches!(events[0], RawInput::MouseMove { .. }));
        Ok(())
    }

    #[test]
    fn wire_notches_become_wheel_delta_units() -> DeskResult<()> {
        assert_eq!(wheel_units(1), 120);
        assert_eq!(wheel_units(-3), -360);
        assert_eq!(wheel_units(i8::MAX), 15_240);
        let (mut inj, rec) = injector();
        inj.mouse(0, 0, 0, wheel_units(-1))?;
        assert!(rec.0.borrow().contains(&RawInput::Wheel { delta: -120 }));
        Ok(())
    }

    #[test]
    fn key_tracking_and_release_all() -> DeskResult<()> {
        let (mut inj, rec) = injector();
        inj.key(0x1E, true)?; // A
        inj.key(0xE04B, true)?; // Left arrow (extended)
        inj.mouse(0, 0, 0b11, 0)?;
        assert_eq!(inj.held_key_count(), 2);
        inj.release_all()?;
        assert_eq!((inj.held_key_count(), inj.held_buttons()), (0, 0));
        let events = rec.0.borrow();
        let key_ups = events
            .iter()
            .filter(|e| matches!(e, RawInput::Key { down: false, .. }))
            .count();
        let button_ups = events
            .iter()
            .filter(|e| matches!(e, RawInput::MouseButton { down: false, .. }))
            .count();
        assert_eq!((key_ups, button_ups), (2, 2));
        assert!(events.contains(&RawInput::Key {
            scan: 0x4B,
            extended: true,
            down: false
        }));
        Ok(())
    }

    #[test]
    fn release_all_is_a_noop_when_nothing_is_held() -> DeskResult<()> {
        let (mut inj, rec) = injector();
        inj.release_all()?;
        assert!(rec.0.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn dropping_the_injector_lifts_held_keys() -> DeskResult<()> {
        let rec = Recorder::default();
        {
            let mut inj = InputInjector::new(rec.clone(), screen());
            inj.key(0x2A, true)?; // Left shift
        }
        assert!(rec.0.borrow().contains(&RawInput::Key {
            scan: 0x2A,
            extended: false,
            down: false
        }));
        Ok(())
    }

    #[test]
    fn zero_scancode_is_ignored() -> DeskResult<()> {
        let (mut inj, rec) = injector();
        inj.key(0, true)?;
        assert!(rec.0.borrow().is_empty());
        Ok(())
    }
}
