//! Finding PCs, and the one-click path from "asleep in another room" to a
//! Moonlight window: wake, wait, pair if needed, stream.

use crate::config::{ClientConfig, KnownPc, StreamSettings};
use crate::moonlight;
use anyhow::{anyhow, bail, Result};
use brolink_core::api::{Ack, PinRequest, PowerAction, PowerRequest, Status};
use brolink_core::{http, tailscale, wake, CONTROL_PORT, SUNSHINE_PORT};
use parking_lot::Mutex;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// A Windows machine on the tailnet, as far as the client can tell.
#[derive(Debug, Clone, Default)]
pub struct Pc {
    pub node_id: String,
    pub name: String,
    pub ip: Option<Ipv4Addr>,
    /// Tailscale's view; lags a wake-up by up to half a minute.
    pub online: bool,
    /// The BroLink control service answered.
    pub host: Option<Status>,
    /// Sunshine's port answered.
    pub sunshine: bool,
    /// What we remembered from an earlier visit: enough to wake it.
    pub known: Option<KnownPc>,
}

impl Pc {
    pub fn can_wake(&self) -> bool {
        self.known.as_ref().and_then(|k| k.mac.as_ref()).is_some()
    }
    pub fn can_stream(&self) -> bool {
        self.ip.is_some() && (self.sunshine || self.host.is_some() || self.can_wake())
    }
    pub fn power_allowed(&self) -> bool {
        self.host.as_ref().is_some_and(|h| h.power_allowed)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Why nothing can be listed, when Tailscale is down.
    pub error: Option<String>,
    pub login: String,
    pub pcs: Vec<Pc>,
    pub refreshed: Option<Instant>,
}

/// Rescan the tailnet every few seconds and remember wake details.
pub fn spawn_discovery(shared: Arc<Mutex<Discovery>>, ctx: egui::Context) {
    std::thread::spawn(move || loop {
        let cfg = ClientConfig::load();
        let scan = scan(&cfg);
        let mut learned = cfg.clone();
        for pc in &scan.pcs {
            if let Some(h) = &pc.host {
                learned.pcs.insert(
                    pc.node_id.clone(),
                    KnownPc {
                        name: pc.name.clone(),
                        mac: h.mac.clone(),
                        lan_ip: h.lan_ip.clone(),
                    },
                );
            }
        }
        if learned.pcs != cfg.pcs {
            if let Err(e) = learned.save() {
                tracing::warn!("could not save what was learned: {e:#}");
            }
        }
        *shared.lock() = scan;
        ctx.request_repaint();
        std::thread::sleep(Duration::from_secs(3));
    });
}

pub fn scan(cfg: &ClientConfig) -> Discovery {
    let st = match tailscale::status() {
        Ok(s) => s,
        Err(e) => {
            return Discovery {
                error: Some(e.to_string()),
                refreshed: Some(Instant::now()),
                ..Default::default()
            }
        }
    };
    let pcs = st
        .windows_peers()
        .into_iter()
        .map(|n| {
            let ip = n.ipv4();
            let (host, sunshine) = match (n.online, ip) {
                (true, Some(ip)) => (
                    host_status(ip, Duration::from_millis(900)),
                    port_open(ip, SUNSHINE_PORT, Duration::from_millis(500)),
                ),
                _ => (None, false),
            };
            Pc {
                node_id: n.id.clone(),
                name: n.host_name.clone(),
                ip,
                online: n.online,
                host,
                sunshine,
                known: cfg.pcs.get(&n.id).cloned(),
            }
        })
        .collect();
    Discovery {
        error: None,
        login: st.self_login().unwrap_or("").to_string(),
        pcs,
        refreshed: Some(Instant::now()),
    }
}

fn host_status(ip: Ipv4Addr, timeout: Duration) -> Option<Status> {
    http::get_json::<Status>((ip, CONTROL_PORT), "/v1/status", timeout)
        .ok()
        .filter(|s| s.app == "brolink")
}

fn port_open(ip: Ipv4Addr, port: u16, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((ip, port)), timeout).is_ok()
}

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Idle,
    Waking,
    Waiting,
    Pairing {
        pin: String,
    },
    Launching,
    Streaming,
    /// The session is over; `error` says why if it was not the user.
    Ended {
        error: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub pc: String,
    pub step: Step,
    pub detail: String,
    pub since: Instant,
    pub cancel: bool,
    /// Sunshine's app list, once Moonlight has fetched it.
    pub apps: Vec<String>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            pc: String::new(),
            step: Step::Idle,
            detail: String::new(),
            since: Instant::now(),
            cancel: false,
            apps: Vec::new(),
        }
    }
}

