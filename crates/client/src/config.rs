//! The client's settings and what it has learned about each PC, in
//! `~/Library/Application Support/BroLink/client.toml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FILE: &str = "client.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    /// 1920×1080
    P1080,
    /// 2560×1440
    P1440,
    /// 3840×2160
    P2160,
    /// This Mac's own screen, pixel for pixel. Exact with Apollo's virtual
    /// display; otherwise Sunshine scales the PC's monitor to fit.
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
    Auto,
    Hevc,
    Av1,
    H264,
}

impl Codec {
    pub fn moonlight(self) -> &'static str {
        match self {
            Codec::Auto => "auto",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
            Codec::H264 => "H.264",
        }
    }
}

/// How Moonlight is started. Everything here maps to one of its flags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamSettings {
    /// Relative mouse for games; off means the Mac cursor maps 1:1 onto the
    /// PC desktop.
    pub game_mode: bool,
    pub resolution: Resolution,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
    pub fullscreen: bool,
    /// The Sunshine app to launch; "Desktop" is the whole PC.
    pub app: String,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            game_mode: false,
            resolution: Resolution::P1440,
            fps: 60,
            bitrate_kbps: 30_000,
            codec: Codec::Auto,
            fullscreen: true,
            app: "Desktop".into(),
        }
    }
}

/// What the client needs to wake a PC that is not answering.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownPc {
    pub name: String,
    pub mac: Option<String>,
    pub lan_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub stream: StreamSettings,
    /// Offer to put the PC to sleep when a session ends.
    pub sleep_prompt: bool,
    /// Keyed by Tailscale node id.
    pub pcs: BTreeMap<String, KnownPc>,
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

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            stream: StreamSettings::default(),
            sleep_prompt: true,
            pcs: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane_and_native_uses_the_screen() {
        let s = StreamSettings::default();
        assert_eq!(s.resolution.pixels((3024, 1964)), (2560, 1440));
        assert_eq!(Resolution::Native.pixels((3024, 1964)), (3024, 1964));
        assert_eq!(Codec::Hevc.moonlight(), "HEVC");
        let c: ClientConfig = toml::from_str("").unwrap();
        assert_eq!(c.stream, StreamSettings::default());
        assert!(c.sleep_prompt);
    }

    #[test]
    fn known_pcs_round_trip() {
        let mut c = ClientConfig::default();
        c.pcs.insert(
            "nPC".into(),
            KnownPc {
                name: "Gaming-PC".into(),
                mac: Some("02:00:00:00:00:01".into()),
                lan_ip: Some("192.168.1.10".into()),
            },
        );
        let text = toml::to_string(&c).unwrap();
        let back: ClientConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
    }
}
