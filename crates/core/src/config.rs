//! Host and client configuration.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::identity::data_dir;
use crate::proto::DEFAULT_PORT;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QualityPreset {
    Competitive,
    Balanced,
    Quality,
    Custom,
}

impl QualityPreset {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Competitive => "Competitive",
            Self::Balanced => "Balanced",
            Self::Quality => "Quality",
            Self::Custom => "Custom",
        }
    }

    pub fn all() -> [Self; 4] {
        [
            Self::Competitive,
            Self::Balanced,
            Self::Quality,
            Self::Custom,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamQuality {
    pub preset: QualityPreset,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

impl StreamQuality {
    pub fn competitive() -> Self {
        Self {
            preset: QualityPreset::Competitive,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 15_000,
        }
    }
    pub fn balanced() -> Self {
        Self {
            preset: QualityPreset::Balanced,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 25_000,
        }
    }
    pub fn quality() -> Self {
        Self {
            preset: QualityPreset::Quality,
            width: 2560,
            height: 1440,
            fps: 60,
            bitrate_kbps: 40_000,
        }
    }

    pub fn from_preset(p: QualityPreset) -> Self {
        match p {
            QualityPreset::Competitive => Self::competitive(),
            QualityPreset::Balanced => Self::balanced(),
            QualityPreset::Quality => Self::quality(),
            QualityPreset::Custom => Self::balanced(),
        }
    }

    /// Clamp to values the encoders and the wire format can actually carry.
    /// H.264 wants even dimensions, so odd values are rounded down.
    pub fn sanitized(&self) -> Self {
        Self {
            preset: self.preset,
            width: self.width.clamp(MIN_WIDTH, MAX_WIDTH) & !1,
            height: self.height.clamp(MIN_HEIGHT, MAX_HEIGHT) & !1,
            fps: self.fps.clamp(MIN_FPS, MAX_FPS),
            bitrate_kbps: self.bitrate_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS),
        }
    }
}

pub const MIN_WIDTH: u32 = 640;
pub const MAX_WIDTH: u32 = 3840;
pub const MIN_HEIGHT: u32 = 360;
pub const MAX_HEIGHT: u32 = 2160;
pub const MIN_FPS: u32 = 24;
pub const MAX_FPS: u32 = 240;
pub const MIN_BITRATE_KBPS: u32 = 2_000;
pub const MAX_BITRATE_KBPS: u32 = 100_000;

impl Default for StreamQuality {
    fn default() -> Self {
        Self::balanced()
    }
}

/// Every field defaults independently, so adding a setting in a later version
/// does not make an existing `host.toml` unreadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    pub name: String,
    pub port: u16,
    pub quality: StreamQuality,
    pub monitor_index: u32,
    pub encoder: String,
    pub ffmpeg_path: String,
    pub enable_audio: bool,
    pub enable_gamepad: bool,
    pub allow_unpaired_with_pin: bool,
    pub auto_trust: bool,
    pub bind: String,
    /// `host:port` of a `brolink-relay` to advertise in the ticket. Empty
    /// disables relaying.
    pub relay: String,
    /// Ask the home router (UPnP / NAT-PMP) to forward the UDP port so a Mac
    /// on another network can reach this PC without Tailscale.
    pub enable_upnp: bool,
    /// Launch the host when this Windows user signs in.
    pub start_with_windows: bool,
    /// Bidirectional clipboard with the client.
    pub enable_clipboard: bool,
    /// Restart the encoder at a new bitrate when the client reports loss.
    pub adaptive_bitrate: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            name: default_host_name(),
            port: DEFAULT_PORT,
            quality: StreamQuality::default(),
            monitor_index: 0,
            encoder: "auto".into(),
            ffmpeg_path: String::new(),
            enable_audio: true,
            enable_gamepad: true,
            allow_unpaired_with_pin: true,
            auto_trust: false,
            bind: "0.0.0.0".into(),
            relay: String::new(),
            enable_upnp: true,
            start_with_windows: false,
            enable_clipboard: true,
            adaptive_bitrate: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedHost {
    pub name: String,
    pub ticket: String,
    #[serde(default)]
    pub last_connected_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub name: String,
    pub last_ticket: String,
    pub quality: StreamQuality,
    pub vsync: bool,
    /// Output gain applied to received audio, 0.0..=2.0.
    pub volume: f32,
    /// PCs this Mac has connected to, most-recent first.
    pub saved_hosts: Vec<SavedHost>,
    pub auto_reconnect: bool,
    pub enable_clipboard: bool,
    pub show_hud: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            name: default_host_name(),
            last_ticket: String::new(),
            quality: StreamQuality::competitive(),
            vsync: false,
            volume: 1.0,
            saved_hosts: Vec::new(),
            auto_reconnect: true,
            enable_clipboard: true,
            show_hud: true,
        }
    }
}

