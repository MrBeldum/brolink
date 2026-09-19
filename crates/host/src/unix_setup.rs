//! Install and start the streaming engine on macOS and Linux.
//!
//! Windows still uses the elevated PowerShell script. Here there is no UAC
//! prompt: BroLink unpacks Sunshine next to its own data, writes the same
//! conf keys, and keeps the process up from the background service.

use anyhow::{bail, Context, Result};
use brolink_core::update::sha256_hex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::setup::{conceal_conf, Plan};
use crate::streamer::{self, Install};

pub const MAC_DMG: &str = "Sunshine-macOS-arm64.dmg";
pub const MAC_DMG_SHA256: &str = "b630d35a184d8eaff39c5104f3c6a0c40e91ddc447ccf7a0c5b24706465fab6a";

pub fn engine_dir() -> Result<PathBuf> {
    Ok(brolink_core::config::data_dir()?.join("engine"))
}

pub fn conf_path() -> Result<PathBuf> {
    Ok(engine_dir()?.join("config").join("sunshine.conf"))
}

/// Candidate engine installs, BroLink's copy first.
pub fn candidates() -> Vec<Install> {
    let mut v = Vec::new();
    if let Ok(dir) = engine_dir() {
        v.push(Install {
            kind: "BroLink",
            dir,
        });
    }
    #[cfg(target_os = "macos")]
    {
        v.push(Install {
            kind: "Sunshine",
            dir: PathBuf::from("/Applications/Sunshine.app"),
        });
        for p in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/opt/homebrew/opt/sunshine/bin",
        ] {
            v.push(Install {
                kind: "Sunshine",
                dir: PathBuf::from(p),
            });
        }
    }
    #[cfg(target_os = "linux")]
    {
        for p in ["/usr/bin", "/usr/local/bin", "/opt/sunshine"] {
            v.push(Install {
                kind: "Sunshine",
                dir: PathBuf::from(p),
            });
        }
    }
    v
}

pub fn exe_in(dir: &Path) -> PathBuf {
    let app = dir.join("Contents/MacOS/sunshine");
    if app.exists() {
        return app;
    }
    let nested = dir.join("Sunshine.app/Contents/MacOS/sunshine");
    if nested.exists() {
        return nested;
    }
    let bin = dir.join("sunshine");
    if bin.exists() {
        return bin;
    }
    PathBuf::from("sunshine")
}

pub fn run(p: &Plan<'_>) -> Result<()> {
    let log =
        crate::setup::log_path().unwrap_or_else(|| std::env::temp_dir().join("brolink-setup.log"));
    let mut lines = Vec::new();
    let result = run_inner(p, &mut lines);
    let body = lines.join("\n") + "\n";
    let _ = fs::write(&log, &body);
    result.with_context(|| format!("see {}", log.display()))
}

