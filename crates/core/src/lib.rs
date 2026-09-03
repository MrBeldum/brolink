//! BroLink shared protocol, cryptography, tickets, STUN, and discovery.

pub mod abr;
pub mod codec;
pub mod config;
pub mod crypto;
pub mod discovery;
pub mod identity;
pub mod net;
pub mod proto;
pub mod stun;
pub mod ticket;
pub mod upnp;
pub mod yuv;

pub use config::{ClientConfig, HostConfig, QualityPreset, StreamQuality};
pub use identity::Identity;
pub use net::{RelayLink, Transport};
pub use proto::{
    AudioFormat, ControlMsg, HelloAck, HelloMsg, InputEvent, InputMsg, PacketType, PairPin,
    PairResult, ProtocolError, SeqCounter, SessionReady, VideoFlags, DEFAULT_PORT, HEADER_LEN,
    MAGIC, MAX_DATAGRAM, MAX_FRAME_BYTES, PROTO_VERSION,
};
pub use ticket::{Candidate, CandidateKind, RelayHint, Ticket};

pub const APP_NAME: &str = "BroLink";
pub const APP_ID: &str = "brolink";
pub const ALPN: &[u8] = b"brolink/1";
/// Clipboard payloads larger than this are truncated rather than fragmented.
/// Sized so the JSON form still fits one datagram.
pub const MAX_CLIPBOARD_CHARS: usize = 800;

/// Default UDP port for media, discovery, STUN keepalives, and pairing.
pub const fn default_port() -> u16 {
    DEFAULT_PORT
}
