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
    GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
};

struct Args {
    port: u16,
    data_dir: PathBuf,
    code: Option<String>,
    seconds: u64,
    /// Print the 9-point click accuracy table instead of the quick cursor check.
    probe: bool,
    /// Agent data dir whose trust store should accept this client (agent must be stopped).
    pretrust: Option<PathBuf>,
    /// Left double-click at this physical screen pixel (`X,Y`) after the stream started.
    dblclick: Option<(i32, i32)>,
}

fn parse() -> DeskResult<Args> {
    let (mut port, mut data_dir, mut code, mut seconds) = (None, None, None, 10u64);
    let mut it = std::env::args().skip(1);
    let mut probe = false;
    let mut pretrust = None;
    let mut dblclick = None;
    while let Some(flag) = it.next() {
        if flag == "--probe" {
            probe = true;
            continue;
        }
        let value = it
            .next()
            .ok_or_else(|| DeskError::Config(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--port" => port = value.parse().ok(),
            "--data-dir" => data_dir = Some(PathBuf::from(value)),
            "--code" => code = Some(value),
            "--dblclick" => {
                dblclick = value
                    .split_once(',')
                    .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)));
            }
            "--pretrust" => pretrust = Some(PathBuf::from(value)),
            "--seconds" => seconds = value.parse().unwrap_or(10),
            other => return Err(DeskError::Config(format!("unknown argument {other}"))),
        }
    }
    Ok(Args {
        port: port
            .or(pretrust.as_ref().map(|_| 0))
            .ok_or_else(|| DeskError::Config("--port required".to_string()))?,
        data_dir: data_dir.ok_or_else(|| DeskError::Config("--data-dir required".to_string()))?,
        code,
        seconds,
        probe,
        pretrust,
        dblclick,
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
    // Physical-pixel cursor coordinates, same mode as the agent helper.
    desk_capture::ensure_per_monitor_dpi_aware();
    std::fs::create_dir_all(&args.data_dir)?;
    let identity = DeviceIdentity::load_or_create(&args.data_dir.join("client.key"))?;
    let mut trust = TrustStore::open(&args.data_dir.join("client-trust.json"))?;
    if let Some(agent_dir) = &args.pretrust {
        let mut agent_trust =
            TrustStore::open(&agent_dir.join("system").join("trusted_peers.dat"))?;
        agent_trust.add(identity.public_key())?;
        let host = DeviceIdentity::load_from_path(&agent_dir.join("system").join("identity.key"))?;
        trust.add(host.public_key())?;
        tracing::info!("client key added to the agent trust store");
        return Ok(());
    }

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
    let (mut shapes, mut positions, mut last_pos) = (0u32, 0u32, None);
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let watch = Instant::now();
    while watch.elapsed() < Duration::from_secs(args.seconds) {
        if args.probe && !cursor_checked && watch.elapsed() > Duration::from_secs(2) {
            cursor_checked = true;
            probe_grid(&mut session)?;
        }
        if let (Some((px, py)), false) = (args.dblclick, cursor_checked) {
            if watch.elapsed() > Duration::from_secs(2) {
                cursor_checked = true;
                double_click(&mut session, px, py)?;
            }
        }
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
            Some(Message::CursorShape {
                id,
                width,
                height,
                hot_x,
                hot_y,
                rgba,
            }) => {
                shapes += 1;
                let opaque = rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
                tracing::info!(id, width, height, hot_x, hot_y, opaque, "cursor shape");
            }
            Some(Message::CursorPos {
                x,
                y,
                shape_id,
                visible,
            }) => {
                positions += 1;
                last_pos = Some((x, y, shape_id, visible));
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
        shapes,
        positions,
        ?last_pos,
        first_frame_ms = first_frame_ms.unwrap_or(0),
        fps = frames as f64 / args.seconds as f64,
        "RESULT"
    );
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// Sends the 3x3 grid of normalised points and prints where the real cursor landed against the
/// pixel the wire value must map to (`v * (len - 1) / 65535`, the same rule `desk-input` uses).
fn probe_grid(session: &mut desk_transport::Session) -> DeskResult<()> {
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let mut worst = 0;
    for ny in [0u16, 32767, 65535] {
        for nx in [0u16, 32767, 65535] {
            let ex = (i64::from(nx) * i64::from(w - 1) / 65535) as i32;
            let ey = (i64::from(ny) * i64::from(h - 1) / 65535) as i32;
            // A person may be moving the physical mouse of this very PC while we measure; a
            // disturbed sample is repeated (best of 5) so only the injection error is reported.
            let (mut err, mut actual, mut remote_seen) = (i32::MAX, (0, 0), None);
            for _attempt in 0..5 {
                session.send(&Message::InputEvent(InputPayload::Mouse {
                    x: nx,
                    y: ny,
                    buttons: 0,
                    wheel: 0,
                }))?;
                // Keep draining the stream so the reported remote cursor is the freshest one.
                let mut remote = None;
                let settle = Instant::now();
                while settle.elapsed() < Duration::from_millis(300) {
                    if let Some(Message::CursorPos { x, y, .. }) =
                        session.recv_timeout(Duration::from_millis(10))?
                    {
                        remote = Some((x, y));
                    }
                }
                let (cx, cy) = cursor();
                let this_err = (cx - ex).abs().max((cy - ey).abs());
                if this_err < err {
                    (err, actual, remote_seen) = (this_err, (cx, cy), remote);
                }
                if err <= 1 {
                    break;
                }
            }
            worst = worst.max(err);
            tracing::info!(nx, ny, expected = ?(ex, ey), actual = ?actual, remote_cursor_msg = ?remote_seen, err_px = err, "GRID");
        }
    }
    tracing::info!(screen = ?(w, h), worst_err_px = worst, "GRID_WORST");
    Ok(())
}

/// Left double-click at a physical pixel, in the exact payload order the panel produces
/// (move, press, release, press, release) with a human-like gap.
fn double_click(session: &mut desk_transport::Session, px: i32, py: i32) -> DeskResult<()> {
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let to_wire =
        |p: i32, len: i32| (i64::from(p) * 65535 / i64::from(len - 1)).clamp(0, 65535) as u16;
    let (x, y) = (to_wire(px, w), to_wire(py, h));
    for buttons in [0u8, 1, 0, 1, 0] {
        session.send(&Message::InputEvent(InputPayload::Mouse {
            x,
            y,
            buttons,
            wheel: 0,
        }))?;
        std::thread::sleep(Duration::from_millis(60));
    }
    tracing::info!(px, py, x, y, "DBLCLICK sent");
    Ok(())
}
