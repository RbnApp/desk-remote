//! Paired-panel probe for the security checks (DAPP-104): connects like a panel without
//! touching the cursor, optionally sends one file, and optionally removes its own pairing.
//!
//! Usage: peer_probe --port P --data-dir D [--code ABCD-2345] [--file PATH] [--unpair]
//!        peer_probe --data-dir D --pretrust AGENT_DIR   (agent must be stopped)

use desk_crypto::{DeviceIdentity, TrustStore};
use desk_error::{DeskError, DeskResult};
use desk_protocol::Message;
use desk_transfer::{new_transfer_id, prepare, send_prepared};
use desk_transport::{connect, PeerTarget};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn main() -> DeskResult<()> {
    let (mut port, mut dir, mut code, mut file, mut unpair) = (0u16, None, None, None, false);
    let mut pretrust: Option<PathBuf> = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--unpair" => unpair = true,
            "--port" => port = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--data-dir" => dir = it.next().map(PathBuf::from),
            "--pretrust" => pretrust = it.next().map(PathBuf::from),
            "--code" => code = it.next(),
            "--file" => file = it.next().map(PathBuf::from),
            other => return Err(DeskError::Config(format!("unknown argument {other}"))),
        }
    }
    let dir = dir.ok_or_else(|| DeskError::Config("--data-dir is required".to_string()))?;
    std::fs::create_dir_all(&dir)?;
    let identity = DeviceIdentity::load_or_create(&dir.join("probe.key"))?;
    let mut trust = TrustStore::open(&dir.join("probe-trust.json"))?;
    if let Some(agent) = pretrust {
        let system = agent.join("system");
        TrustStore::open(&system.join("trusted_peers.dat"))?.add(identity.public_key())?;
        let host = DeviceIdentity::load_from_path(&system.join("identity.key"))?;
        trust.add(host.public_key())?;
        return Ok(());
    }
    let mut session = connect(PeerTarget::loopback(port))?;
    session.authenticate_as_client(&identity, &mut trust, code.as_deref())?;
    if let Some(path) = file {
        let never = AtomicBool::new(false);
        let prepared = prepare(&path, 10, new_transfer_id(), &never)
            .map_err(|e| DeskError::Transport(format!("{e:?}")))?;
        let mut sender = connect(PeerTarget::loopback(port))?;
        sender.authenticate_as_client(&identity, &mut trust, None)?;
        send_prepared(&mut sender, &prepared, &never, |_| {})
            .map_err(|e| DeskError::Transport(format!("{e:?}")))?;
    } else if unpair {
        session.send(&Message::Unpair)?;
    } else {
        // Stay a moment as a plain viewer connection.
        std::thread::sleep(Duration::from_secs(2));
    }
    Ok(())
}
