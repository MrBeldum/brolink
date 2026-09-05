//! The rendezvous half of the BroLink coordinator.
//!
//! Hosts register and keep a registration alive; clients look a host up by its
//! public key and are introduced to it. See `brolink_core::rendezvous` for the
//! wire format and the trust argument.
//!
//! All of the decision-making lives in [`Coordinator::handle`], which is pure:
//! datagram in, datagrams out. Sockets are the caller's problem, which is what
//! makes the abuse cases testable without a network.

use brolink_core::proto::{encode_plain, parse_header, PacketType, HEADER_LEN};
use brolink_core::rendezvous::{
    CookieKey, HostRecord, Lookup, LookupAck, Punch, Register, RegisterAck, Retry, REGISTRATION_TTL,
};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

/// A datagram the caller should send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub to: SocketAddr,
    pub bytes: Vec<u8>,
}

/// Why a packet produced no useful work. Kept for logging and metrics: a
/// coordinator that silently drops traffic is impossible to operate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drop {
    NotRendezvous,
    BadSignature,
    RateLimited,
    Full,
}

pub struct Config {
    /// Maximum registered hosts held in memory.
    pub max_hosts: usize,
    /// Requests allowed per source address per second, averaged.
    pub rate_per_sec: f32,
    /// Burst allowance above the average rate.
    pub rate_burst: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_hosts: 10_000,
            rate_per_sec: 5.0,
            rate_burst: 20.0,
        }
    }
}

struct Entry {
    record: HostRecord,
    expires: Instant,
}

/// Token bucket per source IP. Keyed by IP rather than IP:port so that
/// rotating source ports does not buy an attacker a fresh budget.
struct Bucket {
    tokens: f32,
    last: Instant,
}

pub struct Coordinator {
    cfg: Config,
    cookies: CookieKey,
    hosts: HashMap<[u8; 32], Entry>,
    buckets: HashMap<IpAddr, Bucket>,
    last_gc: Instant,
}

impl Coordinator {
    pub fn new(cfg: Config, now: Instant) -> Self {
        Self {
            cfg,
            cookies: CookieKey::generate(),
            hosts: HashMap::new(),
            buckets: HashMap::new(),
            last_gc: now,
        }
    }

    pub fn registered_hosts(&self) -> usize {
        self.hosts.len()
    }

    /// Handle one datagram. Returns the replies to send, or why nothing
    /// happened.
    pub fn handle(
        &mut self,
        from: SocketAddr,
        packet: &[u8],
        now: Instant,
        now_us: u64,
    ) -> Result<Vec<Reply>, Drop> {
        self.gc(now);

        let header = parse_header(packet).map_err(|_| Drop::NotRendezvous)?;
        if !header.typ.is_rendezvous() {
            return Err(Drop::NotRendezvous);
        }
        // Only requests are meaningful inbound. Replies arriving here are
        // either confused peers or someone trying to use us as a reflector.
        if !matches!(header.typ, PacketType::Register | PacketType::Lookup) {
            return Err(Drop::NotRendezvous);
        }
        if !self.allow(from.ip(), now) {
            return Err(Drop::RateLimited);
        }
        let payload = &packet[HEADER_LEN..];

        match header.typ {
            PacketType::Register => self.on_register(from, payload, now, now_us),
            PacketType::Lookup => self.on_lookup(from, payload, now_us),
            _ => Err(Drop::NotRendezvous),
        }
    }

    fn on_register(
        &mut self,
        from: SocketAddr,
        payload: &[u8],
        now: Instant,
        now_us: u64,
    ) -> Result<Vec<Reply>, Drop> {
        let msg = Register::decode_verified(payload, now_us).map_err(|_| Drop::BadSignature)?;
        if !self.cookie_ok(msg.cookie.as_ref(), &from, now_us) {
            return Ok(vec![self.retry(from, now_us)]);
        }

        // Only refuse a *new* host when full; an existing registration must
        // always be refreshable or a full table would evict live sessions.
        if !self.hosts.contains_key(&msg.host_id) && self.hosts.len() >= self.cfg.max_hosts {
            return Err(Drop::Full);
        }

        self.hosts.insert(
            msg.host_id,
            Entry {
                record: HostRecord {
                    host_id: msg.host_id,
                    name: msg.name,
                    candidates: msg.candidates,
                    observed: from,
                },
                expires: now + REGISTRATION_TTL,
            },
        );

        let ack = RegisterAck {
            observed: from,
            ttl_secs: REGISTRATION_TTL.as_secs() as u16,
        };
        Ok(vec![Reply {
            to: from,
            bytes: encode_plain(PacketType::RegisterAck, 0, &ack.encode()).to_vec(),
        }])
    }

