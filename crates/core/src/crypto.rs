//! Identity-aware handshake and per-session AEAD.
//!
//! Hello / HelloAck carry X25519 ephemerals in the clear. From those we derive
//! two directional ChaCha20-Poly1305 keys. All later packets are sealed with
//! nonce = seq (little-endian, 12-byte padded).

use anyhow::{anyhow, Result};
use chacha20poly1305::aead::{Aead, AeadInPlace, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublic, StaticSecret};
use zeroize::Zeroize;

pub struct SessionKeys {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    /// 0 = we are the client (C->S send key first), 1 = we are the host.
    role: u8,
}

impl SessionKeys {
    pub fn derive(
        shared: &[u8; 32],
        client_nonce: &[u8; 16],
        server_nonce: &[u8; 16],
        is_host: bool,
    ) -> Result<Self> {
        let mut salt = [0u8; 32];
        salt[..16].copy_from_slice(client_nonce);
        salt[16..].copy_from_slice(server_nonce);
        let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
        let mut okm = [0u8; 64];
        hk.expand(b"brolink v1 session", &mut okm)
            .map_err(|_| anyhow!("hkdf expand"))?;
        let c2s = ChaCha20Poly1305::new(Key::from_slice(&okm[..32]));
        let s2c = ChaCha20Poly1305::new(Key::from_slice(&okm[32..]));
        okm.zeroize();
        Ok(if is_host {
            Self {
                send: s2c,
                recv: c2s,
                role: 1,
            }
        } else {
            Self {
                send: c2s,
                recv: s2c,
                role: 0,
            }
        })
    }

    fn nonce(seq: u32, typ: u8) -> Nonce {
        let mut n = [0u8; 12];
        n[0] = typ;
        n[4..8].copy_from_slice(&seq.to_le_bytes());
        Nonce::from(n)
    }

    pub fn seal(&self, seq: u32, typ: u8, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce = Self::nonce(seq, typ);
        let aad = [self.role, typ];
        self.send
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("encrypt failed"))
    }

    pub fn open(&self, seq: u32, typ: u8, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.open_into(seq, typ, ciphertext, &mut out)?;
        Ok(out)
    }

    /// Decrypt into a caller-owned buffer. The hot receive path calls this once
    /// per datagram, so it decrypts in place instead of allocating a fresh Vec
    /// and copying out of it.
    pub fn open_into(&self, seq: u32, typ: u8, ciphertext: &[u8], out: &mut Vec<u8>) -> Result<()> {
        let nonce = Self::nonce(seq, typ);
        // Peer AAD uses the opposite role byte.
        let aad = [1 - self.role, typ];
        out.clear();
        out.extend_from_slice(ciphertext);
        self.recv
            .decrypt_in_place(&nonce, &aad, out)
            .map_err(|_| anyhow!("decrypt failed"))
    }
}

pub struct EphKey {
    secret: StaticSecret,
    pub public: [u8; 32],
}

impl EphKey {
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = XPublic::from(&secret).to_bytes();
        Self { secret, public }
    }

    pub fn shared(&self, peer_pub: &[u8; 32]) -> [u8; 32] {
        let peer = XPublic::from(*peer_pub);
        self.secret.diffie_hellman(&peer).to_bytes()
    }
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::RngCore;
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

