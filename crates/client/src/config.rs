//! Settings and what has been learned about each PC, in
//! `~/Library/Application Support/BroLink/client.toml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FILE: &str = "client.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    P1080,
    P1440,
    P2160,
    /// This screen's own pixel size.
    Native,
}

impl Resolution {
    pub fn pixels(self, native: (u32, u32)) -> (u32, u32) {
        match self {
            Resolution::P1080 => (1920, 1080),
            Resolution::P1440 => (2560, 1440),
            Resolution::P2160 => (3840, 2160),
            Resolution::Native => native,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    /// HEVC when the PC and this machine both decode it, else H.264.
    Auto,
    Hevc,
    H264,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamSettings {
    pub resolution: Resolution,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
    /// The Sunshine app to start; "Desktop" is the whole PC.
    pub app: String,
    pub fullscreen: bool,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::P1440,
            fps: 60,
            bitrate_kbps: 30_000,
            codec: Codec::Auto,
            app: "Desktop".into(),
            fullscreen: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownPc {
    pub name: String,
    pub mac: Option<String>,
    pub lan_ip: Option<String>,
    /// The PC's public address as Tailscale last saw it, for a router that
    /// forwards wake packets.
    pub public_ip: Option<String>,
    /// Sunshine's certificate (hex DER) from pairing; absent until paired.
    pub server_cert: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub stream: StreamSettings,
    /// Offer to put the PC to sleep when a session ends.
    pub sleep_prompt: bool,
    /// The Mac's Command key acts as Ctrl on the PC (else as the Windows key).
    pub cmd_is_ctrl: bool,
    /// Keyed by Tailscale node id.
    pub pcs: BTreeMap<String, KnownPc>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            stream: StreamSettings::default(),
            sleep_prompt: true,
            cmd_is_ctrl: true,
            pcs: BTreeMap::new(),
        }
    }
}

impl ClientConfig {
    pub fn load() -> Self {
        let mut c: Self = brolink_core::config::load(FILE);
        c.stream.fps = c.stream.fps.clamp(30, 240);
        c.stream.bitrate_kbps = c.stream.bitrate_kbps.clamp(5_000, 150_000);
        if c.stream.app.trim().is_empty() {
            c.stream.app = "Desktop".into();
        }
        c
    }

    pub fn save(&self) -> anyhow::Result<()> {
        brolink_core::config::save(FILE, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let s = StreamSettings::default();
        assert_eq!(s.resolution.pixels((3024, 1964)), (2560, 1440));
        assert_eq!(Resolution::Native.pixels((3024, 1964)), (3024, 1964));
        let c: ClientConfig = toml::from_str("").unwrap();
        assert_eq!(c.stream, s);
        assert!(c.sleep_prompt && c.cmd_is_ctrl);
        let mut c = ClientConfig::default();
        c.pcs.insert(
            "n".into(),
            KnownPc {
                name: "Gaming-PC".into(),
                mac: Some("02:00:00:00:00:01".into()),
                lan_ip: Some("192.168.1.10".into()),
                public_ip: None,
                server_cert: Some("3082".into()),
            },
        );
        let back: ClientConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
}
