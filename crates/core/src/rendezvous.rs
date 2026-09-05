//! Rendezvous: how a client finds a host it has no address for.
//!
//! Two machines behind NAT cannot introduce themselves. A coordinator solves
//! exactly that and nothing else:
//!
//! 1. The host **registers** its identity and candidate addresses, and keeps
//!    the registration alive. The coordinator records the address it *observed*
//!    the packet coming from, which is the host's current WAN mapping.
//! 2. A client **looks up** a host by its public key and gets those candidates
//!    plus the observed address back.
//! 3. The coordinator simultaneously **punches**: it tells the host where the
//!    client is, so both sides emit packets at the same moment and each NAT
//!    sees outbound traffic before the peer's arrives.
//! 4. If that fails, both fall back to the relay that shares the coordinator's
//!    address, using the token already in the host's ticket.
//!
//! These packets travel on the **media socket**, not a side channel. The NAT
//! mapping the coordinator observes has to be the one media will arrive on.
//! They are sent unframed even though the coordinator lives at the relay's
//! address: the relay tells the two apart by the `BLK1` magic.
//!
//! ## What the coordinator can and cannot do
//!
//! It never sees plaintext and holds no key material. The media handshake is
//! unchanged: the client still verifies the host's Ed25519 signature against
//! the key it already pinned, so a hostile or compromised coordinator can
//! delay, drop, or misdirect an introduction, but cannot impersonate a host or
//! read a byte of the stream. Registrations are signed by the host identity,
//! so it cannot forge one either. It does learn which client address asks for
//! which host, and when: that metadata is inherent to coordination.
//!
//! ## Abuse resistance
//!
//! Every request must echo a **cookie** the coordinator issued to that exact
//! source address, and the cookie is covered by the signature. This proves the
//! source address is real before the coordinator does any work or sends any
//! packet to a third party. Without it, a spoofed `Lookup` would make the
//! coordinator, and then the host, fire packets at a victim.
//!
//! `Lookup` is also padded so a request is never smaller than its reply, which
//! keeps the amplification factor below 1.

use crate::crypto::random_bytes;
use crate::ticket::{Candidate, CandidateKind};
use crate::wire::{read_addr, write_addr, Cursor};
use anyhow::{anyhow, bail, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::SocketAddr;
use std::time::Duration;

pub const COOKIE_LEN: usize = 16;
pub const NONCE_LEN: usize = 16;
pub const SIG_LEN: usize = 64;

/// A registration this old is dropped. Comfortably more than three missed
/// refreshes, so a brief network blip does not delist a host.
pub const REGISTRATION_TTL: Duration = Duration::from_secs(90);
/// How often a host refreshes. Also holds the NAT mapping open.
pub const REGISTER_INTERVAL: Duration = Duration::from_secs(20);
/// Cookies are valid for their slot and the previous one, so a request that
/// straddles a boundary still works.
const COOKIE_SLOT: Duration = Duration::from_secs(60);
/// Reject a signed request whose clock is further out than this.
const MAX_CLOCK_SKEW_US: u64 = 120_000_000;
/// Requests are padded to at least this, so no reply is larger than the
/// request that triggered it.
pub const MIN_REQUEST_BYTES: usize = 512;
/// Bound on candidates carried in a registration, so a record stays inside one
/// datagram and a malicious host cannot make the coordinator allocate freely.
pub const MAX_CANDIDATES: usize = 8;
/// Bound on the advertised host name.
pub const MAX_NAME_BYTES: usize = 48;

const DOMAIN_REGISTER: &[u8] = b"brolink-rendezvous-register-v1";
const DOMAIN_LOOKUP: &[u8] = b"brolink-rendezvous-lookup-v1";

// ---------------------------------------------------------------- cookies

/// Stateless return-routability token, keyed by a secret the coordinator keeps
/// in memory. Stateless matters: a flood of first-contact packets cannot make
/// the coordinator allocate anything.
#[derive(Clone)]
pub struct CookieKey([u8; 32]);

impl CookieKey {
    pub fn generate() -> Self {
        Self(random_bytes::<32>())
    }

    pub fn from_bytes(k: [u8; 32]) -> Self {
        Self(k)
    }

    fn tag(&self, addr: &SocketAddr, slot: u64) -> [u8; COOKIE_LEN] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.0).expect("hmac takes any key");
        match addr.ip() {
            std::net::IpAddr::V4(v4) => {
                mac.update(&[4]);
                mac.update(&v4.octets());
            }
            std::net::IpAddr::V6(v6) => {
                mac.update(&[6]);
                mac.update(&v6.octets());
            }
        }
        mac.update(&addr.port().to_le_bytes());
        mac.update(&slot.to_le_bytes());
        let out = mac.finalize().into_bytes();
        let mut cookie = [0u8; COOKIE_LEN];
        cookie.copy_from_slice(&out[..COOKIE_LEN]);
        cookie
    }

    pub fn issue(&self, addr: &SocketAddr, now_us: u64) -> [u8; COOKIE_LEN] {
        self.tag(addr, slot_of(now_us))
    }

    /// Accepts the current or previous slot. Comparison is constant-time so a
    /// timing side channel cannot be used to forge one byte at a time.
    pub fn verify(&self, addr: &SocketAddr, cookie: &[u8; COOKIE_LEN], now_us: u64) -> bool {
        let slot = slot_of(now_us);
        let mut ok = ct_eq(&self.tag(addr, slot), cookie);
        ok |= ct_eq(&self.tag(addr, slot.saturating_sub(1)), cookie);
        ok
    }
}

