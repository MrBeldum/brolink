//! Fetching installers with the `curl` every Mac and every Windows 10+ PC
//! already has. No HTTP client crate, no TLS stack of our own.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

fn curl() -> Command {
    let mut c = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    c.args(["-sSL", "--fail", "--retry", "2", "-A", "brolink"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Body of `url` as text.
pub fn text(url: &str) -> Result<String> {
    let out = curl().arg(url).output().context("run curl")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Save `url` to `dest`, replacing it.
pub fn file(url: &str, dest: &Path) -> Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let out = curl()
        .arg("-o")
        .arg(dest)
        .arg(url)
        .output()
        .context("run curl")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The download URL and tag of the newest release asset of `owner/repo`
/// whose name `matches`.
pub fn latest_github_asset(repo: &str, matches: impl Fn(&str) -> bool) -> Result<(String, String)> {
    let json = text(&format!(
        "https://api.github.com/repos/{repo}/releases/latest"
    ))?;
    pick_asset(&json, matches)
}

fn pick_asset(json: &str, matches: impl Fn(&str) -> bool) -> Result<(String, String)> {
    let rel: Release = serde_json::from_str(json).context("parse GitHub release")?;
    let asset = rel
        .assets
        .iter()
        .find(|a| matches(&a.name))
        .with_context(|| format!("release {} has no matching asset", rel.tag_name))?;
    Ok((asset.browser_download_url.clone(), rel.tag_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_matching_asset() {
        let json = r#"{"tag_name":"v2026.516","assets":[
            {"name":"Sunshine-Windows-ARM64-installer.msi","browser_download_url":"https://x/arm"},
            {"name":"Sunshine-Windows-AMD64-installer.msi","browser_download_url":"https://x/amd"}]}"#;
        let (url, tag) = pick_asset(json, |n| n.ends_with("Windows-AMD64-installer.msi")).unwrap();
        assert_eq!(url, "https://x/amd");
        assert_eq!(tag, "v2026.516");
        assert!(pick_asset(json, |n| n.ends_with(".dmg")).is_err());
    }
}
