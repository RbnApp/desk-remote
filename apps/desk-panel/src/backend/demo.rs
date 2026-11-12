//! Synthetic backend for tests: fake PCs, fake pairing codes and a looping H.264 sample.
//!
//! Compiled only with `--features demo`; it is never part of a shipped build.

use super::h264::{avc1_codec, nal_starts};
use super::{
    Backend, Candidate, EventSink, InputEvent, PairError, PcInfo, PcStatus, SessionEvent,
    ViewerSession,
};
use desk_config::PairedPc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SAMPLE: &[u8] = include_bytes!("../../demo/sample.h264");
const FPS: u32 = 30;
const FAILED_ATTEMPTS_AFTER_DROP: u32 = 2;

type FakePc = (
    &'static str,
    &'static str,
    PcStatus,
    u32,
    Option<&'static str>,
);

/// Fake PCs shown next to the ones paired by the user.
const FAKE_PCS: &[FakePc] = &[
    ("demo-1", "Office-Accounting-01", PcStatus::Online, 0, None),
    ("demo-2", "Warehouse-Till", PcStatus::Offline, 2880, None),
    ("demo-3", "Server-Room-NAS", PcStatus::Connecting, 1, None),
    (
        "demo-4",
        "Meeting-TV",
        PcStatus::Warning,
        3,
        Some("latency"),
    ),
    ("demo-5", "Management-Laptop", PcStatus::Online, 0, None),
    ("demo-6", "Workshop-CNC-PC", PcStatus::Offline, 185, None),
    ("demo-7", "Reception", PcStatus::Warning, 12, Some("agent")),
];

/// Backend that serves fake data.
pub struct DemoBackend {
    pair_counter: AtomicU32,
    fail_connects: AtomicU32,
    active_stop: Mutex<Option<Arc<AtomicBool>>>,
    active_sink: Mutex<Option<EventSink>>,
    fail_with: Mutex<Option<PairError>>,
}

impl DemoBackend {
    pub fn new() -> Self {
        Self {
            pair_counter: AtomicU32::new(0),
            fail_connects: AtomicU32::new(0),
            active_stop: Mutex::new(None),
            active_sink: Mutex::new(None),
            fail_with: Mutex::new(None),
        }
    }
}

impl Default for DemoBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for DemoBackend {
    fn list(&self, paired: &[PairedPc]) -> Vec<PcInfo> {
        let mut rows: Vec<PcInfo> = paired
            .iter()
            .map(|pc| PcInfo {
                id: pc.id.clone(),
                name: pc.name.clone(),
                status: PcStatus::Online,
                last_seen_min: 0,
                warn: None,
                latency_ms: Some(2),
            })
            .collect();
        rows.extend(FAKE_PCS.iter().map(|(id, name, status, min, warn)| PcInfo {
            id: (*id).to_string(),
            name: (*name).to_string(),
            status: *status,
            last_seen_min: *min,
            warn: warn.map(str::to_string),
            latency_ms: (*status == PcStatus::Online).then_some(2),
        }));
        rows
    }

    fn discover(&self, _paired: &[PairedPc]) -> Vec<Candidate> {
        Vec::new()
    }

    fn pair(&self, _target: &str, code: &str, name: Option<&str>) -> Result<PairedPc, PairError> {
        match code {
            "ABCD-2345" => {
                let n = self.pair_counter.fetch_add(1, Ordering::Relaxed) + 1;
                let fallback = format!("New-PC-{n}");
                Ok(PairedPc {
                    id: format!("paired-{n}"),
                    name: name
                        .filter(|s| !s.trim().is_empty())
                        .map_or(fallback, |s| s.trim().to_string()),
                    address: None,
                })
            }
            "EXPR-0000" => Err(PairError::Expired),
            "USED-1111" => Err(PairError::Used),
            "NETW-0000" => Err(PairError::Network),
            _ => Err(PairError::Invalid),
        }
    }

