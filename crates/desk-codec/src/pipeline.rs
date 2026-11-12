//! # Capture-to-encoder media pipeline
//!
//! ## Responsibility
//! Runs the host media loop: capture at a fixed pace, encode on the capture GPU and push
//! H.264 access units into an [`EncodedSink`]. The loop exists ONLY while at least one
//! viewer is joined; when the last viewer leaves, capture, encoder and GPU objects are
//! released and the working set is trimmed, so an unwatched host does zero work.
//!
//! ## Scope Boundaries
//! - No networking: the transport layer implements [`EncodedSink`] and calls
//!   `viewer_join` / `viewer_leave` / `request_keyframe`.
//! - Sink calls carry plain values (`width, height, fps` and `ts_us, keyframe, data`)
//!   that map 1:1 onto the protocol's `VideoConfig` and `VideoFrame`.
//!
//! ## Dependencies
//! `desk-capture`, `desk-error`, `encoder`, `settings`.

use crate::encoder::{create_encoder, VideoEncoder};
use crate::settings::{fit_dimensions, EncoderSettings};
use desk_capture::{Capturer, FramePacer, Grab, MonitorSelector};
use desk_error::{DeskError, DeskResult};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::Threading::GetCurrentProcess;

/// First delay before rebuilding a failed session; doubles up to [`MAX_RESTART_DELAY`].
const MIN_RESTART_DELAY: Duration = Duration::from_millis(500);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(5);

/// Receives the encoded stream. Implemented by the transport layer.
pub trait EncodedSink: Send + Sync {
    /// Called before the first frame of every (re)started session.
    fn on_config(&self, width: u32, height: u32, fps: u32);
    /// One H.264 access unit in Annex-B form.
    fn on_frame(&self, ts_us: u64, keyframe: bool, data: &[u8]);
}

/// Pipeline parameters.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub monitor: MonitorSelector,
    pub fps: u32,
    pub max_width: u32,
    pub max_height: u32,
    /// `None` derives the bitrate from the output size.
    pub bitrate_bps: Option<u32>,
    pub allow_software: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            monitor: MonitorSelector::Primary,
            fps: 30,
            max_width: 1920,
            max_height: 1080,
            bitrate_bps: None,
            allow_software: false,
        }
    }
}

/// Counters since the pipeline object was created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineStats {
    /// Pacing ticks processed.
    pub ticks: u64,
    /// Ticks where the screen had not changed.
    pub unchanged_ticks: u64,
    pub frames_out: u64,
    pub keyframes_out: u64,
    pub bytes_out: u64,
    /// Ticks dropped because encoding fell more than a frame behind.
    pub missed_ticks: u64,
    /// Sessions rebuilt after an error or a display mode change.
    pub restarts: u64,
    pub encoder_name: String,
}

#[derive(Default)]
struct Counters {
    ticks: AtomicU64,
    unchanged: AtomicU64,
    frames: AtomicU64,
    keyframes: AtomicU64,
    bytes: AtomicU64,
    missed: AtomicU64,
    restarts: AtomicU64,
}

struct Shared {
    config: PipelineConfig,
    sink: Arc<dyn EncodedSink>,
    stop: AtomicBool,
    want_keyframe: AtomicBool,
    counters: Counters,
    encoder_name: Mutex<String>,
}

#[derive(Default)]
struct Control {
    viewers: usize,
    worker: Option<JoinHandle<()>>,
}

/// Viewer-gated media pipeline.
pub struct MediaPipeline {
    shared: Arc<Shared>,
    control: Mutex<Control>,
}

impl MediaPipeline {
    /// Creates an idle pipeline: no thread, no capture, no GPU objects.
    pub fn new(config: PipelineConfig, sink: Arc<dyn EncodedSink>) -> Self {
        Self {
            shared: Arc::new(Shared {
                config,
                sink,
                stop: AtomicBool::new(false),
                want_keyframe: AtomicBool::new(true),
                counters: Counters::default(),
                encoder_name: Mutex::new(String::new()),
            }),
            control: Mutex::new(Control::default()),
        }
    }

    fn control(&self) -> MutexGuard<'_, Control> {
        self.control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A viewer connected. The first one starts capture and encoding.
    pub fn viewer_join(&self) -> DeskResult<()> {
        let mut control = self.control();
        control.viewers += 1;
        self.shared.want_keyframe.store(true, Ordering::SeqCst);
        if control.worker.is_none() {
            self.shared.stop.store(false, Ordering::SeqCst);
            let shared = Arc::clone(&self.shared);
            let handle = std::thread::Builder::new()
                .name("desk-media".into())
                .spawn(move || worker_main(shared))
                .map_err(|e| DeskError::Codec(format!("cannot start media thread: {e}")))?;
            control.worker = Some(handle);
            tracing::info!("first viewer joined, media pipeline started");
        }
        Ok(())
    }

    /// A viewer disconnected. The last one stops the pipeline and frees its memory.
    pub fn viewer_leave(&self) {
        let mut control = self.control();
        control.viewers = control.viewers.saturating_sub(1);
        if control.viewers == 0 {
            if let Some(handle) = control.worker.take() {
                self.shared.stop.store(true, Ordering::SeqCst);
                let _ = handle.join();
                trim_working_set();
                tracing::info!("last viewer left, media pipeline stopped");
            }
        }
    }

    /// Number of joined viewers.
    pub fn viewer_count(&self) -> usize {
        self.control().viewers
    }

