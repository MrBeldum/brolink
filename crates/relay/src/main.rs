//! Tiny UDP relay for two BroLink peers that cannot hole-punch.
//!
//! Wire format is `16-byte token || payload`. The first two distinct sources
//! sharing a token are paired; each subsequent datagram from one is forwarded
//! to the other with the token stripped, so peers speak the ordinary BroLink
//! protocol inside the tunnel and the relay never sees plaintext. It cannot:
//! everything inside the tunnel is already authenticated and encrypted
//! end-to-end, so a relay operator can drop or delay traffic but not read or
//! forge it.
//!
//! Run it on any VPS with a public UDP port:
//!
//! ```text
//! brolink-relay --bind 0.0.0.0:47851
//! ```
//!
//! then start the host with `--relay your.vps:47851`. The relay address and
//! token travel inside the ticket, so clients pick it up automatically.
//!
//! The same socket also serves the **rendezvous**: hosts register their key and
//! current address, clients look a host up and get introduced (see
//! `brolink_core::rendezvous`). Those packets arrive unframed and start with
//! the `BLK1` magic, which no relay token ever does, so one port does both.

mod coordinator;

use anyhow::{Context, Result};
use brolink_core::proto::{now_us, parse_header, MAGIC, MAX_DATAGRAM};
use clap::Parser;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tracing_subscriber::EnvFilter;

/// Token length in bytes; also the offset of the payload.
const TOKEN_LEN: usize = 16;
/// Drop a pair after this long with no traffic at all.
const SESSION_TTL: Duration = Duration::from_secs(120);
/// A peer slot this quiet may be taken over by a new address, so a NAT
/// rebinding mid-session reconnects instead of being ignored forever.
const PEER_IDLE: Duration = Duration::from_secs(25);
/// How often expired pairs are swept, independent of incoming traffic.
const GC_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Parser, Debug)]
#[command(
    name = "brolink-relay",
    version,
    about = "Optional UDP relay for BroLink when both peers are behind hard NAT"
)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:47851")]
    bind: String,
    /// Maximum concurrent token pairs held in memory.
    ///
    /// Every unrecognised token would otherwise allocate an entry, so a public
    /// relay without this cap can be exhausted by anyone sending random bytes.
    #[arg(long, default_value_t = 512)]
    max_sessions: usize,
    /// Maximum hosts registered with the rendezvous at once.
    #[arg(long, default_value_t = 10_000)]
    max_hosts: usize,
}

#[derive(Debug, Clone, Copy)]
struct Peer {
    addr: SocketAddr,
    last: Instant,
}

#[derive(Debug)]
struct Pair {
    a: Option<Peer>,
    b: Option<Peer>,
    last: Instant,
}

impl Pair {
    fn new(now: Instant) -> Self {
        Self {
            a: None,
            b: None,
            last: now,
        }
    }

    fn is_complete(&self) -> bool {
        self.a.is_some() && self.b.is_some()
    }
}

/// What to do with a datagram.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Send the payload on to this address.
    Forward(SocketAddr),
    /// Peer noted, but there is nobody to forward to (yet).
    Registered,
    /// Ignored, with the reason for the log.
    Dropped(&'static str),
}

struct Router {
    pairs: HashMap<[u8; TOKEN_LEN], Pair>,
    max_sessions: usize,
}

impl Router {
    fn new(max_sessions: usize) -> Self {
        Self {
            pairs: HashMap::new(),
            // A relay that can hold zero sessions is useless; treat 0 as 1.
            max_sessions: max_sessions.max(1),
        }
    }

