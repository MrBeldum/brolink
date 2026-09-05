//! Shared UDP socket helpers and the optional relay encapsulation.

use crate::proto::MAX_DATAGRAM;
use anyhow::{Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{Ipv6Addr, SocketAddr, UdpSocket as StdUdp};
use tokio::net::UdpSocket;

/// Receive buffers must hold the largest datagram we ever accept. Relay frames
/// add a 16-byte token, and we want a little slack to notice oversized junk.
pub const RECV_BUF: usize = MAX_DATAGRAM + 64;

const SOCK_BUF_BYTES: usize = 4 * 1024 * 1024;

fn configure(socket: &Socket) {
    // All of these are best-effort: a kernel that clamps or refuses a large
    // socket buffer should still give us a working socket.
    let _ = socket.set_reuse_address(true);
    let _ = socket.set_recv_buffer_size(SOCK_BUF_BYTES);
    let _ = socket.set_send_buffer_size(SOCK_BUF_BYTES);
    let _ = socket.set_broadcast(true);
}

fn into_tokio(socket: Socket) -> Result<UdpSocket> {
    socket.set_nonblocking(true)?;
    let std: StdUdp = socket.into();
    UdpSocket::from_std(std).context("tokio udp")
}

pub fn bind_udp(addr: SocketAddr) -> Result<UdpSocket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    configure(&socket);
    socket
        .bind(&addr.into())
        .with_context(|| format!("bind UDP {addr}"))?;
    into_tokio(socket)
}

pub fn bind_udp_ephemeral() -> Result<UdpSocket> {
    bind_udp("0.0.0.0:0".parse().expect("literal addr"))
}

/// An IPv6-only socket on `port` (0 for any), to sit beside an IPv4 socket.
///
/// Explicitly v6-only: on Linux a v6 socket is dual-stack by default and would
/// collide with the v4 socket bound to the same port; on Windows it is v6-only
/// already. Saying so makes both behave the same.
pub fn bind_udp_v6_only(port: u16) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(true)?;
    configure(&socket);
    let addr = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));
    socket
        .bind(&addr.into())
        .with_context(|| format!("bind UDP {addr}"))?;
    into_tokio(socket)
}

/// A blocking socket for the discovery listener, which runs on its own thread.
///
/// `SO_REUSEADDR` matters here: the host and a client on the same machine both
/// want to sit on the discovery port, and without it the second one silently
/// falls back to an ephemeral port where no beacon will ever arrive.
pub fn bind_udp_blocking_reuse(addr: SocketAddr) -> Result<StdUdp> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    configure(&socket);
    #[cfg(unix)]
    {
        let _ = socket.set_reuse_port(true);
    }
    socket
        .bind(&addr.into())
        .with_context(|| format!("bind UDP {addr}"))?;
    Ok(socket.into())
}

/// Where a relay tunnel lives, and the token that identifies our peer pair.
#[derive(Debug, Clone, Copy)]
pub struct RelayLink {
    pub addr: SocketAddr,
    pub token: [u8; 16],
}

/// One logical UDP endpoint: an IPv4 socket, an optional IPv6 socket beside
/// it, and the relay's `token || payload` framing when a relay is in play.
///
/// Two sockets rather than one dual-stack socket because the two families
/// then behave identically on every platform: no v4-mapped addresses to
/// translate, no broadcast-through-a-v6-socket questions, and a host that
/// cannot get IPv6 just runs without the second socket.
///
/// The relay strips the token before forwarding, so received datagrams are
/// already plain BroLink packets and the relay's own address stands in as the
/// peer address for the whole session. That keeps every caller above this layer
/// identical whether traffic is direct or tunnelled.
pub struct Transport {
    v4: UdpSocket,
    v6: Option<UdpSocket>,
    relay: Option<RelayLink>,
}

impl Transport {
    pub fn direct(sock: UdpSocket) -> Self {
        Self {
            v4: sock,
            v6: None,
            relay: None,
        }
    }

