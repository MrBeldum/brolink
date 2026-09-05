//! Human-pasteable connection tickets.
//!
//! Format: `blk1_` + unpadded base32 of a compact binary blob.
//!
//! Version 2 (IPv4-only, still emitted when every address is v4):
//!   version u8 = 2
//!   host_id [32]
//!   name_len u8 + name utf8
//!   candidate_count u8
//!     repeated: kind u8 | ip [4] | port u16
//!   relay u8   (0 = none, 1 = followed by ip [4] | port u16 | token [16])
//!
//! Version 3 (emitted when any address is IPv6):
//!   version u8 = 3
//!   host_id [32]
//!   name_len u8 + name utf8
//!   candidate_count u8
//!     repeated: kind u8 | family u8 (4|6) | ip [4|16] | port u16
//!   relay u8
//!     if 1: family u8 | ip | port u16 | token [16]
//!
//! Version 1 (still decoded, no longer emitted) carried only a LAN address and
//! an optional WAN address.

use crate::identity::Identity;
use crate::wire::{read_addr, write_addr, write_v4, Cursor};
use anyhow::{anyhow, bail, Result};
use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

pub const TICKET_PREFIX: &str = "blk1_";
/// A name longer than this is truncated when the ticket is built.
pub const MAX_NAME_BYTES: usize = 48;
/// Refuse absurd tickets early rather than allocating from attacker input.
const MAX_CANDIDATES: usize = 16;

/// How a candidate address was learned. Candidates are tried in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CandidateKind {
    Lan = 0,
    Wan = 1,
    Tailscale = 2,
}

impl CandidateKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Lan,
            1 => Self::Wan,
            2 => Self::Tailscale,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lan => "LAN",
            Self::Wan => "WAN",
            Self::Tailscale => "Tailscale",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub addr: SocketAddr,
}

/// A relay rendezvous: both peers send to `addr` prefixed with `token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayHint {
    pub addr: SocketAddr,
    pub token: [u8; 16],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ticket {
    pub host_id: [u8; 32],
    pub candidates: Vec<Candidate>,
    pub relay: Option<RelayHint>,
    pub name: String,
}

impl Ticket {
    pub fn new(
        identity: &Identity,
        candidates: Vec<Candidate>,
        relay: Option<RelayHint>,
        name: &str,
    ) -> Self {
        let mut name: String = name.chars().take(MAX_NAME_BYTES / 2).collect();
        while name.len() > MAX_NAME_BYTES {
            name.pop();
        }
        Self {
            host_id: identity.public,
            candidates,
            relay,
            name,
        }
    }

    fn uses_v6(&self) -> bool {
        self.candidates.iter().any(|c| c.addr.is_ipv6())
            || self.relay.as_ref().is_some_and(|r| r.addr.is_ipv6())
    }

    pub fn encode(&self) -> String {
        let raw = if self.uses_v6() {
            self.encode_v3()
        } else {
            self.encode_v2()
        };
        let enc = BASE32_NOPAD.encode(&raw).to_ascii_lowercase();
        format!("{TICKET_PREFIX}{enc}")
    }

    fn encode_header(&self, version: u8, buf: &mut Vec<u8>) {
        buf.push(version);
        buf.extend_from_slice(&self.host_id);
        let name = self.name.as_bytes();
        let n = name.len().min(MAX_NAME_BYTES);
        buf.push(n as u8);
        buf.extend_from_slice(&name[..n]);
    }

    fn encode_v2(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(96);
        self.encode_header(2, &mut buf);
        let cands: Vec<&Candidate> = self
            .candidates
            .iter()
            .filter(|c| c.addr.is_ipv4())
            .take(MAX_CANDIDATES)
            .collect();
        buf.push(cands.len() as u8);
        for c in cands {
            buf.push(c.kind as u8);
            write_v4(&mut buf, c.addr);
        }
        match &self.relay {
            Some(r) if r.addr.is_ipv4() => {
                buf.push(1);
                write_v4(&mut buf, r.addr);
                buf.extend_from_slice(&r.token);
            }
            _ => buf.push(0),
        }
        buf
    }

    fn encode_v3(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(160);
        self.encode_header(3, &mut buf);
        let cands = &self.candidates[..self.candidates.len().min(MAX_CANDIDATES)];
        buf.push(cands.len() as u8);
        for c in cands {
            buf.push(c.kind as u8);
            write_addr(&mut buf, c.addr);
        }
        match &self.relay {
            Some(r) => {
                buf.push(1);
                write_addr(&mut buf, r.addr);
                buf.extend_from_slice(&r.token);
            }
            None => buf.push(0),
        }
        buf
    }

