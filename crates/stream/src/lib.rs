//! BroLink's own Moonlight-protocol client. moonlight-common-c (compiled in
//! from `third_party/`) speaks the wire protocol; everything around it is
//! Rust: pairing and the HTTPS control API ([`nvhttp`]), the client identity
//! ([`identity`]), the crypto primitives the C code calls ([`crypto`]), video
//! decode ([`video`]), audio decode and playback ([`audio`]), and the session
//! that ties them together ([`session`]).

pub mod audio;
pub mod crypto;
pub mod ffi;
pub mod identity;
pub mod nvhttp;
pub mod session;
pub mod video;

pub use identity::Identity;
pub use nvhttp::{App, Client, ServerInfo};
pub use session::{Event, Input, Session, Settings, Stats};
pub use video::{Frame, FrameSlot};
