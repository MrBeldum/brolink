//! Wake-on-LAN: turning a sleeping PC back on from wherever the client is.
//!
//! A "magic packet" is six `0xFF` bytes followed by the target's MAC address
//! sixteen times. The network card matches that pattern in hardware while the
//! machine is asleep and powers it up; the IP port is irrelevant. That is what
//! makes waking over the internet possible with nothing but the mapping the
//! host already has: a packet sent to the PC's public `ip:port` is forwarded
//! by the router like any other and the card sees the pattern.
//!
//! Where the packet is sent, in one burst:
//!
//! * the LAN broadcast addresses, which only matter when the client is on the
//!   same network;
//! * the PC's LAN address directly, on the discovery port and the classic
//!   port 9, for switches that still hold its MAC in their tables;
//! * every WAN candidate in the ticket, on the ticket's port, which is the
//!   forwarded path when the client is somewhere else.
//!
//! Waking from sleep (S3) is the reliable case: the card stays powered and
//! most keep answering ARP for the PC's address, so the router can still
//! deliver a unicast. From a full shutdown the router's ARP entry for the PC
//! ages out within minutes and a unicast from outside no longer reaches it;
//! only a LAN broadcast does. The client UI says as much.

use crate::net::Transport;
use crate::ticket::{CandidateKind, Ticket};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// The port a magic packet is traditionally sent to.
pub const WOL_PORT: u16 = 9;
/// Bytes in a magic packet: a 6-byte sync stream and the MAC sixteen times.
pub const MAGIC_LEN: usize = 6 + 16 * 6;

/// An Ethernet MAC address.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr([u8; 6]);

impl MacAddr {
    pub fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    /// Accepts `aa:bb:cc:dd:ee:ff`, `aa-bb-cc-dd-ee-ff`, or `aabbccddeeff`,
    /// in either case.
    pub fn parse(s: &str) -> Option<Self> {
        let hex: String = s
            .trim()
            .chars()
            .filter(|c| *c != ':' && *c != '-' && *c != '.' && !c.is_whitespace())
            .collect();
        if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let mut out = [0u8; 6];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        let mac = Self(out);
        // All-zero and broadcast are what a virtual or unplugged adapter
        // reports; neither can wake anything.
        if mac.is_nil() {
            return None;
        }
        Some(mac)
    }

    pub fn bytes(&self) -> [u8; 6] {
        self.0
    }

    fn is_nil(&self) -> bool {
        self.0 == [0; 6] || self.0 == [0xFF; 6]
    }

    /// The magic packet that wakes this MAC.
    pub fn magic_packet(&self) -> [u8; MAGIC_LEN] {
        let mut pkt = [0xFFu8; MAGIC_LEN];
        for rep in 0..16 {
            pkt[6 + rep * 6..6 + rep * 6 + 6].copy_from_slice(&self.0);
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

/// Everywhere worth sending a magic packet for this ticket, de-duplicated.
pub fn wake_targets(ticket: &Ticket) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    let mut push = |a: SocketAddr| {
        if !a.ip().is_unspecified() && a.port() != 0 && !out.contains(&a) {
            out.push(a);
        }
    };
    push(SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), WOL_PORT));
    for c in &ticket.candidates {
        match (c.kind, c.addr.ip()) {
            (CandidateKind::Lan, IpAddr::V4(ip)) => {
                push(SocketAddr::new(IpAddr::V4(ip), WOL_PORT));
                push(c.addr);
                // Directed broadcast for the PC's /24. Most home LANs are one,
                // and a wrong guess costs a datagram nobody answers.
                let o = ip.octets();
                let subnet = Ipv4Addr::new(o[0], o[1], o[2], 255);
                push(SocketAddr::new(IpAddr::V4(subnet), WOL_PORT));
                push(SocketAddr::new(IpAddr::V4(subnet), c.addr.port()));
            }
            // A sleeping PC's Tailscale daemon is asleep with it.
            (CandidateKind::Tailscale, _) => {}
            // The forwarded port is the only one the router will pass on.
            (CandidateKind::Wan, _) | (CandidateKind::Lan, IpAddr::V6(_)) => push(c.addr),
        }
    }
    out
}

