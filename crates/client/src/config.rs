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

/// Who decides resolution, frame rate and bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Quality {
    /// BroLink picks them from the path to the PC at each connect: less
    /// over a relay or a long round trip, more on a LAN. See `path.rs`.
    #[default]
    Auto,
    /// The values in [`StreamSettings`], as set.
    Custom,
}

/// Three named points on the quality scale, for the toolbar menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Smooth,
    Balanced,
    Sharp,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Smooth, Preset::Balanced, Preset::Sharp];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Smooth => "Smooth",
            Preset::Balanced => "Balanced",
            Preset::Sharp => "Sharp",
        }
    }

    /// Resolution, frames per second, kilobits per second.
    pub fn values(self) -> (Resolution, u32, u32) {
        match self {
            Preset::Smooth => (Resolution::P1080, 30, 4_000),
            Preset::Balanced => (Resolution::P1080, 60, 10_000),
            Preset::Sharp => (Resolution::P1440, 60, 25_000),
        }
    }

    /// "1080p · 30 fps · 4 Mbps"
    pub fn describe(self) -> String {
        let (r, fps, kbps) = self.values();
        format!("{} · {fps} fps · {} Mbps", r.label(), kbps / 1000)
    }
}

impl Resolution {
    pub fn label(self) -> &'static str {
        match self {
            Resolution::P1080 => "1080p",
            Resolution::P1440 => "1440p",
            Resolution::P2160 => "4K",
            Resolution::Native => "This screen",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamSettings {
    pub quality: Quality,
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
            quality: Quality::Auto,
            resolution: Resolution::P1440,
            fps: 60,
            bitrate_kbps: 30_000,
            codec: Codec::Auto,
            app: "Desktop".into(),
            fullscreen: true,
        }
    }
}

impl StreamSettings {
    /// Switch to Custom with a preset's values.
    pub fn apply_preset(&mut self, p: Preset) {
        let (r, fps, kbps) = p.values();
        self.quality = Quality::Custom;
        self.resolution = r;
        self.fps = fps;
        self.bitrate_kbps = kbps;
    }

    /// The preset these values are, if they are one.
    pub fn preset(&self) -> Option<Preset> {
        Preset::ALL
            .into_iter()
            .find(|p| p.values() == (self.resolution, self.fps, self.bitrate_kbps))
    }

    /// "1080p · 30 fps · 4 Mbps"
    pub fn describe(&self) -> String {
        format!(
            "{} · {} fps · {} Mbps",
            self.resolution.label(),
            self.fps,
            self.bitrate_kbps / 1000
        )
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
    /// The PC's Tailscale address, so it can still be listed and reached
    /// when this Mac's Tailscale cannot say.
    pub tailscale_ip: Option<String>,
    /// When the PC was last seen online (Unix seconds).
    pub last_seen_unix: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub stream: StreamSettings,
    /// Offer to put the PC to sleep when a session ends. Off by default:
    /// asleep, Tailscale is asleep too, and the Mac can only wake the PC
    /// from that PC's own network.
    pub sleep_prompt: bool,
    /// The Mac's Command key acts as Ctrl on the PC (else as the Windows key).
    pub cmd_is_ctrl: bool,
    /// Install new releases of this app and send them to the PCs.
    pub auto_update: bool,
    /// A GitHub token for the release downloads, when git has none stored.
    pub github_token: Option<String>,
    /// Keyed by Tailscale node id.
    pub pcs: BTreeMap<String, KnownPc>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            stream: StreamSettings::default(),
            sleep_prompt: false,
            cmd_is_ctrl: true,
            auto_update: true,
            github_token: None,
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
        assert_eq!(c.stream.quality, Quality::Auto, "auto unless someone chose");
        assert!(!c.sleep_prompt && c.cmd_is_ctrl && c.auto_update);
        // A client.toml from 3.0 has no quality key; it gets Auto too.
        let c: ClientConfig = toml::from_str("[stream]\nfps = 90\n").unwrap();
        assert_eq!(c.stream.quality, Quality::Auto);
        assert_eq!(c.stream.fps, 90);
        let mut st = StreamSettings::default();
        assert_eq!(st.preset(), None);
        st.apply_preset(Preset::Smooth);
        assert_eq!(st.quality, Quality::Custom);
        assert_eq!(st.preset(), Some(Preset::Smooth));
        assert_eq!(st.describe(), "1080p · 30 fps · 4 Mbps");
        assert_eq!(Preset::Sharp.describe(), "1440p · 60 fps · 25 Mbps");
        let mut c = ClientConfig::default();
        c.pcs.insert(
            "n".into(),
            KnownPc {
                name: "Gaming-PC".into(),
                mac: Some("02:00:00:00:00:01".into()),
                lan_ip: Some("192.168.1.10".into()),
                public_ip: None,
                server_cert: Some("3082".into()),
                tailscale_ip: Some("100.64.0.10".into()),
                last_seen_unix: Some(1_788_739_200),
            },
        );
        let back: ClientConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }
}
