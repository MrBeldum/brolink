//! Fetch a local FFmpeg if the user never installed one.
//!
//! The host cannot encode without it. Downloading the Gyan essentials build
//! into `%LOCALAPPDATA%\BroLink` is what makes "install the app and click
//! Connect" work on a machine that has never heard of FFmpeg.

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const FFMPEG_ZIP: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("BroLink")
}

pub fn bundled_ffmpeg() -> Option<PathBuf> {
    let p = install_dir().join("ffmpeg.exe");
    p.exists().then_some(p)
}

/// Blocking download + extract. `bytes` / `total` are for a progress bar.
pub fn download_ffmpeg(bytes: &AtomicU64, total: &AtomicU64) -> Result<PathBuf> {
    let dir = install_dir();
    fs::create_dir_all(&dir)?;
    let dest = dir.join("ffmpeg.exe");
    if dest.exists() {
        return Ok(dest);
    }

    tracing::info!("downloading FFmpeg from {FFMPEG_ZIP}");
    let resp = ureq::get(FFMPEG_ZIP)
        .timeout(std::time::Duration::from_secs(120))
        .call()
        .map_err(|e| anyhow!("download FFmpeg: {e}"))?;
    if let Some(len) = resp.header("Content-Length").and_then(|s| s.parse().ok()) {
        total.store(len, Ordering::Relaxed);
    }
    let zip_path = dir.join("ffmpeg-essentials.zip");
    {
        let mut reader = resp.into_reader();
        let mut file =
            fs::File::create(&zip_path).with_context(|| zip_path.display().to_string())?;
        let mut buf = [0u8; 64 * 1024];
        let mut n = 0u64;
        loop {
            let k = reader.read(&mut buf)?;
            if k == 0 {
                break;
            }
            file.write_all(&buf[..k])?;
            n += k as u64;
            bytes.store(n, Ordering::Relaxed);
        }
    }

    let file = fs::File::open(&zip_path)?;
    let mut archive = zip::ZipArchive::new(file).context("open FFmpeg zip")?;
    let mut found = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        if !name.ends_with("/ffmpeg.exe") && name != "ffmpeg.exe" {
            continue;
        }
        let mut out = fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
        found = Some(dest.clone());
        break;
    }
    let _ = fs::remove_file(&zip_path);
    found.ok_or_else(|| anyhow!("the FFmpeg zip did not contain ffmpeg.exe"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_dir_is_under_a_brolink_folder() {
        let d = install_dir();
        assert!(
            d.ends_with("BroLink") || d.to_string_lossy().contains("BroLink"),
            "{}",
            d.display()
        );
    }
}
