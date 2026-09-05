//! Binary packet format for BroLink.
//!
//! Unencrypted header (12 bytes):
//!   magic[4] = b"BLK1"
//!   version[1]
//!   type[1]
//!   seq[4] little-endian
//!   flags[1]
//!   reserved[1]
//!
//! Payload follows. After handshake, payload is ChaCha20-Poly1305 ciphertext
//! (tag appended). Handshake Hello / HelloAck are plaintext.

use crate::crypto::SessionKeys;
use anyhow::{bail, Result};
use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};

pub const MAGIC: [u8; 4] = *b"BLK1";
pub const PROTO_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 12;
pub const DEFAULT_PORT: u16 = 47850;
/// Stay under IPv6/VPN MTU after UDP/IP/AEAD overhead.
pub const MAX_DATAGRAM: usize = 1200;
/// ChaCha20-Poly1305 tag appended to every sealed payload.
pub const AEAD_TAG_LEN: usize = 16;
pub const MAX_PAYLOAD: usize = MAX_DATAGRAM - HEADER_LEN - AEAD_TAG_LEN;
pub const MAX_FRAME_BYTES: usize = 2_000_000;
pub const VIDEO_CHUNK: usize = 1100;
/// frame_id u32 | frag_idx u16 | frag_count u16 | timestamp_us u64
pub const VIDEO_HEADER_LEN: usize = 16;
/// timestamp_us u64
pub const AUDIO_HEADER_LEN: usize = 8;

