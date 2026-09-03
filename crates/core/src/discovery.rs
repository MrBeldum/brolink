//! LAN discovery via UDP broadcast.
//!
//! Hosts emit a Discovery packet every second to 255.255.255.255 and to
//! multicast 239.255.47.85. Clients collect unique hosts by identity.

use crate::proto::{
    encode_plain, json_from_slice, json_payload, parse_header, PacketType, HEADER_LEN,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

pub const DISCOVERY_MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 47, 85);
/// Drop a host from the list once it has been quiet this long.
pub const HOST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Beacon {
    pub name: String,
    pub host_id: String,
    pub port: u16,
    pub version: String,
    pub encoder: String,
    /// The host's full ticket.
    ///
    /// Without this a client that connects from the discovery list has no
    /// expected identity to check the handshake against, which would make LAN
    /// connections the one path where a machine on the network could pose as
    /// the host.
    #[serde(default)]
    pub ticket: String,
}

#[derive(Debug, Clone)]
pub struct DiscoveredHost {
    pub beacon: Beacon,
    pub addr: SocketAddr,
    pub last_seen: Instant,
}

impl DiscoveredHost {
    /// What to hand the connect path: the ticket when the host published one,
    /// otherwise the bare address.
    pub fn connect_target(&self) -> String {
        if self.beacon.ticket.is_empty() {
            self.addr.to_string()
        } else {
            self.beacon.ticket.clone()
        }
    }
}

pub fn encode_beacon(b: &Beacon) -> Result<Vec<u8>> {
    let payload = json_payload(b)?;
    Ok(encode_plain(PacketType::Discovery, 0, &payload).to_vec())
}

pub fn decode_beacon(buf: &[u8]) -> Option<Beacon> {
    let h = parse_header(buf).ok()?;
    if h.typ != PacketType::Discovery {
        return None;
    }
    json_from_slice(buf.get(HEADER_LEN..)?).ok()
}

pub async fn announce(sock: &UdpSocket, beacon: &Beacon) -> Result<()> {
    let bytes = encode_beacon(beacon)?;
    let port = beacon.port;
    // Both are best-effort: a machine with no broadcast route, or with
    // multicast filtered, should still reach the other transport.
    let _ = sock
        .send_to(&bytes, SocketAddrV4::new(Ipv4Addr::BROADCAST, port))
        .await;
    let _ = sock
        .send_to(&bytes, SocketAddrV4::new(DISCOVERY_MULTICAST, port))
        .await;
    Ok(())
}

pub fn prune(hosts: &mut Vec<DiscoveredHost>, max_age: Duration) {
    hosts.retain(|h| h.last_seen.elapsed() < max_age);
}

pub fn upsert(hosts: &mut Vec<DiscoveredHost>, beacon: Beacon, from: SocketAddr) {
    let addr = SocketAddr::new(from.ip(), beacon.port);
    match hosts
        .iter_mut()
        .find(|h| h.beacon.host_id == beacon.host_id)
    {
        Some(existing) => {
            existing.beacon = beacon;
            existing.addr = addr;
            existing.last_seen = Instant::now();
        }
        None => hosts.push(DiscoveredHost {
            beacon,
            addr,
            last_seen: Instant::now(),
        }),
    }
}

/// Join the IPv4 discovery group on every interface we can (best-effort).
///
/// A machine with several interfaces (Wi-Fi plus Tailscale plus a VM bridge)
/// only receives multicast on the ones it has joined, so joining the
/// unspecified interface alone can miss the network the host is actually on.
pub fn join_multicast(sock: &std::net::UdpSocket) {
    let _ = sock.join_multicast_v4(&DISCOVERY_MULTICAST, &Ipv4Addr::UNSPECIFIED);
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in ifaces {
            if let std::net::IpAddr::V4(v4) = ip {
                if !v4.is_loopback() {
                    let _ = sock.join_multicast_v4(&DISCOVERY_MULTICAST, &v4);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beacon(id: &str, port: u16) -> Beacon {
        Beacon {
            name: "PC".into(),
            host_id: id.into(),
            port,
            version: "0.1.0".into(),
            encoder: "h264_amf".into(),
            ticket: "flk1_abc".into(),
        }
    }

    #[test]
    fn beacon_roundtrip() {
        let b = beacon("aa", 47850);
        let bytes = encode_beacon(&b).unwrap();
        assert_eq!(decode_beacon(&bytes).unwrap(), b);
    }

    #[test]
    fn decode_rejects_non_beacons() {
        assert!(decode_beacon(&[]).is_none());
        assert!(decode_beacon(&[0u8; 8]).is_none());
        // Right framing, wrong packet type.
        let wrong = encode_plain(PacketType::Video, 0, b"{}").to_vec();
        assert!(decode_beacon(&wrong).is_none());
        // Right type, payload that is not a beacon.
        let bad_json = encode_plain(PacketType::Discovery, 0, b"not json").to_vec();
        assert!(decode_beacon(&bad_json).is_none());
    }

    #[test]
    fn beacons_from_older_hosts_without_a_ticket_still_decode() {
        let json = br#"{"name":"PC","host_id":"aa","port":47850,"version":"0.1.0","encoder":"x"}"#;
        let pkt = encode_plain(PacketType::Discovery, 0, json).to_vec();
        let b = decode_beacon(&pkt).expect("ticket field must be optional");
        assert!(b.ticket.is_empty());
    }

    #[test]
    fn upsert_dedupes_by_identity_not_address() {
        let mut hosts = Vec::new();
        upsert(
            &mut hosts,
            beacon("aa", 47850),
            "192.168.1.5:1000".parse().unwrap(),
        );
        upsert(
            &mut hosts,
            beacon("bb", 47850),
            "192.168.1.6:1000".parse().unwrap(),
        );
        assert_eq!(hosts.len(), 2);
        // Same host, new source port: still one entry, address refreshed to the
        // advertised service port.
        upsert(
            &mut hosts,
            beacon("aa", 47850),
            "192.168.1.11:2000".parse().unwrap(),
        );
        assert_eq!(hosts.len(), 2);
        let aa = hosts.iter().find(|h| h.beacon.host_id == "aa").unwrap();
        assert_eq!(aa.addr, "192.168.1.11:47850".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn prune_drops_stale_hosts() {
        let mut hosts = vec![DiscoveredHost {
            beacon: beacon("aa", 47850),
            addr: "192.168.1.5:47850".parse().unwrap(),
            last_seen: Instant::now() - Duration::from_secs(30),
        }];
        prune(&mut hosts, HOST_TIMEOUT);
        assert!(hosts.is_empty());
    }

    #[test]
    fn connect_target_prefers_the_ticket() {
        let with = DiscoveredHost {
            beacon: beacon("aa", 47850),
            addr: "192.168.1.5:47850".parse().unwrap(),
            last_seen: Instant::now(),
        };
        assert_eq!(with.connect_target(), "flk1_abc");

        let mut b = beacon("aa", 47850);
        b.ticket.clear();
        let without = DiscoveredHost {
            beacon: b,
            addr: "192.168.1.5:47850".parse().unwrap(),
            last_seen: Instant::now(),
        };
        assert_eq!(without.connect_target(), "192.168.1.5:47850");
    }
}
