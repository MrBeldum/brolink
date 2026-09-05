//! Wake-on-LAN: turning a sleeping PC back on from the Mac.
//!
//! A magic packet is six `0xFF` bytes then the MAC sixteen times; the network
//! card matches it in hardware while the PC sleeps. Tailscale cannot carry
//! it, because the PC's Tailscale is asleep with the PC. What can:
//!
//! * the LAN broadcast, when the Mac is on the PC's own network;
//! * a unicast to the PC's LAN address, which reaches it through a Tailscale
//!   subnet router (or any other route into the LAN) as long as the card
//!   keeps answering ARP while asleep, which ARP offload makes it do.
//!
//! Both are sent every time; the extra datagrams cost nothing.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// Ports a magic packet is traditionally sent to.
pub const WOL_PORTS: [u16; 2] = [9, 7];
pub const MAGIC_LEN: usize = 6 + 16 * 6;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr([u8; 6]);

impl MacAddr {
    /// Accepts `aa:bb:cc:dd:ee:ff`, `AA-BB-CC-DD-EE-FF`, or `aabbccddeeff`.
    /// All-zero and all-ones are what virtual adapters report; neither can
    /// wake anything, so they are rejected too.
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

/// Everywhere worth sending the packet: the limited broadcast, the PC's /24
/// directed broadcast, and the PC itself, on both classic ports.
pub fn targets(lan_ip: Option<Ipv4Addr>) -> Vec<SocketAddr> {
    let mut ips = vec![Ipv4Addr::BROADCAST];
    if let Some(ip) = lan_ip {
        let o = ip.octets();
        ips.push(Ipv4Addr::new(o[0], o[1], o[2], 255));
        ips.push(ip);
    }
    ips.into_iter()
        .flat_map(|ip| WOL_PORTS.map(|p| SocketAddr::new(IpAddr::V4(ip), p)))
        .collect()
}

/// Fire the packet at every target. Returns how many sends the OS accepted;
/// a missing route is normal and only logged.
pub fn send(mac: MacAddr, lan_ip: Option<Ipv4Addr>) -> usize {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
        return 0;
    };
    let _ = sock.set_broadcast(true);
    let pkt = mac.magic_packet();
    let mut sent = 0;
    for t in targets(lan_ip) {
        match sock.send_to(&pkt, t) {
            Ok(_) => sent += 1,
            Err(e) => tracing::debug!("wake packet to {t}: {e}"),
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
    fn targets_cover_broadcasts_and_the_pc_itself() {
        let t = targets(Some("192.168.1.20".parse().unwrap()));
        let has = |s: &str| t.contains(&s.parse::<SocketAddr>().unwrap());
        assert!(has("255.255.255.255:9"));
        assert!(has("192.168.1.255:9"));
        assert!(has("192.168.1.20:9"));
        assert!(has("192.168.1.20:7"));
        assert_eq!(t.len(), 6);
        assert_eq!(targets(None).len(), 2);
    }

    #[test]
    fn sending_never_fails_loudly() {
        // Broadcast may or may not be routable on the test machine; either way
        // this returns rather than panicking.
        let _ = send(MacAddr([1, 2, 3, 4, 5, 6]), None);
    }
}
