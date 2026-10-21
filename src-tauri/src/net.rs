//! Local IPv4 address selection (shown to other devices in wan mode).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// Interface names containing these are treated as virtual/tunnel NICs and deprioritized.
const VIRTUAL_MARKERS: [&str; 16] = [
    "vmware",
    "virtualbox",
    "vbox",
    "vethernet",
    "hyper-v",
    "docker",
    "wsl",
    "loopback",
    "bluetooth",
    "tailscale",
    "zerotier",
    "clash",
    "tap",
    "tun",
    "npcap",
    "radmin",
];

fn is_virtual_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    VIRTUAL_MARKERS.iter().any(|m| n.contains(m))
}

/// Score a candidate address; `None` means it is unusable.
fn score_ipv4(name: &str, ip: Ipv4Addr, allow_virtual: bool) -> Option<u32> {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() {
        return None;
    }
    if ip.is_link_local() {
        return None;
    }
    let o = ip.octets();
    // 198.18.0.0/15 is the RFC 2544 benchmark range, common on proxy TUN NICs
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return None;
    }
    let virtual_nic = is_virtual_name(name);
    if virtual_nic && !allow_virtual {
        return None;
    }

    let private = o[0] == 10
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168);
    let mut score = 0;
    if private {
        score += 100;
    }
    if !virtual_nic {
        score += 50;
    }
    if o[3] != 1 {
        score += 5;
    }
    Some(score)
}

fn best(allowed_virtual: bool) -> Option<Ipv4Addr> {
    let ifaces = if_addrs::get_if_addrs().ok()?;
    let mut best: Option<(u32, Ipv4Addr)> = None;
    for iface in &ifaces {
        let IpAddr::V4(ip) = iface.addr.ip() else {
            continue;
        };
        if let Some(score) = score_ipv4(&iface.name, ip, allowed_virtual) {
            if best.map_or(true, |(b, _)| score > b) {
                best = Some((score, ip));
            }
        }
    }
    best.map(|(_, ip)| ip)
}

/// Best local IPv4 to show other devices; falls back to `127.0.0.1`.
pub fn local_ipv4() -> Ipv4Addr {
    best(false)
        .or_else(|| best(true))
        .or_else(default_route_ipv4)
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

/// Fallback: the default route's local address. UDP `connect` only sets socket
/// state, it sends no packet.
fn default_route_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match sock.local_addr().ok()? {
        SocketAddr::V4(v4) if !v4.ip().is_loopback() => Some(*v4.ip()),
        _ => None,
    }
}