impl ClientConfig {
    /// Remember a successful connection, moving it to the front of the list.
    pub fn remember_host(&mut self, name: &str, ticket: &str) {
        let ticket = ticket.trim();
        if ticket.is_empty() {
            return;
        }
        self.saved_hosts.retain(|h| h.ticket != ticket);
        self.saved_hosts.insert(
            0,
            SavedHost {
                name: name.trim().to_string(),
                ticket: ticket.to_string(),
                last_connected_unix: crate::proto::now_us() / 1_000_000,
            },
        );
        const MAX_SAVED: usize = 16;
        if self.saved_hosts.len() > MAX_SAVED {
            self.saved_hosts.truncate(MAX_SAVED);
        }
        self.last_ticket = ticket.to_string();
    }

    pub fn forget_host(&mut self, ticket: &str) {
        self.saved_hosts.retain(|h| h.ticket != ticket);
    }
}

fn default_host_name() -> String {
    hostname::get_opt().unwrap_or_else(|| "BroLink-PC".into())
}

mod hostname {
    /// Best-effort machine name across Windows, macOS, and Linux.
    ///
    /// A GUI app launched from Finder inherits neither `COMPUTERNAME` nor
    /// `HOSTNAME`, and macOS has no `/etc/hostname`, so the environment alone
    /// is not enough - fall back to asking the system.
    pub fn get_opt() -> Option<String> {
        let from_env = std::env::var("COMPUTERNAME")
            .ok()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .filter(|s| !s.trim().is_empty());
        if let Some(name) = from_env {
            return Some(name.trim().to_string());
        }
        #[cfg(unix)]
        {
            if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
                let s = s.trim();
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
            if let Some(name) = unix_hostname() {
                return Some(name);
            }
        }
        None
    }

    #[cfg(unix)]
    fn unix_hostname() -> Option<String> {
        let out = std::process::Command::new("hostname").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Trim the mDNS suffix macOS appends so the UI reads "Studio-Mac".
        let name = name.trim_end_matches(".local").to_string();
        (!name.is_empty()).then_some(name)
    }
}

/// Move an unparseable config aside so the user can recover hand-edits.
fn quarantine(path: &std::path::Path) {
    let backup = path.with_extension("toml.bak");
    match fs::rename(path, &backup) {
        Ok(()) => tracing::warn!("previous config saved as {}", backup.display()),
        Err(e) => tracing::warn!("could not back up {}: {e}", path.display()),
    }
}

pub fn host_config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("host.toml"))
}
pub fn client_config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("client.toml"))
}
pub fn host_identity_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("host.key"))
}
pub fn client_identity_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("client.key"))
}
pub fn allowlist_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("allowlist.toml"))
}