fn slot_of(now_us: u64) -> u64 {
    now_us / COOKIE_SLOT.as_micros() as u64
}

fn ct_eq(a: &[u8; COOKIE_LEN], b: &[u8; COOKIE_LEN]) -> bool {
    let mut r = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        r |= x ^ y;
    }
    r == 0
}

// ---------------------------------------------------------------- messages

/// Host to coordinator: "I am this key, reachable at these addresses."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    pub host_id: [u8; 32],
    pub cookie: Option<[u8; COOKIE_LEN]>,
    pub ts_us: u64,
    pub name: String,
    pub candidates: Vec<Candidate>,
}

impl Register {
    pub fn signed(
        key: &SigningKey,
        cookie: Option<[u8; COOKIE_LEN]>,
        ts_us: u64,
        name: &str,
        candidates: &[Candidate],
    ) -> Vec<u8> {
        let msg = Self {
            host_id: key.verifying_key().to_bytes(),
            cookie,
            ts_us,
            name: truncate_name(name),
            candidates: candidates.iter().copied().take(MAX_CANDIDATES).collect(),
        };
        let mut buf = msg.body();
        let sig = key.sign(&prefixed(DOMAIN_REGISTER, &buf));
        buf.extend_from_slice(&sig.to_bytes());
        buf
    }

    fn body(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(160);
        buf.extend_from_slice(&self.host_id);
        write_cookie(&mut buf, self.cookie.as_ref());
        buf.extend_from_slice(&self.ts_us.to_le_bytes());
        write_name(&mut buf, &self.name);
        write_candidates(&mut buf, &self.candidates);
        buf
    }

    /// Parses **and verifies**: the signature is checked against the `host_id`
    /// carried in the message, so an unverified `Register` cannot be produced.
    /// `now_us` bounds the timestamp so a captured registration cannot be
    /// replayed later from somewhere else.
    pub fn decode_verified(buf: &[u8], now_us: u64) -> Result<Self> {
        let mut cur = Cursor::new(buf);
        let host_id: [u8; 32] = cur.take_array()?;
        let cookie = read_cookie(&mut cur)?;
        let ts_us = cur.u64()?;
        let name = read_name(&mut cur)?;
        let candidates = read_candidates(&mut cur)?;
        let signed_len = cur.pos();
        let sig: [u8; SIG_LEN] = cur.take_array()?;
        verify_sig(DOMAIN_REGISTER, &host_id, &buf[..signed_len], &sig)?;
        check_skew(ts_us, now_us)?;
        Ok(Self {
            host_id,
            cookie,
            ts_us,
            name,
            candidates,
        })
    }
}

/// Coordinator to host: registration accepted, and this is how you look from
/// the outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterAck {
    pub observed: SocketAddr,
    pub ttl_secs: u16,
}

impl RegisterAck {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24);
        write_addr(&mut buf, self.observed);
        buf.extend_from_slice(&self.ttl_secs.to_le_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(buf);
        let observed = read_addr(&mut cur)?;
        let ttl_secs = cur.u16()?;
        Ok(Self { observed, ttl_secs })
    }
}

