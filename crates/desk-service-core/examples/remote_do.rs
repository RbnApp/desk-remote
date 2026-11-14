//! Scripted viewer probe: connects to a paired host, runs a list of steps, and can save the
//! picture as an Annex-B keyframe for `ffmpeg`.
//!
//! Usage: remote_do --host IP --data-dir D --steps "shot:E:/a.h264;click:32768,32768;key:win;text:game bar;wait:800"
//! `D` must hold copies of `panel_identity.key` and `trusted_hosts.dat`.

use desk_crypto::{DeviceIdentity, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_protocol::{InputPayload, Message};
use desk_transport::{connect, PeerTarget, Session};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn scan(c: char) -> Option<u32> {
    Some(match c {
        'q' => 0x10, 'w' => 0x11, 'e' => 0x12, 'r' => 0x13, 't' => 0x14, 'y' => 0x15,
        'u' => 0x16, 'o' => 0x18, 'p' => 0x19, 'a' => 0x1E, 's' => 0x1F, 'd' => 0x20,
        'f' => 0x21, 'g' => 0x22, 'h' => 0x23, 'j' => 0x24, 'k' => 0x25, 'l' => 0x26,
        'z' => 0x2C, 'x' => 0x2D, 'c' => 0x2E, 'v' => 0x2F, 'b' => 0x30, 'n' => 0x31,
        'm' => 0x32, ' ' => 0x39,
        _ => return None,
    })
}

fn named(n: &str) -> Option<u32> {
    Some(match n {
        "win" => 0xE05B,
        "enter" => 0x1C,
        "esc" => 0x01,
        "tab" => 0x0F,
        "space" => 0x39,
        "alt" => 0x38,
        "f4" => 0x3E,
        "down" => 0xE050,
        "up" => 0xE048,
        _ => return None,
    })
}

fn key(s: &mut Session, code: u32) -> DeskResult<()> {
    for down in [true, false] {
        s.send(&Message::InputEvent(InputPayload::Key { scancode: code, down }))?;
        std::thread::sleep(Duration::from_millis(40));
    }
    Ok(())
}

fn drain(s: &mut Session, ms: u64) -> DeskResult<()> {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {
        let _ = s.recv_timeout(Duration::from_millis(10))?;
    }
    Ok(())
}

fn shot(s: &mut Session, path: &str) -> DeskResult<()> {
    s.send(&Message::RequestKeyframe)?;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(6) {
        if let Some(Message::VideoFrame { keyframe: true, data, .. }) =
            s.recv_timeout(Duration::from_millis(20))?
        {
            std::fs::write(path, &data)?;
            println!("shot {path} {} bytes", data.len());
            return Ok(());
        }
    }
    println!("shot FAILED (no keyframe)");
    Ok(())
}

fn main() -> DeskResult<()> {
    let (mut host, mut dir, mut steps) = (None, None, String::new());
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let v = it.next().ok_or_else(|| DeskError::Config(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--host" => host = Some(v),
            "--data-dir" => dir = Some(PathBuf::from(v)),
            "--steps" => steps = v,
            o => return Err(DeskError::Config(format!("unknown argument {o}"))),
        }
    }
    let host = host.ok_or_else(|| DeskError::Config("--host".into()))?;
    let dir = dir.ok_or_else(|| DeskError::Config("--data-dir".into()))?;
    let identity = DeviceIdentity::load_from_path(&dir.join("panel_identity.key"))?;
    let mut trust = TrustStore::open(&dir.join("trusted_hosts.dat"))?;
    let mut s = connect(PeerTarget::parse(&host)?)?;
    s.authenticate_as_client(&identity, &mut trust, None)?;
    s.send(&Message::ViewerJoin)?;
    drain(&mut s, 1500)?;
    for step in steps.split(';').filter(|x| !x.is_empty()) {
        let (cmd, arg) = step.split_once(':').unwrap_or((step, ""));
        match cmd {
            "shot" => shot(&mut s, arg)?,
            "wait" => drain(&mut s, arg.parse().unwrap_or(500))?,
            "move" | "click" | "dblclick" => {
                let (x, y) = arg.split_once(',').unwrap_or(("0", "0"));
                let (x, y): (u16, u16) = (x.parse().unwrap_or(0), y.parse().unwrap_or(0));
                s.send(&Message::InputEvent(InputPayload::Mouse { x, y, buttons: 0, wheel: 0 }))?;
                drain(&mut s, 150)?;
                if cmd != "move" {
                    for _ in 0..if cmd == "dblclick" { 2 } else { 1 } {
                        for b in [1u8, 0] {
                            s.send(&Message::InputEvent(InputPayload::Mouse { x, y, buttons: b, wheel: 0 }))?;
                            drain(&mut s, 60)?;
                        }
                    }
                }
            }
            "key" => match named(arg) {
                Some(c) => key(&mut s, c)?,
                None => println!("unknown key {arg}"),
            },
            "text" => {
                for c in arg.chars() {
                    match scan(c) {
                        Some(code) => key(&mut s, code)?,
                        None => println!("unmapped char {c:?}"),
                    }
                }
            }
            other => println!("unknown step {other}"),
        }
    }
    s.send(&Message::ViewerLeave)?;
    drain(&mut s, 300)?;
    Ok(())
}
