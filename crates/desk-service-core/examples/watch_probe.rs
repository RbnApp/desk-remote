//! Watch-only viewer probe: connects to a paired host, sends NO input, and prints per-second
//! frame, byte, cursor and ping statistics.
//!
//! Usage: watch_probe --host 100.x.y.z --data-dir D [--seconds 20]
//! `D` must hold a copy of `panel_identity.key` and `trusted_hosts.dat`.

use desk_crypto::{DeviceIdentity, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_protocol::{InputPayload, Message};
use desk_transport::{connect, PeerTarget};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn main() -> DeskResult<()> {
    let (mut host, mut dir, mut seconds) = (None, None, 20u64);
    let mut wiggle = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--wiggle" {
            wiggle = true;
            continue;
        }
        let value = it
            .next()
            .ok_or_else(|| DeskError::Config(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--host" => host = Some(value),
            "--data-dir" => dir = Some(PathBuf::from(value)),
            "--seconds" => seconds = value.parse().unwrap_or(20),
            other => return Err(DeskError::Config(format!("unknown argument {other}"))),
        }
    }
    let host = host.ok_or_else(|| DeskError::Config("--host required".into()))?;
    let dir = dir.ok_or_else(|| DeskError::Config("--data-dir required".into()))?;
    let identity = DeviceIdentity::load_from_path(&dir.join("panel_identity.key"))?;
    let mut trust = TrustStore::open(&dir.join("trusted_hosts.dat"))?;

    let started = Instant::now();
    let mut session = connect(PeerTarget::parse(&host)?)?;
    session.authenticate_as_client(&identity, &mut trust, None)?;
    println!("authenticated in {} ms", started.elapsed().as_millis());
    session.send(&Message::ViewerJoin)?;
    session.send(&Message::RequestKeyframe)?;

    let watch = Instant::now();
    let mut win = Instant::now();
    let mut last_ping = Instant::now();
    let (mut frames, mut keys, mut bytes, mut max_frame, mut curs) = (0u32, 0u32, 0usize, 0usize, 0u32);
    let mut rtts: Vec<u64> = Vec::new();
    let mut gap_max = Duration::ZERO;
    let mut last_move = Instant::now();
    let mut step = 0f64;
    let mut cur_gap_max = Duration::ZERO;
    let mut last_cur = Instant::now();
    let mut last_msg = Instant::now();
    while watch.elapsed() < Duration::from_secs(seconds) {
        if last_ping.elapsed() >= Duration::from_millis(500) {
            last_ping = Instant::now();
            session.send(&Message::Ping { ts: now_us() })?;
        }
        if wiggle && last_move.elapsed() >= Duration::from_millis(16) {
            last_move = Instant::now();
            step += 0.12;
            let x = (32768.0 + 4000.0 * step.cos()) as u16;
            let y = (32768.0 + 4000.0 * step.sin()) as u16;
            session.send(&Message::InputEvent(InputPayload::Mouse { x, y, buttons: 0, wheel: 0 }))?;
        }
        match session.recv_timeout(Duration::from_millis(5))? {
            Some(Message::VideoConfig { codec, width, height, fps }) => {
                println!("config {codec} {width}x{height} fps={fps}");
            }
            Some(Message::VideoFrame { keyframe, data, .. }) => {
                frames += 1;
                keys += u32::from(keyframe);
                bytes += data.len();
                max_frame = max_frame.max(data.len());
                gap_max = gap_max.max(last_msg.elapsed());
                last_msg = Instant::now();
            }
            Some(Message::CursorPos { .. }) => {
                curs += 1;
                cur_gap_max = cur_gap_max.max(last_cur.elapsed());
                last_cur = Instant::now();
            }
            Some(Message::Pong { ts }) => rtts.push(now_us().saturating_sub(ts) / 1000),
            _ => {}
        }
        if win.elapsed() >= Duration::from_secs(1) {
            win = Instant::now();
            println!(
                "t={:>2}s frames={frames:>3} key={keys} kbytes={:>5} maxframe_kb={:>4} cursor_msgs={curs:>3} rtt_ms={:?} maxgap_ms={} cur_gap_ms={}",
                watch.elapsed().as_secs(),
                bytes / 1024,
                max_frame / 1024,
                rtts,
                gap_max.as_millis(),
                cur_gap_max.as_millis()
            );
            cur_gap_max = Duration::ZERO;
            (frames, keys, bytes, max_frame, curs) = (0, 0, 0, 0, 0);
            rtts.clear();
            gap_max = Duration::ZERO;
        }
    }
    session.send(&Message::ViewerLeave)?;
    Ok(())
}
