//! Which address this PC would use on the local network (shown in the agent's tray menu).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// IPv4 address the OS would use for outgoing traffic, or `None` when offline.
///
/// A connected UDP socket only consults the routing table; nothing is sent (the target is a
/// reserved documentation address, RFC 5737).
#[must_use]
pub fn primary_local_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket
        .connect(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 9)))
        .ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(IpAddr::V4(ip)),
        _ => None,
    }
}
