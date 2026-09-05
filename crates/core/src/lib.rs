//! What the BroLink host and client share: the control API they speak, the
//! Tailscale and download helpers, wake packets, config files, the icon.
//!
//! The picture itself never passes through BroLink. Sunshine on the PC and
//! Moonlight on the Mac do the streaming; Tailscale carries it. BroLink is
//! the part those three leave out: turning the PC on and off from the Mac,
//! pairing without touching the PC, and one click to get there.

pub mod api;
pub mod config;
pub mod download;
pub mod http;
pub mod icon;
pub mod tailscale;
pub mod wake;

pub const APP_NAME: &str = "BroLink";
/// TCP port of the host's control API, on loopback and on its Tailscale IP.
pub const CONTROL_PORT: u16 = 47850;
/// Sunshine's HTTP port (Moonlight talks to this one); its web UI is one up.
pub const SUNSHINE_PORT: u16 = 47989;
pub const SUNSHINE_WEB_PORT: u16 = 47990;
