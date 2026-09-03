//! Shared UDP socket helpers and the optional relay encapsulation.

use crate::proto::MAX_DATAGRAM;
use anyhow::{Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{SocketAddr, UdpSocket as StdUdp};
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
    socket.set_nonblocking(true)?;
    let std: StdUdp = socket.into();
    UdpSocket::from_std(std).context("tokio udp")
}

pub fn bind_udp_ephemeral() -> Result<UdpSocket> {
    bind_udp("0.0.0.0:0".parse().expect("literal addr"))
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

/// A UDP socket that optionally speaks the relay's `token || payload` framing.
///
/// The relay strips the token before forwarding, so received datagrams are
/// already plain ForgeLink packets and the relay's own address stands in as the
/// peer address for the whole session. That keeps every caller above this layer
/// identical whether traffic is direct or tunnelled.
pub struct Transport {
    sock: UdpSocket,
    relay: Option<RelayLink>,
}

impl Transport {
    pub fn direct(sock: UdpSocket) -> Self {
        Self { sock, relay: None }
    }

    pub fn new(sock: UdpSocket, relay: Option<RelayLink>) -> Self {
        Self { sock, relay }
    }

    pub fn relay(&self) -> Option<RelayLink> {
        self.relay
    }

    pub fn set_relay(&mut self, relay: Option<RelayLink>) {
        self.relay = relay;
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    /// The raw socket, for callers that need it directly (STUN, discovery).
    pub fn socket(&self) -> &UdpSocket {
        &self.sock
    }

    fn is_relay(&self, target: SocketAddr) -> bool {
        self.relay.is_some_and(|r| r.addr == target)
    }

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
                self.sock.send_to(&framed[..16 + buf.len()], target).await
            }
            _ => self.sock.send_to(buf, target).await,
        }
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.sock.recv_from(buf).await
    }

    /// Register with (and hold open) the relay path. Sends an empty framed
    /// datagram, which the relay forwards as a zero-length packet the peer
    /// ignores. Cheap enough to send every few seconds.
    pub async fn relay_keepalive(&self) -> io::Result<()> {
        if let Some(r) = self.relay {
            self.sock.send_to(&r.token, r.addr).await?;
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
}