    fn route(&mut self, token: [u8; TOKEN_LEN], from: SocketAddr, now: Instant) -> Route {
        if !self.pairs.contains_key(&token) && !self.make_room(now) {
            return Route::Dropped("relay full");
        }
        let pair = self.pairs.entry(token).or_insert_with(|| Pair::new(now));
        pair.last = now;

        let peer = Peer {
            addr: from,
            last: now,
        };
        // Refresh whichever slot this address already owns.
        if pair.a.is_some_and(|p| p.addr == from) {
            pair.a = Some(peer);
        } else if pair.b.is_some_and(|p| p.addr == from) {
            pair.b = Some(peer);
        } else if pair.a.is_none() {
            pair.a = Some(peer);
            tracing::info!("token {} peer A {from}", hex4(&token));
        } else if pair.b.is_none() {
            pair.b = Some(peer);
            tracing::info!("token {} peer B {from}", hex4(&token));
        } else {
            // Both slots are taken by other addresses. Only give one up if it
            // has gone quiet, which is what a NAT rebinding looks like.
            let a_idle = pair
                .a
                .is_some_and(|p| now.duration_since(p.last) > PEER_IDLE);
            let b_idle = pair
                .b
                .is_some_and(|p| now.duration_since(p.last) > PEER_IDLE);
            if a_idle {
                tracing::info!("token {} peer A rebound to {from}", hex4(&token));
                pair.a = Some(peer);
            } else if b_idle {
                tracing::info!("token {} peer B rebound to {from}", hex4(&token));
                pair.b = Some(peer);
            } else {
                return Route::Dropped("token already has two live peers");
            }
        }

        let other = if pair.a.is_some_and(|p| p.addr == from) {
            pair.b
        } else {
            pair.a
        };
        match other {
            Some(p) => Route::Forward(p.addr),
            None => Route::Registered,
        }
    }

    /// Drop everything that has expired.
    fn gc(&mut self, now: Instant) {
        self.pairs
            .retain(|_, p| now.duration_since(p.last) < SESSION_TTL);
    }

    /// Ensure there is space for one more pair. Returns false if there is not.
    fn make_room(&mut self, now: Instant) -> bool {
        if self.pairs.len() < self.max_sessions {
            return true;
        }
        self.gc(now);
        if self.pairs.len() < self.max_sessions {
            return true;
        }
        // Under a flood of random tokens the table fills with half-open pairs
        // that never found a partner. Evict the stalest of those before ever
        // touching a session that actually has two peers talking.
        let victim = self
            .pairs
            .iter()
            .filter(|(_, p)| !p.is_complete())
            .min_by_key(|(_, p)| p.last)
            .map(|(t, _)| *t);
        match victim {
            Some(t) => {
                self.pairs.remove(&t);
                true
            }
            // Everything is a live pair — refuse rather than cut someone off.
            None => false,
        }
    }
}

/// First four bytes of a token, for logs. Never the whole token: it is the
/// shared secret that authorises use of the tunnel.
fn hex4(token: &[u8]) -> String {
    token.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let args = Args::parse();

    let sock = tokio::net::UdpSocket::bind(&args.bind)
        .await
        .with_context(|| format!("bind UDP {}", args.bind))?;
    let listening = sock
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| args.bind.clone());
    tracing::info!(
        "BroLink relay + rendezvous listening on {listening} (max {} sessions, {} hosts)",
        args.max_sessions,
        args.max_hosts
    );

    let mut router = Router::new(args.max_sessions);
    let mut coord = coordinator::Coordinator::new(
        coordinator::Config {
            max_hosts: args.max_hosts,
            ..Default::default()
        },
        Instant::now(),
    );
    let mut buf = vec![0u8; TOKEN_LEN + MAX_DATAGRAM + 64];
    let mut gc = tokio::time::interval(GC_INTERVAL);
    gc.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut forwarded = 0u64;
    let mut bytes = 0u64;
    let (mut introduced, mut refused) = (0u64, 0u64);

    loop {
        tokio::select! {
            received = sock.recv_from(&mut buf) => {
                let (n, from) = match received {
                    Ok(v) => v,
                    // A destination-unreachable ICMP surfaces here on Windows;
                    // it says nothing about the socket's health.
                    Err(e) => {
                        tracing::debug!("recv: {e}");
                        continue;
                    }
                };
                if n < TOKEN_LEN {
                    continue;
                }
                if is_rendezvous(&buf[..n]) {
                    match coord.handle(from, &buf[..n], Instant::now(), now_us()) {
                        Ok(replies) => {
                            if replies.len() == 2 {
                                introduced += 1;
                            }
                            for r in &replies {
                                if let Err(e) = sock.send_to(&r.bytes, r.to).await {
                                    tracing::debug!("rendezvous send to {}: {e}", r.to);
                                }
                            }
                        }
                        // Signature and rate failures are the interesting ones:
                        // they are the shape an attack takes.
                        Err(why) => {
                            refused += 1;
                            tracing::debug!("rendezvous drop from {from}: {why:?}");
                        }
                    }
                    continue;
                }
                let mut token = [0u8; TOKEN_LEN];
                token.copy_from_slice(&buf[..TOKEN_LEN]);
                match router.route(token, from, Instant::now()) {
                    Route::Forward(dst) => {
                        let payload = &buf[TOKEN_LEN..n];
                        // An empty payload is a keepalive: it exists to hold the
                        // NAT mapping open, and forwarding it would hand the peer
                        // a zero-length datagram to choke on.
                        if payload.is_empty() {
                            continue;
                        }
                        if let Err(e) = sock.send_to(payload, dst).await {
                            tracing::debug!("forward to {dst}: {e}");
                            continue;
                        }
                        forwarded += 1;
                        bytes += payload.len() as u64;
                    }
                    Route::Registered => {}
                    Route::Dropped(why) => tracing::debug!("drop from {from}: {why}"),
                }
            }
            _ = gc.tick() => {
                let before = router.pairs.len();
                router.gc(Instant::now());
                tracing::debug!(
                    "{} sessions ({} expired), {forwarded} packets / {:.1} MiB forwarded; \
                     {} hosts registered, {introduced} introductions, {refused} refused",
                    router.pairs.len(),
                    before - router.pairs.len(),
                    bytes as f64 / (1024.0 * 1024.0),
                    coord.registered_hosts(),
                );
            }
        }
    }
}

