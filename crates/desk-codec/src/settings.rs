//! # Encoder settings and output geometry
//!
//! ## Responsibility
//! Defines the parameters every H.264 encoder backend receives and the pure helpers
//! that derive the encoded frame size and a default bitrate from the source.
//!
//! ## Scope Boundaries
//! - Pure data and arithmetic; no device access.
//!
//! ## Dependencies
//! None.

/// Parameters of one encoder instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    /// Distance between IDR frames, in frames.
    pub gop_frames: u32,
}

impl EncoderSettings {
    /// Settings for a given output size with the default bitrate and a 2-second GOP.
    pub fn for_size(width: u32, height: u32, fps: u32) -> Self {
        Self {
            width,
            height,
            fps,
            bitrate_bps: default_bitrate(width, height, fps),
            gop_frames: fps.max(1) * 2,
        }
    }
}

/// One encoded access unit in Annex-B form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedUnit {
    /// Microseconds on the pipeline clock (time the source frame was captured).
    pub ts_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// Largest even size that fits `max_w` x `max_h` and keeps the source aspect ratio.
/// Sources already inside the box keep their size (rounded down to even).
pub fn fit_dimensions(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let src_w = src_w.max(2);
    let src_h = src_h.max(2);
    let (mut w, mut h) = (src_w, src_h);
    if src_w > max_w || src_h > max_h {
        let scale = f64::min(max_w as f64 / src_w as f64, max_h as f64 / src_h as f64);
        w = (src_w as f64 * scale) as u32;
        h = (src_h as f64 * scale) as u32;
    }
    ((w & !1).max(2), (h & !1).max(2))
}

/// About 0.15 bit per pixel per frame, clamped to 1..=50 Mbit/s.
pub fn default_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    let bps = width as u64 * height as u64 * fps as u64 * 15 / 100;
    bps.clamp(1_000_000, 50_000_000) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_even() {
        assert_eq!(fit_dimensions(3440, 1440, 1920, 1080), (1920, 803 & !1));
        assert_eq!(fit_dimensions(3840, 2160, 1920, 1080), (1920, 1080));
        assert_eq!(fit_dimensions(1280, 720, 1920, 1080), (1280, 720));
        assert_eq!(fit_dimensions(1281, 721, 1920, 1080), (1280, 720));
    }

    #[test]
    fn fit_output_is_always_even_and_inside_box() {
        for (w, h) in [(2560, 1440), (1366, 768), (5120, 1440), (800, 600), (3, 3)] {
            let (ow, oh) = fit_dimensions(w, h, 1920, 1080);
            assert!(ow % 2 == 0 && oh % 2 == 0 && ow <= 1920.max(w) && oh <= 1080.max(h));
            assert!(ow <= 1920 || w <= 1920);
        }
    }

    #[test]
    fn bitrate_scales_and_clamps() {
        assert_eq!(default_bitrate(1920, 1080, 30), 9_331_200);
        assert_eq!(default_bitrate(320, 240, 5), 1_000_000);
        assert_eq!(default_bitrate(7680, 4320, 60), 50_000_000);
    }

    #[test]
    fn gop_is_two_seconds() {
        assert_eq!(EncoderSettings::for_size(1280, 720, 30).gop_frames, 60);
    }
}
