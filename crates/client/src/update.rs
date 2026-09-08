//! Keeping this app and every BroLink Host it can see on the newest release.
//!
//! The Mac is the one machine with a GitHub login (the repository is
//! private), so it does the fetching for everyone. Every few hours it asks
//! GitHub for the latest release. A newer app is downloaded, verified against
//! GitHub's digest and its own code signature, and swapped into place once no
//! stream is running; the app then relaunches itself. A newer host is
//! downloaded once, and `brolink-host.exe` is sent to each PC whose host
//! already speaks `/v1/update` (3.1+) and reports an older version, over
//! the same Tailscale-authenticated control API that can put the PC to
//! sleep. A 3.0 host cannot take that (it caps request bodies at 64 KiB
//! and closes, which showed as a broken pipe); for it, the stream's PC
//! menu offers to install the new host through the stream instead (see
//! `handover.rs`). A PC that is asleep gets it the next time it is seen.

use crate::config::ClientConfig;
use crate::session::{Discovery, Live};
use anyhow::{anyhow, bail, Context, Result};
use brolink_core::api::{Ack, UPDATE_PATH, UPDATE_SHA256_HEADER, UPDATE_VERSION_HEADER};
use brolink_core::update::{self, Release, HOST_EXE, MAC_ASSET, WINDOWS_ASSET};
use brolink_core::{http, CONTROL_PORT};
use brolink_ui::Tone;
use parking_lot::Mutex;
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const RETRY_AFTER: Duration = Duration::from_secs(15 * 60);
/// After a host got its update, leave it alone this long: it restarts and
/// then reports the new version by itself.
const PUSH_GRACE: Duration = Duration::from_secs(180);
const TICK: Duration = Duration::from_secs(5);

/// What the updater is doing, for the settings card and the lobby.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// One line: "Up to date", "Could not check…", "Sent 3.1.0 to Gaming-PC".
    pub message: String,
    pub checked: Option<Instant>,
    pub latest: Option<Version>,
    /// The latest release as fetched, for whoever needs its assets.
    pub release: Option<Release>,
    /// Set by the UI to check right away.
    pub check_now: bool,
    /// A new app is downloaded and verified; it goes in once no stream runs.
    pub ready: Option<Version>,
    /// Something the lobby should show while it is true of the moment.
    pub notice: Option<(Tone, String)>,
    /// Hosts sent an update recently: node id to (version, when).
    pub pushed: BTreeMap<String, (Version, Instant)>,
    /// Hosts too old to receive `/v1/update`; told the user once.
    pub told_old: BTreeSet<String>,
}

