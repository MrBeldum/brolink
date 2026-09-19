//! Install and start the streaming engine on macOS and Linux.
//!
//! Windows still uses the elevated PowerShell script. Here there is no UAC
//! prompt: BroLink unpacks Sunshine next to its own data, writes the same
//! conf keys, and keeps the process up from the background service.

use anyhow::{bail, Context, Result};
#[cfg(target_os = "macos")]
use brolink_core::update::sha256_hex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::setup::{conceal_conf, Plan, DESKTOP_APPS_JSON};
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
    #[cfg(target_os = "linux")]
    {
        // BroLink's renamed engine binary, then the distro package we wrap.
        for p in ["/usr/local/bin", "/usr/bin"] {
            v.push(Install {
                kind: "BroLink",
                dir: PathBuf::from(p),
            });
        }
    }
    v
}

pub fn exe_in(dir: &Path) -> PathBuf {
    for name in ["BroLinkStreaming", "brolink-engine", "sunshine"] {
        let app = dir.join("Contents/MacOS").join(name);
        if app.exists() {
            return app;
        }
        let nested = dir.join("Sunshine.app/Contents/MacOS").join(name);
        if nested.exists() {
            return nested;
        }
        let bin = dir.join(name);
        if bin.exists() {
            return bin;
        }
    }
    dir.join("brolink-engine")
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
    let apps = conf
        .parent()
        .map(|p| p.join("apps.json"))
        .unwrap_or_else(|| dir.join("config").join("apps.json"));
    fs::write(&apps, DESKTOP_APPS_JSON).context("write apps.json")?;
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
        if which("brolink-engine").is_some() || which("sunshine").is_some() {
            log.push("using the system streaming engine".into());
            return Ok(());
        }
        bail!(
            "the streaming engine is not installed. On this VPS use the BroLink node Docker image, or run setup again."
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (dir, log);
        bail!("no streaming engine for this OS");
    }
}

/// Rename the engine so it does not appear as a second app in the Dock
/// or Activity Monitor. The on-disk `.app` folder keeps its upstream name
/// because the engine looks up resources next to that bundle.
pub fn conceal_info_plist(xml: &str) -> String {
    let mut xml = set_plist_string(xml, "CFBundleName", "BroLink");
    xml = set_plist_string(&xml, "CFBundleDisplayName", "BroLink");
    xml = set_plist_string(&xml, "CFBundleExecutable", "BroLinkStreaming");
    if xml.contains("LSUIElement") {
        xml
    } else {
        xml.replacen("</dict>", "  <key>LSUIElement</key>\n  <true/>\n</dict>", 1)
    }
}

fn set_plist_string(xml: &str, key: &str, value: &str) -> String {
    let needle = format!("<key>{key}</key>");
    let Some(at) = xml.find(&needle) else {
        return xml.to_string();
    };
    let rest = &xml[at + needle.len()..];
    let Some(s) = rest.find("<string>") else {
        return xml.to_string();
    };
    let inner = s + 8;
    let Some(e) = rest[inner..].find("</string>") else {
        return xml.to_string();
    };
    let start = at + needle.len() + inner;
    let end = start + e;
    format!("{}{value}{}", &xml[..start], &xml[end..])
}