impl Progress {
    pub fn active(&self) -> bool {
        !matches!(self.step, Step::Idle | Step::Ended { .. })
    }
    fn set(&mut self, step: Step, detail: impl Into<String>) {
        if self.step != step {
            self.since = Instant::now();
        }
        self.step = step;
        self.detail = detail.into();
    }
}

/// Everything the worker needs about the PC, copied so the UI can move on.
#[derive(Debug, Clone)]
pub struct Target {
    pub name: String,
    pub ip: Ipv4Addr,
    pub online: bool,
    pub mac: Option<wake::MacAddr>,
    pub lan_ip: Option<Ipv4Addr>,
}

impl Target {
    pub fn from_pc(pc: &Pc) -> Option<Self> {
        Some(Self {
            name: pc.name.clone(),
            ip: pc.ip?,
            online: pc.online && (pc.sunshine || pc.host.is_some()),
            mac: pc
                .known
                .as_ref()
                .and_then(|k| k.mac.as_deref())
                .and_then(wake::MacAddr::parse),
            lan_ip: pc
                .known
                .as_ref()
                .and_then(|k| k.lan_ip.as_deref())
                .and_then(|s| s.parse().ok()),
        })
    }
}

/// Wake, wait, pair, stream: on its own thread, reporting into `progress`.
pub fn connect(
    target: Target,
    settings: StreamSettings,
    native: (u32, u32),
    progress: Arc<Mutex<Progress>>,
    ctx: egui::Context,
) {
    {
        let mut p = progress.lock();
        *p = Progress {
            pc: target.name.clone(),
            ..Default::default()
        };
        p.set(Step::Launching, "Starting…");
    }
    std::thread::spawn(move || {
        let result = run(&target, &settings, native, &progress, &ctx);
        let mut p = progress.lock();
        if !matches!(p.step, Step::Ended { .. }) {
            let error = match result {
                Ok(()) => None,
                Err(e) if p.cancel => {
                    tracing::info!("cancelled: {e:#}");
                    None
                }
                Err(e) => Some(format!("{e:#}")),
            };
            p.set(Step::Ended { error }, "");
        }
        ctx.request_repaint();
    });
}

fn cancelled(progress: &Mutex<Progress>) -> bool {
    progress.lock().cancel
}