pub fn spawn(
    state: Arc<Mutex<State>>,
    discovery: Arc<Mutex<Discovery>>,
    live: Arc<Mutex<Option<Live>>>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let mut next = Instant::now() + FIRST_CHECK;
        let mut release: Option<Release> = None;
        loop {
            std::thread::sleep(TICK);
            let cfg = ClientConfig::load();
            if !cfg.auto_update {
                let mut st = state.lock();
                st.message = "Off. This app and the PCs stay on their current versions.".into();
                st.notice = None;
                continue;
            }
            let token = update::token(cfg.github_token.as_deref());
            let due = state.lock().check_now || Instant::now() >= next;
            if due {
                state.lock().check_now = false;
                match update::latest(token.as_deref()) {
                    Ok(r) => {
                        next = Instant::now() + CHECK_EVERY;
                        let mut st = state.lock();
                        st.checked = Some(Instant::now());
                        st.latest = Some(r.version.clone());
                        st.release = Some(r.clone());
                        st.message = if r.is_newer_than(&update::current()) {
                            format!("BroLink {} is available.", r.version)
                        } else {
                            format!("Up to date (v{}).", update::current())
                        };
                        release = Some(r);
                    }
                    Err(e) => {
                        next = Instant::now() + RETRY_AFTER;
                        let mut st = state.lock();
                        st.checked = Some(Instant::now());
                        st.message = format!("Could not check for updates: {e}.");
                        tracing::warn!("update check: {e:#}");
                    }
                }
                ctx.request_repaint();
            }
            let Some(rel) = release.clone() else { continue };
            let rel = &rel;

            // This app first.
            if rel.is_newer_than(&update::current()) && state.lock().ready.is_none() {
                match prepare_self(rel, token.as_deref()) {
                    Ok(Some(v)) => {
                        let mut st = state.lock();
                        st.ready = Some(v.clone());
                        st.notice = Some((
                            Tone::Info,
                            format!(
                                "BroLink {v} is downloaded and installs when no stream is running."
                            ),
                        ));
                    }
                    Ok(None) => {
                        state.lock().message = format!(
                            "BroLink {} is available. This copy is not in an app bundle, so it is not replaced.",
                            rel.version
                        );
                    }
                    Err(e) => {
                        state.lock().message =
                            format!("Could not download BroLink {}: {e}.", rel.version);
                        tracing::warn!("self-update: {e:#}");
                        // Try again at the next check rather than every tick.
                        release = None;
                        next = Instant::now() + RETRY_AFTER;
                        ctx.request_repaint();
                        continue;
                    }
                }
                ctx.request_repaint();
            }
            let ready = state.lock().ready.clone();
            if let Some(v) = ready {
                if live.lock().is_none() {
                    match install_self(rel) {
                        Ok(()) => relaunch(),
                        Err(e) => {
                            let mut st = state.lock();
                            st.ready = None;
                            st.notice = None;
                            st.message = format!("Could not install BroLink {v}: {e}.");
                            tracing::error!("self-update: {e:#}");
                            release = None;
                            next = Instant::now() + RETRY_AFTER;
                        }
                    }
                    ctx.request_repaint();
                }
            }

            // Then the PCs whose host is older.
            let disc = discovery.lock().clone();
            let streaming_to = live.lock().as_ref().map(|l| l.ip);
            for pc in disc.pcs.iter().filter(|p| !p.remembered) {
                let (Some(ip), Some(h)) = (pc.ip, pc.host.as_ref()) else {
                    continue;
                };
                let Ok(v) = Version::parse(&h.version) else {
                    continue;
                };
                if !update::host_can_receive_update(&v) {
                    // The lobby shows this PC what to do; here, just note it.
                    let mut st = state.lock();
                    if st.told_old.insert(pc.node_id.clone()) {
                        st.message = old_host_message(&pc.name, &v);
                        ctx.request_repaint();
                    }
                    continue;
                }
                if !should_push(&v, rel) || streaming_to == Some(ip) {
                    continue;
                }
                let recently = state
                    .lock()
                    .pushed
                    .get(&pc.node_id)
                    .is_some_and(|(pv, at)| *pv == rel.version && at.elapsed() < PUSH_GRACE);
                if recently {
                    continue;
                }
                let outcome = push_host(rel, token.as_deref(), ip);
                let mut st = state.lock();
                st.pushed
                    .insert(pc.node_id.clone(), (rel.version.clone(), Instant::now()));
                match outcome {
                    Ok(()) => {
                        st.message = format!(
                            "Sent BroLink Host {} to {}; it restarts by itself.",
                            rel.version, pc.name
                        );
                        st.notice = Some((Tone::Info, st.message.clone()));
                        tracing::info!("sent host {} to {} ({ip})", rel.version, pc.name);
                    }
                    Err(e) => {
                        st.message = format!("Could not update {}: {e}.", pc.name);
                        st.notice = Some((Tone::Danger, st.message.clone()));
                        tracing::warn!("host update for {}: {e:#}", pc.name);
                    }
                }
                ctx.request_repaint();
            }
            // Notices about hosts fade once the grace period has passed.
            let mut st = state.lock();
            if st.ready.is_none() && st.pushed.values().all(|(_, at)| at.elapsed() > PUSH_GRACE) {
                st.notice = None;
            }
        }
    });
}

/// Where downloads for `rel` live: `<data dir>/updates/<tag>/`.
fn updates_dir(rel: &Release) -> Result<PathBuf> {
    let dir = brolink_core::config::data_dir()?
        .join("updates")
        .join(&rel.tag);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The asset on disk, downloading it if needed. A file that exists was
/// verified when it was written.
fn fetch_asset(rel: &Release, name: &str, token: Option<&str>) -> Result<PathBuf> {
    let asset = rel
        .asset(name)
        .ok_or_else(|| anyhow!("release {} has no {name}", rel.tag))?;
    let dest = updates_dir(rel)?.join(name);
    if !dest.exists() {
        update::download(asset, token, &dest)?;
    }
    Ok(dest)
}

/// The `.app` this executable runs from, when it does.
pub fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // .../BroLink.app/Contents/MacOS/BroLink
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension().is_some_and(|e| e == "app")).then(|| app.to_path_buf())
}

fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| cmd.to_string())?;
    if !out.status.success() {
        bail!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Download and verify the new app. `Ok(None)` when this copy is not in a
/// bundle (a development build), which is left alone.
fn prepare_self(rel: &Release, token: Option<&str>) -> Result<Option<Version>> {
    if bundle_path().is_none() {
        return Ok(None);
    }
    let tarball = fetch_asset(rel, MAC_ASSET, token)?;
    let unpacked = updates_dir(rel)?.join("unpacked");
    let app = unpacked.join("BroLink.app");
    if !app.exists() {
        let _ = std::fs::remove_dir_all(&unpacked);
        std::fs::create_dir_all(&unpacked)?;
        run(
            "/usr/bin/tar",
            &[
                "xzf",
                &tarball.display().to_string(),
                "-C",
                &unpacked.display().to_string(),
            ],
        )?;
    }
    anyhow::ensure!(app.exists(), "the download holds no BroLink.app");
    run(
        "/usr/bin/codesign",
        &["--verify", "--deep", "--strict", &app.display().to_string()],
    )
    .context("the new app's signature does not verify")?;
    let version = run(
        "/usr/bin/plutil",
        &[
            "-extract",
            "CFBundleShortVersionString",
            "raw",
            &app.join("Contents/Info.plist").display().to_string(),
        ],
    )?;
    let version =
        Version::parse(&version).with_context(|| format!("bundle version {version:?}"))?;
    anyhow::ensure!(
        version == rel.version,
        "the download says {version}, the release {}",
        rel.version
    );
    Ok(Some(version))
}

/// Move the verified app over this one. macOS keeps the running executable
/// alive across the rename, so the swap is safe while we are still up.
fn install_self(rel: &Release) -> Result<()> {
    let bundle = bundle_path().ok_or_else(|| anyhow!("not running from an app bundle"))?;
    let new = updates_dir(rel)?.join("unpacked").join("BroLink.app");
    anyhow::ensure!(new.exists(), "nothing is downloaded");
    let parked = bundle.with_extension("old");
    let _ = std::fs::remove_dir_all(&parked);
    std::fs::rename(&bundle, &parked)
        .with_context(|| format!("move {} aside", bundle.display()))?;
    let moved = std::fs::rename(&new, &bundle).or_else(|_| {
        // Another volume: copy instead.
        run(
            "/usr/bin/ditto",
            &[&new.display().to_string(), &bundle.display().to_string()],
        )
        .map(|_| ())
    });
    if let Err(e) = moved {
        let _ = std::fs::rename(&parked, &bundle);
        return Err(e).context("put the new app in place");
    }
    let _ = run(
        "/usr/bin/xattr",
        &["-dr", "com.apple.quarantine", &bundle.display().to_string()],
    );
    let _ = std::fs::remove_dir_all(&parked);
    let _ = std::fs::remove_dir_all(updates_dir(rel)?.join("unpacked"));
    Ok(())
}

/// Start the new copy and leave. `open` goes through LaunchServices, so the
/// new instance gets a proper app launch rather than inheriting this one.
fn relaunch() {
    if let Some(bundle) = bundle_path() {
        let script = format!("sleep 1; /usr/bin/open \"{}\"", bundle.display());
        let _ = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    tracing::info!("relaunching into the new version");
    std::process::exit(0);
}

/// `brolink-host.exe` out of the Windows zip, kept beside it.
pub(crate) fn host_exe(rel: &Release, token: Option<&str>) -> Result<Vec<u8>> {
    let dir = updates_dir(rel)?;
    let exe = dir.join(HOST_EXE);
    if let Ok(bytes) = std::fs::read(&exe) {
        if bytes.len() > 1024 * 1024 {
            return Ok(bytes);
        }
    }
    let zip = fetch_asset(rel, WINDOWS_ASSET, token)?;
    let out = std::process::Command::new("/usr/bin/unzip")
        .args(["-p", &zip.display().to_string(), HOST_EXE])
        .output()
        .context("unzip")?;
    if !out.status.success() || out.stdout.len() < 1024 * 1024 {
        bail!("{} holds no {HOST_EXE}", WINDOWS_ASSET);
    }
    std::fs::write(&exe, &out.stdout)?;
    Ok(out.stdout)
}

/// A host that already speaks `/v1/update`, and a release newer than it.
fn should_push(running: &Version, rel: &Release) -> bool {
    update::host_can_receive_update(running) && rel.is_newer_than(running)
}

pub fn old_host_message(name: &str, version: &Version) -> String {
    format!(
        "{name} runs BroLink Host {version}, which cannot take an update over the network. Connect to it and choose PC → Update BroLink Host in the toolbar: this Mac installs the new version through the stream. After that, updates are automatic."
    )
}

fn is_transient(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("timed out")
        || s.contains("connection closed")
        || s.contains("Broken pipe")
        || s.contains("Connection reset")
        || s.contains("os error 32")
        || s.contains("os error 35")
}

/// Send the new host to the PC at `ip`. The host checks the digest, swaps
/// the file in and restarts.
fn push_host(rel: &Release, token: Option<&str>, ip: Ipv4Addr) -> Result<()> {
    let exe = host_exe(rel, token)?;
    let sha = update::sha256_hex(&exe);
    let version = rel.version.to_string();
    let mut last = None;
    for attempt in 1..=3 {
        match send_host(ip, &version, &sha, &exe) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 3 && is_transient(&e) => {
                tracing::warn!("host update attempt {attempt}/3: {e:#}");
                std::thread::sleep(Duration::from_secs(2 * attempt as u64));
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("update failed")))
}

fn send_host(ip: Ipv4Addr, version: &str, sha: &str, exe: &[u8]) -> Result<()> {
    let r = http::request_with(
        (ip, CONTROL_PORT),
        "POST",
        UPDATE_PATH,
        &[
            (UPDATE_VERSION_HEADER, version),
            (UPDATE_SHA256_HEADER, sha),
            ("Content-Type", "application/octet-stream"),
        ],
        exe,
        Duration::from_secs(120),
    )?;
    if r.status != 200 {
        let why = r
            .parse::<Ack>()
            .ok()
            .and_then(|a| a.error)
            .unwrap_or_else(|| format!("HTTP {}", r.status));
        bail!("{why}");
    }
    Ok(())
}

/// For the settings card: when the last check happened.
pub fn ago(at: Option<Instant>) -> String {
    match at {
        None => "not checked yet".into(),
        Some(t) => {
            let s = t.elapsed().as_secs();
            if s < 90 {
                "checked just now".into()
            } else if s < 5400 {
                format!("checked {} min ago", s / 60)
            } else {
                format!("checked {} h ago", s / 3600)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_found_only_inside_an_app() {
        // The test binary lives in target/, not in a .app.
        assert_eq!(bundle_path(), None);
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(None), "not checked yet");
        assert_eq!(ago(Some(Instant::now())), "checked just now");
        let earlier = Instant::now() - Duration::from_secs(20 * 60);
        assert_eq!(ago(Some(earlier)), "checked 20 min ago");
        let earlier = Instant::now() - Duration::from_secs(3 * 3600);
        assert_eq!(ago(Some(earlier)), "checked 3 h ago");
    }

    fn rel(v: &str) -> Release {
        Release {
            version: Version::parse(v).unwrap(),
            tag: format!("v{v}"),
            prerelease: false,
            assets: vec![],
        }
    }

    #[test]
    fn a_host_older_than_3_1_is_not_posted_the_executable() {
        // Gaming-PC today: 3.0.0. GitHub latest: 3.0.1. POSTing 10 MB at that
        // host is a broken pipe; the Mac must skip it.
        assert!(!should_push(&Version::new(3, 0, 0), &rel("3.0.1")));
        assert!(!should_push(&Version::new(3, 0, 0), &rel("3.1.0")));
        assert!(!should_push(&Version::new(3, 0, 1), &rel("3.1.0")));
        assert!(!should_push(&Version::new(3, 1, 0), &rel("3.1.0")));
        assert!(should_push(&Version::new(3, 1, 0), &rel("3.2.0")));
        assert!(should_push(
            &Version::parse("3.1.1").unwrap(),
            &rel("3.2.0")
        ));
        let msg = old_host_message("Gaming-PC", &Version::new(3, 0, 0));
        assert!(msg.contains("Gaming-PC"), "{msg}");
        assert!(msg.contains("3.0.0"), "{msg}");
        assert!(msg.contains("Update BroLink Host"), "{msg}");
        assert!(!msg.contains("Broken pipe"), "{msg}");
        assert!(!msg.contains("os error"), "{msg}");
    }

    #[test]
    fn pipe_and_eagain_are_retried() {
        assert!(is_transient(&anyhow!("connection closed")));
        assert!(is_transient(&anyhow!("timed out")));
        assert!(is_transient(&anyhow!("Broken pipe (os error 32)")));
        assert!(is_transient(&anyhow!(
            "Resource temporarily unavailable (os error 35)"
        )));
        assert!(!is_transient(&anyhow!(
            "GitHub refused the token (HTTP 401)"
        )));
        assert!(!is_transient(&anyhow!(
            "the upload is not a Windows executable"
        )));
    }
}
