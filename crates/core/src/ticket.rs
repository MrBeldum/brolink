//! Human-pasteable connection tickets.
//!
//! Format: `flk1_` + unpadded base32 of a compact binary blob.
//!
//! Version 2 (current):
//!   version u8 = 2
//!   host_id [32]
//!   name_len u8 + name utf8
//!   candidate_count u8
//!     repeated: kind u8 | ip [4] | port u16
//!   relay u8   (0 = none, 1 = followed by ip [4] | port u16 | token [16])
//!
//! Version 1 (still decoded, no longer emitted) carried only a LAN address and
//! an optional WAN address.

use crate::identity::Identity;
use anyhow::{anyhow, bail, Result};
use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

pub const TICKET_PREFIX: &str = "flk1_";
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
    pub addr: SocketAddrV4,
}

/// A relay rendezvous: both peers send to `addr` prefixed with `token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayHint {
    pub addr: SocketAddrV4,
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

    pub fn encode(&self) -> String {
        let mut buf = Vec::with_capacity(96);
        buf.push(2u8);
        buf.extend_from_slice(&self.host_id);
        let name = self.name.as_bytes();
        let n = name.len().min(MAX_NAME_BYTES);
        // `n` may land mid-codepoint; the decoder is lossy, so only bytes matter here.
        buf.push(n as u8);
        buf.extend_from_slice(&name[..n]);
        let cands = &self.candidates[..self.candidates.len().min(MAX_CANDIDATES)];
        buf.push(cands.len() as u8);
        for c in cands {
            buf.push(c.kind as u8);
            buf.extend_from_slice(&c.addr.ip().octets());
            buf.extend_from_slice(&c.addr.port().to_le_bytes());
        }
        match &self.relay {
            Some(r) => {
                buf.push(1);
                buf.extend_from_slice(&r.addr.ip().octets());
                buf.extend_from_slice(&r.addr.port().to_le_bytes());
                buf.extend_from_slice(&r.token);
            }
            None => buf.push(0),
        }
        let enc = BASE32_NOPAD.encode(&buf).to_ascii_lowercase();
        format!("{TICKET_PREFIX}{enc}")
    }

    pub fn decode(s: &str) -> Result<Self> {
        let s = s.trim().replace([' ', '\n', '\r', '\t', '-'], "");
        let rest = s
            .strip_prefix(TICKET_PREFIX)
            .or_else(|| s.strip_prefix("FLK1_"))
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
            let addr = SocketAddr::V4(c.addr);
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
        out
    }

    pub fn lan(&self) -> Option<SocketAddrV4> {
        self.candidates
            .iter()
            .find(|c| c.kind == CandidateKind::Lan)
            .map(|c| c.addr)
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
        addr: lan,
    }];
    if flags & 1 != 0 && !wan_ip.is_unspecified() {
        candidates.push(Candidate {
            kind: CandidateKind::Wan,
            addr: SocketAddrV4::new(wan_ip, wan_port),
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
    let host_id: [u8; 32] = cur.take(32)?.try_into().expect("32 bytes");
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
            addr: SocketAddrV4::new(ip, port),
        });
    }
    let relay = if cur.u8()? == 1 {
        let ip = cur.take(4)?;
        let ip = Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]);
        let port = cur.u16()?;
        let token: [u8; 16] = cur.take(16)?.try_into().expect("16 bytes");
        Some(RelayHint {
            addr: SocketAddrV4::new(ip, port),
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

/// Bounds-checked reader so a malformed ticket returns an error instead of panicking.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| anyhow!("ticket truncated"))?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
}

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
    if let Some((host, port)) = s.rsplit_once(':') {
        if let (Ok(ip), Ok(p)) = (host.parse::<std::net::IpAddr>(), port.parse::<u16>()) {
            return Ok(SocketAddr::new(ip, p));
        }
    }
    bail!("cannot parse endpoint '{s}' (expected IP, IP:port, or flk1_ ticket)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), port)
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
        assert!(t.display_code().starts_with("flk1_"));
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
        assert_eq!(addrs[0], SocketAddr::V4(v4(192, 168, 1, 20, 47850)));
        assert_eq!(addrs[1], SocketAddr::V4(v4(203, 0, 113, 9, 47850)));
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
        assert_eq!(t.lan(), Some(v4(192, 168, 0, 5, 47850)));
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
        for s in ["", "flk1_", "flk1_!!!!", "not a ticket", "flk1_aaaaaaaa"] {
            let _ = Ticket::decode(s);
        }
    }

    #[test]
    fn parse_endpoint_accepts_ip_port_and_ticket() {
        assert_eq!(
            parse_endpoint("192.168.1.5", 47850).unwrap(),
            SocketAddr::V4(v4(192, 168, 1, 5, 47850))
        );
        assert_eq!(
            parse_endpoint("192.168.1.5:9000", 47850).unwrap(),
            SocketAddr::V4(v4(192, 168, 1, 5, 9000))
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
            SocketAddr::V4(v4(10, 1, 2, 3, 47850))
        );
        assert!(parse_endpoint("nonsense", 47850).is_err());
    }
}