/// Client to coordinator: "where is this host, and tell it I am coming."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub host_id: [u8; 32],
    pub client_id: [u8; 32],
    pub cookie: Option<[u8; COOKIE_LEN]>,
    pub nonce: [u8; NONCE_LEN],
    pub ts_us: u64,
}

impl Lookup {
    pub fn signed(
        key: &SigningKey,
        host_id: &[u8; 32],
        cookie: Option<[u8; COOKIE_LEN]>,
        nonce: &[u8; NONCE_LEN],
        ts_us: u64,
    ) -> Vec<u8> {
        let msg = Self {
            host_id: *host_id,
            client_id: key.verifying_key().to_bytes(),
            cookie,
            nonce: *nonce,
            ts_us,
        };
        let mut buf = msg.body();
        let sig = key.sign(&prefixed(DOMAIN_LOOKUP, &buf));
        buf.extend_from_slice(&sig.to_bytes());
        // Padding is part of the datagram but not of the signed body, so it
        // costs nothing to verify and cannot be used to smuggle content.
        if buf.len() < MIN_REQUEST_BYTES {
            buf.resize(MIN_REQUEST_BYTES, 0);
        }
        buf
    }

    fn body(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&self.host_id);
        buf.extend_from_slice(&self.client_id);
        write_cookie(&mut buf, self.cookie.as_ref());
        buf.extend_from_slice(&self.nonce);
        buf.extend_from_slice(&self.ts_us.to_le_bytes());
        buf
    }

    /// Parses and verifies against the `client_id` in the message. That key is
    /// not trusted for anything by itself; it makes lookups attributable and
    /// rate-limitable rather than anonymous.
    pub fn decode_verified(buf: &[u8], now_us: u64) -> Result<Self> {
        let mut cur = Cursor::new(buf);
        let host_id: [u8; 32] = cur.take_array()?;
        let client_id: [u8; 32] = cur.take_array()?;
        let cookie = read_cookie(&mut cur)?;
        let nonce: [u8; NONCE_LEN] = cur.take_array()?;
        let ts_us = cur.u64()?;
        let signed_len = cur.pos();
        let sig: [u8; SIG_LEN] = cur.take_array()?;
        verify_sig(DOMAIN_LOOKUP, &client_id, &buf[..signed_len], &sig)?;
        check_skew(ts_us, now_us)?;
        Ok(Self {
            host_id,
            client_id,
            cookie,
            nonce,
            ts_us,
        })
    }
}

/// What the coordinator knows about a registered host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRecord {
    pub host_id: [u8; 32],
    pub name: String,
    /// Addresses the host reported: LAN, Tailscale, global IPv6.
    pub candidates: Vec<Candidate>,
    /// The address the coordinator saw the registration arrive from. This is
    /// the host's live WAN mapping and is usually the one that works.
    pub observed: SocketAddr,
}

/// Coordinator to client. `host: None` means "no such registration", which is
/// also what an unknown key gets, so the reply does not confirm existence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupAck {
    pub host: Option<HostRecord>,
}

impl LookupAck {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(160);
        match &self.host {
            None => buf.push(0),
            Some(h) => {
                buf.push(1);
                buf.extend_from_slice(&h.host_id);
                write_name(&mut buf, &h.name);
                write_candidates(&mut buf, &h.candidates);
                write_addr(&mut buf, h.observed);
            }
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(buf);
        let host = match cur.u8()? {
            0 => None,
            1 => {
                let host_id: [u8; 32] = cur.take_array()?;
                let name = read_name(&mut cur)?;
                let candidates = read_candidates(&mut cur)?;
                let observed = read_addr(&mut cur)?;
                Some(HostRecord {
                    host_id,
                    name,
                    candidates,
                    observed,
                })
            }
            v => bail!("unknown lookup reply tag {v}"),
        };
        Ok(Self { host })
    }
}

/// Coordinator to host: a client is waiting at this address, punch now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Punch {
    pub client_addr: SocketAddr,
    pub client_id: [u8; 32],
    pub nonce: [u8; NONCE_LEN],
}

impl Punch {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(96);
        write_addr(&mut buf, self.client_addr);
        buf.extend_from_slice(&self.client_id);
        buf.extend_from_slice(&self.nonce);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(buf);
        let client_addr = read_addr(&mut cur)?;
        let client_id: [u8; 32] = cur.take_array()?;
        let nonce: [u8; NONCE_LEN] = cur.take_array()?;
        Ok(Self {
            client_addr,
            client_id,
            nonce,
        })
    }
}

