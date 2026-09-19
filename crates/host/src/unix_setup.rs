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
            dir: PathBuf::from("/Applications/Sunshine.app/Contents/MacOS"),
        });
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(p) = which("sunshine") {
            if let Some(parent) = p.parent() {
                v.push(Install {
                    kind: "system",
                    dir: parent.to_path_buf(),
                });
            }
        }
    }
    v
}

/// Run the non-interactive setup path used by `--setup` and the first-run UI.
pub fn run(plan: &Plan, log: &mut Vec<String>) -> Result<()> {
    let dir = engine_dir()?;
    fs::create_dir_all(&dir).context("create engine dir")?;
    fs::create_dir_all(dir.join("config")).context("create engine config dir")?;

    ensure_engine(&dir, log)?;
    write_conf(&dir, plan, log)?;
    start_engine(&dir, log)?;
    Ok(())
}

fn ensure_engine(dir: &Path, log: &mut Vec<String>) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return install_macos(dir, log);
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
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(PathBuf::from(t))
            }
        })
}

fn write_conf(dir: &Path, plan: &Plan, log: &mut Vec<String>) -> Result<()> {
    let conf = dir.join("config").join("sunshine.conf");
    let body = conceal_conf(plan);
    fs::write(&conf, body).context("write sunshine.conf")?;
    log.push(format!("wrote {}", conf.display()));
    Ok(())
}

fn start_engine(dir: &Path, log: &mut Vec<String>) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let bin = if dir.join("Sunshine.app/Contents/MacOS/sunshine").exists() {
            dir.join("Sunshine.app/Contents/MacOS/sunshine")
        } else {
            PathBuf::from("/Applications/Sunshine.app/Contents/MacOS/sunshine")
        };
        anyhow::ensure!(bin.exists(), "Sunshine binary missing at {}", bin.display());
        let conf = dir.join("config").join("sunshine.conf");
        let mut cmd = Command::new(&bin);
        cmd.arg(&conf)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        // Detach so BroLink's --setup / UI can exit.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().context("spawn sunshine")?;
        log.push(format!("started sunshine pid {}", child.id()));
        std::thread::sleep(Duration::from_millis(400));
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let bin = which("sunshine").context("sunshine not on PATH")?;
        let conf = dir.join("config").join("sunshine.conf");
        let mut cmd = Command::new(&bin);
        cmd.arg(&conf)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().context("spawn sunshine")?;
        log.push(format!("started sunshine pid {}", child.id()));
        std::thread::sleep(Duration::from_millis(400));
        let _ = streamer::ping_local();
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (dir, log);
        bail!("cannot start engine on this OS");
    }
}
