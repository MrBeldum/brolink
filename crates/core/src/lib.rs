//! ForgeLink shared protocol, cryptography, tickets, STUN, and discovery.

pub mod codec;
pub mod config;
pub mod crypto;
pub mod discovery;
pub mod identity;
pub mod net;
pub mod proto;
pub mod stun;
pub mod ticket;
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

pub const APP_NAME: &str = "ForgeLink";
pub const APP_ID: &str = "forgelink";
pub const ALPN: &[u8] = b"forgelink/1";

/// Default UDP port for media, discovery, STUN keepalives, and pairing.
pub const fn default_port() -> u16 {
    DEFAULT_PORT
}
