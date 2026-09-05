//! Driving Moonlight through its command line: install it, list a PC's
//! apps, pair, stream. Moonlight does the decoding, input and audio; this
//! file only knows how to start it.

use crate::config::StreamSettings;
use anyhow::{anyhow, bail, Context, Result};
use brolink_core::download;
use std::io::Read;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const REPO: &str = "moonlight-stream/moonlight-qt";

/// The Moonlight binary, if installed.
pub fn cli() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        let mut v = vec![PathBuf::from(
            "/Applications/Moonlight.app/Contents/MacOS/Moonlight",
        )];
        if let Some(home) = std::env::var_os("HOME") {
            v.push(PathBuf::from(home).join("Applications/Moonlight.app/Contents/MacOS/Moonlight"));
        }
        v
    } else if cfg!(windows) {
        let mut v = Vec::new();
        for base in ["ProgramFiles", "LOCALAPPDATA"] {
            if let Some(p) = std::env::var_os(base) {
                let p = PathBuf::from(p);
                v.push(p.join("Moonlight Game Streaming").join("Moonlight.exe"));
                v.push(
                    p.join("Programs")
                        .join("Moonlight Game Streaming")
                        .join("Moonlight.exe"),
                );
            }
        }
        v
    } else {
        vec![
            PathBuf::from("/usr/bin/moonlight-qt"),
            PathBuf::from("/usr/local/bin/moonlight-qt"),
        ]
    };
    candidates.retain(|p| p.exists());
    candidates.into_iter().next()
}

pub fn is_dmg(name: &str) -> bool {
    name.starts_with("Moonlight") && name.ends_with(".dmg")
}

/// macOS: fetch the latest release DMG and copy Moonlight.app into
/// /Applications. `note` is shown in the UI while this runs.
pub fn install(note: &dyn Fn(&str)) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("Install Moonlight from https://moonlight-stream.org and come back.");
    }
    note("Finding the latest Moonlight release…");
    let (url, tag) = download::latest_github_asset(REPO, is_dmg)?;
    let dmg = std::env::temp_dir().join("brolink-moonlight.dmg");
    let mnt = std::env::temp_dir().join("brolink-moonlight-mnt");
    note(&format!("Downloading Moonlight {tag}…"));
    download::file(&url, &dmg)?;
    note("Copying Moonlight to /Applications…");
    let _ = sh(&[
        "hdiutil",
        "detach",
        "-quiet",
        mnt.to_str().unwrap_or_default(),
    ]);
    sh(&[
        "hdiutil",
        "attach",
        "-nobrowse",
        "-quiet",
        "-readonly",
        "-mountpoint",
        mnt.to_str().unwrap_or_default(),
        dmg.to_str().unwrap_or_default(),
    ])?;
    let copied = (|| -> Result<()> {
        let _ = std::fs::remove_dir_all("/Applications/Moonlight.app");
        sh(&[
            "ditto",
            &format!("{}/Moonlight.app", mnt.display()),
            "/Applications/Moonlight.app",
        ])?;
        Ok(())
    })();
    let _ = sh(&[
        "hdiutil",
        "detach",
        "-quiet",
        mnt.to_str().unwrap_or_default(),
    ]);
    let _ = std::fs::remove_file(&dmg);
    copied?;
    if cli().is_none() {
        bail!("Moonlight was copied but is not where expected");
    }
    Ok(())
}

