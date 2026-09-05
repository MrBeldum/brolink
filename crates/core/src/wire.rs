//! Bounds-checked binary reader/writer shared by tickets and rendezvous.
//!
//! Both formats are compact binary blobs parsed from untrusted input: a
//! ticket a user pasted, or a datagram from the internet. Every read is
//! length-checked so malformed input returns an error instead of panicking.

use anyhow::{anyhow, bail, Result};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

pub(crate) fn write_v4(buf: &mut Vec<u8>, addr: SocketAddr) {
    let SocketAddr::V4(v4) = addr else {
        return;
    };
    buf.extend_from_slice(&v4.ip().octets());
    buf.extend_from_slice(&v4.port().to_le_bytes());
}

/// Family-tagged address: `4 | ip[4] | port` or `6 | ip[16] | port`.
pub(crate) fn write_addr(buf: &mut Vec<u8>, addr: SocketAddr) {
    match addr {
        SocketAddr::V4(v4) => {
            buf.push(4);
            buf.extend_from_slice(&v4.ip().octets());
            buf.extend_from_slice(&v4.port().to_le_bytes());
        }
        SocketAddr::V6(v6) => {
            buf.push(6);
            buf.extend_from_slice(&v6.ip().octets());
            buf.extend_from_slice(&v6.port().to_le_bytes());
        }
    }
}

pub(crate) fn read_addr(cur: &mut Cursor<'_>) -> Result<SocketAddr> {
    match cur.u8()? {
        4 => {
            let ip = cur.take(4)?;
            let ip = Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]);
            let port = cur.u16()?;
            Ok(SocketAddr::from((ip, port)))
        }
        6 => {
            let b = cur.take(16)?;
            let mut oct = [0u8; 16];
            oct.copy_from_slice(b);
            let port = cur.u16()?;
            Ok(SocketAddr::from((Ipv6Addr::from(oct), port)))
        }
        f => bail!("unknown address family {f}"),
    }
}

/// Bounds-checked reader so malformed input errors instead of panicking.
pub(crate) struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| anyhow!("input truncated"))?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn take_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Bytes consumed so far, used to sign or verify a prefix of the buffer.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_roundtrips_both_families() {
        for s in ["1.2.3.4:47850", "[2001:db8::1]:47851"] {
            let addr: SocketAddr = s.parse().unwrap();
            let mut buf = Vec::new();
            write_addr(&mut buf, addr);
            let mut cur = Cursor::new(&buf);
            assert_eq!(read_addr(&mut cur).unwrap(), addr);
        }
    }

    #[test]
    fn truncated_input_errors_instead_of_panicking() {
        let mut buf = Vec::new();
        write_addr(&mut buf, "1.2.3.4:47850".parse().unwrap());
        buf.pop();
        let mut cur = Cursor::new(&buf);
        assert!(read_addr(&mut cur).is_err());
    }

    #[test]
    fn unknown_family_errors() {
        let mut cur = Cursor::new(&[9u8, 0, 0, 0, 0, 0, 0]);
        assert!(read_addr(&mut cur).is_err());
    }
}
