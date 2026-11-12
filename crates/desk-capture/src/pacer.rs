//! # Fixed-rate frame pacer
//!
//! ## Responsibility
//! Produces evenly spaced capture ticks (absolute deadlines, no drift) so a 180 Hz
//! desktop is sampled at exactly the target fps. Resynchronises instead of bursting
//! when the caller falls behind.
//!
//! ## Scope Boundaries
//! - Timing only; knows nothing about frames or devices.
//!
//! ## Dependencies
//! `std::time` only.

use std::time::{Duration, Instant};

/// Absolute-deadline ticker.
#[derive(Debug)]
pub struct FramePacer {
    interval: Duration,
    next: Instant,
    missed: u64,
}

impl FramePacer {
    /// `fps` is clamped to 1..=240.
    pub fn new(fps: u32) -> Self {
        Self::starting_at(fps, Instant::now())
    }

    fn starting_at(fps: u32, now: Instant) -> Self {
        let fps = fps.clamp(1, 240);
        let interval = Duration::from_nanos(1_000_000_000 / fps as u64);
        Self {
            interval,
            next: now + interval,
            missed: 0,
        }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Ticks skipped because the caller was more than one interval late.
    pub fn missed_ticks(&self) -> u64 {
        self.missed
    }

    /// Returns how long to sleep before the next tick and schedules the following one.
    fn advance(&mut self, now: Instant) -> Duration {
        let wait = self.next.saturating_duration_since(now);
        if now > self.next + self.interval {
            // Far behind: drop the backlog instead of firing a burst of ticks.
            let late = now.duration_since(self.next);
            self.missed += (late.as_nanos() / self.interval.as_nanos().max(1)) as u64;
            self.next = now + self.interval;
        } else {
            self.next += self.interval;
        }
        wait
    }

    /// Blocks until the next tick and returns the tick time.
    pub fn wait_next(&mut self) -> Instant {
        let wait = self.advance(Instant::now());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_do_not_drift() {
        let t0 = Instant::now();
        let mut p = FramePacer::starting_at(50, t0);
        let mut now = t0;
        for i in 1..=100u32 {
            let wait = p.advance(now);
            now += wait;
            assert_eq!(now, t0 + p.interval() * i);
        }
        assert_eq!(p.missed_ticks(), 0);
    }

    #[test]
    fn late_caller_resyncs_without_burst() {
        let t0 = Instant::now();
        let mut p = FramePacer::starting_at(100, t0);
        let wait = p.advance(t0 + Duration::from_millis(100));
        assert!(wait.is_zero());
        assert!(p.missed_ticks() >= 8);
        let wait = p.advance(t0 + Duration::from_millis(100));
        assert!(wait <= p.interval());
    }

    #[test]
    fn fps_is_clamped() {
        assert_eq!(FramePacer::new(0).interval(), Duration::from_secs(1));
        assert_eq!(
            FramePacer::new(1000).interval(),
            Duration::from_nanos(1_000_000_000 / 240)
        );
    }
}