fn run(
    t: &Target,
    settings: &StreamSettings,
    native: (u32, u32),
    progress: &Arc<Mutex<Progress>>,
    ctx: &egui::Context,
) -> Result<()> {
    let report = |step: Step, detail: String| {
        progress.lock().set(step, detail);
        ctx.request_repaint();
    };

    // 1. Wake it if nothing answers.
    if !t.online && !port_open(t.ip, SUNSHINE_PORT, Duration::from_millis(1200)) {
        let mac = t.mac.ok_or_else(|| {
            anyhow!("{} is not answering and this Mac does not know how to wake it yet. Turn the PC on once while BroLink Host is running so it can learn.", t.name)
        })?;
        let start = Instant::now();
        let mut last_wake = Instant::now() - Duration::from_secs(60);
        loop {
            if cancelled(progress) {
                bail!("cancelled");
            }
            if last_wake.elapsed() >= Duration::from_secs(6) {
                let n = wake::send(mac, t.lan_ip);
                tracing::info!("sent {n} wake packets for {mac}");
                last_wake = Instant::now();
            }
            report(
                Step::Waking,
                format!("Waking {}… {}s", t.name, start.elapsed().as_secs()),
            );
            if port_open(t.ip, SUNSHINE_PORT, Duration::from_millis(1500))
                || port_open(t.ip, CONTROL_PORT, Duration::from_millis(1500))
            {
                break;
            }
            if start.elapsed() > Duration::from_secs(120) {
                bail!(
                    "{} did not wake up. Wake-on-LAN only reaches it from its own network unless a Tailscale subnet router is on that network; if the PC was shut down rather than asleep, it may not wake at all.",
                    t.name
                );
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    // 2. Sunshine's port.
    let start = Instant::now();
    while !port_open(t.ip, SUNSHINE_PORT, Duration::from_millis(1500)) {
        if cancelled(progress) {
            bail!("cancelled");
        }
        report(
            Step::Waiting,
            format!(
                "Waiting for Sunshine on {}… {}s",
                t.name,
                start.elapsed().as_secs()
            ),
        );
        if start.elapsed() > Duration::from_secs(60) {
            bail!("{} is up but Sunshine is not answering on port {SUNSHINE_PORT}. Open BroLink Host on the PC and run setup.", t.name);
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    // 3. Paired?
    report(Step::Launching, "Checking pairing…".into());
    let apps = match moonlight::list(t.ip) {
        Ok(apps) => apps,
        Err(e) if e.downcast_ref::<moonlight::NotPaired>().is_some() => {
            pair(t, progress, ctx)?;
            moonlight::list(t.ip).unwrap_or_default()
        }
        Err(e) => return Err(e),
    };
    progress.lock().apps = apps.clone();
    if !apps.is_empty() && !apps.iter().any(|a| a.eq_ignore_ascii_case(&settings.app)) {
        bail!(
            "Sunshine on {} has no app called \"{}\". It offers: {}. Pick one under Settings.",
            t.name,
            settings.app,
            apps.join(", ")
        );
    }

    // 4. Stream.
    if cancelled(progress) {
        bail!("cancelled");
    }
    report(Step::Launching, "Starting Moonlight…".into());
    let mut child = moonlight::stream(t.ip, settings, native)?;
    let stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut r) = stderr {
            let _ = std::io::Read::read_to_string(&mut r, &mut s);
        }
        s
    });
    let started = Instant::now();
    report(Step::Streaming, format!("Streaming {}", t.name));
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if cancelled(progress) {
            moonlight::stop(&mut child);
            bail!("cancelled");
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let stderr = err_reader.join().unwrap_or_default();
    // Moonlight exits non-zero for its own errors; a stream the user ended
    // exits clean. A very short session with a message is a failure to start.
    if !status.success()
        || (started.elapsed() < Duration::from_secs(8) && stderr.contains("Failed"))
    {
        bail!(
            "{}",
            moonlight::first_line(&stderr, "Moonlight stopped unexpectedly")
        );
    }
    Ok(())
}

/// Moonlight waits with a PIN; the PC's BroLink host accepts it. Order
/// matters: Sunshine only takes the PIN once Moonlight has asked to pair.
fn pair(t: &Target, progress: &Arc<Mutex<Progress>>, ctx: &egui::Context) -> Result<()> {
    let pin = format!("{:04}", rand::random::<u16>() % 10_000);
    progress.lock().set(
        Step::Pairing { pin: pin.clone() },
        format!("Pairing with {}…", t.name),
    );
    ctx.request_repaint();
    let mut child = moonlight::pair(t.ip, &pin)?;
    let req = PinRequest {
        pin: pin.clone(),
        name: brolink_core::config::machine_name(),
    };
    let start = Instant::now();
    let mut accepted = false;
    let mut last_err = String::new();
    while start.elapsed() < Duration::from_secs(30) {
        if cancelled(progress) {
            let _ = child.kill();
            bail!("cancelled");
        }
        std::thread::sleep(Duration::from_millis(800));
        match http::post_json::<_, Ack>(
            (t.ip, CONTROL_PORT),
            "/v1/pin",
            &req,
            Duration::from_secs(10),
        ) {
            Ok(a) if a.ok => {
                accepted = true;
                break;
            }
            Ok(a) => last_err = a.error.unwrap_or_default(),
            Err(e) => last_err = e.to_string(),
        }
        if let Some(s) = child.try_wait()? {
            if !s.success() {
                bail!("Moonlight gave up pairing: {last_err}");
            }
        }
    }
    if !accepted {
        let _ = child.kill();
        bail!(
            "{} did not accept the PIN. {}",
            t.name,
            if last_err.is_empty() {
                String::new()
            } else {
                format!("({last_err})")
            }
        );
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(s) = child.try_wait()? {
            if s.success() {
                return Ok(());
            }
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut e, &mut err);
            }
            bail!(
                "pairing failed: {}",
                moonlight::first_line(&err, "Moonlight exited")
            );
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("pairing did not complete in time");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------
// Power
// ---------------------------------------------------------------------------

/// Ask the host to sleep, restart, or shut down.
pub fn power(ip: Ipv4Addr, action: PowerAction) -> Result<()> {
    let a: Ack = http::post_json(
        (ip, CONTROL_PORT),
        "/v1/power",
        &PowerRequest { action },
        Duration::from_secs(6),
    )?;
    if a.ok {
        Ok(())
    } else {
        bail!("{}", a.error.unwrap_or_else(|| "refused".into()))
    }
}

/// Send the wake packet once, without connecting.
pub fn wake_only(pc: &Pc) -> Result<usize> {
    let t = Target::from_pc(pc).ok_or_else(|| anyhow!("no address for {}", pc.name))?;
    let mac = t
        .mac
        .ok_or_else(|| anyhow!("this Mac does not know {}'s MAC address yet", pc.name))?;
    Ok(wake::send(mac, t.lan_ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pc_is_streamable_when_reachable_or_wakeable() {
        let mut pc = Pc {
            ip: Some("203.0.113.10".parse().unwrap()),
            ..Default::default()
        };
        assert!(!pc.can_stream());
        pc.sunshine = true;
        assert!(pc.can_stream());
        pc.sunshine = false;
        pc.known = Some(KnownPc {
            mac: Some("02:00:00:00:00:01".into()),
            ..Default::default()
        });
        assert!(pc.can_wake() && pc.can_stream());
        pc.ip = None;
        assert!(!pc.can_stream());
    }

    #[test]
    fn target_parses_what_it_learned() {
        let pc = Pc {
            name: "Gaming-PC".into(),
            ip: Some("203.0.113.10".parse().unwrap()),
            online: true,
            sunshine: true,
            known: Some(KnownPc {
                name: "Gaming-PC".into(),
                mac: Some("02:00:00:00:00:01".into()),
                lan_ip: Some("192.168.1.10".into()),
            }),
            ..Default::default()
        };
        let t = Target::from_pc(&pc).unwrap();
        assert!(t.online);
        assert_eq!(t.mac.unwrap().to_string(), "02:00:00:00:00:01");
        assert_eq!(t.lan_ip, Some("192.168.1.10".parse().unwrap()));
    }

    /// `cargo test -p brolink-client scan_real -- --ignored --nocapture` on a
    /// machine with Tailscale: prints what the lobby would list.
    #[test]
    #[ignore = "needs Tailscale"]
    fn scan_real_tailnet() {
        let d = scan(&ClientConfig::load());
        eprintln!("error={:?} login={} pcs={}", d.error, d.login, d.pcs.len());
        for pc in &d.pcs {
            eprintln!(
                "  {} ip={:?} online={} host={} sunshine={} wake={}",
                pc.name,
                pc.ip,
                pc.online,
                pc.host.is_some(),
                pc.sunshine,
                pc.can_wake()
            );
        }
        assert!(d.error.is_some() || !d.login.is_empty());
    }

    #[test]
    fn progress_tracks_step_changes() {
        let mut p = Progress::default();
        assert!(!p.active());
        p.set(Step::Waking, "x");
        assert!(p.active());
        let since = p.since;
        std::thread::sleep(Duration::from_millis(5));
        p.set(Step::Waking, "y");
        assert_eq!(p.since, since, "same step keeps its start time");
        p.set(Step::Ended { error: None }, "");
        assert!(!p.active());
    }
}
