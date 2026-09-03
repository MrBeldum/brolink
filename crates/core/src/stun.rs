//! Minimal STUN Binding client (RFC 5389) used to learn our reflexive address.

use anyhow::{anyhow, bail, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

const MAGIC_COOKIE: u32 = 0x2112A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const MAPPED_ADDRESS: u16 = 0x0001;

pub const GOOGLE_STUN: &str = "stun.l.google.com:19302";
pub const CLOUDFLARE_STUN: &str = "stun.cloudflare.com:3478";

pub async fn reflexive_addr(sock: &UdpSocket, stun_server: &str) -> Result<SocketAddr> {
    let server: SocketAddr = tokio::net::lookup_host(stun_server)
        .await?
        .next()
        .ok_or_else(|| anyhow!("stun lookup failed for {stun_server}"))?;

    let mut txn = [0u8; 12];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut txn);

    let mut req = [0u8; 20];
    req[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    req[2..4].copy_from_slice(&0u16.to_be_bytes());
    req[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    req[8..20].copy_from_slice(&txn);

    sock.send_to(&req, server).await?;

    let mut buf = [0u8; 1500];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("STUN timeout talking to {stun_server}");
        }
        let n = match tokio::time::timeout(remaining, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => n,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => bail!("STUN timeout talking to {stun_server}"),
        };
        if n < 20 {
            continue;
        }
        if buf[8..20] != txn {
            continue;
        }
        let msg_type = u16::from_be_bytes([buf[0], buf[1]]);
        if msg_type != BINDING_SUCCESS {
            continue;
        }
        return parse_mapped(&buf[..n]);
    }
}

fn parse_mapped(msg: &[u8]) -> Result<SocketAddr> {
    let length = u16::from_be_bytes([msg[2], msg[3]]) as usize;
    let end = 20 + length;
    if msg.len() < end {
        bail!("truncated STUN");
    }
    let mut i = 20;
    while i + 4 <= end {
        let atype = u16::from_be_bytes([msg[i], msg[i + 1]]);
        let alen = u16::from_be_bytes([msg[i + 2], msg[i + 3]]) as usize;
        let val_start = i + 4;
        let val_end = val_start + alen;
        if val_end > end {
            bail!("truncated STUN attr");
        }
        let val = &msg[val_start..val_end];
        if atype == XOR_MAPPED_ADDRESS {
            return decode_address(val, true);
        }
        if atype == MAPPED_ADDRESS {
            return decode_address(val, false);
        }
        i = val_end;
        // 4-byte padding
        if i % 4 != 0 {
            i += 4 - (i % 4);
        }
    }
    bail!("STUN response had no mapped address")
}

fn decode_address(val: &[u8], xor: bool) -> Result<SocketAddr> {
    if val.len() < 8 {
        bail!("short address attr");
    }
    let family = val[1];
    let mut port = u16::from_be_bytes([val[2], val[3]]);
    if xor {
        port ^= (MAGIC_COOKIE >> 16) as u16;
    }
    match family {
        0x01 => {
            if val.len() < 8 {
                bail!("short v4");
            }
            let mut ip = [val[4], val[5], val[6], val[7]];
            if xor {
                let cookie = MAGIC_COOKIE.to_be_bytes();
                for i in 0..4 {
                    ip[i] ^= cookie[i];
                }
            }
            Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port))
        }
        _ => bail!("unsupported STUN address family {family}"),
    }
}

/// Best-effort: try Google then Cloudflare.
pub async fn discover_wan(sock: &UdpSocket) -> Option<SocketAddr> {
    match reflexive_addr(sock, GOOGLE_STUN).await {
        Ok(a) => return Some(a),
        Err(e) => tracing::warn!("google STUN failed: {e}"),
    }
    match reflexive_addr(sock, CLOUDFLARE_STUN).await {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::warn!("cloudflare STUN failed: {e}");
            None
        }
    }
}