/// Rendezvous requests arrive unframed and start with the protocol magic. A
/// relay frame starts with a 16-byte random token, which the host never lets
/// begin with the magic, so the first four bytes are enough to tell them apart.
fn is_rendezvous(pkt: &[u8]) -> bool {
    pkt.starts_with(&MAGIC) && parse_header(pkt).is_ok_and(|h| h.typ.is_rendezvous())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u16) -> SocketAddr {
        format!("127.0.0.1:{n}").parse().unwrap()
    }

    fn token(n: u8) -> [u8; TOKEN_LEN] {
        [n; TOKEN_LEN]
    }

    #[test]
    fn two_peers_sharing_a_token_are_wired_together() {
        let mut r = Router::new(8);
        let now = Instant::now();
        assert_eq!(r.route(token(1), addr(100), now), Route::Registered);
        // The second peer learns about the first immediately.
        assert_eq!(r.route(token(1), addr(200), now), Route::Forward(addr(100)));
        // And traffic flows both ways from then on.
        assert_eq!(r.route(token(1), addr(100), now), Route::Forward(addr(200)));
        assert_eq!(r.route(token(1), addr(200), now), Route::Forward(addr(100)));
    }

    #[test]
    fn different_tokens_never_cross() {
        let mut r = Router::new(8);
        let now = Instant::now();
        r.route(token(1), addr(100), now);
        r.route(token(1), addr(200), now);
        r.route(token(2), addr(300), now);
        assert_eq!(r.route(token(2), addr(400), now), Route::Forward(addr(300)));
        assert_eq!(r.route(token(1), addr(100), now), Route::Forward(addr(200)));
    }

    #[test]
    fn a_third_address_is_ignored_while_both_peers_are_live() {
        let mut r = Router::new(8);
        let now = Instant::now();
        r.route(token(1), addr(100), now);
        r.route(token(1), addr(200), now);
        assert_eq!(
            r.route(token(1), addr(999), now),
            Route::Dropped("token already has two live peers")
        );
        // The intruder did not displace anyone.
        assert_eq!(r.route(token(1), addr(100), now), Route::Forward(addr(200)));
    }

    #[test]
    fn a_quiet_peer_slot_can_be_rebound_after_a_nat_change() {
        let mut r = Router::new(8);
        let start = Instant::now();
        r.route(token(1), addr(100), start);
        r.route(token(1), addr(200), start);
        // Peer B keeps talking; peer A goes silent past the idle window.
        let later = start + PEER_IDLE + Duration::from_secs(1);
        r.route(token(1), addr(200), later);
        // A reappears from a new port, as a NAT rebinding would look.
        assert_eq!(
            r.route(token(1), addr(101), later),
            Route::Forward(addr(200))
        );
        assert_eq!(
            r.route(token(1), addr(200), later),
            Route::Forward(addr(101))
        );
    }

    #[test]
    fn expired_pairs_are_swept() {
        let mut r = Router::new(8);
        let start = Instant::now();
        r.route(token(1), addr(100), start);
        r.route(token(2), addr(200), start);
        let fresh = start + SESSION_TTL - Duration::from_secs(1);
        r.route(token(2), addr(200), fresh);
        r.gc(start + SESSION_TTL + Duration::from_secs(1));
        assert!(!r.pairs.contains_key(&token(1)), "idle pair dropped");
        assert!(r.pairs.contains_key(&token(2)), "recently active pair kept");
    }

    #[test]
    fn the_session_table_is_bounded() {
        let mut r = Router::new(4);
        let now = Instant::now();
        for i in 0..200u8 {
            r.route(token(i), addr(1000 + i as u16), now);
        }
        assert!(
            r.pairs.len() <= 4,
            "a flood of random tokens must not grow the table: {}",
            r.pairs.len()
        );
    }

    #[test]
    fn a_flood_evicts_half_open_pairs_before_live_sessions() {
        let mut r = Router::new(3);
        let now = Instant::now();
        // One real session, using two of the three slots' worth of capacity.
        r.route(token(0), addr(100), now);
        r.route(token(0), addr(200), now);
        assert!(r.pairs[&token(0)].is_complete());

        // Now flood with tokens nobody answers.
        for i in 1..100u8 {
            r.route(
                token(i),
                addr(1000 + i as u16),
                now + Duration::from_millis(i as u64),
            );
        }
        assert!(
            r.pairs.contains_key(&token(0)),
            "the live session survived the flood"
        );
        assert_eq!(
            r.route(token(0), addr(100), now),
            Route::Forward(addr(200)),
            "and still routes"
        );
    }

    #[test]
    fn a_full_table_of_live_sessions_refuses_new_tokens() {
        let mut r = Router::new(2);
        let now = Instant::now();
        for t in [0u8, 1] {
            r.route(token(t), addr(100 + t as u16), now);
            r.route(token(t), addr(200 + t as u16), now);
        }
        assert_eq!(
            r.route(token(9), addr(900), now),
            Route::Dropped("relay full"),
            "established sessions are never cut off for a newcomer"
        );
    }

    #[test]
    fn a_full_table_still_routes_known_tokens() {
        let mut r = Router::new(1);
        let now = Instant::now();
        r.route(token(0), addr(100), now);
        r.route(token(0), addr(200), now);
        assert_eq!(r.route(token(0), addr(100), now), Route::Forward(addr(200)));
    }

    #[test]
    fn capacity_is_never_zero() {
        let mut r = Router::new(0);
        let now = Instant::now();
        assert_eq!(r.route(token(1), addr(100), now), Route::Registered);
    }

    #[test]
    fn tokens_are_logged_as_short_hex() {
        assert_eq!(hex4(&[0x0a, 0xbc, 0xde, 0xf0, 0xff]), "0abcdef0");
    }

    #[test]
    fn rendezvous_and_relay_frames_are_told_apart_by_the_magic() {
        use brolink_core::proto::{encode_plain, PacketType};
        let lookup = encode_plain(PacketType::Lookup, 0, &[0u8; 40]);
        assert!(is_rendezvous(&lookup));
        // A media packet with the magic is not a rendezvous request either.
        let hello = encode_plain(PacketType::Hello, 0, b"{}");
        assert!(!is_rendezvous(&hello));
        // A relay frame: random token then a BroLink packet.
        let mut framed = vec![0x5Au8; TOKEN_LEN];
        framed.extend_from_slice(&lookup);
        assert!(!is_rendezvous(&framed));
        assert!(!is_rendezvous(&[]));
    }
}
