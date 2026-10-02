//! What Latch nodes share: the control API they speak, the Tailscale CLI,
//! wake packets, config files, the icon.
//!
//! The picture never passes through here. `latch-stream` speaks the
//! GameStream protocol to the streaming engine and Tailscale carries it;
//! this crate holds the rest: finding machines, waking them, pairing
//! without touching them, turning them off again, and fetching new releases.

pub mod api;
pub mod config;
pub mod dates;
pub mod http;
pub mod icon;
pub mod legacy;
pub mod screens;
pub mod tailscale;
pub mod update;
pub mod wake;

pub const APP_NAME: &str = "Latch";
/// `Status::app` of a Latch node, so a scan can tell one from any other
/// service that answers on the control port.
pub const APP_ID: &str = "latch";
/// `Streamer::kind` of the streaming engine Latch itself set up, as opposed
/// to a Sunshine or Apollo it found installed.
pub const STREAMER_KIND: &str = "Latch";
/// TCP port of the host's control API, on loopback and on its Tailscale IP.
pub const CONTROL_PORT: u16 = 47850;
/// TCP port a Mac serves a Latch Host install from, on its Tailscale IP,
/// for a PC whose host is too old to take `/v1/update`.
pub const HANDOVER_PORT: u16 = 47851;
/// Sunshine's HTTP port (pairing and launch); its web UI is one up.
pub const SUNSHINE_PORT: u16 = 47989;
pub const SUNSHINE_WEB_PORT: u16 = 47990;
