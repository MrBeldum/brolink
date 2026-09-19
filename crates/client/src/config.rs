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
    /// This choice applied to a screen of `native` pixels: the standard
    /// 16:9 sizes are exactly that, and "Match screen" is the screen
    /// itself. See `brolink_core::screens`.
    pub fn pixels(self, native: (u32, u32)) -> (u32, u32) {
        let [p1080, p1440, p2160] = brolink_core::screens::STANDARD;
        match self {
            Resolution::P1080 => p1080,
            Resolution::P1440 => p1440,
            Resolution::P2160 => p2160,
            Resolution::Native => brolink_core::screens::even(native),
        }
    }

    /// "1080p · 1920 × 1080", or "Match screen · 3024 × 1964".
    pub fn describe(self, native: (u32, u32)) -> String {
        let (w, h) = self.pixels(native);
        format!("{} · {w} × {h}", self.label())
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
    /// Recommended quality for this screen. Route latency is not bandwidth.
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
            Preset::Smooth => (Resolution::P1080, 60, 20_000),
            Preset::Balanced => (Resolution::Native, 60, 50_000),
            Preset::Sharp => (Resolution::Native, 60, 100_000),
        }
    }

    /// "1080p · 60 fps · 20 Mbps"
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
            Resolution::Native => "Match screen",
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
            resolution: Resolution::Native,
            fps: 60,
            bitrate_kbps: 50_000,
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

    /// "1080p · 60 fps · 20 Mbps"
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
    /// Tailscale OS string, so a remembered machine still shows Windows /
    /// macOS / Linux when Tailscale is down.
    #[serde(default)]
    pub os: String,
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
    /// A click on the picture captures the mouse: the cursor is hidden and
    /// raw movement goes to the PC, which is what games read. Off, the Mac
    /// cursor's position is sent instead and nothing is captured.
    pub capture_mouse: bool,
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
            capture_mouse: true,
            auto_update: true,
            github_token: None,
            pcs: BTreeMap::new(),
        }
    }
}

impl ClientConfig {
    pub fn load() -> Self {
        let mut c: Self = brolink_core::config::load(FILE);
        if c.stream.quality == Quality::Auto {
            c.stream.resolution = Resolution::Native;
        }
        c.normalise();
        c
    }

    /// Keep a hand-edited or older file within what the UI offers. The
    /// bitrate floor is the slider's 2 Mbps, under the Smooth preset's 4.
    fn normalise(&mut self) {
        self.stream.fps = self.stream.fps.clamp(30, 240);
        self.stream.bitrate_kbps = self.stream.bitrate_kbps.clamp(2_000, 150_000);
        if self.stream.app.trim().is_empty() {
            self.stream.app = "Desktop".into();
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        brolink_core::config::save(FILE, self)
    }

    pub fn forget_pin_on_mismatch(&mut self, node_id: &str, err: &anyhow::Error) -> bool {
        if !brolink_stream::nvhttp::is_pin_mismatch(err) {
            return false;
        }
        if let Some(pc) = self.pcs.get_mut(node_id) {
            pc.server_cert = None;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let s = StreamSettings::default();
        assert_eq!(s.resolution.pixels((3024, 1964)), (3024, 1964));
        assert_eq!(Resolution::Native.pixels((3024, 1964)), (3024, 1964));
        assert_eq!(Resolution::P1080.pixels((3024, 1964)), (1920, 1080));
        assert_eq!(Resolution::P1440.pixels((3024, 1964)), (2560, 1440));
        assert_eq!(Resolution::P2160.pixels((1920, 1080)), (3840, 2160));
        assert_eq!(
            Resolution::Native.describe((3024, 1964)),
            "Match screen · 3024 × 1964"
        );
        assert_eq!(
            Resolution::P1080.describe((3024, 1964)),
            "1080p · 1920 × 1080"
        );
        let modes = brolink_core::screens::stream_modes();
        for r in [
            Resolution::P1080,
            Resolution::P1440,
            Resolution::P2160,
            Resolution::Native,
        ] {
            for &screen in brolink_core::screens::SCREENS {
                assert!(
                    modes.contains(&r.pixels(screen)),
                    "{r:?} on {screen:?} is a listed mode"
                );
            }
        }
        let c: ClientConfig = toml::from_str("").unwrap();
        assert_eq!(c.stream, s);
        assert_eq!(c.stream.quality, Quality::Auto, "auto unless someone chose");
        assert!(!c.sleep_prompt && c.cmd_is_ctrl && c.auto_update);
        assert!(c.capture_mouse, "games need raw movement, so capture is on");
        // A client.toml from 3.0 has no quality key; it gets Auto too.
        let c: ClientConfig = toml::from_str("[stream]\nfps = 90\n").unwrap();
        assert_eq!(c.stream.quality, Quality::Auto);
        assert_eq!(c.stream.fps, 90);
        let mut st = StreamSettings::default();
        assert_eq!(st.preset(), Some(Preset::Balanced));
        st.apply_preset(Preset::Smooth);
        assert_eq!(st.quality, Quality::Custom);
        assert_eq!(st.preset(), Some(Preset::Smooth));
        assert_eq!(st.describe(), "1080p · 60 fps · 20 Mbps");
        assert_eq!(Preset::Sharp.describe(), "Match screen · 60 fps · 100 Mbps");
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
                ..Default::default()
            },
        );
        let back: ClientConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn every_preset_survives_a_reload() {
        for p in Preset::ALL {
            let mut c = ClientConfig::default();
            c.stream.apply_preset(p);
            if c.stream.quality == Quality::Auto {
                c.stream.resolution = Resolution::Native;
            }
            c.normalise();
            assert_eq!(c.stream.preset(), Some(p), "{p:?} was clamped away");
        }
        let mut c = ClientConfig::default();
        c.stream.bitrate_kbps = 500;
        c.stream.fps = 5;
        c.stream.app = "  ".into();
        if c.stream.quality == Quality::Auto {
            c.stream.resolution = Resolution::Native;
        }
        c.normalise();
        assert_eq!(c.stream.bitrate_kbps, 2_000);
        assert_eq!(c.stream.fps, 30);
        assert_eq!(c.stream.app, "Desktop");
    }

    #[test]
    fn pin_mismatch_clears_the_stored_server_cert() {
        let mut c = ClientConfig::default();
        c.pcs.insert(
            "n".into(),
            KnownPc {
                name: "Gaming-PC".into(),
                server_cert: Some("3082".into()),
                ..Default::default()
            },
        );
        let mismatch = anyhow::Error::from(brolink_stream::nvhttp::PinMismatch);
        assert!(c.forget_pin_on_mismatch("n", &mismatch));
        assert_eq!(c.pcs["n"].server_cert, None);

        c.pcs.get_mut("n").unwrap().server_cert = Some("3082".into());
        let generic = anyhow::anyhow!("invalid peer certificate: expired");
        assert!(!c.forget_pin_on_mismatch("n", &generic));
        assert_eq!(c.pcs["n"].server_cert.as_deref(), Some("3082"));
    }
}
