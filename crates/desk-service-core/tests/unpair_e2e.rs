//! End-to-end "remove pairing" (DAPP-104): a paired panel asks the host to forget it and
//! cannot connect again without a new pairing code.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use desk_crypto::{DeviceIdentity, PairingManager, TrustStore};
use desk_error::DeskResult;
use desk_paths::DeskPaths;
use desk_protocol::Message;
use desk_service_core::{HostOptions, ServiceHost};
use desk_transport::{connect, PeerTarget};
use std::time::Duration;

#[test]
fn a_panel_that_removed_its_pairing_cannot_reconnect() -> DeskResult<()> {
    let work = tempfile::tempdir().map_err(desk_error::DeskError::Io)?;
    let paths = DeskPaths::custom(work.path().join("system"), work.path().join("user"));
    paths.mark_panel_pc()?;
    let mut opts = HostOptions::for_panel_in_app(paths.clone(), Some(0), true);
    opts.custom_identity = Some(DeviceIdentity::generate()?);
    opts.custom_trust = Some(TrustStore::open(&paths.trusted_hosts_path())?);
    let host = ServiceHost::start(opts)?;
    let port = host.port()?;

    let panel = DeviceIdentity::generate()?;
    let mut trust = TrustStore::open(&work.path().join("panel-trust.json"))?;
    let code = host
        .hub()
        .with_pairing(PairingManager::create_pairing_code)?;

    // First connection pairs with the one-time code.
    let mut first = connect(PeerTarget::loopback(port))?;
    first.authenticate_as_client(&panel, &mut trust, Some(&code))?;
    drop(first);
    // A later connection needs no code: the pairing is permanent.
    let mut again = connect(PeerTarget::loopback(port))?;
    again.authenticate_as_client(&panel, &mut trust, None)?;

    // The panel asks the host to forget it.
    again.send(&Message::Unpair)?;
    drop(again);
    let gone = (0..50).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        !host
            .hub()
            .with_pairing(|p| p.trust().is_device_trusted(&panel.device_id()))
    });
    assert!(gone, "the host must forget the panel");

    // Same panel, same keys, no code: refused.
    let mut denied = connect(PeerTarget::loopback(port))?;
    let outcome = denied.authenticate_as_client(&panel, &mut trust, None);
    assert!(outcome.is_err(), "an unpaired panel must not reconnect");
    Ok(())
}