    pub fn decode(s: &str) -> Result<Self> {
        let s = s.trim().replace([' ', '\n', '\r', '\t', '-'], "");
        let rest = s
            .strip_prefix(TICKET_PREFIX)
            .or_else(|| s.strip_prefix("BLK1_"))
            .unwrap_or(&s);
        if rest.is_empty() {
            bail!("empty ticket");
        }
        let raw = BASE32_NOPAD
            .decode(rest.to_ascii_uppercase().as_bytes())
            .map_err(|e| anyhow!("ticket decode: {e}"))?;
        match raw.first() {
            Some(1) => decode_v1(&raw),
            Some(2) => decode_v2(&raw),
            Some(3) => decode_v3(&raw),
            Some(v) => bail!("unsupported ticket version {v}"),
            None => bail!("empty ticket"),
        }
    }

    /// Addresses to try, in preference order, de-duplicated.
    pub fn candidate_addrs(&self) -> Vec<SocketAddr> {
        let mut sorted: Vec<&Candidate> = self.candidates.iter().collect();
        sorted.sort_by_key(|c| c.kind);
        let mut out: Vec<SocketAddr> = Vec::with_capacity(sorted.len());
        for c in sorted {
            if c.addr.ip().is_unspecified() || c.addr.port() == 0 {
                continue;
            }
            if !out.contains(&c.addr) {
                out.push(c.addr);
            }
        }
        out
    }

    pub fn lan(&self) -> Option<SocketAddrV4> {
        self.candidates.iter().find_map(|c| {
            if c.kind != CandidateKind::Lan {
                return None;
            }
            match c.addr {
                SocketAddr::V4(v4) => Some(v4),
                SocketAddr::V6(_) => None,
            }
        })
    }

    /// The ticket split into dash-separated groups so it survives being read aloud.
    pub fn display_code(&self) -> String {
        let enc = self.encode();
        let body = enc.trim_start_matches(TICKET_PREFIX);
        let mut grouped = String::with_capacity(enc.len() + body.len() / 5);
        grouped.push_str(TICKET_PREFIX);
        for (i, c) in body.chars().enumerate() {
            if i > 0 && i % 5 == 0 {
                grouped.push('-');
            }
            grouped.push(c);
        }
        grouped
    }
}

fn decode_v1(raw: &[u8]) -> Result<Ticket> {
    if raw.len() < 47 {
        bail!("ticket too short");
    }
    let mut host_id = [0u8; 32];
    host_id.copy_from_slice(&raw[1..33]);
    let lan = SocketAddrV4::new(
        Ipv4Addr::new(raw[33], raw[34], raw[35], raw[36]),
        u16::from_le_bytes([raw[37], raw[38]]),
    );
    let wan_ip = Ipv4Addr::new(raw[39], raw[40], raw[41], raw[42]);
    let wan_port = u16::from_le_bytes([raw[43], raw[44]]);
    let flags = raw[45];
    let nlen = raw[46] as usize;
    if raw.len() < 47 + nlen {
        bail!("ticket name truncated");
    }
    let name = String::from_utf8_lossy(&raw[47..47 + nlen]).into_owned();
    let mut candidates = vec![Candidate {
        kind: CandidateKind::Lan,
        addr: SocketAddr::V4(lan),
    }];
    if flags & 1 != 0 && !wan_ip.is_unspecified() {
        candidates.push(Candidate {
            kind: CandidateKind::Wan,
            addr: SocketAddr::V4(SocketAddrV4::new(wan_ip, wan_port)),
        });
    }
    Ok(Ticket {
        host_id,
        candidates,
        relay: None,
        name,
    })
}

fn decode_v2(raw: &[u8]) -> Result<Ticket> {
    let mut cur = Cursor::new(raw);
    cur.skip(1)?;
    let host_id: [u8; 32] = cur.take_array()?;
    let nlen = cur.u8()? as usize;
    let name = String::from_utf8_lossy(cur.take(nlen)?).into_owned();
    let count = cur.u8()? as usize;
    if count > MAX_CANDIDATES {
        bail!("ticket lists {count} candidates (max {MAX_CANDIDATES})");
    }
    let mut candidates = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = cur.u8()?;
        let ip = cur.take(4)?;
        let ip = Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]);
        let port = cur.u16()?;
        // An unknown kind is a newer host advertising something we cannot rank;
        // keep the address and treat it as WAN-ish rather than failing the ticket.
        let kind = CandidateKind::from_u8(kind).unwrap_or(CandidateKind::Wan);
        candidates.push(Candidate {
            kind,
            addr: SocketAddr::V4(SocketAddrV4::new(ip, port)),
        });
    }
    let relay = if cur.u8()? == 1 {
        let ip = cur.take(4)?;
        let ip = Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]);
        let port = cur.u16()?;
        let token: [u8; 16] = cur.take_array()?;
        Some(RelayHint {
            addr: SocketAddr::V4(SocketAddrV4::new(ip, port)),
            token,
        })
    } else {
        None
    };
    Ok(Ticket {
        host_id,
        candidates,
        relay,
        name,
    })
}