    pub fn new(sock: UdpSocket, relay: Option<RelayLink>) -> Self {
        Self {
            v4: sock,
            v6: None,
            relay,
        }
    }

    /// Add an IPv6 socket so the ticket's global v6 candidates are reachable.
    pub fn with_v6(mut self, v6: Option<UdpSocket>) -> Self {
        self.v6 = v6;
        self
    }

    /// Bind an IPv4 socket on `v4_addr` and, best-effort, an IPv6 socket on the
    /// same port. When the v4 port was ephemeral and that port is taken on v6,
    /// the v6 socket takes any port: a client does not advertise its port.
    pub fn bind(v4_addr: SocketAddr, relay: Option<RelayLink>) -> Result<Self> {
        let v4 = bind_udp(v4_addr)?;
        let port = v4.local_addr().map(|a| a.port()).unwrap_or(0);
        let v6 = bind_udp_v6_only(port)
            .or_else(|e| {
                if v4_addr.port() == 0 {
                    bind_udp_v6_only(0)
                } else {
                    Err(e)
                }
            })
            .map_err(|e| tracing::info!("no IPv6 socket ({e:#}); IPv6 candidates will be skipped"))
            .ok();
        Ok(Self { v4, v6, relay })
    }

    pub fn relay(&self) -> Option<RelayLink> {
        self.relay
    }

    pub fn set_relay(&mut self, relay: Option<RelayLink>) {
        self.relay = relay;
    }

    pub fn has_v6(&self) -> bool {
        self.v6.is_some()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.v4.local_addr()
    }

    /// The raw IPv4 socket, for callers that need it directly (STUN, discovery).
    pub fn socket(&self) -> &UdpSocket {
        &self.v4
    }

    fn is_relay(&self, target: SocketAddr) -> bool {
        self.relay.is_some_and(|r| r.addr == target)
    }