fn run_inner(p: &Plan<'_>, log: &mut Vec<String>) -> Result<()> {
    fn step(log: &mut Vec<String>, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::info!("{msg}");
        log.push(msg);
    }

    let dir = engine_dir()?;
    fs::create_dir_all(dir.join("config")).context("engine config dir")?;
    step(log, format!("engine directory {}", dir.display()));

    if p.install_engine || streamer::find().is_none() {
        install_engine(&dir, log)?;
    }

    let conf = conf_path()?;
    let existing = fs::read_to_string(&conf).unwrap_or_default();
    fs::write(&conf, conceal_conf(&existing)).context("write sunshine.conf")?;
    step(
        log,
        "wrote streaming profile (constant bitrate, Tailscale packet size)",
    );

    let Some(install) = streamer::find() else {
        bail!("the streaming engine is not installed");
    };
    let exe = install.exe();
    if !p.sunshine_user.is_empty() && !p.sunshine_pass.is_empty() {
        let out = Command::new(&exe)
            .arg(&conf)
            .args(["--creds", p.sunshine_user, p.sunshine_pass])
            .output()
            .context("sunshine --creds")?;
        if !out.status.success() {
            step(
                log,
                format!(
                    "sunshine --creds: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            );
        } else {
            step(log, "engine login set");
        }
    }

    stop_engine();
    std::thread::sleep(Duration::from_millis(400));
    streamer::start(&install).context("start streaming engine")?;
    step(log, "streaming engine started");

    if let Ok(exe) = std::env::current_exe() {
        set_autostart(true, &exe)?;
        step(log, "BroLink starts at login so others can connect");
    }
    Ok(())
}

fn install_engine(dir: &Path, log: &mut Vec<String>) -> Result<()> {
    if exe_in(dir).exists() {
        log.push("engine already unpacked".into());
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        install_macos(dir, log)
    }
    #[cfg(target_os = "linux")]
    {
        if which("sunshine").is_some() {
            log.push("using the system sunshine binary".into());
            return Ok(());
        }
        bail!(
            "Sunshine is not installed. On this VPS use the BroLink node Docker image, or install Sunshine and run setup again."
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (dir, log);
        bail!("no streaming engine for this OS");
    }
}

#[cfg(target_os = "macos")]
fn install_macos(dir: &Path, log: &mut Vec<String>) -> Result<()> {
    if Path::new("/Applications/Sunshine.app/Contents/MacOS/sunshine").exists() {
        log.push("using /Applications/Sunshine.app".into());
        return Ok(());
    }
    let dmg = dir.join(MAC_DMG);
    let url = format!(
        "https://github.com/LizardByte/Sunshine/releases/download/{}/{MAC_DMG}",
        crate::setup::ENGINE_TAG
    );
    log.push(format!("downloading {url}"));
    let status = Command::new("curl")
        .args(["-fsSL", "-A", "brolink", "-o"])
        .arg(&dmg)
        .arg(&url)
        .status()
        .context("curl Sunshine dmg")?;
    anyhow::ensure!(status.success(), "download of Sunshine failed");
    let bytes = fs::read(&dmg).context("read dmg")?;
    let got = sha256_hex(&bytes);
    anyhow::ensure!(
        got.eq_ignore_ascii_case(MAC_DMG_SHA256),
        "Sunshine dmg digest mismatch: expected {MAC_DMG_SHA256}, got {got}"
    );
    let mount = std::env::temp_dir().join(format!("brolink-sunshine-{}", std::process::id()));
    let _ = fs::create_dir_all(&mount);
    let attach = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
        .arg(&mount)
        .arg(&dmg)
        .status()
        .context("hdiutil attach")?;
    anyhow::ensure!(attach.success(), "could not mount the Sunshine disk image");
    let copied = (|| {
        let src = mount.join("Sunshine.app");
        anyhow::ensure!(src.is_dir(), "Sunshine.app missing from the disk image");
        let dest = dir.join("Sunshine.app");
        let _ = fs::remove_dir_all(&dest);
        let status = Command::new("cp")
            .args(["-R"])
            .arg(&src)
            .arg(&dest)
            .status()
            .context("copy Sunshine.app")?;
        anyhow::ensure!(status.success(), "copy Sunshine.app failed");
        Ok(())
    })();
    let _ = Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount)
        .status();
    let _ = fs::remove_file(&dmg);
    copied?;
    log.push("Sunshine.app unpacked into BroLink's engine folder".into());
    Ok(())
}

#[cfg(target_os = "linux")]
fn which(name: &str) -> Option<PathBuf> {
    Command::new("which")
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub fn stop_engine() {
    let _ = Command::new("pkill")
        .args(["-f", "sunshine"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn set_autostart(enable: bool, exe: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos_launch_agent(enable, exe)
    }
    #[cfg(target_os = "linux")]
    {
        linux_user_unit(enable, exe)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (enable, exe);
        Ok(())
    }
}

pub fn autostart_enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        launch_agent_path().is_ok_and(|p| p.exists())
    }
    #[cfg(target_os = "linux")]
    {
        user_unit_path().is_ok_and(|p| p.exists())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Result<PathBuf> {
    let home = dirs_home()?;
    Ok(home.join("Library/LaunchAgents/dev.brolink.node.plist"))
}

#[cfg(target_os = "macos")]
fn macos_launch_agent(enable: bool, exe: &Path) -> Result<()> {
    let path = launch_agent_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = Command::new("launchctl")
        .args(["unload"])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !enable {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let exe = exe.display().to_string();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.brolink.node</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--background</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
"#
    );
    fs::write(&path, plist)?;
    let _ = Command::new("launchctl")
        .args(["load"])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn user_unit_path() -> Result<PathBuf> {
    let home = dirs_home()?;
    Ok(home.join(".config/systemd/user/brolink.service"))
}

#[cfg(target_os = "linux")]
fn linux_user_unit(enable: bool, exe: &Path) -> Result<()> {
    let path = user_unit_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if !enable {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "brolink.service"])
            .status();
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let unit = format!(
        "[Unit]\nDescription=BroLink\nAfter=network.target\n\n[Service]\nExecStart={} --background\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
        exe.display()
    );
    fs::write(&path, unit)?;
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let _ = Command::new("systemctl")
        .args(["--user", "enable", "--now", "brolink.service"])
        .status();
    Ok(())
}

fn dirs_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_dmg_pin_is_64_hex() {
        assert_eq!(MAC_DMG_SHA256.len(), 64);
        assert_eq!(MAC_DMG, "Sunshine-macOS-arm64.dmg");
        assert!(!crate::setup::ENGINE_TAG.is_empty());
        assert!(crate::setup::ENGINE_CONF
            .iter()
            .any(|(k, v)| *k == "amd_rc" && *v == "cbr"));
    }

    #[test]
    fn exe_in_prefers_app_bundle_then_bare_binary() {
        let tmp = std::env::temp_dir().join(format!("brolink-exe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("Sunshine.app/Contents/MacOS")).unwrap();
        fs::write(tmp.join("Sunshine.app/Contents/MacOS/sunshine"), b"").unwrap();
        assert!(exe_in(&tmp).ends_with("Contents/MacOS/sunshine"));
        let _ = fs::remove_dir_all(&tmp);
    }
}
