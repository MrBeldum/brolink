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

/// The end of Sunshine's log, which says which encoder it settled on and
/// whether it could capture audio. `None` when it cannot be read.
pub fn log_text(install: &Install) -> Option<String> {
    let path = install.dir.join("config").join("sunshine.log");
    read_tail(&path, 512 * 1024)
}

/// The encoder family Sunshine settled on at its last start, from its log:
/// "nvenc", "amf", "quicksync", "software", or `None` when the log says
/// nothing. Software means no GPU encoder worked, which makes every stream
/// slow whatever the network does. The last `Found H.264 encoder: <name>
/// [<family>]` line counts, or the HEVC one when there is no H.264 line.
pub fn encoder_in(log: &str) -> Option<String> {
    let family = |line: &str| -> Option<String> {
        let start = line.rfind('[')? + 1;
        let end = line[start..].find(']')? + start;
        let f = line[start..end].trim();
        (!f.is_empty()).then(|| f.to_string())
    };
    for key in ["Found H.264 encoder:", "Found HEVC encoder:"] {
        if let Some(line) = log.lines().rev().find(|l| l.contains(key)) {
            if let Some(f) = family(line) {
                return Some(f);
            }
        }
    }
    None
}

/// Why Sunshine's audio capture failed, in its own words, when the last
/// thing its log says about audio is a failure rather than a working
/// capture format. A PC with no monitor or speakers usually has no audio
/// endpoint at all; Sunshine then streams silence and says so here.
pub fn audio_problem_in(log: &str) -> Option<String> {
    const FAILED: [&str; 8] = [
        "Unable to initialize audio capture",
        "There will be no audio",
        "Couldn't get default audio endpoint",
        "Couldn't find audio sink",
        "Audio sink not found",
        "Couldn't find supported format for audio",
        "Couldn't initialize audio client",
        "Couldn't initialize audio capture client",
    ];
    const WORKED: [&str; 2] = ["Audio capture format is", "Opus initialized"];
    let mut problem = None;
    for line in log.lines() {
        if WORKED.iter().any(|k| line.contains(k)) {
            problem = None;
        } else if FAILED.iter().any(|k| line.contains(k)) {
            problem = Some(log_message(line));
        }
    }
    problem
}

/// `[2026-09-07 10:00:00.001]: Error: Couldn't …` without its prefix.
fn log_message(line: &str) -> String {
    let l = line.trim();
    let l = match (l.starts_with('['), l.find("]: ")) {
        (true, Some(i)) => &l[i + 3..],
        _ => l,
    };
    ["Error: ", "Warning: ", "Info: ", "Fatal: "]
        .iter()
        .find_map(|p| l.strip_prefix(p))
        .unwrap_or(l)
        .trim()
        .to_string()
}

/// The last `max` bytes of a file as text, from a line boundary.
fn read_tail(path: &std::path::Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > max {
        f.seek(SeekFrom::Start(len - max)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(match text.find('\n') {
        Some(i) if len > max => text[i + 1..].to_string(),
        _ => text,
    })
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

/// Sunshine's GameStream port answers on loopback.
pub fn running() -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], SUNSHINE_PORT).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

pub struct Api<'a> {
    pub user: &'a str,
    pub pass: &'a str,
}

