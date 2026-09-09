//! The network between this Mac and a PC: direct or through a relay, and
//! how long a round trip takes. From that, what to ask of the stream when
//! the quality setting is Auto, and, when the path is relayed, why and what
//! would fix it.
//!
//! Tailscale connects two machines directly when it can punch through both
//! NATs; when it cannot, every packet goes through one of its DERP relays.
//! A relay adds a detour and is shared, so a stream through one is laggy
//! and thin however fast the two networks are. Nothing in BroLink can make
//! a direct path exist, but it can say which router is in the way.

use crate::config::{Quality, Resolution, StreamSettings};
use brolink_core::api::NatReport;
use brolink_core::tailscale::derp_city;
use brolink_ui::Tone;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Path {
    /// `Some(true)` when packets go straight to the PC, `Some(false)` when
    /// they go through a relay, `None` before any traffic has flowed.
    pub direct: Option<bool>,
    /// The relay's region code ("tok") when relayed.
    pub relay: String,
    /// One round trip to the PC, from a TCP connect.
    pub rtt_ms: Option<u32>,
}

impl Path {
    pub fn relayed(&self) -> bool {
        self.direct == Some(false)
    }

    /// "Direct · 38 ms", "Relayed via Tokyo · 210 ms", "Path unknown".
    pub fn label(&self) -> String {
        let rtt = self
            .rtt_ms
            .map(|ms| format!(" · {ms} ms"))
            .unwrap_or_default();
        match self.direct {
            Some(true) => format!("Direct{rtt}"),
            Some(false) if self.relay.is_empty() => format!("Relayed{rtt}"),
            Some(false) => format!("Relayed via {}{rtt}", derp_city(&self.relay)),
            None => format!("Path unknown{rtt}"),
        }
    }

    pub fn tone(&self) -> Tone {
        match (self.direct, self.rtt_ms) {
            (Some(false), _) => Tone::Danger,
            (Some(true), Some(ms)) if ms >= 80 => Tone::Accent,
            (Some(true), _) => Tone::Success,
            (None, _) => Tone::Neutral,
        }
    }
}

/// Resolution, frame rate and bitrate for a path, when the user left the
/// choice to BroLink. The numbers are what a Moonlight stream needs to
/// stay fluid: a relay carries a few megabits at best, a long round trip
/// makes every lost packet expensive, a LAN can take whatever the PC gives.
pub fn auto_values(path: &Path) -> (Resolution, u32, u32) {
    let rtt = path.rtt_ms.unwrap_or(40);
    if path.relayed() || rtt >= 200 {
        (Resolution::P1080, 30, 4_000)
    } else if rtt >= 80 {
        (Resolution::P1080, 60, 8_000)
    } else if rtt >= 25 || path.direct.is_none() {
        (Resolution::P1440, 60, 15_000)
    } else {
        (Resolution::P1440, 60, 30_000)
    }
}

/// `settings` as the connection will use them: untouched when Custom,
/// filled from [`auto_values`] when Auto.
pub fn effective(settings: &StreamSettings, path: &Path) -> StreamSettings {
    let mut s = settings.clone();
    if s.quality == Quality::Auto {
        let (r, fps, kbps) = auto_values(path);
        s.resolution = r;
        s.fps = fps;
        s.bitrate_kbps = kbps;
    }
    s
}