fn sh(argv: &[&str]) -> Result<()> {
    let out = Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .with_context(|| argv[0].to_string())?;
    if !out.status.success() {
        bail!(
            "{} failed: {}",
            argv[0],
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn command() -> Result<Command> {
    let bin = cli().ok_or_else(|| anyhow!("Moonlight is not installed"))?;
    let mut c = Command::new(bin);
    c.stdin(Stdio::null());
    Ok(c)
}

/// The app names Sunshine offers, or why Moonlight could not ask.
/// A host that is not paired yet is reported as [`NotPaired`].
pub fn list(ip: Ipv4Addr) -> Result<Vec<String>> {
    let mut c = command()?;
    c.args(["list", &ip.to_string()]);
    let out = run_with_timeout(c, Duration::from_secs(25))?;
    if out.ok {
        Ok(out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    } else if out.stderr.contains("not been paired") {
        Err(anyhow!(NotPaired))
    } else {
        Err(anyhow!(
            "{}",
            first_line(&out.stderr, "Moonlight could not reach the PC")
        ))
    }
}

#[derive(Debug)]
pub struct NotPaired;
impl std::fmt::Display for NotPaired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not paired")
    }
}
impl std::error::Error for NotPaired {}

/// Start pairing with `pin`; Moonlight waits until the PC accepts it.
pub fn pair(ip: Ipv4Addr, pin: &str) -> Result<Child> {
    let mut c = command()?;
    c.args(["pair", &ip.to_string(), "--pin", pin])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c.spawn().context("start Moonlight pairing")
}

/// Start the stream. The child lives as long as the session.
pub fn stream(ip: Ipv4Addr, s: &StreamSettings, native: (u32, u32)) -> Result<Child> {
    let mut c = command()?;
    c.args(stream_args(ip, s, native))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    c.spawn().context("start Moonlight")
}

pub fn stream_args(ip: Ipv4Addr, s: &StreamSettings, native: (u32, u32)) -> Vec<String> {
    let (w, h) = s.resolution.pixels(native);
    let mut v: Vec<String> = vec![
        "stream".into(),
        ip.to_string(),
        s.app.clone(),
        "--display-mode".into(),
        if s.fullscreen {
            "fullscreen"
        } else {
            "windowed"
        }
        .into(),
        "--resolution".into(),
        format!("{w}x{h}"),
        "--fps".into(),
        s.fps.to_string(),
        "--bitrate".into(),
        s.bitrate_kbps.to_string(),
        "--video-codec".into(),
        s.codec.moonlight().into(),
        "--audio-config".into(),
        "stereo".into(),
        // Cmd-Tab and friends go to the PC while the stream fills the screen.
        "--capture-system-keys".into(),
        "fullscreen".into(),
    ];
    v.push(
        if s.game_mode {
            "--no-absolute-mouse"
        } else {
            "--absolute-mouse"
        }
        .into(),
    );
    v
}

/// Ask a streaming Moonlight to stop: gently first, so Sunshine sees a clean
/// disconnect, then by force.
pub fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run to completion, killing the process at the deadline. Output is drained
/// on threads so a chatty child cannot wedge on a full pipe.
pub fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<Output> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("start Moonlight")?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = out.join().unwrap_or_default();
    let mut stderr = err.join().unwrap_or_default();
    if status.is_none() {
        stderr.push_str("\nMoonlight did not answer in time");
    }
    Ok(Output {
        ok: status.is_some_and(|s| s.success()),
        stdout,
        stderr,
    })
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut r) = r {
            let _ = r.read_to_string(&mut s);
        }
        s
    })
}

pub fn first_line<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Codec, Resolution};

    #[test]
    fn stream_args_map_settings_to_moonlight_flags() {
        let ip: Ipv4Addr = "100.64.0.10".parse().unwrap();
        let s = StreamSettings {
            game_mode: false,
            resolution: Resolution::Native,
            fps: 120,
            bitrate_kbps: 40_000,
            codec: Codec::Hevc,
            fullscreen: true,
            app: "Desktop".into(),
        };
        let a = stream_args(ip, &s, (3024, 1964));
        assert_eq!(&a[..3], &["stream", "100.64.0.10", "Desktop"]);
        let joined = a.join(" ");
        assert!(joined.contains("--display-mode fullscreen"));
        assert!(joined.contains("--resolution 3024x1964"));
        assert!(joined.contains("--fps 120"));
        assert!(joined.contains("--bitrate 40000"));
        assert!(joined.contains("--video-codec HEVC"));
        assert!(joined.ends_with("--absolute-mouse"));

        let s = StreamSettings {
            game_mode: true,
            fullscreen: false,
            ..s
        };
        let joined = stream_args(ip, &s, (1, 1)).join(" ");
        assert!(joined.contains("--display-mode windowed"));
        assert!(joined.ends_with("--no-absolute-mouse"));
    }

    #[test]
    fn dmg_asset_is_recognised() {
        assert!(is_dmg("Moonlight-6.1.0.dmg"));
        assert!(!is_dmg("MoonlightSetup-6.1.0.exe"));
        assert!(!is_dmg("Moonlight-SteamLink-6.1.0.zip"));
    }

    #[test]
    fn timeouts_kill_the_child() {
        let mut c = Command::new(if cfg!(windows) { "ping" } else { "sleep" });
        if cfg!(windows) {
            c.args(["-n", "10", "127.0.0.1"]);
        } else {
            c.arg("10");
        }
        let t = Instant::now();
        let out = run_with_timeout(c, Duration::from_millis(300)).unwrap();
        assert!(!out.ok);
        assert!(out.stderr.contains("did not answer"));
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn first_line_skips_blanks() {
        assert_eq!(
            first_line("\n  \nFailed to connect to X\nmore", "?"),
            "Failed to connect to X"
        );
        assert_eq!(first_line("", "fallback"), "fallback");
    }
}