#[cfg(target_os = "macos")]
fn install_macos(dir: &Path, log: &mut Vec<String>) -> Result<()> {
    let dest = dir.join("Sunshine.app");
    if dest.join("Contents/MacOS/BroLinkStreaming").exists()
        || dest.join("Contents/MacOS/sunshine").exists()
    {
        conceal_engine_bundle(&dest)?;
        log.push("streaming engine already unpacked".into());
        return Ok(());
    }
    let dmg = dir.join(MAC_DMG);
    let url = format!(
        "https://github.com/LizardByte/Sunshine/releases/download/{}/{MAC_DMG}",
        crate::setup::ENGINE_TAG
    );
    log.push("downloading the streaming engine".into());
    let status = Command::new("curl")
        .args(["-fsSL", "-A", "brolink", "-o"])
        .arg(&dmg)
        .arg(&url)
        .status()
        .context("download streaming engine")?;
    anyhow::ensure!(status.success(), "download of the streaming engine failed");
    let bytes = fs::read(&dmg).context("read engine archive")?;
    let got = sha256_hex(&bytes);
    anyhow::ensure!(
        got.eq_ignore_ascii_case(MAC_DMG_SHA256),
        "streaming engine digest mismatch: expected {MAC_DMG_SHA256}, got {got}"
    );
    let mount = std::env::temp_dir().join(format!("brolink-engine-{}", std::process::id()));
    let _ = fs::create_dir_all(&mount);
    let attach = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
        .arg(&mount)
        .arg(&dmg)
        .status()
        .context("hdiutil attach")?;
    anyhow::ensure!(attach.success(), "could not mount the streaming engine");
    let copied = (|| {
        let src = mount.join("Sunshine.app");
        anyhow::ensure!(src.is_dir(), "engine app missing from the disk image");
        let _ = fs::remove_dir_all(&dest);
        let status = Command::new("cp")
            .args(["-R"])
            .arg(&src)
            .arg(&dest)
            .status()
            .context("copy streaming engine")?;
        anyhow::ensure!(status.success(), "copy of the streaming engine failed");
        Ok(())
    })();
    let _ = Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount)
        .status();
    let _ = fs::remove_file(&dmg);
    copied?;
    conceal_engine_bundle(&dest)?;
    log.push("streaming engine unpacked into BroLink's data folder".into());
    Ok(())
}

#[cfg(target_os = "macos")]
fn conceal_engine_bundle(app: &Path) -> Result<()> {
    let macos = app.join("Contents/MacOS");
    let src = macos.join("sunshine");
    let dest = macos.join("BroLinkStreaming");
    if src.exists() && !dest.exists() {
        fs::rename(&src, &dest).context("rename engine binary")?;
    }
    let plist = app.join("Contents/Info.plist");
    if plist.exists() {
        let xml = fs::read_to_string(&plist).unwrap_or_default();
        fs::write(&plist, conceal_info_plist(&xml)).context("write engine Info.plist")?;
    }
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
    for name in ["brolink-engine", "BroLinkStreaming", "sunshine"] {
        let _ = Command::new("pkill")
            .args(["-x", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
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
        assert!(crate::setup::ENGINE_CONF
            .iter()
            .any(|(k, v)| *k == "sw_preset" && *v == "ultrafast"));
        assert!(crate::setup::ENGINE_CONF
            .iter()
            .any(|(k, v)| *k == "amd_quality" && *v == "speed"));
    }

    #[test]
    fn exe_in_prefers_app_bundle_then_bare_binary() {
        let tmp = std::env::temp_dir().join(format!("brolink-exe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("Sunshine.app/Contents/MacOS")).unwrap();
        fs::write(tmp.join("Sunshine.app/Contents/MacOS/sunshine"), b"").unwrap();
        assert!(exe_in(&tmp).ends_with("Contents/MacOS/sunshine"));
        fs::write(
            tmp.join("Sunshine.app/Contents/MacOS/BroLinkStreaming"),
            b"",
        )
        .unwrap();
        assert!(exe_in(&tmp).ends_with("Contents/MacOS/BroLinkStreaming"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn conceal_info_plist_renames_the_engine_and_hides_it_from_the_dock() {
        let xml = r#"<?xml version="1.0"?>
<dict>
  <key>CFBundleName</key>
  <string>Sunshine</string>
  <key>CFBundleDisplayName</key>
  <string>Sunshine</string>
  <key>CFBundleExecutable</key>
  <string>sunshine</string>
  <key>CFBundleIdentifier</key>
  <string>dev.lizardbyte.sunshine</string>
</dict>
"#;
        let got = conceal_info_plist(xml);
        assert!(got.contains("<string>BroLink</string>"), "{got}");
        assert!(got.contains("<string>BroLinkStreaming</string>"), "{got}");
        assert!(got.contains("LSUIElement"), "{got}");
        assert!(
            got.contains("dev.lizardbyte.sunshine"),
            "identifier stays so the engine still finds its files:\n{got}"
        );
        assert_eq!(conceal_info_plist(&got), got);
    }
}