/// Why the PC is reached through a relay and what would give a direct
/// path, from the two machines' own network reports. `None` when the path
/// is direct or not known yet.
pub fn explain(
    pc: &str,
    path: &Path,
    pc_nat: Option<&NatReport>,
    mac_nat: Option<&NatReport>,
) -> Option<String> {
    if !path.relayed() {
        return None;
    }
    let via = if path.relay.is_empty() {
        "a Tailscale relay".to_string()
    } else {
        format!("Tailscale's {} relay", derp_city(&path.relay))
    };
    let mut out = format!(
        "{pc} is reached through {via}, not directly. Every packet takes that detour and the relay holds the stream to a few megabits, whatever the two networks can do. "
    );
    let cause = match (pc_nat, mac_nat) {
        (Some(p), _) if !p.udp => format!(
            "{pc}'s network blocks UDP, so no direct path is possible from anywhere. A different network for {pc}, or a Tailscale subnet router beside it, is the only fix."
        ),
        (_, Some(m)) if !m.udp => "This Mac's network blocks UDP, so nothing can reach it directly; another network (a phone's hotspot will do) fixes it.".to_string(),
        (Some(p), Some(m)) => {
            let pc_hard = p.hard == Some(true) && !p.portmap;
            let mac_hard = m.hard == Some(true) && !m.portmap;
            if pc_hard {
                let mut t = format!(
                    "{pc}'s router is a hard NAT with no UPnP: it hides {pc} from anyone who has not been spoken to first. Turn UPnP or NAT-PMP on in that router (on {pc}, its page is usually at the gateway address), or forward a UDP port to {pc} and tell Tailscale; Tailscale then connects directly."
                );
                if mac_hard {
                    t.push_str(" This Mac's network is a hard NAT too, which makes the relay certain until one side changes.");
                }
                if p.ipv6 && !m.ipv6 {
                    t.push_str(&format!(
                        " {pc} has IPv6; a network with IPv6 on this Mac's side would also connect directly."
                    ));
                }
                t
            } else if mac_hard {
                format!(
                    "{pc}'s network is fine; this Mac's is a hard NAT with no UPnP. From another network, or with UPnP on this router, the connection is direct."
                )
            } else {
                "Both networks look easy to traverse, so Tailscale should switch to a direct path within a minute of traffic. If it never does, the routers may be blocking UDP between them.".to_string()
            }
        }
        (None, Some(m)) => {
            let mut t = format!(
                "BroLink Host 3.1 on {pc} would report that side of the network and say which router is in the way."
            );
            if m.hard == Some(true) && !m.portmap {
                t.push_str(" This Mac's network is a hard NAT with no UPnP, which is often enough on its own; another network here may connect directly.");
            }
            t
        }
        (Some(p), None) => {
            if p.hard == Some(true) && !p.portmap {
                format!(
                    "{pc}'s router is a hard NAT with no UPnP. Turn UPnP or NAT-PMP on in that router, or forward a UDP port to {pc}; Tailscale then connects directly."
                )
            } else {
                format!("{pc}'s network looks easy to traverse; this Mac's side has not been checked yet.")
            }
        }
        (None, None) => {
            "A direct path needs UPnP or NAT-PMP on at least one router, or IPv6 on both networks.".to_string()
        }
    };
    out.push_str(&cause);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nat(udp: bool, hard: Option<bool>, portmap: bool, ipv6: bool) -> NatReport {
        NatReport {
            udp,
            ipv4: true,
            ipv6,
            hard,
            portmap,
            derp: "tok".into(),
        }
    }

    #[test]
    fn labels_say_direct_relayed_or_unknown() {
        let p = Path {
            direct: Some(false),
            relay: "tok".into(),
            rtt_ms: Some(210),
        };
        assert_eq!(p.label(), "Relayed via Tokyo · 210 ms");
        assert_eq!(p.tone(), Tone::Danger);
        let p = Path {
            direct: Some(true),
            relay: "tok".into(),
            rtt_ms: Some(38),
        };
        assert_eq!(p.label(), "Direct · 38 ms");
        assert_eq!(p.tone(), Tone::Success);
        let p = Path {
            direct: Some(true),
            rtt_ms: Some(120),
            ..Default::default()
        };
        assert_eq!(
            p.tone(),
            Tone::Accent,
            "a long round trip is worth a colour"
        );
        assert_eq!(Path::default().label(), "Path unknown");
        assert_eq!(Path::default().tone(), Tone::Neutral);
    }

    #[test]
    fn auto_asks_less_of_a_relay_and_more_of_a_lan() {
        let relayed = Path {
            direct: Some(false),
            relay: "tok".into(),
            rtt_ms: Some(150),
        };
        assert_eq!(auto_values(&relayed), (Resolution::P1080, 30, 4_000));
        let far = Path {
            direct: Some(true),
            rtt_ms: Some(110),
            ..Default::default()
        };
        assert_eq!(auto_values(&far), (Resolution::P1080, 60, 8_000));
        let near = Path {
            direct: Some(true),
            rtt_ms: Some(30),
            ..Default::default()
        };
        assert_eq!(auto_values(&near), (Resolution::P1440, 60, 15_000));
        let lan = Path {
            direct: Some(true),
            rtt_ms: Some(2),
            ..Default::default()
        };
        assert_eq!(auto_values(&lan), (Resolution::P1440, 60, 30_000));
        // Unknown path: the middle, not the top.
        assert_eq!(
            auto_values(&Path::default()),
            (Resolution::P1440, 60, 15_000)
        );
        // A very slow direct path is treated like a relay.
        let slow = Path {
            direct: Some(true),
            rtt_ms: Some(400),
            ..Default::default()
        };
        assert_eq!(auto_values(&slow).1, 30);

        let custom = StreamSettings {
            quality: Quality::Custom,
            bitrate_kbps: 77_000,
            ..Default::default()
        };
        assert_eq!(effective(&custom, &relayed).bitrate_kbps, 77_000);
        let auto = StreamSettings::default();
        let e = effective(&auto, &relayed);
        assert_eq!(e.bitrate_kbps, 4_000);
        assert_eq!(e.quality, Quality::Auto, "auto stays auto");
        assert_eq!(e.app, "Desktop");
    }

    #[test]
    fn explanations_name_the_router_in_the_way() {
        let relayed = Path {
            direct: Some(false),
            relay: "tok".into(),
            rtt_ms: Some(200),
        };
        let direct = Path {
            direct: Some(true),
            ..Default::default()
        };
        assert_eq!(explain("Gaming-PC", &direct, None, None), None);
        assert_eq!(explain("Gaming-PC", &Path::default(), None, None), None);

        let pc_hard = nat(true, Some(true), false, true);
        let mac_easy = nat(true, Some(false), false, false);
        let t = explain("Gaming-PC", &relayed, Some(&pc_hard), Some(&mac_easy)).unwrap();
        assert!(
            t.starts_with("Gaming-PC is reached through Tailscale's Tokyo relay"),
            "{t}"
        );
        assert!(
            t.contains("Gaming-PC's router is a hard NAT with no UPnP"),
            "{t}"
        );
        assert!(t.contains("IPv6"), "{t}");
        assert!(!t.contains("hard NAT too"), "{t}");

        let mac_hard = nat(true, Some(true), false, false);
        let t = explain("Gaming-PC", &relayed, Some(&pc_hard), Some(&mac_hard)).unwrap();
        assert!(t.contains("hard NAT too"), "{t}");

        let pc_easy = nat(true, Some(false), true, false);
        let t = explain("Gaming-PC", &relayed, Some(&pc_easy), Some(&mac_hard)).unwrap();
        assert!(t.contains("this Mac's is a hard NAT"), "{t}");

        let t = explain("Gaming-PC", &relayed, Some(&pc_easy), Some(&mac_easy)).unwrap();
        assert!(t.contains("within a minute"), "{t}");

        let no_udp = nat(false, None, false, false);
        let t = explain("Gaming-PC", &relayed, Some(&no_udp), Some(&mac_easy)).unwrap();
        assert!(t.contains("blocks UDP"), "{t}");

        // An old host reports nothing: say so, and what the Mac knows.
        let t = explain("Gaming-PC", &relayed, None, Some(&mac_hard)).unwrap();
        assert!(t.contains("BroLink Host 3.1"), "{t}");
        assert!(t.contains("often enough on its own"), "{t}");
        let t = explain("Gaming-PC", &relayed, None, None).unwrap();
        assert!(t.contains("UPnP"), "{t}");
        // Nothing here is a wall of text.
        assert!(t.len() < 600, "{}", t.len());
    }
}