    fn connect(&self, pc: &PairedPc, sink: EventSink) -> Result<Box<dyn ViewerSession>, PairError> {
        let pc_id = pc.id.as_str();
        if let Some(error) = self.fail_with.lock().ok().and_then(|mut slot| slot.take()) {
            return Err(error);
        }
        if self.fail_connects.load(Ordering::Relaxed) > 0 {
            self.fail_connects.fetch_sub(1, Ordering::Relaxed);
            return Err(PairError::Network);
        }
        if let Some((_, _, status, _, _)) = FAKE_PCS.iter().find(|p| p.0 == pc_id) {
            if matches!(status, PcStatus::Offline | PcStatus::Connecting) {
                return Err(PairError::Network);
            }
        }
        let units = split_access_units(SAMPLE);
        let codec = codec_string(&units).ok_or(PairError::Network)?;
        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut slot) = self.active_stop.lock() {
            if let Some(previous) = slot.replace(Arc::clone(&stop)) {
                previous.store(true, Ordering::Relaxed);
            }
        }
        if let Ok(mut slot) = self.active_sink.lock() {
            *slot = Some(Arc::clone(&sink));
        }
        let thread_stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("demo-session".to_string())
            .spawn(move || stream(&units, &codec, &sink, &thread_stop))
            .map_err(|_| PairError::Network)?;
        Ok(Box::new(DemoSession { stop }))
    }

    fn is_demo(&self) -> bool {
        true
    }

    fn simulate_notice(&self, kind: &str) {
        let kind = match kind {
            "disconnected_by_host" => "disconnected_by_host",
            "view_only_on" => "view_only_on",
            _ => "view_only_off",
        };
        if let Some(sink) = self.active_sink.lock().ok().and_then(|slot| slot.clone()) {
            sink(SessionEvent::Notice { kind });
            if kind == "disconnected_by_host" {
                // The host closes the link right after announcing it.
                if let Ok(slot) = self.active_stop.lock() {
                    if let Some(stop) = slot.as_ref() {
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    fn simulate_connect_failure(&self, code: &str) {
        let error = match code {
            "denied" => PairError::Denied,
            "no_answer" => PairError::NoAnswer,
            _ => PairError::Network,
        };
        if let Ok(mut slot) = self.fail_with.lock() {
            *slot = Some(error);
        }
    }

    fn simulate_drop(&self) {
        self.fail_connects
            .store(FAILED_ATTEMPTS_AFTER_DROP, Ordering::Relaxed);
        if let Ok(slot) = self.active_stop.lock() {
            if let Some(stop) = slot.as_ref() {
                stop.store(true, Ordering::Relaxed);
            }
        }
    }
}

struct DemoSession {
    stop: Arc<AtomicBool>,
}

impl ViewerSession for DemoSession {
    fn send_input(&self, event: &InputEvent) {
        match event {
            InputEvent::MouseMove { x, y } => tracing::debug!(x, y, "demo: mouse move"),
            InputEvent::MouseButton {
                button,
                pressed,
                x,
                y,
            } => tracing::debug!(button, pressed, x, y, "demo: mouse button"),
            InputEvent::Wheel { dy } => tracing::debug!(dy, "demo: wheel"),
            InputEvent::Key { code, pressed } => tracing::debug!(code, pressed, "demo: key"),
        }
    }

    fn request_keyframe(&self) {
        tracing::debug!("demo session keyframe request (the sample loops on keyframes anyway)");
    }

    fn close(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One Annex-B access unit.
struct AccessUnit {
    keyframe: bool,
    data: Vec<u8>,
}

fn stream(units: &[AccessUnit], codec: &str, sink: &EventSink, stop: &AtomicBool) {
    sink(SessionEvent::Config {
        codec: codec.to_string(),
        fps: FPS,
        width: None,
        height: None,
    });
    let frame_time = Duration::from_micros(1_000_000 / u64::from(FPS));
    let mut next = Instant::now();
    let mut ts_us: u64 = 0;
    let mut last_rtt = Instant::now();
    'outer: loop {
        for unit in units {
            if stop.load(Ordering::Relaxed) {
                break 'outer;
            }
            sink(SessionEvent::Frame {
                ts_us,
                keyframe: unit.keyframe,
                data: unit.data.clone(),
            });
            ts_us += frame_time.as_micros() as u64;
            if last_rtt.elapsed() >= Duration::from_secs(1) {
                // A local sample stream has no network hop.
                sink(SessionEvent::Rtt { ms: 0 });
                last_rtt = Instant::now();
            }
            next += frame_time;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            }
        }
    }
    sink(SessionEvent::Closed);
}

/// Groups NAL units into access units: parameter sets and SEI travel with the next slice.
fn split_access_units(stream: &[u8]) -> Vec<AccessUnit> {
    let starts = nal_starts(stream);
    let mut units = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    for (idx, &start) in starts.iter().enumerate() {
        let end = starts
            .get(idx + 1)
            .map_or(stream.len(), |next| next.saturating_sub(3));
        let nal = &stream[start..end];
        let Some(&header) = nal.first() else { continue };
        pending.extend_from_slice(&[0, 0, 0, 1]);
        pending.extend_from_slice(nal);
        let nal_type = header & 0x1f;
        if nal_type == 1 || nal_type == 5 {
            units.push(AccessUnit {
                keyframe: nal_type == 5,
                data: std::mem::take(&mut pending),
            });
        }
    }
    units
}

/// `avc1.PPCCLL` of the first access unit.
fn codec_string(units: &[AccessUnit]) -> Option<String> {
    avc1_codec(&units.first()?.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_splits_into_keyframe_led_units() {
        let units = split_access_units(SAMPLE);
        assert!(units.len() > 30);
        assert!(units[0].keyframe);
        assert!(codec_string(&units).is_some_and(|c| c.starts_with("avc1.")));
    }

    #[test]
    fn pairing_codes_map_to_outcomes() {
        let backend = DemoBackend::new();
        assert!(backend.pair("", "ABCD-2345", Some("Desk")).is_ok());
        assert_eq!(
            backend.pair("", "EXPR-0000", None).err(),
            Some(PairError::Expired)
        );
        assert_eq!(
            backend.pair("", "ZZZZ-9999", None).err(),
            Some(PairError::Invalid)
        );
    }
}