impl HostConfig {
    /// Never fails on a damaged file: a config we cannot parse is moved aside
    /// and replaced with defaults, because refusing to start is worse than
    /// losing settings the user can re-pick in a few clicks.
    pub fn load() -> Result<Self> {
        let path = host_config_path()?;
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let s = fs::read_to_string(&path).with_context(|| path.display().to_string())?;
        match toml::from_str(&s) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                tracing::warn!(
                    "{} is unreadable ({e}); starting from defaults",
                    path.display()
                );
                quarantine(&path);
                let cfg = Self::default();
                let _ = cfg.save();
                Ok(cfg)
            }
        }
    }
    pub fn save(&self) -> Result<()> {
        let path = host_config_path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

impl ClientConfig {
    /// See [`HostConfig::load`] - a damaged config is moved aside, not fatal.
    pub fn load() -> Result<Self> {
        let path = client_config_path()?;
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let s = fs::read_to_string(&path).with_context(|| path.display().to_string())?;
        match toml::from_str(&s) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                tracing::warn!(
                    "{} is unreadable ({e}); starting from defaults",
                    path.display()
                );
                quarantine(&path);
                let cfg = Self::default();
                let _ = cfg.save();
                Ok(cfg)
            }
        }
    }
    pub fn save(&self) -> Result<()> {
        let path = client_config_path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// True for the CGNAT range 100.64.0.0/10 that Tailscale hands out.
pub fn is_cgnat_v4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// Detect a Tailscale IPv4 (CGNAT 100.64.0.0/10) on this machine.
pub fn tailscale_v4() -> Option<std::net::Ipv4Addr> {
    use local_ip_address::list_afinet_netifas;
    let ifaces = list_afinet_netifas().ok()?;
    ifaces.into_iter().find_map(|(_name, ip)| match ip {
        std::net::IpAddr::V4(v4) if is_cgnat_v4(v4) => Some(v4),
        _ => None,
    })
}

pub fn primary_lan_v4() -> Option<std::net::Ipv4Addr> {
    match local_ip_address::local_ip() {
        Ok(std::net::IpAddr::V4(v4)) if !v4.is_loopback() => Some(v4),
        _ => None,
    }
}

/// Globally-routable IPv6 addresses on this machine (not link-local, not ULA).
///
/// A host with a real IPv6 prefix can be reached from anywhere without UPnP,
/// because there is no NAT in the way.
pub fn global_v6() -> Vec<std::net::Ipv6Addr> {
    use local_ip_address::list_afinet_netifas;
    let Ok(ifaces) = list_afinet_netifas() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_name, ip) in ifaces {
        let std::net::IpAddr::V6(v6) = ip else {
            continue;
        };
        if is_global_v6(v6) && !out.contains(&v6) {
            out.push(v6);
        }
    }
    out
}