pub fn random_pin() -> String {
    use rand::Rng;
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

/// Sliding-window replay filter for packet sequence numbers.
///
/// Accepts anything newer than the highest sequence seen, and anything within
/// the last 64 sequences that has not been seen before. Everything else — a
/// duplicate, or a straggler more than 64 packets old — is rejected.
#[derive(Debug, Default)]
pub struct ReplayWindow {
    last: u64,
    mask: u64,
}

impl ReplayWindow {
    pub fn check_and_update(&mut self, seq: u32) -> bool {
        let seq = seq as u64;
        if seq + 64 <= self.last {
            return false;
        }
        if seq > self.last {
            let shift = seq - self.last;
            if shift >= 64 {
                self.mask = 1;
            } else {
                self.mask = (self.mask << shift) | 1;
            }
            self.last = seq;
            true
        } else {
            let bit = self.last - seq;
            let flag = 1u64 << bit;
            if self.mask & flag != 0 {
                return false;
            }
            self.mask |= flag;
            true
        }
    }
}

pub fn sign_handshake(
    identity: &ed25519_dalek::SigningKey,
    client_eph: &[u8; 32],
    server_eph: &[u8; 32],
    client_nonce: &[u8; 16],
    server_nonce: &[u8; 16],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    let mut msg = Vec::with_capacity(96);
    msg.extend_from_slice(client_eph);
    msg.extend_from_slice(server_eph);
    msg.extend_from_slice(client_nonce);
    msg.extend_from_slice(server_nonce);
    identity.sign(&msg).to_bytes().to_vec()
}

pub fn verify_handshake(
    server_id: &[u8; 32],
    signature: &[u8],
    client_eph: &[u8; 32],
    server_eph: &[u8; 32],
    client_nonce: &[u8; 16],
    server_nonce: &[u8; 16],
) -> Result<()> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let vk = VerifyingKey::from_bytes(server_id).map_err(|e| anyhow!(e))?;
    let sig_bytes: [u8; 64] = signature
        .try_into()
        .map_err(|_| anyhow!("bad signature length"))?;
    let sig = Signature::from_bytes(&sig_bytes);
    let mut msg = Vec::with_capacity(96);
    msg.extend_from_slice(client_eph);
    msg.extend_from_slice(server_eph);
    msg.extend_from_slice(client_nonce);
    msg.extend_from_slice(server_nonce);
    vk.verify(&msg, &sig)
        .map_err(|_| anyhow!("handshake signature"))?;
    Ok(())
}