/// Send one magic packet to each target. Returns how many sends succeeded;
/// failures are expected (no route to a broadcast, no IPv6 socket) and logged
/// at debug level only.
pub async fn send_wake(transport: &Transport, mac: MacAddr, targets: &[SocketAddr]) -> usize {
    let pkt = mac.magic_packet();
    let mut sent = 0;
    for t in targets {
        match transport.send_raw(&pkt, *t).await {
            Ok(_) => sent += 1,
            Err(e) => tracing::debug!("wake packet to {t}: {e}"),
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::ticket::Candidate;

    #[test]
    fn mac_parses_common_spellings() {
        let want = MacAddr::new([0xAA, 0xBB, 0xCC, 0x01, 0x02, 0x03]);
        for s in [
            "aa:bb:cc:01:02:03",
            "AA-BB-CC-01-02-03",
            "aabbcc010203",
            " AA:bb:CC:01:02:03 ",
            "aabb.cc01.0203",
        ] {
            assert_eq!(MacAddr::parse(s), Some(want), "{s}");
        }
        assert_eq!(want.to_string(), "aa:bb:cc:01:02:03");
    }

    #[test]
    fn junk_and_nil_macs_are_rejected() {
        for s in [
            "",
            "aa:bb:cc",
            "aa:bb:cc:dd:ee:ff:00",
            "zz:bb:cc:dd:ee:ff",
            "00:00:00:00:00:00",
            "ff:ff:ff:ff:ff:ff",
        ] {
            assert!(MacAddr::parse(s).is_none(), "{s:?}");
        }
    }

    #[test]
    fn magic_packet_is_sync_stream_then_mac_sixteen_times() {
        let mac = MacAddr::new([1, 2, 3, 4, 5, 6]);
        let pkt = mac.magic_packet();
        assert_eq!(pkt.len(), 102);
        assert_eq!(&pkt[..6], &[0xFF; 6]);
        for rep in 0..16 {
            assert_eq!(&pkt[6 + rep * 6..12 + rep * 6], &[1, 2, 3, 4, 5, 6]);
        }
    }

    #[test]
    fn targets_cover_lan_broadcast_and_every_forwarded_wan_address() {
        let id = Identity::generate();
        let t = Ticket::new(
            &id,
            vec![
                Candidate {
                    kind: CandidateKind::Lan,
                    addr: "192.168.1.20:47850".parse().unwrap(),
                },
                Candidate {
                    kind: CandidateKind::Wan,
                    addr: "203.0.113.9:40000".parse().unwrap(),
                },
                Candidate {
                    kind: CandidateKind::Wan,
                    addr: "[2001:db8::5]:47850".parse().unwrap(),
                },
                Candidate {
                    kind: CandidateKind::Tailscale,
                    addr: "100.64.0.30:47850".parse().unwrap(),
                },
            ],
            None,
            "PC",
        );
        let targets = wake_targets(&t);
        let has = |s: &str| targets.contains(&s.parse::<SocketAddr>().unwrap());
        assert!(has("255.255.255.255:9"));
        assert!(has("192.168.1.20:9"));
        assert!(has("192.168.1.20:47850"));
        assert!(has("192.168.1.255:9"));
        assert!(has("192.168.1.255:47850"));
        assert!(has("203.0.113.9:40000"), "the forwarded port, not port 9");
        assert!(has("[2001:db8::5]:47850"));
        assert!(
            !targets
                .iter()
                .any(|a| a.ip().to_string().starts_with("100.")),
            "Tailscale cannot carry a wake packet: {targets:?}"
        );
        let unique: std::collections::HashSet<_> = targets.iter().collect();
        assert_eq!(unique.len(), targets.len(), "no duplicates");
    }

    #[test]
    fn an_empty_ticket_still_broadcasts() {
        let id = Identity::generate();
        let t = Ticket::new(&id, Vec::new(), None, "PC");
        assert_eq!(
            wake_targets(&t),
            vec!["255.255.255.255:9".parse::<SocketAddr>().unwrap()]
        );
    }
}