// A video fragment plus its payload header must still fit one sealed datagram.
const _: () = assert!(VIDEO_CHUNK + VIDEO_HEADER_LEN <= MAX_PAYLOAD);

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Hello = 1,
    HelloAck = 2,
    PairPin = 3,
    PairResult = 4,
    SessionReady = 5,
    Video = 6,
    Audio = 7,
    Input = 8,
    Control = 9,
    Ping = 10,
    Pong = 11,
    Goodbye = 12,
    Discovery = 13,
    // Rendezvous. These travel on the media socket so the mapping the
    // coordinator observes is the one media will actually arrive on, and they
    // are never sealed: there is no session with the coordinator.
    Register = 14,
    RegisterAck = 15,
    Lookup = 16,
    LookupAck = 17,
    Punch = 18,
    PunchProbe = 19,
    Retry = 20,
}

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Hello,
            2 => Self::HelloAck,
            3 => Self::PairPin,
            4 => Self::PairResult,
            5 => Self::SessionReady,
            6 => Self::Video,
            7 => Self::Audio,
            8 => Self::Input,
            9 => Self::Control,
            10 => Self::Ping,
            11 => Self::Pong,
            12 => Self::Goodbye,
            13 => Self::Discovery,
            14 => Self::Register,
            15 => Self::RegisterAck,
            16 => Self::Lookup,
            17 => Self::LookupAck,
            18 => Self::Punch,
            19 => Self::PunchProbe,
            20 => Self::Retry,
            _ => return None,
        })
    }

    pub fn is_handshake(self) -> bool {
        matches!(self, Self::Hello | Self::HelloAck | Self::Discovery)
    }

    /// Coordination traffic, which is signed rather than sealed and belongs to
    /// no session.
    pub fn is_rendezvous(self) -> bool {
        matches!(
            self,
            Self::Register
                | Self::RegisterAck
                | Self::Lookup
                | Self::LookupAck
                | Self::Punch
                | Self::PunchProbe
                | Self::Retry
        )
    }

    /// Carries an unsealed payload, and so is also exempt from the session
    /// replay window: these packets have no session sequence to replay.
    pub fn is_plaintext(self) -> bool {
        self.is_handshake() || self.is_rendezvous()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoFlags(pub u8);

impl VideoFlags {
    pub const KEYFRAME: Self = Self(1 << 0);
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
    pub fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported protocol version {0}")]
    Version(u8),
    #[error("unknown packet type {0}")]
    Type(u8),
    #[error("truncated packet")]
    Truncated,
    #[error("decrypt failed")]
    Decrypt,
    #[error("payload too large")]
    TooLarge,
    #[error("sequence space exhausted — reconnect for fresh keys")]
    SeqExhausted,
}

/// Monotonic outbound packet counter.
///
/// The AEAD nonce is derived from `(packet type, sequence)`, so a wrap would
/// reuse a nonce under the same key — the one failure mode that breaks
/// ChaCha20-Poly1305 outright. Rather than wrap, we refuse to send and the
/// session is torn down; reconnecting derives fresh keys.
#[derive(Debug, Default, Clone)]
pub struct SeqCounter(u32);

impl SeqCounter {
    pub fn new() -> Self {
        Self(0)
    }

    /// Named `next_seq` rather than `next` so it is never mistaken for the
    /// `Iterator` method — this one can fail, and callers must handle that.
    pub fn next_seq(&mut self) -> Result<u32, ProtocolError> {
        let next = self.0.checked_add(1).ok_or(ProtocolError::SeqExhausted)?;
        self.0 = next;
        Ok(next)
    }

    pub fn current(&self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct WireHeader {
    pub version: u8,
    pub typ: PacketType,
    pub seq: u32,
    pub flags: u8,
}

pub fn write_header(buf: &mut BytesMut, typ: PacketType, seq: u32, flags: u8) {
    buf.put_slice(&MAGIC);
    buf.put_u8(PROTO_VERSION);
    buf.put_u8(typ as u8);
    buf.put_u32_le(seq);
    buf.put_u8(flags);
    buf.put_u8(0);
}

pub fn parse_header(buf: &[u8]) -> Result<WireHeader, ProtocolError> {
    if buf.len() < HEADER_LEN {
        return Err(ProtocolError::Truncated);
    }
    if buf[0..4] != MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    let version = buf[4];
    if version != PROTO_VERSION {
        return Err(ProtocolError::Version(version));
    }
    let typ = PacketType::from_u8(buf[5]).ok_or(ProtocolError::Type(buf[5]))?;
    let seq = u32::from_le_bytes(buf[6..10].try_into().unwrap());
    Ok(WireHeader {
        version,
        typ,
        seq,
        flags: buf[10],
    })
}

pub fn seal(
    keys: &SessionKeys,
    typ: PacketType,
    seq: u32,
    flags: u8,
    plaintext: &[u8],
    out: &mut BytesMut,
) -> Result<()> {
    // Check before doing the crypto: an oversized payload is a caller bug, and
    // failing early keeps the error cheap and the message specific.
    if plaintext.len() > MAX_PAYLOAD {
        bail!(ProtocolError::TooLarge);
    }
    out.clear();
    out.reserve(HEADER_LEN + plaintext.len() + AEAD_TAG_LEN);
    write_header(out, typ, seq, flags);
    let ct = keys.seal(seq, typ as u8, plaintext)?;
    out.extend_from_slice(&ct);
    Ok(())
}

pub fn open<'a>(
    keys: &SessionKeys,
    buf: &'a [u8],
    scratch: &'a mut Vec<u8>,
) -> Result<(WireHeader, &'a [u8]), ProtocolError> {
    let header = parse_header(buf)?;
    if header.typ.is_plaintext() {
        return Ok((header, &buf[HEADER_LEN..]));
    }
    keys.open_into(header.seq, header.typ as u8, &buf[HEADER_LEN..], scratch)
        .map_err(|_| ProtocolError::Decrypt)?;
    Ok((header, scratch.as_slice()))
}

// ---------- handshake / control payloads (manual, compact) ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloMsg {
    pub client_id: [u8; 32],
    pub client_eph: [u8; 32],
    pub nonce: [u8; 16],
    pub name: String,
    pub app_version: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
    /// Client's own STUN reflexive address (`ip:port`), so the host can send
    /// a packet back and complete a UDP hole punch.
    #[serde(default)]
    pub client_wan: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub server_id: [u8; 32],
    pub server_eph: [u8; 32],
    pub nonce: [u8; 16],
    pub session_id: [u8; 16],
    pub needs_pin: bool,
    pub host_name: String,
    /// Ed25519 signature over client_eph || server_eph || both nonces.
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairPin {
    pub session_id: [u8; 16],
    pub pin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioFormat {
    PcmS16Le48kStereo = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReady {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: String,
    pub encoder: String,
    pub audio: AudioFormat,
    pub monitor_name: String,
    /// The PC's advertised name, so the client can label the saved entry.
    #[serde(default)]
    pub host_name: String,
    /// MAC of the host's LAN adapter, `aa:bb:cc:dd:ee:ff`, when the host could
    /// learn it. The client keeps it so it can wake the PC later.
    #[serde(default)]
    pub wake_mac: Option<String>,
    /// Whether the host will honour `ControlMsg::Power`.
    #[serde(default)]
    pub power_control: bool,
}

/// Remote power actions a paired client may ask the host to perform.
///
/// There is deliberately no "lock": the host runs in the user's session and
/// cannot capture or drive the secure desktop, so a locked PC could never be
/// unlocked again from the client.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PowerAction {
    /// Suspend to RAM. The recommended "off" state: Wake-on-LAN brings the PC
    /// back in seconds with the session intact.
    Sleep,
    Hibernate,
    Restart,
    Shutdown,
}

impl PowerAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sleep => "sleep",
            Self::Hibernate => "hibernate",
            Self::Restart => "restart",
            Self::Shutdown => "shut down",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMsg {
    RequestIdr,
    SetBitrate {
        kbps: u32,
    },
    SetFps {
        fps: u32,
    },
    Stats {
        loss_pct: f32,
        rtt_ms: f32,
        fps: f32,
    },
    ClientQuality {
        preset: String,
        bitrate_kbps: u32,
        fps: u32,
        width: u32,
        height: u32,
    },
    MouseCaptured {
        captured: bool,
        relative: bool,
    },
    /// Drop every key and button the host currently believes is held.
    ///
    /// Sent when the client releases mouse capture, loses focus, or
    /// disconnects, so a key held at that moment does not stay down on the
    /// remote machine.
    ReleaseAllInput,
    /// UTF-8 clipboard. Capped at [`crate::MAX_CLIPBOARD_CHARS`] characters;
    /// oversized pastes are dropped rather than split across datagrams.
    Clipboard {
        text: String,
    },
    /// Put the PC to sleep, hibernate, restart, or shut it down. The host ends
    /// the session first, then acts. Only honoured when the host owner has
    /// left remote power control enabled.
    Power {
        action: PowerAction,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMsg {
    pub events: Vec<InputEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputEvent {
    MouseMoveRel {
        dx: i32,
        dy: i32,
    },
    MouseMoveAbs {
        x: u16,
        y: u16,
    }, // 0..65535 virtual desktop
    MouseButton {
        button: u8,
        down: bool,
    },
    MouseWheel {
        dx: i32,
        dy: i32,
    },
    Key {
        scancode: u16,
        vk: u16,
        down: bool,
    },
    Gamepad {
        buttons: u16,
        lt: u8,
        rt: u8,
        lx: i16,
        ly: i16,
        rx: i16,
        ry: i16,
    },
}

pub fn json_payload<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(v)?)
}

/// Split input events into batches whose encoded form fits a single datagram.
///
/// A burst of mouse and gamepad events can easily exceed the MTU; without this
/// the whole batch would fail to seal and the input would be dropped on the
/// floor, which feels like the remote machine ignoring you.
pub fn chunk_input_events(events: Vec<InputEvent>) -> Vec<Vec<InputEvent>> {
    fn fits(events: &[InputEvent]) -> bool {
        serde_json::to_vec(&InputMsg {
            events: events.to_vec(),
        })
        .map(|v| v.len() <= MAX_PAYLOAD)
        .unwrap_or(false)
    }
    fn split(events: &[InputEvent], out: &mut Vec<Vec<InputEvent>>) {
        if events.is_empty() {
            return;
        }
        if events.len() == 1 || fits(events) {
            out.push(events.to_vec());
            return;
        }
        let mid = events.len() / 2;
        split(&events[..mid], out);
        split(&events[mid..], out);
    }
    let mut out = Vec::new();
    split(&events, &mut out);
    out
}

pub fn json_from_slice<T: for<'de> Deserialize<'de>>(b: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(b)?)
}

/// Drop a clipboard paste that cannot fit one datagram of JSON.
pub fn clipboard_or_skip(text: &str) -> Option<ControlMsg> {
    let text = text.trim_end_matches('\0');
    if text.is_empty() {
        return None;
    }
    let mut clipped: String = text.chars().take(crate::MAX_CLIPBOARD_CHARS).collect();
    loop {
        let msg = ControlMsg::Clipboard {
            text: clipped.clone(),
        };
        match json_payload(&msg) {
            Ok(v) if v.len() <= MAX_PAYLOAD => return Some(msg),
            _ => {
                let n = clipped.chars().count();
                if n <= 8 {
                    return None;
                }
                clipped = clipped.chars().take(n - 32).collect();
            }
        }
    }
}

/// Video payload (not JSON):
/// frame_id u32 | frag_idx u16 | frag_count u16 | timestamp_us u64 | data
pub fn write_video_payload(
    frame_id: u32,
    frag_idx: u16,
    frag_count: u16,
    timestamp_us: u64,
    data: &[u8],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + data.len());
    v.extend_from_slice(&frame_id.to_le_bytes());
    v.extend_from_slice(&frag_idx.to_le_bytes());
    v.extend_from_slice(&frag_count.to_le_bytes());
    v.extend_from_slice(&timestamp_us.to_le_bytes());
    v.extend_from_slice(data);
    v
}

pub fn parse_video_payload(buf: &[u8]) -> Result<(u32, u16, u16, u64, &[u8]), ProtocolError> {
    if buf.len() < 16 {
        return Err(ProtocolError::Truncated);
    }
    let frame_id = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let frag_idx = u16::from_le_bytes(buf[4..6].try_into().unwrap());
    let frag_count = u16::from_le_bytes(buf[6..8].try_into().unwrap());
    let ts = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    Ok((frame_id, frag_idx, frag_count, ts, &buf[16..]))
}

/// Audio payload: timestamp_us u64 | data
pub fn write_audio_payload(timestamp_us: u64, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + data.len());
    v.extend_from_slice(&timestamp_us.to_le_bytes());
    v.extend_from_slice(data);
    v
}

pub fn parse_audio_payload(buf: &[u8]) -> Result<(u64, &[u8]), ProtocolError> {
    if buf.len() < 8 {
        return Err(ProtocolError::Truncated);
    }
    let ts = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    Ok((ts, &buf[8..]))
}

pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

pub fn encode_plain(typ: PacketType, seq: u32, payload: &[u8]) -> BytesMut {
    let mut buf = BytesMut::with_capacity(HEADER_LEN + payload.len());
    write_header(&mut buf, typ, seq, 0);
    buf.extend_from_slice(payload);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let mut buf = BytesMut::new();
        write_header(&mut buf, PacketType::Ping, 42, 7);
        let h = parse_header(&buf).unwrap();
        assert_eq!(h.seq, 42);
        assert_eq!(h.typ, PacketType::Ping);
        assert_eq!(h.flags, 7);
    }

    #[test]
    fn video_payload_roundtrip() {
        let p = write_video_payload(9, 1, 3, 123, b"abc");
        let (id, i, n, ts, data) = parse_video_payload(&p).unwrap();
        assert_eq!((id, i, n, ts, data), (9, 1, 3, 123, &b"abc"[..]));
    }

    #[test]
    fn parse_header_rejects_junk() {
        assert!(matches!(parse_header(&[]), Err(ProtocolError::Truncated)));
        assert!(matches!(
            parse_header(&[0u8; HEADER_LEN - 1]),
            Err(ProtocolError::Truncated)
        ));
        let mut good = BytesMut::new();
        write_header(&mut good, PacketType::Video, 1, 0);
        let mut bad_magic = good.to_vec();
        bad_magic[0] = b'X';
        assert!(matches!(
            parse_header(&bad_magic),
            Err(ProtocolError::BadMagic)
        ));
        let mut bad_version = good.to_vec();
        bad_version[4] = 99;
        assert!(matches!(
            parse_header(&bad_version),
            Err(ProtocolError::Version(99))
        ));
        let mut bad_type = good.to_vec();
        bad_type[5] = 200;
        assert!(matches!(
            parse_header(&bad_type),
            Err(ProtocolError::Type(200))
        ));
    }

    #[test]
    fn truncated_payloads_error_instead_of_panicking() {
        for n in 0..VIDEO_HEADER_LEN {
            assert!(parse_video_payload(&vec![0u8; n]).is_err());
        }
        for n in 0..AUDIO_HEADER_LEN {
            assert!(parse_audio_payload(&vec![0u8; n]).is_err());
        }
        // Exactly the header, no data, is a valid (empty) fragment.
        assert!(parse_video_payload(&[0u8; VIDEO_HEADER_LEN]).is_ok());
        assert!(parse_audio_payload(&[0u8; AUDIO_HEADER_LEN]).is_ok());
    }

    #[test]
    fn seq_counter_refuses_to_wrap() {
        let mut c = SeqCounter::new();
        assert_eq!(c.next_seq().unwrap(), 1);
        assert_eq!(c.next_seq().unwrap(), 2);
        let mut c = SeqCounter(u32::MAX - 1);
        assert_eq!(c.next_seq().unwrap(), u32::MAX);
        assert!(matches!(c.next_seq(), Err(ProtocolError::SeqExhausted)));
        // And it stays exhausted rather than silently wrapping to 0.
        assert!(matches!(c.next_seq(), Err(ProtocolError::SeqExhausted)));
    }

    #[test]
    fn seal_rejects_oversized_payloads() {
        let e1 = crate::crypto::EphKey::generate();
        let e2 = crate::crypto::EphKey::generate();
        let n = [0u8; 16];
        let k = SessionKeys::derive(&e1.shared(&e2.public), &n, &n, false).unwrap();
        let mut out = BytesMut::new();
        assert!(seal(
            &k,
            PacketType::Input,
            1,
            0,
            &vec![0u8; MAX_PAYLOAD],
            &mut out
        )
        .is_ok());
        assert!(out.len() <= MAX_DATAGRAM);
        assert!(seal(
            &k,
            PacketType::Input,
            2,
            0,
            &vec![0u8; MAX_PAYLOAD + 1],
            &mut out
        )
        .is_err());
    }

    #[test]
    fn seal_open_roundtrip_through_the_wire_helpers() {
        let e1 = crate::crypto::EphKey::generate();
        let e2 = crate::crypto::EphKey::generate();
        let n1 = [1u8; 16];
        let n2 = [2u8; 16];
        let client = SessionKeys::derive(&e1.shared(&e2.public), &n1, &n2, false).unwrap();
        let host = SessionKeys::derive(&e2.shared(&e1.public), &n1, &n2, true).unwrap();
        let mut wire = BytesMut::new();
        seal(&client, PacketType::Input, 42, 3, b"press W", &mut wire).unwrap();
        let mut scratch = Vec::new();
        let (h, pt) = open(&host, &wire, &mut scratch).unwrap();
        assert_eq!(h.typ, PacketType::Input);
        assert_eq!(h.seq, 42);
        assert_eq!(h.flags, 3);
        assert_eq!(pt, b"press W");
    }

    #[test]
    fn a_full_video_fragment_fits_one_datagram() {
        let e1 = crate::crypto::EphKey::generate();
        let e2 = crate::crypto::EphKey::generate();
        let n = [0u8; 16];
        let k = SessionKeys::derive(&e1.shared(&e2.public), &n, &n, true).unwrap();
        let payload = write_video_payload(1, 0, 1, 0, &vec![0xAB; VIDEO_CHUNK]);
        let mut out = BytesMut::new();
        seal(&k, PacketType::Video, 1, 1, &payload, &mut out).unwrap();
        assert!(
            out.len() <= MAX_DATAGRAM,
            "video datagram {} > MTU budget",
            out.len()
        );
    }

    #[test]
    fn chunk_input_events_keeps_every_event_and_respects_the_mtu() {
        let events: Vec<InputEvent> = (0..500)
            .map(|i| InputEvent::MouseMoveRel { dx: i, dy: -i })
            .collect();
        let chunks = chunk_input_events(events.clone());
        assert!(chunks.len() > 1, "500 events should not fit one datagram");
        let flat: Vec<InputEvent> = chunks.iter().flatten().cloned().collect();
        assert_eq!(
            flat, events,
            "chunking must preserve order and drop nothing"
        );
        for c in &chunks {
            let n = json_payload(&InputMsg { events: c.clone() }).unwrap().len();
            assert!(
                n <= MAX_PAYLOAD,
                "chunk of {} events encodes to {n} bytes",
                c.len()
            );
        }
    }

    #[test]
    fn chunk_input_events_passes_small_batches_through_whole() {
        let events = vec![
            InputEvent::Key {
                scancode: 0x11,
                vk: 0x57,
                down: true,
            },
            InputEvent::MouseMoveRel { dx: 3, dy: 4 },
        ];
        let chunks = chunk_input_events(events.clone());
        assert_eq!(chunks, vec![events]);
        assert!(chunk_input_events(Vec::new()).is_empty());
    }

    #[test]
    fn clipboard_skips_empty_and_fits_the_mtu() {
        assert!(clipboard_or_skip("").is_none());
        assert!(clipboard_or_skip("   ").is_some());
        let msg = clipboard_or_skip("hello from the Mac").unwrap();
        match msg {
            ControlMsg::Clipboard { text } => assert_eq!(text, "hello from the Mac"),
            _ => panic!("wrong variant"),
        }
        let huge = "x".repeat(crate::MAX_CLIPBOARD_CHARS + 50);
        let msg = clipboard_or_skip(&huge).expect("oversize is truncated, not dropped");
        match msg {
            ControlMsg::Clipboard { text } => {
                assert!(text.chars().count() <= crate::MAX_CLIPBOARD_CHARS);
                assert!(
                    json_payload(&ControlMsg::Clipboard { text }).unwrap().len() <= MAX_PAYLOAD
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn session_ready_still_decodes_without_the_new_optional_fields() {
        // A v1.0 host does not send host_name, wake_mac, or power_control.
        let json = br#"{"width":1920,"height":1080,"fps":60,"bitrate_kbps":15000,"codec":"h264","encoder":"h264_nvenc","audio":"PcmS16Le48kStereo","monitor_name":"Display 0"}"#;
        let r: SessionReady = serde_json::from_slice(json).unwrap();
        assert!(r.host_name.is_empty());
        assert!(r.wake_mac.is_none());
        assert!(!r.power_control);
    }

    #[test]
    fn power_control_roundtrips_and_fits_a_datagram() {
        for action in [
            PowerAction::Sleep,
            PowerAction::Hibernate,
            PowerAction::Restart,
            PowerAction::Shutdown,
        ] {
            let msg = ControlMsg::Power { action };
            let bytes = json_payload(&msg).unwrap();
            assert!(bytes.len() <= MAX_PAYLOAD);
            match json_from_slice::<ControlMsg>(&bytes).unwrap() {
                ControlMsg::Power { action: back } => assert_eq!(back, action),
                other => panic!("wrong variant {other:?}"),
            }
        }
    }

    #[test]
    fn hello_still_decodes_without_the_new_optional_fields() {
        let json = br#"{"client_id":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"client_eph":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"nonce":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"name":"Mac","app_version":"1.0.0"}"#;
        let h: HelloMsg = serde_json::from_slice(json).unwrap();
        assert!(h.client_wan.is_none());
        assert_eq!(h.name, "Mac");
    }
}
