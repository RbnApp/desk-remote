//! End-to-end viewer: pairs (or reconnects), watches for N seconds, moves the cursor,
//! leaves, and logs frame statistics. Uses only `desk-transport` for networking.
//!
//! Usage: e2e_client --port P --data-dir D [--code ABCD-2345] [--seconds 10]

use desk_crypto::{DeviceIdentity, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_logger::{LogLevel, LoggerConfig};
use desk_protocol::{InputPayload, Message};
use desk_transport::{connect, PeerTarget};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetProcessDPIAware, SM_CXSCREEN, SM_CYSCREEN,
};

struct Args {
    port: u16,
    data_dir: PathBuf,
    code: Option<String>,
    seconds: u64,
}

fn parse() -> DeskResult<Args> {
    let (mut port, mut data_dir, mut code, mut seconds) = (None, None, None, 10u64);
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .ok_or_else(|| DeskError::Config(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--port" => port = value.parse().ok(),
            "--data-dir" => data_dir = Some(PathBuf::from(value)),
            "--code" => code = Some(value),
            "--seconds" => seconds = value.parse().unwrap_or(10),
            other => return Err(DeskError::Config(format!("unknown argument {other}"))),
        }
    }
    Ok(Args {
        port: port.ok_or_else(|| DeskError::Config("--port required".to_string()))?,
        data_dir: data_dir.ok_or_else(|| DeskError::Config("--data-dir required".to_string()))?,
        code,
        seconds,
    })
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: valid out-parameter.
    let _ = unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

fn main() -> DeskResult<()> {
    let args = parse()?;
    let _guard = desk_logger::init(&LoggerConfig {
        level: LogLevel::Info,
        log_dir: None,
        file_name_prefix: "e2e-client".to_string(),
        console_output: true,
        json_format: false,
    })?;
    // SAFETY: process-wide DPI mode so cursor coordinates are physical pixels.
    let _ = unsafe { SetProcessDPIAware() };
    std::fs::create_dir_all(&args.data_dir)?;
    let identity = DeviceIdentity::load_or_create(&args.data_dir.join("client.key"))?;
    let mut trust = TrustStore::open(&args.data_dir.join("client-trust.json"))?;

    let started = Instant::now();
    let mut session = connect(PeerTarget::loopback(args.port))?;
    session.authenticate_as_client(&identity, &mut trust, args.code.as_deref())?;
    tracing::info!(
        paired_with = %session.peer_device_id().unwrap_or_default(),
        handshake_ms = started.elapsed().as_millis() as u64,
        "authenticated"
    );

    session.send(&Message::ViewerJoin)?;
    let (mut configs, mut frames, mut keyframes, mut bytes, mut statuses) =
        (0u32, 0u64, 0u64, 0u64, 0u32);
    let mut first_frame_ms = None;
    let mut cursor_checked = false;
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let watch = Instant::now();
    while watch.elapsed() < Duration::from_secs(args.seconds) {
        if !cursor_checked && watch.elapsed() > Duration::from_secs(2) {
            cursor_checked = true;
            // Normalized targets (0..65535) and the pixel they must land on.
            for (nx, ny) in [(16384u16, 16384u16), (32768, 32768), (49152, 8192)] {
                session.send(&Message::InputEvent(InputPayload::Mouse {
                    x: nx,
                    y: ny,
                    buttons: 0,
                    wheel: 0,
                }))?;
                std::thread::sleep(Duration::from_millis(250));
                let (cx, cy) = cursor();
                let ex = (i64::from(nx) * i64::from(w) / 65536) as i32;
                let ey = (i64::from(ny) * i64::from(h) / 65536) as i32;
                let ok = (cx - ex).abs() <= 3 && (cy - ey).abs() <= 3;
                tracing::info!(
                    cursor_x = cx,
                    cursor_y = cy,
                    expected_x = ex,
                    expected_y = ey,
                    ok,
                    "cursor check"
                );
            }
        }
        match session.recv_timeout(Duration::from_millis(50))? {
            Some(Message::VideoConfig {
                width, height, fps, ..
            }) => {
                configs += 1;
                tracing::info!(width, height, fps, "video config");
            }
            Some(Message::VideoFrame { keyframe, data, .. }) => {
                frames += 1;
                keyframes += u64::from(keyframe);
                bytes += data.len() as u64;
                first_frame_ms.get_or_insert(watch.elapsed().as_millis() as u64);
            }
            Some(Message::Status { online, .. }) => {
                statuses += 1;
                tracing::info!(online, "status");
            }
            _ => {}
        }
    }
    session.send(&Message::ViewerLeave)?;
    tracing::info!(
        seconds = args.seconds,
        configs,
        frames,
        keyframes,
        bytes,
        statuses,
        first_frame_ms = first_frame_ms.unwrap_or(0),
        fps = frames as f64 / args.seconds as f64,
        "RESULT"
    );
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}