fn decode_v3(raw: &[u8]) -> Result<Ticket> {
    let mut cur = Cursor::new(raw);
    cur.skip(1)?;
    let host_id: [u8; 32] = cur.take_array()?;
    let nlen = cur.u8()? as usize;
    let name = String::from_utf8_lossy(cur.take(nlen)?).into_owned();
    let count = cur.u8()? as usize;
    if count > MAX_CANDIDATES {
        bail!("ticket lists {count} candidates (max {MAX_CANDIDATES})");
    }
    let mut candidates = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = CandidateKind::from_u8(cur.u8()?).unwrap_or(CandidateKind::Wan);
        let addr = read_addr(&mut cur)?;
        candidates.push(Candidate { kind, addr });
    }
    let relay = if cur.u8()? == 1 {
        let addr = read_addr(&mut cur)?;
        let token: [u8; 16] = cur.take_array()?;
        Some(RelayHint { addr, token })
    } else {
        None
    };
    Ok(Ticket {
        host_id,
        candidates,
        relay,
        name,
    })
}

/// Turn what the user typed into one address: a ticket's best candidate, an
/// IP, `ip:port`, or a DNS name (with or without a port). Name resolution is
/// a blocking system call, so this belongs on a connect path, not a UI frame.
pub fn parse_endpoint(s: &str, default_port: u16) -> Result<SocketAddr> {
    let s = s.trim();
    if let Ok(t) = Ticket::decode(s) {
        if let Some(first) = t.candidate_addrs().first() {
            return Ok(*first);
        }
        bail!("ticket contains no usable address");
    }
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = s.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    // `name:port`, but not an IPv6 literal (those have several colons and
    // were handled above when bracketed).
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, p.parse::<u16>().ok()),
        _ => (s, Some(default_port)),
    };
    let Some(port) = port else {
        bail!("'{s}' has an invalid port");
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        bail!("cannot parse endpoint '{s}' (expected IP, IP:port, a host name, or a blk1_ ticket)");
    }
    use std::net::ToSocketAddrs;
    let mut resolved = (host, port)
        .to_socket_addrs()
        .map_err(|e| anyhow!("could not resolve '{host}': {e}"))?;
    // Prefer IPv4: the media socket is always v4, and v6 only when a second
    // socket could be bound.
    let all: Vec<SocketAddr> = resolved.by_ref().collect();
    all.iter()
        .copied()
        .find(SocketAddr::is_ipv4)
        .or_else(|| all.first().copied())
        .ok_or_else(|| anyhow!("'{host}' did not resolve to any address"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), port))
    }

    #[test]
    fn ticket_roundtrip() {
        let id = Identity::generate();
        let t = Ticket::new(
            &id,
            vec![
                Candidate {
                    kind: CandidateKind::Lan,
                    addr: v4(192, 168, 1, 20, 47850),
                },
                Candidate {
                    kind: CandidateKind::Wan,
                    addr: v4(203, 0, 113, 9, 47850),
                },
                Candidate {
                    kind: CandidateKind::Tailscale,
                    addr: v4(100, 90, 1, 2, 47850),
                },
            ],
            Some(RelayHint {
                addr: v4(198, 51, 100, 7, 47851),
                token: [7u8; 16],
            }),
            "OFFICE-PC",
        );
        let back = Ticket::decode(&t.display_code()).unwrap();
        assert_eq!(back, t);
        assert!(t.display_code().starts_with("blk1_"));
    }

    #[test]
    fn candidates_are_ordered_lan_first_and_deduped() {
        let id = Identity::generate();
        let t = Ticket::new(
            &id,
            vec![
                Candidate {
                    kind: CandidateKind::Wan,
                    addr: v4(203, 0, 113, 9, 47850),
                },
                Candidate {
                    kind: CandidateKind::Lan,
                    addr: v4(192, 168, 1, 20, 47850),
                },
                // Duplicate of the WAN entry and an unusable 0.0.0.0 entry.
                Candidate {
                    kind: CandidateKind::Tailscale,
                    addr: v4(203, 0, 113, 9, 47850),
                },
                Candidate {
                    kind: CandidateKind::Tailscale,
                    addr: v4(0, 0, 0, 0, 47850),
                },
            ],
            None,
            "PC",
        );
        let addrs = t.candidate_addrs();
        assert_eq!(addrs.len(), 2);
        assert_eq!(addrs[0], v4(192, 168, 1, 20, 47850));
        assert_eq!(addrs[1], v4(203, 0, 113, 9, 47850));
    }

    #[test]
    fn decodes_legacy_v1_tickets() {
        // Hand-built v1 blob: version, id, lan, wan, flags, name.
        let mut raw = vec![1u8];
        raw.extend_from_slice(&[9u8; 32]);
        raw.extend_from_slice(&[192, 168, 0, 5]);
        raw.extend_from_slice(&47850u16.to_le_bytes());
        raw.extend_from_slice(&[203, 0, 113, 1]);
        raw.extend_from_slice(&40000u16.to_le_bytes());
        raw.push(1);
        raw.push(2);
        raw.extend_from_slice(b"PC");
        let enc = format!(
            "{TICKET_PREFIX}{}",
            BASE32_NOPAD.encode(&raw).to_ascii_lowercase()
        );
        let t = Ticket::decode(&enc).unwrap();
        assert_eq!(t.name, "PC");
        assert_eq!(t.host_id, [9u8; 32]);
        assert_eq!(t.candidates.len(), 2);
        assert_eq!(
            t.lan(),
            Some(SocketAddrV4::new(Ipv4Addr::new(192, 168, 0, 5), 47850))
        );
        assert!(t.relay.is_none());
    }

    #[test]
    fn truncated_tickets_error_instead_of_panicking() {
        let id = Identity::generate();
        let full = Ticket::new(
            &id,
            vec![Candidate {
                kind: CandidateKind::Lan,
                addr: v4(10, 0, 0, 1, 47850),
            }],
            None,
            "PC",
        )
        .encode();
        let body = full.trim_start_matches(TICKET_PREFIX);
        for cut in 1..body.len() {
            let short = format!("{TICKET_PREFIX}{}", &body[..cut]);
            // Must not panic; a truncated ticket is simply invalid (or, rarely,
            // decodes to something still well-formed).
            let _ = Ticket::decode(&short);
        }
    }

    #[test]
    fn garbage_never_panics() {
        for s in ["", "blk1_", "blk1_!!!!", "not a ticket", "blk1_aaaaaaaa"] {
            let _ = Ticket::decode(s);
        }
    }

    #[test]
    fn parse_endpoint_accepts_ip_port_and_ticket() {
        assert_eq!(
            parse_endpoint("192.168.1.5", 47850).unwrap(),
            v4(192, 168, 1, 5, 47850)
        );
        assert_eq!(
            parse_endpoint("192.168.1.5:9000", 47850).unwrap(),
            v4(192, 168, 1, 5, 9000)
        );
        let id = Identity::generate();
        let t = Ticket::new(
            &id,
            vec![Candidate {
                kind: CandidateKind::Lan,
                addr: v4(10, 1, 2, 3, 47850),
            }],
            None,
            "PC",
        );
        assert_eq!(
            parse_endpoint(&t.display_code(), 47850).unwrap(),
            v4(10, 1, 2, 3, 47850)
        );
        assert!(parse_endpoint("nonsense", 47850).is_err());
        assert!(parse_endpoint("", 47850).is_err());
        assert!(parse_endpoint("host:notaport", 47850).is_err());
        assert!(parse_endpoint("no spaces allowed", 47850).is_err());
    }

    #[test]
    fn parse_endpoint_resolves_host_names() {
        // localhost is the one name every machine can resolve.
        let a = parse_endpoint("localhost", 47850).unwrap();
        assert!(a.ip().is_loopback());
        assert_eq!(a.port(), 47850);
        let a = parse_endpoint("localhost:9000", 47850).unwrap();
        assert_eq!(a.port(), 9000);
        // Bracketed v6 literals still go through the address path.
        assert_eq!(
            parse_endpoint("[::1]:9000", 47850).unwrap(),
            "[::1]:9000".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn ipv6_tickets_roundtrip_as_v3() {
        let id = Identity::generate();
        let v6: SocketAddr = "[2001:db8::10]:47850".parse().unwrap();
        let t = Ticket::new(
            &id,
            vec![
                Candidate {
                    kind: CandidateKind::Lan,
                    addr: v4(10, 0, 0, 5, 47850),
                },
                Candidate {
                    kind: CandidateKind::Wan,
                    addr: v6,
                },
            ],
            None,
            "PC",
        );
        assert!(t.uses_v6());
        let back = Ticket::decode(&t.encode()).unwrap();
        assert_eq!(back, t);
        let addrs = back.candidate_addrs();
        assert!(addrs.contains(&v6));
        assert!(addrs.contains(&v4(10, 0, 0, 5, 47850)));
    }
}