pub fn constant_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut r = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        r |= x ^ y;
    }
    r == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aead_roundtrip() {
        let e1 = EphKey::generate();
        let e2 = EphKey::generate();
        let shared1 = e1.shared(&e2.public);
        let shared2 = e2.shared(&e1.public);
        assert_eq!(shared1, shared2);
        let n1 = random_bytes::<16>();
        let n2 = random_bytes::<16>();
        let c = SessionKeys::derive(&shared1, &n1, &n2, false).unwrap();
        let s = SessionKeys::derive(&shared2, &n1, &n2, true).unwrap();
        let ct = c.seal(7, 6, b"hello").unwrap();
        let pt = s.open(7, 6, &ct).unwrap();
        assert_eq!(pt, b"hello");
    }

    #[test]
    fn open_into_matches_open() {
        let e1 = EphKey::generate();
        let e2 = EphKey::generate();
        let n1 = random_bytes::<16>();
        let n2 = random_bytes::<16>();
        let c = SessionKeys::derive(&e1.shared(&e2.public), &n1, &n2, false).unwrap();
        let s = SessionKeys::derive(&e2.shared(&e1.public), &n1, &n2, true).unwrap();
        let ct = c.seal(11, 6, b"payload").unwrap();
        let mut scratch = vec![0xAA; 99]; // pre-dirtied: open_into must clear it
        s.open_into(11, 6, &ct, &mut scratch).unwrap();
        assert_eq!(scratch, b"payload");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let e1 = EphKey::generate();
        let e2 = EphKey::generate();
        let n1 = random_bytes::<16>();
        let n2 = random_bytes::<16>();
        let c = SessionKeys::derive(&e1.shared(&e2.public), &n1, &n2, false).unwrap();
        let s = SessionKeys::derive(&e2.shared(&e1.public), &n1, &n2, true).unwrap();
        let mut ct = c.seal(3, 6, b"payload").unwrap();
        ct[0] ^= 0x01;
        assert!(s.open(3, 6, &ct).is_err());
    }

    #[test]
    fn wrong_seq_or_type_is_rejected() {
        let e1 = EphKey::generate();
        let e2 = EphKey::generate();
        let n1 = random_bytes::<16>();
        let n2 = random_bytes::<16>();
        let c = SessionKeys::derive(&e1.shared(&e2.public), &n1, &n2, false).unwrap();
        let s = SessionKeys::derive(&e2.shared(&e1.public), &n1, &n2, true).unwrap();
        let ct = c.seal(3, 6, b"payload").unwrap();
        assert!(
            s.open(4, 6, &ct).is_err(),
            "seq is authenticated via the nonce"
        );
        assert!(
            s.open(3, 7, &ct).is_err(),
            "packet type is authenticated via AAD"
        );
    }

    #[test]
    fn a_peer_cannot_replay_our_own_packets_back_at_us() {
        // Directional keys mean the host must not accept a packet it sent.
        let e1 = EphKey::generate();
        let e2 = EphKey::generate();
        let n1 = random_bytes::<16>();
        let n2 = random_bytes::<16>();
        let host = SessionKeys::derive(&e2.shared(&e1.public), &n1, &n2, true).unwrap();
        let ct = host.seal(5, 6, b"video").unwrap();
        assert!(host.open(5, 6, &ct).is_err());
    }

    #[test]
    fn handshake_signature_is_bound_to_both_ephemerals_and_nonces() {
        let id = crate::identity::Identity::generate();
        let ce = random_bytes::<32>();
        let se = random_bytes::<32>();
        let cn = random_bytes::<16>();
        let sn = random_bytes::<16>();
        let sig = sign_handshake(&id.signing, &ce, &se, &cn, &sn);
        assert!(verify_handshake(&id.public, &sig, &ce, &se, &cn, &sn).is_ok());
        // Any substitution must fail.
        let other = random_bytes::<32>();
        assert!(verify_handshake(&id.public, &sig, &other, &se, &cn, &sn).is_err());
        assert!(verify_handshake(&id.public, &sig, &ce, &other, &cn, &sn).is_err());
        let othern = random_bytes::<16>();
        assert!(verify_handshake(&id.public, &sig, &ce, &se, &othern, &sn).is_err());
        // A different host identity must fail even with a well-formed signature.
        let attacker = crate::identity::Identity::generate();
        assert!(verify_handshake(&attacker.public, &sig, &ce, &se, &cn, &sn).is_err());
    }

    #[test]
    fn verify_handshake_rejects_malformed_signatures() {
        let id = crate::identity::Identity::generate();
        let z = [0u8; 32];
        let n = [0u8; 16];
        assert!(verify_handshake(&id.public, &[], &z, &z, &n, &n).is_err());
        assert!(verify_handshake(&id.public, &[0u8; 63], &z, &z, &n, &n).is_err());
        assert!(verify_handshake(&id.public, &[0u8; 65], &z, &z, &n, &n).is_err());
    }

    #[test]
    fn replay_rejects_duplicates() {
        let mut w = ReplayWindow::default();
        assert!(w.check_and_update(1));
        assert!(!w.check_and_update(1));
        assert!(w.check_and_update(2));
        assert!(w.check_and_update(10));
        assert!(!w.check_and_update(2));
    }

    #[test]
    fn replay_accepts_in_order_and_reordered_within_the_window() {
        let mut w = ReplayWindow::default();
        // Deliver 1..=200 in order, but hold back 160 as if the network
        // reordered it.
        for seq in 1..=200 {
            if seq == 160 {
                continue;
            }
            assert!(w.check_and_update(seq), "in-order seq {seq} must pass");
        }
        // The straggler is 40 behind the head, still inside the 64-wide window.
        assert!(
            w.check_and_update(160),
            "late-but-in-window packet must be accepted"
        );
        assert!(!w.check_and_update(160), "but only once");
        // Anything already delivered is a duplicate.
        assert!(!w.check_and_update(199));
        // And anything older than the window is dropped rather than trusted.
        assert!(!w.check_and_update(100));
    }

    #[test]
    fn replay_survives_a_large_forward_jump() {
        let mut w = ReplayWindow::default();
        assert!(w.check_and_update(1));
        assert!(w.check_and_update(1_000_000));
        assert!(!w.check_and_update(1_000_000));
        assert!(!w.check_and_update(1));
        assert!(w.check_and_update(1_000_001));
    }

    #[test]
    fn constant_eq_compares_contents() {
        assert!(constant_eq("123456", "123456"));
        assert!(!constant_eq("123456", "123457"));
        assert!(!constant_eq("123456", "12345"));
        assert!(constant_eq("", ""));
    }

    #[test]
    fn random_pin_is_six_digits() {
        for _ in 0..200 {
            let p = random_pin();
            assert_eq!(p.len(), 6);
            assert!(p.chars().all(|c| c.is_ascii_digit()));
        }
    }
}
