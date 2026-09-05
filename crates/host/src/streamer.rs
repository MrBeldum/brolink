//! Sunshine (or Apollo, its fork) on this PC: where it is, whether it is up,
//! and the two web-API calls BroLink needs. The API is HTTPS with a
//! self-signed certificate on localhost, so calls go through `curl.exe -k`
//! rather than a TLS stack of our own.

use anyhow::{bail, Context, Result};
use brolink_core::{SUNSHINE_PORT, SUNSHINE_WEB_PORT};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Install directories, in the order they are preferred when both exist.
pub const INSTALL_DIRS: [(&str, &str); 2] = [
    ("Sunshine", r"C:\Program Files\Sunshine"),
    ("Apollo", r"C:\Program Files\Apollo"),
];

/// GitHub repository the setup script fetches the installer from.
pub const REPO: &str = "LizardByte/Sunshine";

#[derive(Debug, Clone)]
pub struct Install {
    pub kind: &'static str,
    pub dir: PathBuf,
}

impl Install {
    pub fn exe(&self) -> PathBuf {
        self.dir.join("sunshine.exe")
    }
}

pub fn find() -> Option<Install> {
    INSTALL_DIRS
        .iter()
        .map(|(kind, dir)| Install {
            kind,
            dir: PathBuf::from(dir),
        })
        .find(|i| i.exe().exists())
}

/// Moonlight's port answers on loopback.
pub fn running() -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], SUNSHINE_PORT).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

pub struct Api<'a> {
    pub user: &'a str,
    pub pass: &'a str,
}

impl Api<'_> {
    fn call(&self, method: &str, path: &str, body: Option<&str>) -> Result<serde_json::Value> {
        let mut c = Command::new("curl.exe");
        c.args([
            "-sk",
            "--max-time",
            "8",
            "-u",
            &format!("{}:{}", self.user, self.pass),
        ])
        .args(["-X", method, "-H", "Content-Type: application/json"])
        .arg(format!("https://localhost:{SUNSHINE_WEB_PORT}{path}"));
        if let Some(b) = body {
            c.args(["-d", b]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        let out = c.output().context("run curl")?;
        if !out.status.success() {
            bail!("curl: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        parse_reply(&text)
    }

    /// True when the saved login is accepted.
    pub fn ok(&self) -> bool {
        self.call("GET", "/api/apps", None).is_ok()
    }

    /// Accept the PIN Moonlight is waiting with. Sunshine answers
    /// `{"status":false}` until Moonlight has actually started pairing, so
    /// callers retry.
    pub fn submit_pin(&self, pin: &str, name: &str) -> Result<()> {
        let body = serde_json::json!({ "pin": pin, "name": name }).to_string();
        let v = self.call("POST", "/api/pin", Some(&body))?;
        if v["status"].as_bool() == Some(true) || v["status"].as_str() == Some("true") {
            Ok(())
        } else {
            bail!("Sunshine did not accept the PIN (is Moonlight pairing right now?)")
        }
    }

    /// End whatever is streaming, so a power action does not cut a session
    /// off mid-frame.
    pub fn close_app(&self) {
        let _ = self.call("POST", "/api/apps/close", None);
    }

    /// Names of the paired clients, with the uuid to unpair them by.
    pub fn clients(&self) -> Vec<(String, String)> {
        self.call("GET", "/api/clients/list", None)
            .map(|v| parse_clients(&v))
            .unwrap_or_default()
    }

    pub fn unpair(&self, uuid: &str) -> Result<()> {
        let body = serde_json::json!({ "uuid": uuid }).to_string();
        self.call("POST", "/api/clients/unpair", Some(&body))
            .map(|_| ())
    }

    /// Whether the virtual gamepad driver (ViGEmBus) is installed, if the
    /// running Sunshine can tell us.
    pub fn gamepad_driver(&self) -> Option<bool> {
        let v = self.call("GET", "/api/vigembus/status", None).ok()?;
        v["installed"].as_bool().or_else(|| v["status"].as_bool())
    }

    pub fn install_gamepad_driver(&self) -> Result<()> {
        self.call("POST", "/api/vigembus/install", None).map(|_| ())
    }
}

fn parse_reply(text: &str) -> Result<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(text.trim()).with_context(|| {
        if text.trim().is_empty() {
            "empty reply (wrong login?)".to_string()
        } else {
            format!(
                "unexpected reply: {}",
                text.trim().chars().take(120).collect::<String>()
            )
        }
    })?;
    if v.get("status").and_then(|s| s.as_bool()) == Some(false) {
        if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
            bail!("{e}");
        }
    }
    Ok(v)
}

fn parse_clients(v: &serde_json::Value) -> Vec<(String, String)> {
    v.get("named_certs")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    Some((
                        c.get("name")?.as_str()?.to_string(),
                        c.get("uuid")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_parsed_and_errors_surfaced() {
        assert!(parse_reply(r#"{"status":true}"#).is_ok());
        let e = parse_reply(r#"{"status":false,"error":"Invalid PIN"}"#).unwrap_err();
        assert!(e.to_string().contains("Invalid PIN"));
        let e = parse_reply("").unwrap_err();
        assert!(e.to_string().contains("wrong login"), "{e}");
        assert!(parse_reply("<html>401</html>").is_err());
    }

    #[test]
    fn clients_list_is_read_leniently() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"status":true,"named_certs":[{"name":"Example Mac","uuid":"abc"},{"uuid":"nameless"},{"name":"x","uuid":"y","extra":1}]}"#,
        )
        .unwrap();
        assert_eq!(
            parse_clients(&v),
            vec![
                ("Example Mac".to_string(), "abc".to_string()),
                ("x".into(), "y".into())
            ]
        );
        assert!(parse_clients(&serde_json::json!({"status": true})).is_empty());
    }
}