    fn socket_for(&self, target: SocketAddr) -> io::Result<&UdpSocket> {
        match target {
            SocketAddr::V4(_) => Ok(&self.v4),
            SocketAddr::V6(_) => self.v6.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "no IPv6 socket on this machine",
                )
            }),
        }
    }

    /// Send a BroLink packet, framed for the relay when `target` is the relay.
    pub async fn send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        match self.relay {
            Some(r) if r.addr == target => {
                if buf.len() > MAX_DATAGRAM {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "datagram too large to relay",
                    ));
                }
                let mut framed = [0u8; 16 + MAX_DATAGRAM];
                framed[..16].copy_from_slice(&r.token);
                framed[16..16 + buf.len()].copy_from_slice(buf);
                self.socket_for(target)?
                    .send_to(&framed[..16 + buf.len()], target)
                    .await
            }
            _ => self.socket_for(target)?.send_to(buf, target).await,
        }
    }

    /// Send bytes exactly as given, never relay-framed. For traffic that is
    /// not part of a relayed session: rendezvous with the coordinator (which
    /// shares the relay's address) and Wake-on-LAN magic packets.
    pub async fn send_raw(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.socket_for(target)?.send_to(buf, target).await
    }

    /// Receive from whichever socket has a datagram first.
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let Some(v6) = self.v6.as_ref() else {
            return self.v4.recv_from(buf).await;
        };
        loop {
            // Wait for readiness rather than racing two `recv_from` futures,
            // which would each need the buffer.
            let sock = tokio::select! {
                r = self.v4.readable() => { r?; &self.v4 }
                r = v6.readable() => { r?; v6 }
            };
            match sock.try_recv_from(buf) {
                Ok(v) => return Ok(v),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Register with (and hold open) the relay path. Sends an empty framed
    /// datagram, which the relay forwards as a zero-length packet the peer
    /// ignores. Cheap enough to send every few seconds.
    pub async fn relay_keepalive(&self) -> io::Result<()> {
        if let Some(r) = self.relay {
            self.socket_for(r.addr)?.send_to(&r.token, r.addr).await?;
        }
        Ok(())
    }

    /// True when `addr` is a plausible source for session traffic. With a relay
    /// in play every packet arrives from the relay's address.
    pub fn accepts_from(&self, peer: SocketAddr, from: SocketAddr) -> bool {
        from == peer || self.is_relay(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relay_framing_prefixes_the_token_and_peer_sees_payload() {
        // Stand-in for the relay: receives token||payload.
        let relay = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let token = [0xABu8; 16];

        let client = Transport::new(
            bind_udp_ephemeral().unwrap(),
            Some(RelayLink {
                addr: relay_addr,
                token,
            }),
        );
        client.send_to(b"hello", relay_addr).await.unwrap();

        let mut buf = [0u8; RECV_BUF];
        let (n, _) = relay.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..16], &token);
        assert_eq!(&buf[16..n], b"hello");

        // send_raw goes to the same address without the token.
        client.send_raw(b"raw", relay_addr).await.unwrap();
        let (n, _) = relay.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"raw");
    }

    #[tokio::test]
    async fn direct_sends_are_not_framed() {
        let peer = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        let peer_addr = peer.local_addr().unwrap();
        let t = Transport::direct(bind_udp_ephemeral().unwrap());
        t.send_to(b"hello", peer_addr).await.unwrap();
        let mut buf = [0u8; RECV_BUF];
        let (n, _) = peer.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[tokio::test]
    async fn non_relay_targets_bypass_framing_even_when_a_relay_is_set() {
        let peer = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        let peer_addr = peer.local_addr().unwrap();
        let t = Transport::new(
            bind_udp_ephemeral().unwrap(),
            Some(RelayLink {
                addr: "127.0.0.1:1".parse().unwrap(),
                token: [1u8; 16],
            }),
        );
        t.send_to(b"direct", peer_addr).await.unwrap();
        let mut buf = [0u8; RECV_BUF];
        let (n, _) = peer.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"direct");
    }

    #[tokio::test]
    async fn oversized_relay_datagrams_are_rejected_not_truncated() {
        let t = Transport::new(
            bind_udp_ephemeral().unwrap(),
            Some(RelayLink {
                addr: "127.0.0.1:9".parse().unwrap(),
                token: [0u8; 16],
            }),
        );
        let big = vec![0u8; MAX_DATAGRAM + 1];
        let err = t
            .send_to(&big, "127.0.0.1:9".parse().unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn a_v6_target_without_a_v6_socket_is_an_error_not_a_panic() {
        let t = Transport::direct(bind_udp_ephemeral().unwrap());
        let err = t
            .send_to(b"x", "[::1]:9".parse().unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }

    #[tokio::test]
    async fn dual_stack_transport_receives_on_both_families() {
        // Skip quietly where the loopback has no IPv6.
        let Ok(t) = Transport::bind("127.0.0.1:0".parse().unwrap(), None) else {
            return;
        };
        if !t.has_v6() {
            return;
        }
        let port = t.local_addr().unwrap().port();
        let v4_peer = bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        let v6_peer = match bind_udp("[::1]:0".parse().unwrap()) {
            Ok(s) => s,
            Err(_) => return,
        };
        v4_peer
            .send_to(
                b"four",
                format!("127.0.0.1:{port}").parse::<SocketAddr>().unwrap(),
            )
            .await
            .unwrap();
        v6_peer
            .send_to(
                b"six",
                format!("[::1]:{port}").parse::<SocketAddr>().unwrap(),
            )
            .await
            .unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; RECV_BUF];
        for _ in 0..2 {
            let (n, from) =
                tokio::time::timeout(std::time::Duration::from_secs(2), t.recv_from(&mut buf))
                    .await
                    .expect("both datagrams arrive")
                    .unwrap();
            got.push((buf[..n].to_vec(), from.is_ipv6()));
            // And we can answer on the family the packet came from.
            t.send_to(b"ack", from).await.unwrap();
        }
        got.sort();
        assert_eq!(
            got,
            vec![(b"four".to_vec(), false), (b"six".to_vec(), true)]
        );
    }
}
