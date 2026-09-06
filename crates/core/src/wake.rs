//! Wake-on-LAN. A magic packet is six `0xFF` bytes then the MAC sixteen
//! times; the network card matches it in hardware while the PC sleeps.
//!
//! Tailscale cannot carry it (the sleeping PC's Tailscale is asleep too), so
//! the packet goes out on every network this machine is on, to the broadcast
//! and to the PC's own LAN address, and to the PC's public address for a
//! router that forwards UDP 9 to it.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

pub const WOL_PORTS: [u16; 2] = [9, 7];
pub const MAGIC_LEN: usize = 6 + 16 * 6;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr([u8; 6]);

impl MacAddr {
    /// Accepts `aa:bb:cc:dd:ee:ff`, `AA-BB-CC-DD-EE-FF`, or `aabbccddeeff`.
    /// All-zero and all-ones are what virtual adapters report; rejected.
    pub fn parse(s: &str) -> Option<Self> {
        let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if hex.len() != 12
            || s.chars()
                .any(|c| !(c.is_ascii_hexdigit() || ":-. ".contains(c)))
        {
            return None;
        }
        let mut out = [0u8; 6];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        if out == [0; 6] || out == [0xFF; 6] {
            return None;
        }
        Some(Self(out))
    }

    pub fn magic_packet(&self) -> [u8; MAGIC_LEN] {
        let mut pkt = [0xFFu8; MAGIC_LEN];
        for rep in 0..16 {
            pkt[6 + rep * 6..12 + rep * 6].copy_from_slice(&self.0);
        }
        pkt
    }
}

/// The MAC a well-formed magic packet is addressed to.
pub fn magic_target(pkt: &[u8]) -> Option<MacAddr> {
    let body = pkt.get(..MAGIC_LEN)?;
    if body[..6] != [0xFF; 6] {
        return None;
    }
    let mac: [u8; 6] = body[6..12].try_into().ok()?;
    (mac != [0xFF; 6] && body[6..].as_chunks::<6>().0.iter().all(|c| *c == mac))
        .then_some(MacAddr(mac))
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl fmt::Debug for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MacAddr({self})")
    }
}

/// Where to send from and to: one entry per local IPv4 interface (its
/// broadcast, the PC's subnet broadcast and the PC itself), plus the public
/// address through the default route.
pub fn plan(
    lan_ip: Option<Ipv4Addr>,
    public_ip: Option<Ipv4Addr>,
) -> Vec<(Ipv4Addr, Vec<Ipv4Addr>)> {
    let mut locals: Vec<(Ipv4Addr, Option<Ipv4Addr>)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() && !is_tailnet(a.ip) => {
                Some((a.ip, a.broadcast))
            }
            _ => None,
        })
        .collect();
    if locals.is_empty() {
        locals.push((Ipv4Addr::UNSPECIFIED, None));
    }
    let mut out = Vec::new();
    for (local, broadcast) in locals {
        let mut targets = vec![Ipv4Addr::BROADCAST];
        targets.extend(broadcast);
        if let Some(ip) = lan_ip {
            let o = ip.octets();
            targets.push(Ipv4Addr::new(o[0], o[1], o[2], 255));
            targets.push(ip);
        }
        targets.dedup();
        out.push((local, targets));
    }
    if let Some(p) = public_ip {
        out.push((Ipv4Addr::UNSPECIFIED, vec![p]));
    }
    out
}

fn is_tailnet(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

/// Fire the packet everywhere in [`plan`]. Returns how many sends the OS
/// accepted; unroutable targets are normal and only logged.
pub fn send(mac: MacAddr, lan_ip: Option<Ipv4Addr>, public_ip: Option<Ipv4Addr>) -> usize {
    let pkt = mac.magic_packet();
    let mut sent = 0;
    for (local, targets) in plan(lan_ip, public_ip) {
        let Ok(sock) = UdpSocket::bind(SocketAddr::new(IpAddr::V4(local), 0)) else {
            continue;
        };
        let _ = sock.set_broadcast(true);
        for ip in targets {
            for port in WOL_PORTS {
                match sock.send_to(&pkt, SocketAddr::new(IpAddr::V4(ip), port)) {
                    Ok(_) => sent += 1,
                    Err(e) => tracing::debug!("wake packet {local} -> {ip}:{port}: {e}"),
                }
            }
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_parses_common_spellings_and_rejects_junk() {
        let want = MacAddr([0xAA, 0xBB, 0xCC, 0x01, 0x02, 0x03]);
        for s in [
            "aa:bb:cc:01:02:03",
            "AA-BB-CC-01-02-03",
            "aabbcc010203",
            "aabb.cc01.0203",
        ] {
            assert_eq!(MacAddr::parse(s), Some(want), "{s}");
        }
        assert_eq!(want.to_string(), "aa:bb:cc:01:02:03");
        for s in [
            "",
            "aa:bb:cc",
            "zz:bb:cc:dd:ee:ff",
            "00:00:00:00:00:00",
            "ff:ff:ff:ff:ff:ff",
            "aa:bb:cc:dd:ee:ff:00",
        ] {
            assert!(MacAddr::parse(s).is_none(), "{s:?}");
        }
    }

    #[test]
    fn magic_packet_is_sync_stream_then_mac_sixteen_times() {
        let pkt = MacAddr([1, 2, 3, 4, 5, 6]).magic_packet();
        assert_eq!(pkt.len(), 102);
        assert_eq!(&pkt[..6], &[0xFF; 6]);
        for rep in 0..16 {
            assert_eq!(&pkt[6 + rep * 6..12 + rep * 6], &[1, 2, 3, 4, 5, 6]);
        }
    }

    #[test]
    fn magic_target_reads_back_the_mac_and_rejects_the_rest() {
        let mac = MacAddr([1, 2, 3, 4, 5, 6]);
        let pkt = mac.magic_packet();
        assert_eq!(magic_target(&pkt), Some(mac));
        // A SecureOn password after the MACs is allowed.
        let mut long = pkt.to_vec();
        long.extend_from_slice(&[9; 6]);
        assert_eq!(magic_target(&long), Some(mac));
        assert_eq!(magic_target(&pkt[..100]), None);
        let mut bad = pkt;
        bad[50] ^= 1;
        assert_eq!(magic_target(&bad), None);
        assert_eq!(magic_target(&[0xFF; MAGIC_LEN]), None);
    }

    #[test]
    fn plan_covers_broadcasts_the_pc_and_the_public_address() {
        let lan: Ipv4Addr = "192.168.1.20".parse().unwrap();
        let public: Ipv4Addr = "203.0.113.5".parse().unwrap();
        let p = plan(Some(lan), Some(public));
        assert!(!p.is_empty());
        let all: Vec<Ipv4Addr> = p.iter().flat_map(|(_, t)| t.clone()).collect();
        assert!(all.contains(&Ipv4Addr::BROADCAST));
        assert!(all.contains(&"192.168.1.255".parse().unwrap()));
        assert!(all.contains(&lan));
        assert!(all.contains(&public));
        assert!(plan(None, None).iter().all(|(_, t)| !t.is_empty()));
    }

    #[test]
    fn sending_never_fails_loudly() {
        let _ = send(MacAddr([1, 2, 3, 4, 5, 6]), None, None);
    }
}