/// Host to client, straight at the address the coordinator gave. Its only job
/// is to open the host's NAT, but it doubles as a signal: a client that sees
/// one knows which of its candidate paths is live and can send `Hello` there
/// immediately instead of waiting out the retry timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchProbe {
    pub host_id: [u8; 32],
    pub nonce: [u8; NONCE_LEN],
}

impl PunchProbe {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(48);
        buf.extend_from_slice(&self.host_id);
        buf.extend_from_slice(&self.nonce);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(buf);
        Ok(Self {
            host_id: cur.take_array()?,
            nonce: cur.take_array()?,
        })
    }
}

/// Coordinator to either peer: "resend with this cookie."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub cookie: [u8; COOKIE_LEN],
}

impl Retry {
    pub fn encode(&self) -> Vec<u8> {
        self.cookie.to_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(buf);
        Ok(Self {
            cookie: cur.take_array()?,
        })
    }
}

// ---------------------------------------------------------------- helpers

fn prefixed(domain: &[u8], body: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(domain.len() + body.len());
    msg.extend_from_slice(domain);
    msg.extend_from_slice(body);
    msg
}

fn verify_sig(domain: &[u8], key: &[u8; 32], body: &[u8], sig: &[u8; SIG_LEN]) -> Result<()> {
    let vk = VerifyingKey::from_bytes(key).map_err(|_| anyhow!("bad public key"))?;
    vk.verify(&prefixed(domain, body), &Signature::from_bytes(sig))
        .map_err(|_| anyhow!("rendezvous signature"))
}

fn check_skew(ts_us: u64, now_us: u64) -> Result<()> {
    let delta = ts_us.abs_diff(now_us);
    if delta > MAX_CLOCK_SKEW_US {
        bail!("rendezvous timestamp is {}s out", delta / 1_000_000);
    }
    Ok(())
}

fn truncate_name(name: &str) -> String {
    let mut n: String = name.chars().take(MAX_NAME_BYTES / 2).collect();
    while n.len() > MAX_NAME_BYTES {
        n.pop();
    }
    n
}

fn write_cookie(buf: &mut Vec<u8>, cookie: Option<&[u8; COOKIE_LEN]>) {
    match cookie {
        Some(c) => {
            buf.push(1);
            buf.extend_from_slice(c);
        }
        None => buf.push(0),
    }
}

fn read_cookie(cur: &mut Cursor<'_>) -> Result<Option<[u8; COOKIE_LEN]>> {
    match cur.u8()? {
        0 => Ok(None),
        1 => Ok(Some(cur.take_array()?)),
        v => bail!("unknown cookie tag {v}"),
    }
}

fn write_name(buf: &mut Vec<u8>, name: &str) {
    let b = name.as_bytes();
    let n = b.len().min(MAX_NAME_BYTES);
    buf.push(n as u8);
    buf.extend_from_slice(&b[..n]);
}

fn read_name(cur: &mut Cursor<'_>) -> Result<String> {
    let n = cur.u8()? as usize;
    if n > MAX_NAME_BYTES {
        bail!("name too long");
    }
    Ok(String::from_utf8_lossy(cur.take(n)?).into_owned())
}

fn write_candidates(buf: &mut Vec<u8>, cands: &[Candidate]) {
    let cands = &cands[..cands.len().min(MAX_CANDIDATES)];
    buf.push(cands.len() as u8);
    for c in cands {
        buf.push(c.kind as u8);
        write_addr(buf, c.addr);
    }
}

fn read_candidates(cur: &mut Cursor<'_>) -> Result<Vec<Candidate>> {
    let n = cur.u8()? as usize;
    if n > MAX_CANDIDATES {
        bail!("too many candidates");
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let kind = kind_from_u8(cur.u8()?)?;
        let addr = read_addr(cur)?;
        out.push(Candidate { kind, addr });
    }
    Ok(out)
}

