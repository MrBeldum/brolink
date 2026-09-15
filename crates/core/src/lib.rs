//! What the BroLink host and client share: the control API they speak, the
//! Tailscale CLI, wake packets, config files, the icon.
//!
//! The picture never passes through here. `brolink-stream` speaks the
//! GameStream protocol to Sunshine on the PC and Tailscale carries it; this
//! crate holds the rest: finding the PC, waking it, pairing without touching
//! it, turning it off again, and fetching new releases.

pub mod api;
pub mod config;
pub mod dates;
pub mod http;
pub mod icon;
pub mod screens;
pub mod tailscale;
pub mod update;
pub mod wake;

pub const APP_NAME: &str = "BroLink";
/// TCP port of the host's control API, on loopback and on its Tailscale IP.
pub const CONTROL_PORT: u16 = 47850;
/// TCP port a Mac serves a BroLink Host install from, on its Tailscale IP,
/// for a PC whose host is too old to take `/v1/update`.
pub const HANDOVER_PORT: u16 = 47851;
/// Sunshine's HTTP port (pairing and launch); its web UI is one up.
pub const SUNSHINE_PORT: u16 = 47989;
pub const SUNSHINE_WEB_PORT: u16 = 47990;
