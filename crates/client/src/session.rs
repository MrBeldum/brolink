//! Finding PCs on the tailnet, and the path from "asleep in another room" to
//! a live stream: wake, wait, pair if needed, launch, connect.

use crate::config::{ClientConfig, Codec, KnownPc, StreamSettings};
use anyhow::{anyhow, bail, Result};
use brolink_core::api::{Ack, PinRequest, PowerAction, PowerRequest, Status};
use brolink_core::{http, tailscale, wake, CONTROL_PORT, SUNSHINE_PORT};
use brolink_stream::session::Server;
use brolink_stream::{Client, Event, FrameSlot, Identity, Session, Settings};
use parking_lot::Mutex;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Rescan the tailnet every few seconds and remember what each PC needs to
/// be woken.
pub fn spawn_discovery(shared: Arc<Mutex<Discovery>>, ctx: egui::Context) {
    std::thread::spawn(move || loop {
        let cfg = ClientConfig::load();
        let scan = scan(&cfg);
        let mut learned = cfg.clone();
        for pc in &scan.pcs {
            let entry = learned.pcs.entry(pc.node_id.clone()).or_default();
            entry.name = pc.name.clone();
            if let Some(h) = &pc.host {
                if h.mac.is_some() {
                    entry.mac = h.mac.clone();
                    entry.lan_ip = h.lan_ip.clone();
                }
            }
            if let Some(p) = &scan_public(pc) {
                entry.public_ip = Some(p.to_string());
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

fn scan_public(pc: &Pc) -> Option<Ipv4Addr> {
    pc.known.as_ref()?.public_ip.as_deref()?.parse().ok()
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
    let mut peers = st.windows_peers();
    let local;
    if std::env::var_os("BROLINK_DEV_LOCAL").is_some() {
        local = tailscale::Node {
            id: "local".into(),
            host_name: format!("{} (this PC)", brolink_core::config::machine_name()),
            os: "windows".into(),
            tailscale_ips: vec!["127.0.0.1".into()],
            online: true,
            ..Default::default()
        };
        peers.push(&local);
    }
    let pcs = peers
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
            let mut known = cfg.pcs.get(&n.id).cloned();
            if let Some(p) = n.public_ipv4() {
                known.get_or_insert_with(Default::default).public_ip = Some(p.to_string());
            }
            Pc {
                node_id: n.id.clone(),
                name: n.host_name.clone(),
                ip,
                online: n.online,
                host,
                sunshine,
                known,
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

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Idle,
    Waking,
    Waiting,
    Pairing {
        pin: String,
    },
    Launching,
    /// The stream is being negotiated; the text is the current stage.
    Connecting,
    Streaming,
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
    /// Sunshine's app names, once listed.
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

/// A running stream and everything the window needs to show and drive it.
pub struct Live {
    pub pc: String,
    pub ip: Ipv4Addr,
    pub session: Session,
    pub frames: Arc<FrameSlot>,
    pub events: Receiver<Event>,
    pub started: Instant,
    pub codec: &'static str,
    pub requested: (u32, u32, u32),
}

/// Everything the worker needs about the PC, copied so the UI can move on.
#[derive(Debug, Clone)]
pub struct Target {
    pub node_id: String,
    pub name: String,
    pub ip: Ipv4Addr,
    pub online: bool,
    pub mac: Option<wake::MacAddr>,
    pub lan_ip: Option<Ipv4Addr>,
    pub public_ip: Option<Ipv4Addr>,
    pub server_cert: Option<Vec<u8>>,
}

impl Target {
    pub fn from_pc(pc: &Pc) -> Option<Self> {
        let k = pc.known.as_ref();
        let parse = |s: Option<&String>| s.and_then(|s| s.parse().ok());
        Some(Self {
            node_id: pc.node_id.clone(),
            name: pc.name.clone(),
            ip: pc.ip?,
            online: pc.online && (pc.sunshine || pc.host.is_some()),
            mac: k
                .and_then(|k| k.mac.as_deref())
                .and_then(wake::MacAddr::parse),
            lan_ip: parse(k.and_then(|k| k.lan_ip.as_ref())),
            public_ip: parse(k.and_then(|k| k.public_ip.as_ref())),
            server_cert: k
                .and_then(|k| k.server_cert.as_deref())
                .and_then(brolink_stream::nvhttp::unhex),
        })
    }
}

pub struct Connect {
    pub target: Target,
    pub settings: StreamSettings,
    pub native: (u32, u32),
    pub progress: Arc<Mutex<Progress>>,
    pub live: Arc<Mutex<Option<Live>>>,
    pub ctx: egui::Context,
}

/// Wake, wait, pair, launch, connect: on its own thread, reporting into
/// `progress` and leaving the stream in `live`.
pub fn connect(c: Connect) {
    {
        let mut p = c.progress.lock();
        *p = Progress {
            pc: c.target.name.clone(),
            ..Default::default()
        };
        p.set(Step::Launching, "Starting…");
    }
    std::thread::spawn(move || {
        let result = run(&c);
        let mut p = c.progress.lock();
        match result {
            Ok(()) => {}
            Err(e) if p.cancel => {
                tracing::info!("cancelled: {e:#}");
                p.set(Step::Ended { error: None }, "");
            }
            Err(e) => p.set(
                Step::Ended {
                    error: Some(format!("{e:#}")),
                },
                "",
            ),
        }
        c.ctx.request_repaint();
    });
}

fn cancelled(progress: &Mutex<Progress>) -> bool {
    progress.lock().cancel
}

fn run(c: &Connect) -> Result<()> {
    let t = &c.target;
    let report = |step: Step, detail: String| {
        c.progress.lock().set(step, detail);
        c.ctx.request_repaint();
    };

    // 1. Wake it if nothing answers.
    if !t.online && !port_open(t.ip, SUNSHINE_PORT, Duration::from_millis(1200)) {
        let mac = t.mac.ok_or_else(|| {
            anyhow!("{} is not answering and this Mac does not know how to wake it yet. Turn the PC on once while BroLink Host is running so it can learn.", t.name)
        })?;
        let start = Instant::now();
        let mut last_wake = Instant::now() - Duration::from_secs(60);
        loop {
            if cancelled(&c.progress) {
                bail!("cancelled");
            }
            if last_wake.elapsed() >= Duration::from_secs(5) {
                let n = wake::send(mac, t.lan_ip, t.public_ip);
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
                    "{} did not wake up. A wake packet only reaches it from its own network, or through a router that forwards UDP 9 to it. Check Wake-on-LAN in BroLink Host on the PC.",
                    t.name
                );
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    // 2. Sunshine's port.
    let start = Instant::now();
    while !port_open(t.ip, SUNSHINE_PORT, Duration::from_millis(1500)) {
        if cancelled(&c.progress) {
            bail!("cancelled");
        }
        report(
            Step::Waiting,
            format!("Waiting for {}… {}s", t.name, start.elapsed().as_secs()),
        );
        if start.elapsed() > Duration::from_secs(60) {
            bail!(
                "{} is up but nothing is streaming from it. Open BroLink Host on the PC and run setup.",
                t.name
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    // 3. Pair if this Mac is not known to the PC yet.
    report(Step::Launching, "Checking pairing…".into());
    let identity = Identity::load_or_create(&brolink_core::config::data_dir()?.join("identity"))?;
    let mut client = Client::new(&identity, IpAddr::V4(t.ip), t.server_cert.clone())?;
    let mut info = retry(8, || client.server_info())?;
    if !info.paired {
        let pin = format!("{:04}", rand::random::<u16>() % 10_000);
        report(Step::Pairing { pin: pin.clone() }, String::new());
        let der = client.pair(&pin, &brolink_core::config::machine_name(), || {
            submit_pin(t.ip, &pin, &c.progress)
        })?;
        remember_cert(&t.node_id, &t.name, &der);
        info = client.server_info()?;
        if !info.paired {
            bail!("{} did not accept the pairing", t.name);
        }
    }
    if cancelled(&c.progress) {
        bail!("cancelled");
    }

    // 4. Pick the app and launch or resume it.
    report(Step::Launching, "Starting the stream…".into());
    let apps = client.app_list()?;
    c.progress.lock().apps = apps.iter().map(|a| a.title.clone()).collect();
    let app = apps
        .iter()
        .find(|a| a.title.eq_ignore_ascii_case(&c.settings.app))
        .or_else(|| apps.iter().find(|a| a.title == "Desktop"))
        .or_else(|| apps.first())
        .ok_or_else(|| anyhow!("Sunshine on {} offers nothing to stream", t.name))?;
    if info.current_game != 0 && info.current_game != app.id {
        let _ = client.quit();
        std::thread::sleep(Duration::from_millis(500));
        info = client.server_info()?;
    }
    let (w, h) = c.settings.resolution.pixels(c.native);
    let fps = c.settings.fps;
    let ri_key: [u8; 16] = rand::random();
    let ri_id: u32 = rand::random();
    let mut ri_iv = [0u8; 16];
    ri_iv[..4].copy_from_slice(&ri_id.to_be_bytes());
    let rtsp = client.launch(app.id, w, h, fps, &ri_key, ri_id, info.current_game != 0)?;

    // 5. Connect.
    if cancelled(&c.progress) {
        let _ = client.quit();
        bail!("cancelled");
    }
    let hevc = c.settings.codec == Codec::Auto && info.codec_mode_support & 0x0F00 != 0;
    let frames = Arc::new(FrameSlot::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = c.ctx.clone();
    let session = Session::start(
        Server {
            address: t.ip.to_string(),
            app_version: info.app_version.clone(),
            gfe_version: info.gfe_version.clone(),
            rtsp_url: rtsp,
            codec_mode_support: info.codec_mode_support,
        },
        Settings {
            width: w,
            height: h,
            fps,
            bitrate_kbps: c.settings.bitrate_kbps,
            hevc,
            remote: !t.ip.is_private(),
        },
        ri_key,
        ri_iv,
        frames.clone(),
        tx,
        move || ctx.request_repaint(),
    );
    *c.live.lock() = Some(Live {
        pc: t.name.clone(),
        ip: t.ip,
        session,
        frames,
        events: rx,
        started: Instant::now(),
        codec: if hevc { "HEVC" } else { "H.264" },
        requested: (w, h, fps),
    });
    report(Step::Connecting, "Connecting…".into());
    Ok(())
}

fn retry<T>(times: u32, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut last = None;
    for _ in 0..times {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(700));
    }
    Err(last.unwrap())
}

/// Hand the PIN to BroLink Host on the PC, which types it into Sunshine. If
/// there is no BroLink Host, the PIN stays on screen for someone at the PC.
fn submit_pin(ip: Ipv4Addr, pin: &str, progress: &Mutex<Progress>) {
    let req = PinRequest {
        pin: pin.into(),
        name: brolink_core::config::machine_name(),
    };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(75) {
        if progress.lock().cancel {
            return;
        }
        match http::post_json::<_, Ack>(
            (ip, CONTROL_PORT),
            "/v1/pin",
            &req,
            Duration::from_secs(10),
        ) {
            Ok(a) if a.ok => return,
            Ok(a) => tracing::info!("PIN not accepted yet: {}", a.error.unwrap_or_default()),
            Err(e) => {
                tracing::info!("BroLink Host did not take the PIN: {e}");
                progress.lock().detail =
                    "BroLink Host is not answering on the PC. Enter the PIN in Sunshine's web page (https://<pc>:47990) to pair.".into();
            }
        }
        std::thread::sleep(Duration::from_millis(800));
    }
}

fn remember_cert(node_id: &str, name: &str, der: &[u8]) {
    let mut cfg = ClientConfig::load();
    let e = cfg.pcs.entry(node_id.to_string()).or_default();
    e.name = name.to_string();
    e.server_cert = Some(brolink_stream::nvhttp::hex(der));
    if let Err(e) = cfg.save() {
        tracing::warn!("could not save the PC's certificate: {e:#}");
    }
}

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

/// Send the wake packet to a PC that is awake and ask BroLink Host on it
/// whether the packet arrived: the same path a real wake would take.
pub fn wake_test(pc: &Pc) -> Result<bool> {
    let ip = pc.ip.ok_or_else(|| anyhow!("no address for {}", pc.name))?;
    wake_only(pc)?;
    std::thread::sleep(Duration::from_millis(1500));
    let st = host_status(ip, Duration::from_secs(3))
        .ok_or_else(|| anyhow!("BroLink Host on {} is not answering", pc.name))?;
    Ok(st.wake_packet_age_secs.is_some_and(|s| s <= 5))
}

/// Send the wake packet once, without connecting.
pub fn wake_only(pc: &Pc) -> Result<usize> {
    let t = Target::from_pc(pc).ok_or_else(|| anyhow!("no address for {}", pc.name))?;
    let mac = t
        .mac
        .ok_or_else(|| anyhow!("this Mac does not know {}'s MAC address yet", pc.name))?;
    Ok(wake::send(mac, t.lan_ip, t.public_ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against BroLink Host running on this machine: the packet goes out on
    /// the LAN and the host reports it. `cargo test -p brolink-client wake_test_real -- --ignored`
    #[test]
    #[ignore = "needs BroLink Host running on this machine"]
    fn wake_test_real() {
        let ip: Ipv4Addr = "127.0.0.1".parse().unwrap();
        let host = host_status(ip, Duration::from_secs(2)).expect("host status");
        let pc = Pc {
            name: host.name.clone(),
            ip: Some(ip),
            online: true,
            known: Some(KnownPc {
                mac: host.mac.clone(),
                lan_ip: host.lan_ip.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(wake_test(&pc).unwrap());
    }

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
                public_ip: Some("203.0.113.5".into()),
                server_cert: Some("3082".into()),
            }),
            ..Default::default()
        };
        let t = Target::from_pc(&pc).unwrap();
        assert!(t.online);
        assert_eq!(t.mac.unwrap().to_string(), "02:00:00:00:00:01");
        assert_eq!(t.lan_ip, Some("192.168.1.10".parse().unwrap()));
        assert_eq!(t.public_ip, Some("203.0.113.5".parse().unwrap()));
        assert_eq!(t.server_cert, Some(vec![0x30, 0x82]));
    }

    /// `cargo test -p brolink-client scan_real -- --ignored --nocapture`
    #[test]
    #[ignore = "needs Tailscale"]
    fn scan_real_tailnet() {
        let d = scan(&ClientConfig::load());
        eprintln!("error={:?} login={} pcs={}", d.error, d.login, d.pcs.len());
        for pc in &d.pcs {
            eprintln!(
                "  {} ip={:?} online={} host={} sunshine={} wake={} known={:?}",
                pc.name,
                pc.ip,
                pc.online,
                pc.host.is_some(),
                pc.sunshine,
                pc.can_wake(),
                pc.known
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
        assert_eq!(p.since, since);
        p.set(Step::Ended { error: None }, "");
        assert!(!p.active());
    }
}
