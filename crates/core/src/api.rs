//! The JSON the host serves and the client reads. Plain HTTP over Tailscale:
//! WireGuard already encrypts it, and `tailscale whois` already says who is
//! asking, so BroLink adds neither a cipher nor a password of its own.
//!
//! Routes (all JSON, all on [`crate::CONTROL_PORT`]):
//!
//! | Method | Path        | Body            | Reply        |
//! |--------|-------------|-----------------|--------------|
//! | GET    | /v1/status  |                 | [`Status`]   |
//! | POST   | /v1/pin     | [`PinRequest`]  | [`Ack`]      |
//! | POST   | /v1/power   | [`PowerRequest`]| [`Ack`]      |
//! | POST   | /v1/quit    |                 | [`Ack`] (loopback only) |

use serde::{Deserialize, Serialize};

/// Everything the Mac needs to know about the PC, and everything the host's
/// own control panel shows.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Status {
    pub app: String,
    pub version: String,
    /// The PC's name as Windows knows it.
    pub name: String,
    pub tailscale_ip: Option<String>,
    /// Who the PC is signed in to Tailscale as. Only this account may talk
    /// to the host from the tailnet.
    pub tailscale_login: Option<String>,
    /// LAN address and MAC of the adapter a wake packet must reach.
    pub lan_ip: Option<String>,
    pub mac: Option<String>,
    /// `Some(true)` once the adapter wakes on a magic packet and Windows lets
    /// it wake the PC. `None` while unknown.
    pub wake_ready: Option<bool>,
    /// The adapter's name and driver description, which the setup step
    /// needs to arm it.
    pub wake_adapter: String,
    pub wake_adapter_description: String,
    pub streamer: Streamer,
    /// Whether the owner lets a Mac sleep, restart, or shut this PC down.
    pub power_allowed: bool,
    /// What the host thinks still needs doing, for the setup card.
    pub setup: Vec<String>,
    /// Recent log lines, newest last.
    pub log: Vec<String>,
}

/// The streaming server on the PC: Sunshine, or its fork Apollo.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Streamer {
    /// "Sunshine" or "Apollo"; empty when neither is installed.
    pub kind: String,
    pub installed: bool,
    /// Its Moonlight-facing port answers.
    pub running: bool,
    /// The host can log in to its web API with the saved credentials, which
    /// is what auto-pairing needs.
    pub api_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinRequest {
    /// The 4 digits Moonlight was started with.
    pub pin: String,
    /// Shown in Sunshine's client list.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerAction {
    Sleep,
    Restart,
    Shutdown,
}

impl PowerAction {
    pub fn label(self) -> &'static str {
        match self {
            PowerAction::Sleep => "Sleep",
            PowerAction::Restart => "Restart",
            PowerAction::Shutdown => "Shut down",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerRequest {
    pub action: PowerAction,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ack {
    pub ok: bool,
    pub error: Option<String>,
}

impl Ack {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_and_tolerates_missing_fields() {
        let s: Status = serde_json::from_str(r#"{"name":"PC","mac":"aa:bb:cc:dd:ee:ff"}"#).unwrap();
        assert_eq!(s.name, "PC");
        assert_eq!(s.mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert!(!s.power_allowed);
        let back: Status = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn power_actions_are_lowercase_on_the_wire() {
        let r = PowerRequest {
            action: PowerAction::Shutdown,
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"action":"shutdown"}"#
        );
        let r: PowerRequest = serde_json::from_str(r#"{"action":"sleep"}"#).unwrap();
        assert_eq!(r.action, PowerAction::Sleep);
        assert!(serde_json::from_str::<PowerRequest>(r#"{"action":"Sleep"}"#).is_err());
    }
}
