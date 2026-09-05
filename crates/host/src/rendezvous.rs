//! Keep this host registered with the coordinator, and answer its punches.
//!
//! The coordinator shares the relay's address, so everything here goes out
//! unframed with `send_raw`; the relay tells the two apart by the magic.

use brolink_core::identity::Identity;
use brolink_core::net::{RelayLink, Transport};
use brolink_core::proto::{encode_plain, now_us, PacketType, WireHeader};
use brolink_core::rendezvous::{
    Punch, PunchProbe, Register, RegisterAck, Retry, COOKIE_LEN, REGISTER_INTERVAL,
};
use brolink_core::ticket::Candidate;
use std::net::SocketAddr;
use std::time::Instant;

pub struct Rendezvous {
    link: RelayLink,
    cookie: Option<[u8; COOKIE_LEN]>,
    last_register: Instant,
    /// Our address as the coordinator sees it: the live WAN mapping.
    pub observed: Option<SocketAddr>,
}

impl Rendezvous {
    pub fn new(link: RelayLink) -> Self {
        Self {
            link,
            cookie: None,
            last_register: Instant::now() - REGISTER_INTERVAL,
            observed: None,
        }
    }

    pub fn is_coordinator(&self, from: SocketAddr) -> bool {
        from == self.link.addr
    }

    /// Refresh the registration when it is due.
    pub async fn tick(&mut self, t: &Transport, id: &Identity, name: &str, cands: &[Candidate]) {
        if self.last_register.elapsed() >= REGISTER_INTERVAL {
            self.register(t, id, name, cands).await;
        }
    }

    async fn register(&mut self, t: &Transport, id: &Identity, name: &str, cands: &[Candidate]) {
        self.last_register = Instant::now();
        let body = Register::signed(&id.signing, self.cookie, now_us(), name, cands);
        let pkt = encode_plain(PacketType::Register, 0, &body);
        if let Err(e) = t.send_raw(&pkt, self.link.addr).await {
            tracing::debug!("register with {}: {e}", self.link.addr);
        }
    }

    /// Handle a packet from the coordinator. Returns the observed address the
    /// first time it is learned or whenever it changes.
    pub async fn on_packet(
        &mut self,
        t: &Transport,
        id: &Identity,
        name: &str,
        cands: &[Candidate],
        header: &WireHeader,
        payload: &[u8],
    ) -> Option<SocketAddr> {
        match header.typ {
            PacketType::Retry => {
                self.cookie = Some(Retry::decode(payload).ok()?.cookie);
                self.register(t, id, name, cands).await;
                None
            }
            PacketType::RegisterAck => {
                let ack = RegisterAck::decode(payload).ok()?;
                if self.observed == Some(ack.observed) {
                    return None;
                }
                self.observed = Some(ack.observed);
                Some(ack.observed)
            }
            PacketType::Punch => {
                let punch = Punch::decode(payload).ok()?;
                let probe = PunchProbe {
                    host_id: id.public,
                    nonce: punch.nonce,
                };
                let pkt = encode_plain(PacketType::PunchProbe, 0, &probe.encode());
                // Two, a moment apart: the first opens our NAT, and if it is
                // lost on the way the second still tells the client which
                // path is live.
                for _ in 0..2 {
                    if let Err(e) = t.send_raw(&pkt, punch.client_addr).await {
                        tracing::debug!("punch probe to {}: {e}", punch.client_addr);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                tracing::info!("punched towards {}", punch.client_addr);
                None
            }
            _ => None,
        }
    }
}