fn kind_from_u8(v: u8) -> Result<CandidateKind> {
    Ok(match v {
        0 => CandidateKind::Lan,
        1 => CandidateKind::Wan,
        2 => CandidateKind::Tailscale,
        _ => bail!("unknown candidate kind {v}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::proto::MAX_DATAGRAM;

    fn cands() -> Vec<Candidate> {
        vec![
            Candidate {
                kind: CandidateKind::Lan,
                addr: "192.168.1.10:47850".parse().unwrap(),
            },
            Candidate {
                kind: CandidateKind::Tailscale,
                addr: "100.64.1.2:47850".parse().unwrap(),
            },
            Candidate {
                kind: CandidateKind::Wan,
                addr: "[2001:db8::5]:47850".parse().unwrap(),
            },
        ]
    }

    #[test]
    fn register_roundtrips_and_verifies() {
        let id = Identity::generate();
        let now = 1_700_000_000_000_000;
        let cookie = Some([7u8; COOKIE_LEN]);
        let raw = Register::signed(&id.signing, cookie, now, "OFFICE-PC", &cands());
        let got = Register::decode_verified(&raw, now).unwrap();
        assert_eq!(got.host_id, id.public);
        assert_eq!(got.name, "OFFICE-PC");
        assert_eq!(got.candidates, cands());
        assert_eq!(got.cookie, cookie);
    }

    #[test]
    fn a_tampered_registration_is_rejected() {
        let id = Identity::generate();
        let now = 1_700_000_000_000_000;
        let raw = Register::signed(&id.signing, None, now, "PC", &cands());
        // Flip a byte in the candidate list, which is the whole point of
        // signing: an operator must not be able to redirect a host.
        for idx in [40usize, 60, 70] {
            let mut bad = raw.clone();
            bad[idx] ^= 0x01;
            assert!(Register::decode_verified(&bad, now).is_err(), "idx {idx}");
        }
    }

    #[test]
    fn a_registration_signed_by_another_key_is_rejected() {
        let real = Identity::generate();
        let impostor = Identity::generate();
        let now = 1_700_000_000_000_000;
        let mut raw = Register::signed(&impostor.signing, None, now, "PC", &cands());
        raw[..32].copy_from_slice(&real.public);
        assert!(Register::decode_verified(&raw, now).is_err());
    }

    #[test]
    fn a_stale_registration_is_rejected() {
        let id = Identity::generate();
        let signed_at = 1_700_000_000_000_000;
        let raw = Register::signed(&id.signing, None, signed_at, "PC", &cands());
        assert!(Register::decode_verified(&raw, signed_at).is_ok());
        let much_later = signed_at + MAX_CLOCK_SKEW_US + 1;
        assert!(Register::decode_verified(&raw, much_later).is_err());
    }

    #[test]
    fn lookup_roundtrips_and_is_padded_against_amplification() {
        let id = Identity::generate();
        let host = Identity::generate();
        let now = 1_700_000_000_000_000;
        let nonce = [3u8; NONCE_LEN];
        let raw = Lookup::signed(
            &id.signing,
            &host.public,
            Some([1u8; COOKIE_LEN]),
            &nonce,
            now,
        );
        assert!(raw.len() >= MIN_REQUEST_BYTES);
        let got = Lookup::decode_verified(&raw, now).unwrap();
        assert_eq!(got.host_id, host.public);
        assert_eq!(got.client_id, id.public);
        assert_eq!(got.nonce, nonce);

        // The reply this triggers must never be bigger than the request.
        let ack = LookupAck {
            host: Some(HostRecord {
                host_id: host.public,
                name: "a".repeat(MAX_NAME_BYTES),
                candidates: vec![cands()[2]; MAX_CANDIDATES],
                observed: "[2001:db8::9]:47850".parse().unwrap(),
            }),
        };
        assert!(ack.encode().len() <= raw.len());
        assert!(ack.encode().len() <= MAX_DATAGRAM);
    }

    #[test]
    fn a_tampered_lookup_is_rejected() {
        let id = Identity::generate();
        let host = Identity::generate();
        let now = 1_700_000_000_000_000;
        let raw = Lookup::signed(&id.signing, &host.public, None, &[0u8; NONCE_LEN], now);
        let mut bad = raw.clone();
        bad[0] ^= 0x01;
        assert!(Lookup::decode_verified(&bad, now).is_err());
    }

    #[test]
    fn lookup_padding_is_not_signed_so_it_can_be_stripped_or_grown() {
        let id = Identity::generate();
        let host = Identity::generate();
        let now = 1_700_000_000_000_000;
        let mut raw = Lookup::signed(&id.signing, &host.public, None, &[0u8; NONCE_LEN], now);
        raw.extend_from_slice(&[0u8; 64]);
        assert!(Lookup::decode_verified(&raw, now).is_ok());
    }

    #[test]
    fn lookup_ack_roundtrips_both_found_and_missing() {
        let host = Identity::generate();
        let found = LookupAck {
            host: Some(HostRecord {
                host_id: host.public,
                name: "OFFICE-PC".into(),
                candidates: cands(),
                observed: "203.0.113.7:47850".parse().unwrap(),
            }),
        };
        assert_eq!(LookupAck::decode(&found.encode()).unwrap(), found);

        let missing = LookupAck { host: None };
        assert_eq!(LookupAck::decode(&missing.encode()).unwrap(), missing);
    }

    #[test]
    fn punch_and_probe_roundtrip() {
        let punch = Punch {
            client_addr: "203.0.113.9:51000".parse().unwrap(),
            client_id: [4u8; 32],
            nonce: [6u8; NONCE_LEN],
        };
        assert_eq!(Punch::decode(&punch.encode()).unwrap(), punch);

        let probe = PunchProbe {
            host_id: [8u8; 32],
            nonce: [6u8; NONCE_LEN],
        };
        assert_eq!(PunchProbe::decode(&probe.encode()).unwrap(), probe);

        let retry = Retry {
            cookie: [1u8; COOKIE_LEN],
        };
        assert_eq!(Retry::decode(&retry.encode()).unwrap(), retry);
    }

    #[test]
    fn cookies_are_bound_to_the_source_address() {
        let key = CookieKey::generate();
        let now = 1_700_000_000_000_000;
        let a: SocketAddr = "203.0.113.9:51000".parse().unwrap();
        let b: SocketAddr = "203.0.113.9:51001".parse().unwrap();
        let c: SocketAddr = "203.0.113.10:51000".parse().unwrap();
        let cookie = key.issue(&a, now);
        assert!(key.verify(&a, &cookie, now));
        // A different port or address must not accept it: that is exactly the
        // spoofed-source case the cookie exists to stop.
        assert!(!key.verify(&b, &cookie, now));
        assert!(!key.verify(&c, &cookie, now));
    }

    #[test]
    fn cookies_survive_a_slot_boundary_then_expire() {
        let key = CookieKey::generate();
        let now = 1_700_000_000_000_000;
        let addr: SocketAddr = "203.0.113.9:51000".parse().unwrap();
        let cookie = key.issue(&addr, now);
        let slot = COOKIE_SLOT.as_micros() as u64;
        assert!(key.verify(&addr, &cookie, now + slot));
        assert!(!key.verify(&addr, &cookie, now + slot * 3));
    }

    #[test]
    fn another_coordinators_cookie_is_not_accepted() {
        let mine = CookieKey::generate();
        let theirs = CookieKey::generate();
        let now = 1_700_000_000_000_000;
        let addr: SocketAddr = "203.0.113.9:51000".parse().unwrap();
        assert!(!mine.verify(&addr, &theirs.issue(&addr, now), now));
    }

    #[test]
    fn truncated_and_junk_input_never_panics() {
        let id = Identity::generate();
        let now = 1_700_000_000_000_000;
        let raw = Register::signed(&id.signing, None, now, "PC", &cands());
        for n in 0..raw.len() {
            let _ = Register::decode_verified(&raw[..n], now);
        }
        for n in 0..64 {
            let junk = vec![0xABu8; n];
            let _ = Register::decode_verified(&junk, now);
            let _ = Lookup::decode_verified(&junk, now);
            let _ = LookupAck::decode(&junk);
            let _ = Punch::decode(&junk);
            let _ = PunchProbe::decode(&junk);
            let _ = RegisterAck::decode(&junk);
            let _ = Retry::decode(&junk);
        }
    }

    #[test]
    fn a_registration_stays_inside_one_datagram() {
        let id = Identity::generate();
        let now = 1_700_000_000_000_000;
        let many = vec![cands()[2]; MAX_CANDIDATES];
        let raw = Register::signed(
            &id.signing,
            Some([0u8; COOKIE_LEN]),
            now,
            &"n".repeat(MAX_NAME_BYTES),
            &many,
        );
        assert!(raw.len() <= MAX_DATAGRAM);
    }

    #[test]
    fn over_long_candidate_lists_are_refused_not_allocated() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0u8; 32]);
        buf.push(0);
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.push(0);
        buf.push(255);
        let mut cur = Cursor::new(&buf[42..]);
        assert!(read_candidates(&mut cur).is_err());
    }
}
