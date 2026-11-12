//! # Capture -> encode probe
//!
//! ## Responsibility
//! Runs the real [`MediaPipeline`] for a fixed time and writes the Annex-B H.264 stream to
//! a file, logging encoder selection and pipeline statistics (used for measurements).
//!
//! ## Scope Boundaries
//! - Manual diagnostics only; not part of the shipped product.
//!
//! ## Dependencies
//! `desk-codec`, `desk-capture`, `desk-logger`.

use desk_capture::{list_monitors, MonitorSelector};
use desk_codec::{
    encoder_name_for_adapter, list_hardware_encoders, EncodedSink, MediaPipeline, PipelineConfig,
};
use desk_error::{DeskError, DeskResult};
use desk_logger::{init, LoggerConfig};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct FileSink {
    file: Mutex<std::fs::File>,
}

impl EncodedSink for FileSink {
    fn on_config(&self, width: u32, height: u32, fps: u32) {
        tracing::info!(width, height, fps, "stream config");
    }

    fn on_frame(&self, _ts_us: u64, _keyframe: bool, data: &[u8]) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(data);
        }
    }
}

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() -> DeskResult<()> {
    let _guard = init(&LoggerConfig::default())?;
    let secs: u64 = arg("--secs", 10);
    let viewers: usize = arg("--viewers", 1);
    let out: String = arg("--out", "probe.h264".to_string());
    let monitor_index: Option<usize> = std::env::args()
        .collect::<Vec<_>>()
        .iter()
        .position(|a| a == "--monitor")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|v| v.parse().ok());

    for m in list_monitors()? {
        tracing::info!(
            index = m.index,
            adapter = %m.adapter_name,
            vendor = format!("{:#06x}", m.vendor_id),
            mft = ?encoder_name_for_adapter(m.adapter_luid),
            "monitor"
        );
    }
    for name in list_hardware_encoders()? {
        tracing::info!(encoder = %name, "hardware H.264 MFT");
    }

    let config = PipelineConfig {
        monitor: monitor_index.map_or(MonitorSelector::Primary, MonitorSelector::Index),
        fps: arg("--fps", 30),
        max_width: arg("--max-width", 1920),
        max_height: arg("--max-height", 1080),
        bitrate_bps: std::env::args()
            .collect::<Vec<_>>()
            .iter()
            .position(|a| a == "--bitrate")
            .and_then(|i| std::env::args().nth(i + 1))
            .and_then(|v| v.parse().ok()),
        allow_software: std::env::args().any(|a| a == "--software"),
    };
    let file = std::fs::File::create(&out).map_err(|e| DeskError::Codec(e.to_string()))?;
    let sink = Arc::new(FileSink {
        file: Mutex::new(file),
    });
    let pipeline = MediaPipeline::new(config, sink);
    for _ in 0..viewers {
        pipeline.viewer_join()?;
    }
    for second in 1..=secs {
        std::thread::sleep(Duration::from_secs(1));
        let s = pipeline.stats();
        tracing::info!(
            second,
            frames = s.frames_out,
            keyframes = s.keyframes_out,
            unchanged = s.unchanged_ticks,
            missed = s.missed_ticks,
            restarts = s.restarts,
            bytes = s.bytes_out,
            encoder = %s.encoder_name,
            "stats"
        );
    }
    for _ in 0..viewers {
        pipeline.viewer_leave();
    }
    tracing::info!(viewers = pipeline.viewer_count(), "probe finished");
    Ok(())
}