impl Api<'_> {
    /// Everything Sunshine will say about its own capture, minus the login
    /// it is protected by. `/api/logs` answers with plain text, so the tail
    /// is taken from the raw body rather than a JSON field.
    pub fn display_diagnostics(&self) -> serde_json::Value {
        let mut result = serde_json::Map::new();
        match self.call("GET", "/api/config", None) {
            Ok(serde_json::Value::Object(config)) => {
                let kept = config
                    .into_iter()
                    .filter(|(k, _)| !is_secret(k))
                    .collect::<serde_json::Map<_, _>>();
                result.insert("config".into(), kept.into());
            }
            Ok(other) => {
                result.insert("config".into(), other);
            }
            Err(e) => {
                result.insert("config_error".into(), e.to_string().into());
            }
        }
        match self.raw("GET", "/api/logs") {
            Ok(log) => {
                // Some builds wrap the log in JSON, others return the file.
                let text = serde_json::from_str::<serde_json::Value>(log.trim())
                    .ok()
                    .and_then(|v| {
                        ["content", "logs", "log"]
                            .iter()
                            .find_map(|k| v.get(*k).and_then(|l| l.as_str()).map(String::from))
                    })
                    .unwrap_or(log);
                result.insert("log".into(), tail(&text, 150).into());
            }
            Err(e) => {
                result.insert("log_error".into(), e.to_string().into());
            }
        }
        result.into()
    }

    fn call(&self, method: &str, path: &str, body: Option<&str>) -> Result<serde_json::Value> {
        parse_reply(&self.request(method, path, body)?)
    }

    /// A GET whose reply is read as text: not every endpoint answers JSON.
    fn raw(&self, method: &str, path: &str) -> Result<String> {
        self.request(method, path, None)
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<String> {
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
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// True when the saved login is accepted.
    pub fn ok(&self) -> bool {
        self.call("GET", "/api/apps", None).is_ok()
    }

    /// Accept the PIN the Mac is pairing with. Sunshine says no until the
    /// Mac has actually started pairing, so callers retry.
    ///
    /// Releases up to 2026.5 take `{pin, name}`; later ones list the waiting
    /// requests on `GET /api/pin` and want the request's `pairing_id` too.
    pub fn submit_pin(&self, pin: &str, name: &str) -> Result<()> {
        let pending = pending_pairings(self.call("GET", "/api/pin", None).ok());
        for id in pending {
            let mut body = serde_json::json!({ "pin": pin, "name": name });
            if let Some(id) = id {
                body["pairing_id"] = id.into();
            }
            let v = self.call("POST", "/api/pin", Some(&body.to_string()))?;
            if v["status"].as_bool() == Some(true) || v["status"].as_str() == Some("true") {
                return Ok(());
            }
        }
        bail!("Sunshine did not accept the PIN (is the Mac pairing right now?)")
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

/// The pairing ids `GET /api/pin` lists on a Sunshine master build. Every
/// other reply (a release Sunshine answers 200 `{"error":"Not Found"}`, an
/// old one 404, curl failing) means "no ids": still send the PIN once
/// without one, which is what those versions want.
fn pending_pairings(reply: Option<serde_json::Value>) -> Vec<Option<String>> {
    let ids: Vec<Option<String>> = reply
        .as_ref()
        .and_then(|v| v["pairings"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p["id"].as_str().map(|s| Some(s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        vec![None]
    } else {
        ids
    }
}

/// Sunshine's config carries its own web login and the pairing secrets.
/// Display diagnostics travel to the Mac, so those keys never leave the PC.
fn is_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["pass", "user", "salt", "token", "key", "cert", "secret", "pin"]
        .iter()
        .any(|needle| k.contains(needle))
}

/// The last `lines` lines: a capture failure is at the end of the log.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
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
    fn the_encoder_family_is_read_from_the_log() {
        let log = "[2026-09-07 10:00:00.001]: Info: // Testing for available encoders //\n\
                   [2026-09-07 10:00:01.002]: Info: Found H.264 encoder: h264_nvenc [nvenc]\n\
                   [2026-09-07 10:00:01.003]: Info: Found HEVC encoder: hevc_nvenc [nvenc]\n\
                   [2026-09-07 10:00:01.004]: Info: Found AV1 encoder: av1_nvenc [nvenc]\n";
        assert_eq!(encoder_in(log).as_deref(), Some("nvenc"));
        let sw = "Info: Found H.264 encoder: libx264 [software]\nInfo: Found HEVC encoder: libx265 [software]\n";
        assert_eq!(encoder_in(sw).as_deref(), Some("software"));
        // Two starts: the later one counts.
        let two = format!("{log}{sw}");
        assert_eq!(encoder_in(&two).as_deref(), Some("software"));
        let hevc_only = "Info: Found HEVC encoder: hevc_amf [amf]\n";
        assert_eq!(encoder_in(hevc_only).as_deref(), Some("amf"));
        assert_eq!(encoder_in("Info: nothing about encoders\n"), None);
        assert_eq!(encoder_in("Found H.264 encoder: x []"), None);
        assert_eq!(
            read_tail(std::path::Path::new("/nonexistent/sunshine.log"), 10),
            None
        );
    }

    #[test]
    fn a_missing_audio_device_is_read_from_the_log() {
        let bad = "[2026-09-08 20:00:00.000]: Info: Found H.264 encoder: h264_nvenc [nvenc]\n\
                   [2026-09-08 20:00:05.000]: Error: Couldn't get default audio endpoint [0x80070490]\n\
                   [2026-09-08 20:00:05.001]: Error: Unable to initialize audio capture. The stream will not have audio.\n";
        assert_eq!(
            audio_problem_in(bad).as_deref(),
            Some("Unable to initialize audio capture. The stream will not have audio.")
        );
        // A later start that captured fine clears it.
        let good = format!(
            "{bad}[2026-09-08 21:00:00.000]: Info: Audio capture format is [48kHz, 32-bit float, 2 channels]\n"
        );
        assert_eq!(audio_problem_in(&good), None);
        assert_eq!(audio_problem_in("Info: nothing about audio\n"), None);
        let sink = "[x]: Warning: Audio sink not found: Steam Streaming Speakers\n";
        assert_eq!(
            audio_problem_in(sink).as_deref(),
            Some("Audio sink not found: Steam Streaming Speakers")
        );
        assert_eq!(
            log_message("  Couldn't capture audio [0x1]  "),
            "Couldn't capture audio [0x1]"
        );
    }

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
    fn pin_is_sent_once_without_an_id_unless_sunshine_lists_some() {
        // Release Sunshine: 200 with a "Not Found" body, no pairings key.
        let not_found = serde_json::json!({"error": "Not Found", "status_code": 404});
        assert_eq!(pending_pairings(Some(not_found)), vec![None]);
        assert_eq!(pending_pairings(None), vec![None]);
        assert_eq!(
            pending_pairings(Some(serde_json::json!({"pairings": []}))),
            vec![None]
        );
        let master = serde_json::json!({"pairings": [{"id": "a", "name": "mac"}, {"id": "b"}]});
        assert_eq!(
            pending_pairings(Some(master)),
            vec![Some("a".to_string()), Some("b".to_string())]
        );
    }

    #[test]
    fn diagnostics_keep_the_capture_settings_and_drop_the_login() {
        assert!(is_secret("username"));
        assert!(is_secret("password"));
        assert!(is_secret("origin_web_ui_allowed_pass"));
        assert!(is_secret("salt"));
        assert!(is_secret("pkey"));
        assert!(is_secret("cert"));
        for keep in [
            "output_name",
            "adapter_name",
            "capture",
            "encoder",
            "hevc_mode",
            "dd_configuration_option",
            "resolutions",
        ] {
            assert!(!is_secret(keep), "{keep} is what the diagnosis needs");
        }
    }

    #[test]
    fn the_log_tail_is_the_end_of_the_log() {
        let log = (1..=200).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let cut = tail(&log, 150);
        assert!(cut.starts_with("51\n52\n"), "{}", &cut[..8]);
        assert!(cut.ends_with("\n200"));
        assert_eq!(cut.lines().count(), 150);
        assert_eq!(tail("one\ntwo", 150), "one\ntwo");
        assert_eq!(tail("", 150), "");
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