    fn on_lookup(
        &mut self,
        from: SocketAddr,
        payload: &[u8],
        now_us: u64,
    ) -> Result<Vec<Reply>, Drop> {
        let msg = Lookup::decode_verified(payload, now_us).map_err(|_| Drop::BadSignature)?;
        if !self.cookie_ok(msg.cookie.as_ref(), &from, now_us) {
            return Ok(vec![self.retry(from, now_us)]);
        }

        let Some(entry) = self.hosts.get(&msg.host_id) else {
            // Same shape of answer as a known-but-offline host, so a probe
            // cannot use us to enumerate which keys exist.
            let ack = LookupAck { host: None };
            return Ok(vec![Reply {
                to: from,
                bytes: encode_plain(PacketType::LookupAck, 0, &ack.encode()).to_vec(),
            }]);
        };

        let host_addr = entry.record.observed;
        let ack = LookupAck {
            host: Some(entry.record.clone()),
        };
        let punch = Punch {
            client_addr: from,
            client_id: msg.client_id,
            nonce: msg.nonce,
        };

        // Both go out now, so the two NATs open within a round trip of each
        // other. That simultaneity is the entire trick.
        Ok(vec![
            Reply {
                to: from,
                bytes: encode_plain(PacketType::LookupAck, 0, &ack.encode()).to_vec(),
            },
            Reply {
                to: host_addr,
                bytes: encode_plain(PacketType::Punch, 0, &punch.encode()).to_vec(),
            },
        ])
    }

    fn retry(&self, to: SocketAddr, now_us: u64) -> Reply {
        let r = Retry {
            cookie: self.cookies.issue(&to, now_us),
        };
        Reply {
            to,
            bytes: encode_plain(PacketType::Retry, 0, &r.encode()).to_vec(),
        }
    }

    fn cookie_ok(&self, cookie: Option<&[u8; 16]>, from: &SocketAddr, now_us: u64) -> bool {
        cookie.is_some_and(|c| self.cookies.verify(from, c, now_us))
    }

    fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        let cfg_rate = self.cfg.rate_per_sec;
        let cfg_burst = self.cfg.rate_burst;
        let b = self.buckets.entry(ip).or_insert(Bucket {
            tokens: cfg_burst,
            last: now,
        });
        let elapsed = now.saturating_duration_since(b.last).as_secs_f32();
        b.last = now;
        b.tokens = (b.tokens + elapsed * cfg_rate).min(cfg_burst);
        if b.tokens < 1.0 {
            return false;
        }
        b.tokens -= 1.0;
        true
    }

    fn gc(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_gc) < Duration::from_secs(15) {
            return;
        }
        self.last_gc = now;
        self.hosts.retain(|_, e| e.expires > now);
        // A bucket at full tokens is indistinguishable from no bucket, so it
        // can be forgotten. Without this the map grows once per source IP.
        let rate = self.cfg.rate_per_sec;
        let burst = self.cfg.rate_burst;
        self.buckets.retain(|_, b| {
            let elapsed = now.saturating_duration_since(b.last).as_secs_f32();
            b.tokens + elapsed * rate < burst
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brolink_core::identity::Identity;
    use brolink_core::rendezvous::{PunchProbe, COOKIE_LEN, NONCE_LEN};
    use brolink_core::ticket::{Candidate, CandidateKind};

    const NOW_US: u64 = 1_700_000_000_000_000;

    fn coord() -> (Coordinator, Instant) {
        let now = Instant::now();
        (Coordinator::new(Config::default(), now), now)
    }

    fn cands() -> Vec<Candidate> {
        vec![Candidate {
            kind: CandidateKind::Lan,
            addr: "192.168.1.10:47850".parse().unwrap(),
        }]
    }

    fn wrap(typ: PacketType, payload: Vec<u8>) -> Vec<u8> {
        encode_plain(typ, 0, &payload).to_vec()
    }

    /// Drive the cookie exchange and return a registered host.
    fn register(c: &mut Coordinator, id: &Identity, from: SocketAddr, now: Instant) -> Vec<Reply> {
        let first = wrap(
            PacketType::Register,
            Register::signed(&id.signing, None, NOW_US, "PC", &cands()),
        );
        let out = c.handle(from, &first, now, NOW_US).unwrap();
        let cookie = cookie_from(&out[0]);
        let second = wrap(
            PacketType::Register,
            Register::signed(&id.signing, Some(cookie), NOW_US, "PC", &cands()),
        );
        c.handle(from, &second, now, NOW_US).unwrap()
    }

    fn cookie_from(r: &Reply) -> [u8; COOKIE_LEN] {
        let h = parse_header(&r.bytes).unwrap();
        assert_eq!(h.typ, PacketType::Retry);
        Retry::decode(&r.bytes[HEADER_LEN..]).unwrap().cookie
    }

    #[test]
    fn a_first_request_gets_a_cookie_and_nothing_else() {
        let (mut c, now) = coord();
        let id = Identity::generate();
        let from: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let pkt = wrap(
            PacketType::Register,
            Register::signed(&id.signing, None, NOW_US, "PC", &cands()),
        );
        let out = c.handle(from, &pkt, now, NOW_US).unwrap();
        assert_eq!(out.len(), 1);
        let _ = cookie_from(&out[0]);
        // Crucially: nothing was stored. A flood of cookieless packets must
        // not cost the coordinator memory.
        assert_eq!(c.registered_hosts(), 0);
    }

    #[test]
    fn registration_records_the_observed_address_not_a_claimed_one() {
        let (mut c, now) = coord();
        let id = Identity::generate();
        let from: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let out = register(&mut c, &id, from, now);
        assert_eq!(c.registered_hosts(), 1);

        let h = parse_header(&out[0].bytes).unwrap();
        assert_eq!(h.typ, PacketType::RegisterAck);
        let ack = RegisterAck::decode(&out[0].bytes[HEADER_LEN..]).unwrap();
        // The host advertised only a LAN address; its WAN mapping is what the
        // coordinator saw, and that is what gets stored.
        assert_eq!(ack.observed, from);
    }

    #[test]
    fn a_lookup_introduces_both_sides() {
        let (mut c, now) = coord();
        let host = Identity::generate();
        let client = Identity::generate();
        let host_addr: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let client_addr: SocketAddr = "198.51.100.9:50000".parse().unwrap();
        register(&mut c, &host, host_addr, now);

        let first = wrap(
            PacketType::Lookup,
            Lookup::signed(
                &client.signing,
                &host.public,
                None,
                &[1u8; NONCE_LEN],
                NOW_US,
            ),
        );
        let out = c.handle(client_addr, &first, now, NOW_US).unwrap();
        let cookie = cookie_from(&out[0]);

        let second = wrap(
            PacketType::Lookup,
            Lookup::signed(
                &client.signing,
                &host.public,
                Some(cookie),
                &[1u8; NONCE_LEN],
                NOW_US,
            ),
        );
        let out = c.handle(client_addr, &second, now, NOW_US).unwrap();
        assert_eq!(out.len(), 2, "client is answered and host is punched");

        assert_eq!(out[0].to, client_addr);
        let ack = LookupAck::decode(&out[0].bytes[HEADER_LEN..]).unwrap();
        let rec = ack.host.expect("host was registered");
        assert_eq!(rec.host_id, host.public);
        assert_eq!(rec.observed, host_addr);
        assert_eq!(rec.candidates, cands());

        assert_eq!(
            out[1].to, host_addr,
            "punch goes to the host's live mapping"
        );
        let punch = Punch::decode(&out[1].bytes[HEADER_LEN..]).unwrap();
        assert_eq!(punch.client_addr, client_addr);
        assert_eq!(punch.client_id, client.public);
        assert_eq!(punch.nonce, [1u8; NONCE_LEN]);
    }

    #[test]
    fn a_spoofed_lookup_cannot_make_us_punch_a_victim() {
        let (mut c, now) = coord();
        let host = Identity::generate();
        let client = Identity::generate();
        let host_addr: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        register(&mut c, &host, host_addr, now);

        // The attacker forges the source address of a victim. They never see
        // the Retry, so they cannot produce a valid cookie for it, and the
        // coordinator sends nothing to the host.
        let victim: SocketAddr = "192.0.2.77:9".parse().unwrap();
        let pkt = wrap(
            PacketType::Lookup,
            Lookup::signed(
                &client.signing,
                &host.public,
                None,
                &[1u8; NONCE_LEN],
                NOW_US,
            ),
        );
        let out = c.handle(victim, &pkt, now, NOW_US).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].to, victim,
            "only a cookie, and only back to the source"
        );
        let h = parse_header(&out[0].bytes).unwrap();
        assert_eq!(h.typ, PacketType::Retry);
    }

    #[test]
    fn a_cookie_issued_to_one_address_does_not_work_from_another() {
        let (mut c, now) = coord();
        let id = Identity::generate();
        let a: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let b: SocketAddr = "203.0.113.6:40000".parse().unwrap();
        let first = wrap(
            PacketType::Register,
            Register::signed(&id.signing, None, NOW_US, "PC", &cands()),
        );
        let cookie = cookie_from(&c.handle(a, &first, now, NOW_US).unwrap()[0]);

        let stolen = wrap(
            PacketType::Register,
            Register::signed(&id.signing, Some(cookie), NOW_US, "PC", &cands()),
        );
        let out = c.handle(b, &stolen, now, NOW_US).unwrap();
        assert_eq!(parse_header(&out[0].bytes).unwrap().typ, PacketType::Retry);
        assert_eq!(c.registered_hosts(), 0);
    }

    #[test]
    fn an_unknown_host_gets_an_empty_answer_and_no_punch() {
        let (mut c, now) = coord();
        let client = Identity::generate();
        let unknown = Identity::generate();
        let client_addr: SocketAddr = "198.51.100.9:50000".parse().unwrap();
        let first = wrap(
            PacketType::Lookup,
            Lookup::signed(
                &client.signing,
                &unknown.public,
                None,
                &[1u8; NONCE_LEN],
                NOW_US,
            ),
        );
        let cookie = cookie_from(&c.handle(client_addr, &first, now, NOW_US).unwrap()[0]);
        let second = wrap(
            PacketType::Lookup,
            Lookup::signed(
                &client.signing,
                &unknown.public,
                Some(cookie),
                &[1u8; NONCE_LEN],
                NOW_US,
            ),
        );
        let out = c.handle(client_addr, &second, now, NOW_US).unwrap();
        assert_eq!(out.len(), 1);
        let ack = LookupAck::decode(&out[0].bytes[HEADER_LEN..]).unwrap();
        assert!(ack.host.is_none());
    }

    #[test]
    fn a_forged_registration_for_someone_elses_key_is_refused() {
        let (mut c, now) = coord();
        let real = Identity::generate();
        let impostor = Identity::generate();
        let from: SocketAddr = "203.0.113.5:40000".parse().unwrap();

        let mut body = Register::signed(&impostor.signing, None, NOW_US, "PC", &cands());
        body[..32].copy_from_slice(&real.public);
        let pkt = wrap(PacketType::Register, body);
        assert_eq!(
            c.handle(from, &pkt, now, NOW_US),
            Err(Drop::BadSignature),
            "the coordinator must not be able to be told a lie about a key"
        );
    }

    #[test]
    fn replies_are_never_accepted_as_requests() {
        let (mut c, now) = coord();
        let from: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        for typ in [
            PacketType::RegisterAck,
            PacketType::LookupAck,
            PacketType::Punch,
            PacketType::PunchProbe,
            PacketType::Retry,
        ] {
            let pkt = wrap(typ, vec![0u8; 32]);
            assert_eq!(c.handle(from, &pkt, now, NOW_US), Err(Drop::NotRendezvous));
        }
    }

    #[test]
    fn media_packets_are_not_our_business() {
        let (mut c, now) = coord();
        let from: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        for typ in [PacketType::Hello, PacketType::Video, PacketType::Input] {
            let pkt = wrap(typ, vec![0u8; 32]);
            assert_eq!(c.handle(from, &pkt, now, NOW_US), Err(Drop::NotRendezvous));
        }
        let junk = vec![0xAAu8; 40];
        assert_eq!(c.handle(from, &junk, now, NOW_US), Err(Drop::NotRendezvous));
    }

    #[test]
    fn a_flood_from_one_address_is_rate_limited() {
        let (mut c, now) = coord();
        let id = Identity::generate();
        let from: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let pkt = wrap(
            PacketType::Register,
            Register::signed(&id.signing, None, NOW_US, "PC", &cands()),
        );
        let mut limited = false;
        for _ in 0..100 {
            if c.handle(from, &pkt, now, NOW_US) == Err(Drop::RateLimited) {
                limited = true;
                break;
            }
        }
        assert!(
            limited,
            "an unbounded cookie flood must eventually be cut off"
        );

        // A different source is unaffected: the limit is per address, so one
        // abuser cannot deny service to everyone.
        let other: SocketAddr = "203.0.113.6:40000".parse().unwrap();
        assert!(c.handle(other, &pkt, now, NOW_US).is_ok());
    }

    #[test]
    fn rotating_source_ports_does_not_refill_the_budget() {
        let (mut c, now) = coord();
        let id = Identity::generate();
        let pkt = wrap(
            PacketType::Register,
            Register::signed(&id.signing, None, NOW_US, "PC", &cands()),
        );
        let mut limited = false;
        for port in 1000..1100u16 {
            let from: SocketAddr = format!("203.0.113.5:{port}").parse().unwrap();
            if c.handle(from, &pkt, now, NOW_US) == Err(Drop::RateLimited) {
                limited = true;
                break;
            }
        }
        assert!(limited);
    }

    #[test]
    fn the_table_is_capped_but_existing_hosts_can_still_refresh() {
        let now = Instant::now();
        let mut c = Coordinator::new(
            Config {
                max_hosts: 1,
                rate_per_sec: 1e6,
                rate_burst: 1e6,
            },
            now,
        );
        let first = Identity::generate();
        let second = Identity::generate();
        let a: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let b: SocketAddr = "203.0.113.6:40000".parse().unwrap();
        register(&mut c, &first, a, now);
        assert_eq!(c.registered_hosts(), 1);

        // A new host cannot displace it...
        let cookie = cookie_from(
            &c.handle(
                b,
                &wrap(
                    PacketType::Register,
                    Register::signed(&second.signing, None, NOW_US, "PC", &cands()),
                ),
                now,
                NOW_US,
            )
            .unwrap()[0],
        );
        let pkt = wrap(
            PacketType::Register,
            Register::signed(&second.signing, Some(cookie), NOW_US, "PC", &cands()),
        );
        assert_eq!(c.handle(b, &pkt, now, NOW_US), Err(Drop::Full));

        // ...but the host already there must always be able to keep its slot.
        let out = register(&mut c, &first, a, now);
        assert_eq!(
            parse_header(&out[0].bytes).unwrap().typ,
            PacketType::RegisterAck
        );
    }

    #[test]
    fn registrations_expire_and_are_swept() {
        let (mut c, now) = coord();
        let id = Identity::generate();
        let from: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        register(&mut c, &id, from, now);
        assert_eq!(c.registered_hosts(), 1);

        // Any later packet drives the sweep; use one that is dropped early so
        // only the GC can be responsible for the change.
        let later = now + REGISTRATION_TTL + Duration::from_secs(1);
        let _ = c.handle(from, &[0u8; 8], later, NOW_US);
        assert_eq!(c.registered_hosts(), 0);
    }

    #[test]
    fn a_host_that_moves_networks_updates_its_address() {
        let (mut c, now) = coord();
        let id = Identity::generate();
        let home: SocketAddr = "203.0.113.5:40000".parse().unwrap();
        let cafe: SocketAddr = "198.51.100.20:41000".parse().unwrap();
        register(&mut c, &id, home, now);
        let out = register(&mut c, &id, cafe, now);
        let ack = RegisterAck::decode(&out[0].bytes[HEADER_LEN..]).unwrap();
        assert_eq!(ack.observed, cafe);
        assert_eq!(c.registered_hosts(), 1, "it moved, it did not multiply");
    }

    #[test]
    fn the_probe_a_host_sends_back_is_well_formed() {
        // Guards the shape the client matches on: a probe carrying the nonce
        // the client chose is what lets it skip its retry timer.
        let probe = PunchProbe {
            host_id: [1u8; 32],
            nonce: [2u8; NONCE_LEN],
        };
        let raw = wrap(PacketType::PunchProbe, probe.encode());
        let h = parse_header(&raw).unwrap();
        assert_eq!(h.typ, PacketType::PunchProbe);
        assert!(h.typ.is_plaintext());
        assert_eq!(PunchProbe::decode(&raw[HEADER_LEN..]).unwrap(), probe);
    }
}
