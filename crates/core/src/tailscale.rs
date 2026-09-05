//! Reading Tailscale through its own CLI. The tailnet is BroLink's network
//! *and* its list of trusted machines, so both apps lean on it entirely.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Command;

/// Where the CLI lives, first hit wins. The macOS App Store build keeps it
/// inside the bundle; the standalone build symlinks it; Homebrew's is last.
pub fn cli() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\Tailscale\tailscale.exe",
            r"C:\Program Files (x86)\Tailscale\tailscale.exe",
        ]
    } else {
        &[
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
        ]
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .or_else(|| {
            let out = Command::new("tailscale").arg("--version").output().ok()?;
            out.status.success().then(|| PathBuf::from("tailscale"))
        })
}

/// `tailscale status --json`, the parts BroLink reads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Status {
    pub backend_state: String,
    #[serde(rename = "Self")]
    pub self_node: Node,
    pub peer: BTreeMap<String, Node>,
    pub user: BTreeMap<String, User>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "PascalCase")]
pub struct Node {
    /// Stable node id, the key the client stores wake details under.
    #[serde(rename = "ID")]
    pub id: String,
    pub host_name: String,
    #[serde(rename = "DNSName")]
    pub dns_name: String,
    #[serde(rename = "OS")]
    pub os: String,
    #[serde(rename = "UserID")]
    pub user_id: u64,
    #[serde(rename = "TailscaleIPs")]
    pub tailscale_ips: Vec<String>,
    pub online: bool,
}

impl Node {
    pub fn ipv4(&self) -> Option<Ipv4Addr> {
        self.tailscale_ips.iter().find_map(|s| s.parse().ok())
    }
    pub fn is_windows(&self) -> bool {
        self.os.eq_ignore_ascii_case("windows")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct User {
    #[serde(rename = "ID")]
    pub id: u64,
    pub login_name: String,
    pub display_name: String,
}

impl Status {
    pub fn running(&self) -> bool {
        self.backend_state == "Running"
    }
    pub fn self_login(&self) -> Option<&str> {
        self.user
            .get(&self.self_node.user_id.to_string())
            .map(|u| u.login_name.as_str())
    }
    /// Peers running Windows, the only kind BroLink can host on.
    pub fn windows_peers(&self) -> Vec<&Node> {
        let mut v: Vec<&Node> = self.peer.values().filter(|n| n.is_windows()).collect();
        v.sort_by(|a, b| a.host_name.cmp(&b.host_name));
        v
    }
}

pub fn parse_status(json: &str) -> Result<Status> {
    serde_json::from_str(json).context("parse tailscale status")
}

/// Run `tailscale status --json`. Errors when the CLI is missing or the
/// daemon is not running (the CLI prints why on stderr).
pub fn status() -> Result<Status> {
    let cli = cli().ok_or_else(|| anyhow::anyhow!("Tailscale is not installed"))?;
    let out = run(Command::new(cli).args(["status", "--json"]))?;
    let st = parse_status(&out)?;
    if !st.running() {
        bail!(
            "Tailscale is {}",
            if st.backend_state.is_empty() {
                "not running".into()
            } else {
                st.backend_state.to_lowercase()
            }
        );
    }
    Ok(st)
}

/// `tailscale whois --json IP`: which account owns the machine at `ip`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct WhoIs {
    pub user_profile: User,
    pub node: WhoIsNode,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct WhoIsNode {
    pub name: String,
    #[serde(rename = "ComputedName")]
    pub computed_name: String,
}

pub fn whois(ip: std::net::IpAddr) -> Result<WhoIs> {
    let cli = cli().ok_or_else(|| anyhow::anyhow!("Tailscale is not installed"))?;
    let out = run(Command::new(cli).args(["whois", "--json", &ip.to_string()]))?;
    serde_json::from_str(&out).context("parse tailscale whois")
}

fn run(cmd: &mut Command) -> Result<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().context("run tailscale")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "{}",
            err.trim().lines().next().unwrap_or("tailscale failed")
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "Version": "1.102.3", "BackendState": "Running",
      "Self": {"ID": "nSELF", "HostName": "Gaming-PC", "DNSName": "gaming-pc.example.ts.net.", "OS": "windows",
               "UserID": 42, "TailscaleIPs": ["100.64.0.10", "fd7a::1"], "Online": true},
      "Peer": {
        "nodekey:a": {"ID": "nMAC", "HostName": "Example Mac", "DNSName": "mac.example.ts.net.", "OS": "macOS",
                      "UserID": 42, "TailscaleIPs": ["100.64.0.20"], "Online": false},
        "nodekey:b": {"ID": "nPC2", "HostName": "Office", "DNSName": "office.example.ts.net.", "OS": "windows",
                      "UserID": 42, "TailscaleIPs": ["fd7a::2", "100.64.0.30"], "Online": true},
        "nodekey:c": {"ID": "nPC1", "HostName": "Den", "DNSName": "den.example.ts.net.", "OS": "windows",
                      "UserID": 43, "TailscaleIPs": ["100.64.0.31"], "Online": false}
      },
      "User": {"42": {"ID": 42, "LoginName": "user@example.com", "DisplayName": "Example User"}}
    }"#;

    #[test]
    fn status_is_parsed_and_windows_peers_sorted() {
        let st = parse_status(SAMPLE).unwrap();
        assert!(st.running());
        assert_eq!(st.self_node.ipv4(), Some("100.64.0.10".parse().unwrap()));
        assert_eq!(st.self_login(), Some("user@example.com"));
        let pcs = st.windows_peers();
        assert_eq!(
            pcs.iter().map(|n| n.host_name.as_str()).collect::<Vec<_>>(),
            ["Den", "Office"]
        );
        assert_eq!(
            pcs[1].ipv4(),
            Some("100.64.0.30".parse().unwrap()),
            "v4 is picked whatever the order"
        );
        assert!(pcs[1].online);
    }

    #[test]
    fn a_stopped_daemon_is_not_running() {
        let st = parse_status(r#"{"BackendState":"Stopped"}"#).unwrap();
        assert!(!st.running());
        assert!(st.windows_peers().is_empty());
        assert_eq!(st.self_login(), None);
    }

    #[test]
    fn whois_is_parsed() {
        let w: WhoIs = serde_json::from_str(
            r#"{"Node":{"Name":"mac.example.ts.net.","ComputedName":"mac"},"UserProfile":{"ID":42,"LoginName":"user@example.com","DisplayName":"Example User"}}"#,
        )
        .unwrap();
        assert_eq!(w.user_profile.id, 42);
        assert_eq!(w.node.computed_name, "mac");
    }
}