fn is_global_v6(ip: std::net::Ipv6Addr) -> bool {
    // 2000::/3 is the global unicast range. Skip loopback, link-local (fe80::/10),
    // ULA (fc00::/7), and multicast.
    let segs = ip.segments();
    if ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() {
        return false;
    }
    if (segs[0] & 0xffc0) == 0xfe80 {
        return false;
    }
    if (segs[0] & 0xfe00) == 0xfc00 {
        return false;
    }
    (segs[0] & 0xe000) == 0x2000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn cgnat_range_is_exact() {
        assert!(is_cgnat_v4(Ipv4Addr::new(100, 64, 0, 0)));
        assert!(is_cgnat_v4(Ipv4Addr::new(100, 127, 255, 255)));
        assert!(is_cgnat_v4(Ipv4Addr::new(100, 90, 1, 2)));
        // Just outside 100.64.0.0/10 on both sides.
        assert!(!is_cgnat_v4(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_cgnat_v4(Ipv4Addr::new(100, 128, 0, 0)));
        // Ordinary addresses that share the first octet or look similar.
        assert!(!is_cgnat_v4(Ipv4Addr::new(100, 0, 0, 1)));
        assert!(!is_cgnat_v4(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!is_cgnat_v4(Ipv4Addr::new(10, 64, 0, 1)));
    }

    #[test]
    fn presets_are_sane_and_survive_sanitizing() {
        for p in QualityPreset::all() {
            let q = StreamQuality::from_preset(p);
            assert_eq!(
                (q.width, q.height, q.fps, q.bitrate_kbps),
                {
                    let s = q.sanitized();
                    (s.width, s.height, s.fps, s.bitrate_kbps)
                },
                "preset {} should already be within limits",
                p.as_str()
            );
        }
    }

    #[test]
    fn sanitize_clamps_hostile_values() {
        let q = StreamQuality {
            preset: QualityPreset::Custom,
            width: 0,
            height: 0,
            fps: 0,
            bitrate_kbps: 0,
        }
        .sanitized();
        assert_eq!((q.width, q.height), (MIN_WIDTH, MIN_HEIGHT));
        assert_eq!(q.fps, MIN_FPS);
        assert_eq!(q.bitrate_kbps, MIN_BITRATE_KBPS);

        let q = StreamQuality {
            preset: QualityPreset::Custom,
            width: u32::MAX,
            height: u32::MAX,
            fps: u32::MAX,
            bitrate_kbps: u32::MAX,
        }
        .sanitized();
        assert_eq!((q.width, q.height), (MAX_WIDTH, MAX_HEIGHT));
        assert_eq!(q.fps, MAX_FPS);
        assert_eq!(q.bitrate_kbps, MAX_BITRATE_KBPS);
    }

    #[test]
    fn sanitize_rounds_dimensions_down_to_even() {
        let q = StreamQuality {
            preset: QualityPreset::Custom,
            width: 1921,
            height: 1081,
            fps: 60,
            bitrate_kbps: 20_000,
        }
        .sanitized();
        assert_eq!(
            (q.width, q.height),
            (1920, 1080),
            "H.264 needs even dimensions"
        );
    }

    #[test]
    fn configs_survive_a_file_missing_every_optional_field() {
        // An empty document must deserialize to defaults rather than erroring,
        // which is what keeps an old config from bricking a new build.
        let h: HostConfig = toml::from_str("").expect("host config from empty file");
        assert_eq!(h.port, DEFAULT_PORT);
        assert!(h.relay.is_empty());
        let c: ClientConfig = toml::from_str("").expect("client config from empty file");
        assert_eq!(c.volume, 1.0);
        assert!(c.auto_reconnect);
        assert!(h.enable_upnp);
    }

    #[test]
    fn config_roundtrips_through_toml() {
        let h = HostConfig {
            name: "OFFICE-PC".into(),
            relay: "relay.example.com:47851".into(),
            quality: StreamQuality::quality(),
            ..Default::default()
        };
        let back: HostConfig = toml::from_str(&toml::to_string_pretty(&h).unwrap()).unwrap();
        assert_eq!(back.name, h.name);
        assert_eq!(back.relay, h.relay);
        assert_eq!(back.quality.bitrate_kbps, h.quality.bitrate_kbps);

        let c = ClientConfig {
            last_ticket: "blk1_xyz".into(),
            volume: 0.25,
            ..Default::default()
        };
        let back: ClientConfig = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.last_ticket, c.last_ticket);
        assert_eq!(back.volume, 0.25);
    }

    #[test]
    fn a_partial_config_keeps_the_fields_it_does_have() {
        let h: HostConfig = toml::from_str("name = \"MY-PC\"\nport = 5000\n").unwrap();
        assert_eq!(h.name, "MY-PC");
        assert_eq!(h.port, 5000);
        assert!(h.enable_audio, "unspecified fields fall back to defaults");
    }

    #[test]
    fn host_name_is_never_empty() {
        assert!(!default_host_name().trim().is_empty());
    }

    #[test]
    fn remembering_a_host_dedupes_and_caps() {
        let mut c = ClientConfig::default();
        c.remember_host("Office", "blk1_aaa");
        c.remember_host("Office", "blk1_aaa");
        c.remember_host("Home", "blk1_bbb");
        assert_eq!(c.saved_hosts.len(), 2);
        assert_eq!(c.saved_hosts[0].name, "Home", "most recent first");
        assert_eq!(c.last_ticket, "blk1_bbb");
        c.forget_host("blk1_bbb");
        assert_eq!(c.saved_hosts.len(), 1);
        assert_eq!(c.saved_hosts[0].ticket, "blk1_aaa");
    }

    #[test]
    fn global_v6_filter_is_exact() {
        use std::net::Ipv6Addr;
        assert!(is_global_v6("2001:db8::1".parse().unwrap()));
        assert!(!is_global_v6(Ipv6Addr::LOCALHOST));
        assert!(!is_global_v6("fe80::1".parse().unwrap()));
        assert!(!is_global_v6("fd12:3456::1".parse().unwrap()));
        assert!(!is_global_v6("ff02::1".parse().unwrap()));
    }
}