    /// Asks for an IDR frame (new viewer, packet loss). Re-encodes the last image if the
    /// screen is static.
    pub fn request_keyframe(&self) {
        self.shared.want_keyframe.store(true, Ordering::SeqCst);
    }

    pub fn stats(&self) -> PipelineStats {
        let c = &self.shared.counters;
        PipelineStats {
            ticks: c.ticks.load(Ordering::Relaxed),
            unchanged_ticks: c.unchanged.load(Ordering::Relaxed),
            frames_out: c.frames.load(Ordering::Relaxed),
            keyframes_out: c.keyframes.load(Ordering::Relaxed),
            bytes_out: c.bytes.load(Ordering::Relaxed),
            missed_ticks: c.missed.load(Ordering::Relaxed),
            restarts: c.restarts.load(Ordering::Relaxed),
            encoder_name: self
                .shared
                .encoder_name
                .lock()
                .map(|n| n.clone())
                .unwrap_or_default(),
        }
    }
}

impl Drop for MediaPipeline {
    fn drop(&mut self) {
        let worker = self.control().worker.take();
        if let Some(handle) = worker {
            self.shared.stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
        }
    }
}

/// Why a session ended without error.
enum SessionEnd {
    Stopped,
    /// Display mode changed: rebuild immediately with the new size.
    ModeChanged,
}

fn worker_main(shared: Arc<Shared>) {
    let mut delay = MIN_RESTART_DELAY;
    while !shared.stop.load(Ordering::SeqCst) {
        match run_session(&shared) {
            Ok(SessionEnd::Stopped) => break,
            Ok(SessionEnd::ModeChanged) => {
                shared.counters.restarts.fetch_add(1, Ordering::Relaxed);
                tracing::info!("display mode changed, restarting session");
            }
            Err(e) => {
                shared.counters.restarts.fetch_add(1, Ordering::Relaxed);
                tracing::error!(code = e.error_code(), error = %e, "media session failed");
                sleep_unless_stopped(&shared, delay);
                delay = (delay * 2).min(MAX_RESTART_DELAY);
            }
        }
    }
}

fn sleep_unless_stopped(shared: &Shared, total: Duration) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline && !shared.stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_session(shared: &Shared) -> DeskResult<SessionEnd> {
    let config = &shared.config;
    let mut capturer = Capturer::new(config.monitor)?;
    let source = capturer.size();
    let (width, height) = fit_dimensions(source.0, source.1, config.max_width, config.max_height);
    let mut settings = EncoderSettings::for_size(width, height, config.fps);
    if let Some(bitrate) = config.bitrate_bps {
        settings.bitrate_bps = bitrate;
    }
    let monitor = capturer.monitor().clone();
    let mut encoder: Box<dyn VideoEncoder> = create_encoder(
        capturer.device(),
        monitor.adapter_luid,
        monitor.vendor_id,
        settings,
        config.allow_software,
    )?;
    if let Ok(mut name) = shared.encoder_name.lock() {
        *name = encoder.name().to_string();
    }
    tracing::info!(
        gpu = %monitor.adapter_name,
        encoder = encoder.name(),
        source_w = source.0,
        source_h = source.1,
        width,
        height,
        fps = config.fps,
        bitrate_bps = settings.bitrate_bps,
        "media session started"
    );
    shared.sink.on_config(width, height, config.fps);

    let mut pacer = FramePacer::new(config.fps);
    let started = Instant::now();
    let mut reported_missed = 0u64;
    while !shared.stop.load(Ordering::SeqCst) {
        pacer.wait_next();
        shared.counters.ticks.fetch_add(1, Ordering::Relaxed);
        let missed = pacer.missed_ticks();
        if missed > reported_missed {
            shared
                .counters
                .missed
                .fetch_add(missed - reported_missed, Ordering::Relaxed);
            reported_missed = missed;
        }

        let grab = capturer.acquire(0)?;
        if capturer.size() != source {
            return Ok(SessionEnd::ModeChanged);
        }
        let want_key = shared.want_keyframe.load(Ordering::SeqCst);
        let must_encode = match grab {
            Grab::Updated => true,
            // Static screen but a keyframe is wanted: re-encode the last image.
            Grab::Unchanged | Grab::Lost => want_key && capturer.frame().is_some(),
        };
        if grab == Grab::Unchanged {
            shared.counters.unchanged.fetch_add(1, Ordering::Relaxed);
        }
        if !must_encode {
            continue;
        }
        let Some(frame) = capturer.frame() else {
            continue;
        };
        let ts_us = started.elapsed().as_micros() as u64;
        let units = encoder.encode(frame, ts_us, want_key)?;
        if want_key {
            shared.want_keyframe.store(false, Ordering::SeqCst);
        }
        for unit in units {
            let c = &shared.counters;
            c.frames.fetch_add(1, Ordering::Relaxed);
            c.bytes.fetch_add(unit.data.len() as u64, Ordering::Relaxed);
            if unit.keyframe {
                c.keyframes.fetch_add(1, Ordering::Relaxed);
            }
            shared.sink.on_frame(unit.ts_us, unit.keyframe, &unit.data);
        }
    }
    Ok(SessionEnd::Stopped)
}

/// Returns freed pages to the OS so an idle host keeps a small resident set.
fn trim_working_set() {
    // SAFETY: operates on the current process handle only.
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}
