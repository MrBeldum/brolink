//! Finding PCs on the tailnet, and the path from "asleep in another room" to
//! a live stream: wake, wait, pair if needed, launch, connect.

use crate::clipboard;
use crate::config::{ClientConfig, Codec, KnownPc, StreamSettings};
use crate::path::{self, Path};
use anyhow::{anyhow, bail, Result};
use brolink_core::api::{Ack, NatReport, PinRequest, PowerAction, PowerRequest, Status};
use brolink_core::{http, tailscale, wake, CONTROL_PORT, SUNSHINE_PORT};
use brolink_stream::session::Server;
use brolink_stream::{Client, Event, FrameSlot, Identity, Input, Session, Settings};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Each [`connect`] call gets a new id so a cancelled worker cannot finish
/// into a later attempt's `progress` / `live`.
static NEXT_CONNECT: AtomicU64 = AtomicU64::new(1);

/// How often this Mac's own NAT is re-examined. `tailscale netcheck` takes
/// seconds and networks change when the Mac moves, not more often.
const NETCHECK_EVERY: Duration = Duration::from_secs(15 * 60);

/// How often the debug command is polled for the granted relay set. It is a
/// cheap local call; the ACL changes rarely, so 30 s is plenty.
const PEER_RELAY_EVERY: Duration = Duration::from_secs(30);

/// A machine on the tailnet BroLink can open, as far as this node can tell.
#[derive(Debug, Clone, Default)]
pub struct Pc {
    pub node_id: String,
    pub name: String,
    /// Tailscale OS string ("windows", "macOS", "linux").
    pub os: String,
    pub ip: Option<Ipv4Addr>,
    /// Tailscale's view; lags a wake-up by up to half a minute.
    pub online: bool,
    /// The BroLink control service answered.
    pub host: Option<Status>,
    /// Sunshine's port answered.
    pub sunshine: bool,
    pub known: Option<KnownPc>,
    /// Listed from what was saved, because Tailscale on this Mac could not
    /// say; nothing has been probed.
    pub remembered: bool,
    /// Days until the PC's Tailscale key expires; `None` when it never does.
    pub key_expiry_days: Option<i64>,
    /// Direct or relayed, and how far away.
    pub path: Path,
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

/// A tailnet node tagged `tag:relay`: a candidate peer relay, never a PC.
#[derive(Debug, Clone, Default)]
pub struct Relay {
    pub name: String,
    pub ip: Option<Ipv4Addr>,
    pub online: bool,
}

/// The relay server set from `tailscale debug peer-relay-servers`. `Unknown`
/// when the command could not run (CLI missing, timeout, garbage) — a failed
/// debug probe proves nothing about the ACL. `Known` is a real answer: the
/// IPs this node may use right now. Empty is not proof of ACL denial — the
/// tagged node may simply not be configured as a relay. Non-empty is the
/// permitted set, not a promise that a `tag:relay` node matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PeerRelayServers {
    #[default]
    Unknown,
    Known(Vec<String>),
}

#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Why nothing can be probed, when Tailscale is down. The list then
    /// holds what was remembered.
    pub error: Option<String>,
    pub login: String,
    pub pcs: Vec<Pc>,
    pub refreshed: Option<Instant>,
    /// Days until this Mac's own Tailscale key expires.
    pub self_key_days: Option<i64>,
    /// This Mac's side of the NAT story, once `tailscale netcheck` has run.
    pub self_nat: Option<NatReport>,
    /// This Mac's own Tailscale address, the one a PC can reach.
    pub self_ip: Option<Ipv4Addr>,
    /// Relay nodes on the tailnet, from `status --json`. Never streamable.
    pub relays: Vec<Relay>,
    /// Relay servers this device is granted to use, from the debug command.
    pub peer_relay_servers: PeerRelayServers,
}

/// Rescan the tailnet every few seconds and remember what each PC needs to
/// be woken.
pub fn spawn_discovery(shared: Arc<Mutex<Discovery>>, ctx: egui::Context) {
    let nat: Arc<Mutex<Option<NatReport>>> = Arc::default();
    spawn_netcheck(nat.clone(), ctx.clone());
    let servers: Arc<Mutex<PeerRelayServers>> = Arc::default();
    spawn_peer_relay_probe(servers.clone(), ctx.clone());
    std::thread::spawn(move || {
        // Round trips per PC, newest last; the smallest of the last few is
        // the path's real round trip (a first connect also pays for the
        // WireGuard handshake).
        let mut rtts: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        loop {
            let cfg = ClientConfig::load();
            let mut scan = scan(&cfg);
            for pc in &mut scan.pcs {
                if let Some(ms) = pc.path.rtt_ms {
                    let h = rtts.entry(pc.node_id.clone()).or_default();
                    h.push(ms);
                    if h.len() > 5 {
                        h.remove(0);
                    }
                    pc.path.rtt_ms = h.iter().copied().min();
                }
            }
            scan.self_nat = nat.lock().clone();
            scan.peer_relay_servers = servers.lock().clone();
            learn(&scan);
            *shared.lock() = scan;
            ctx.request_repaint();
            std::thread::sleep(Duration::from_secs(3));
        }
    });
}

/// `tailscale netcheck` now and then, for [`Discovery::self_nat`].
fn spawn_netcheck(slot: Arc<Mutex<Option<NatReport>>>, ctx: egui::Context) {
    std::thread::spawn(move || loop {
        match tailscale::netcheck() {
            Ok(n) => {
                *slot.lock() = Some(n.report());
                ctx.request_repaint();
            }
            Err(e) => tracing::info!("netcheck: {e}"),
        }
        std::thread::sleep(NETCHECK_EVERY);
    });
}

/// `tailscale debug peer-relay-servers`, at most once every 30 s, on its own
/// thread so the scan loop and UI never wait on it. The command itself is
/// bounded (see `tailscale::PEER_RELAY_SERVERS_TIMEOUT`). Any failure leaves
/// the result `Unknown`, never a denial.
fn spawn_peer_relay_probe(slot: Arc<Mutex<PeerRelayServers>>, ctx: egui::Context) {
    std::thread::spawn(move || loop {
        let result = match tailscale::peer_relay_servers() {
            Ok(servers) => PeerRelayServers::Known(servers),
            Err(_) => PeerRelayServers::Unknown,
        };
        *slot.lock() = result;
        ctx.request_repaint();
        std::thread::sleep(PEER_RELAY_EVERY);
    });
}

/// Save what a scan taught about each PC: address, wake details, when it
/// was last seen.
fn learn(scan: &Discovery) {
    if let Err(e) = ClientConfig::update(|learned| {
        for pc in &scan.pcs {
            if pc.remembered {
                continue;
            }
            let entry = learned.pcs.entry(pc.node_id.clone()).or_default();
            entry.name = pc.name.clone();
            if !pc.os.is_empty() {
                entry.os = pc.os.clone();
            }
            if let Some(ip) = pc.ip {
                entry.tailscale_ip = Some(ip.to_string());
            }
            if pc.online {
                // Coarse on purpose: the file is rewritten only when this moves.
                let now = brolink_core::dates::now_unix();
                if entry
                    .last_seen_unix
                    .is_none_or(|t| now.saturating_sub(t) > 600)
                {
                    entry.last_seen_unix = Some(now);
                }
            }
            if let Some(h) = &pc.host {
                if h.mac.is_some() {
                    entry.mac = h.mac.clone();
                    entry.lan_ip = h.lan_ip.clone();
                }
            }
            if let Some(p) = scan_public(pc) {
                entry.public_ip = Some(p.to_string());
            }
        }
    }) {
        tracing::warn!("could not save what was learned: {e:#}");
    }
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
                pcs: remembered(cfg),
                refreshed: Some(Instant::now()),
                ..Default::default()
            }
        }
    };
    let mut peers = st.machine_peers();
    let local;
    if std::env::var_os("BROLINK_DEV_LOCAL").is_some() {
        local = tailscale::Node {
            id: "local".into(),
            host_name: format!("{} (this machine)", brolink_core::config::machine_name()),
            os: std::env::consts::OS.into(),
            tailscale_ips: vec!["127.0.0.1".into()],
            online: true,
            ..Default::default()
        };
        peers.push(&local);
    }
    let mut pcs: Vec<Pc> = peers.into_iter().map(|n| probe_peer(cfg, n)).collect();
    // A tag:relay node that also runs BroLink (a VPS desktop on the same
    // box as the packet relay) belongs in the machine list too.
    for n in st.relay_peers() {
        let pc = probe_peer(cfg, n);
        if pc.host.is_some() || pc.sunshine {
            pcs.push(pc);
        }
    }
    pcs.sort_by(|a, b| a.name.cmp(&b.name));
    let relays = st
        .relay_peers()
        .into_iter()
        .map(|n| Relay {
            name: n.host_name.clone(),
            ip: n.ipv4(),
            online: n.online,
        })
        .collect();
    Discovery {
        error: None,
        login: st.self_login().unwrap_or("").to_string(),
        pcs,
        refreshed: Some(Instant::now()),
        self_key_days: st.self_node.key_expiry_days(),
        self_nat: None,
        self_ip: st.self_node.ipv4(),
        relays,
        peer_relay_servers: PeerRelayServers::default(),
    }
}

/// The PCs as last saved, for when Tailscale cannot list them. Sorted by
/// name like the live list.
pub fn remembered(cfg: &ClientConfig) -> Vec<Pc> {
    let mut pcs: Vec<Pc> = cfg
        .pcs
        .iter()
        .filter(|(_, k)| !k.name.is_empty())
        .map(|(id, k)| Pc {
            node_id: id.clone(),
            name: k.name.clone(),
            os: k.os.clone(),
            ip: k.tailscale_ip.as_deref().and_then(|s| s.parse().ok()),
            known: Some(k.clone()),
            remembered: true,
            ..Default::default()
        })
        .collect();
    pcs.sort_by(|a, b| a.name.cmp(&b.name));
    pcs
}

fn probe_peer(cfg: &ClientConfig, n: &tailscale::Node) -> Pc {
    let ip = n.ipv4();
    let (host, sunshine, rtt_ms) = match (n.online, ip) {
        (true, Some(ip)) => {
            let host = host_status(ip, Duration::from_millis(900));
            let (sunshine, rtt) = timed_port_open(ip, SUNSHINE_PORT, Duration::from_millis(900));
            (host, sunshine, rtt)
        }
        _ => (None, false, None),
    };
    let mut known = cfg.pcs.get(&n.id).cloned();
    if let Some(p) = n.public_ipv4() {
        known.get_or_insert_with(Default::default).public_ip = Some(p.to_string());
    }
    Pc {
        node_id: n.id.clone(),
        name: n.host_name.clone(),
        os: n.os.clone(),
        ip,
        online: n.online,
        host,
        sunshine,
        known,
        remembered: false,
        key_expiry_days: n.key_expiry_days(),
        path: Path {
            direct: n.direct(),
            relay: n.relay.clone(),
            peer_relay: n.peer_relay.clone(),
            rtt_ms,
        },
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

/// Whether the port answers, and how long the connect took: one round
/// trip, which is the path's latency.
fn timed_port_open(ip: Ipv4Addr, port: u16, timeout: Duration) -> (bool, Option<u32>) {
    let t = Instant::now();
    let open = port_open(ip, port, timeout);
    (open, open.then(|| t.elapsed().as_millis() as u32))
}

/// A fresh look at the path to `ip` right before connecting: direct or
/// relayed from Tailscale's own view of the peer, latency from the best of
/// a few connects.
fn measure_path(node_id: &str, ip: Ipv4Addr, prior: &Path) -> Path {
    let mut p = prior.clone();
    if let Ok(st) = tailscale::status() {
        if let Some(n) = st.peer.values().find(|n| n.id == node_id) {
            p.peer_relay = n.peer_relay.clone();
            if let Some(d) = n.direct() {
                p.direct = Some(d);
                p.relay = n.relay.clone();
            }
        }
    }
    let mut best: Option<u32> = prior.rtt_ms;
    for _ in 0..3 {
        if let (true, Some(ms)) = timed_port_open(ip, SUNSHINE_PORT, Duration::from_millis(1500)) {
            best = Some(best.map_or(ms, |b| b.min(ms)));
        }
    }
    p.rtt_ms = best;
    p
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
    /// Blocks new connections while the updater swaps and relaunches this app.
    pub updating: bool,
    /// Sunshine's app names, once listed.
    pub apps: Vec<String>,
    /// Distinguishes this attempt from an older worker that is still running.
    pub generation: u64,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            pc: String::new(),
            step: Step::Idle,
            detail: String::new(),
            since: Instant::now(),
            cancel: false,
            updating: false,
            apps: Vec::new(),
            generation: 0,
        }
    }
}

impl Progress {
    pub fn active(&self) -> bool {
        self.updating || !matches!(self.step, Step::Idle | Step::Ended { .. })
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
    pub node_id: String,
    pub ip: Ipv4Addr,
    pub session: Session,
    /// The session's input side, for the view and for workers.
    pub input: Input,
    pub frames: Arc<FrameSlot>,
    pub events: Receiver<Event>,
    pub started: Instant,
    pub codec: &'static str,
    pub requested: (u32, u32, u32),
    /// The settings the stream was started with, after Auto had its say.
    pub settings: StreamSettings,
    /// The path as measured right before connecting.
    pub path: Path,
    /// Clipboard both ways, through BroLink Host on the PC.
    pub clipboard: clipboard::Sync,
}

impl Live {
    /// "Auto · Smooth", "Custom · 1440p · 60 fps · 25 Mbps".
    pub fn quality_label(&self) -> String {
        let mode = match self.settings.quality {
            crate::config::Quality::Auto => "Auto",
            crate::config::Quality::Custom => "Custom",
        };
        match self.settings.preset() {
            Some(p) => format!("{mode} · {}", p.label()),
            None => format!("{mode} · {}", self.settings.describe()),
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.clipboard.stop();
    }
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
    /// What discovery last knew about the path; re-measured at connect.
    pub path: Path,
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
            path: pc.path.clone(),
        })
    }
}

pub struct Connect {
    pub target: Target,
    pub settings: StreamSettings,
    /// Reset Sunshine's Desktop capture instead of resuming a broken one.
    pub restart_capture: bool,
    pub native: (u32, u32),
    pub progress: Arc<Mutex<Progress>>,
    pub live: Arc<Mutex<Option<Live>>>,
    pub ctx: egui::Context,
}

/// Wake, wait, pair, launch, connect: on its own thread, reporting into
/// `progress` and leaving the stream in `live`.
pub fn connect(c: Connect) {
    let generation = NEXT_CONNECT.fetch_add(1, Ordering::Relaxed);
    {
        let mut p = c.progress.lock();
        if p.active() {
            return;
        }
        *p = Progress {
            pc: c.target.name.clone(),
            generation,
            ..Default::default()
        };
        p.set(Step::Launching, "Starting…");
    }
    std::thread::spawn(move || {
        let result = run(&c, generation);
        let mut p = c.progress.lock();
        if p.generation != generation {
            return;
        }
        match result {
            Ok(()) => {}
            Err(e) if p.cancel => {
                tracing::info!("cancelled: {e:#}");
                p.set(Step::Ended { error: None }, "");
            }
            Err(e) => p.set(
                Step::Ended {
                    error: Some(format!("{e}")),
                },
                "",
            ),
        }
        c.ctx.request_repaint();
    });
}

fn stale(progress: &Mutex<Progress>, generation: u64) -> bool {
    let p = progress.lock();
    p.cancel || p.generation != generation
}

fn run(c: &Connect, generation: u64) -> Result<()> {
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
            if stale(&c.progress, generation) {
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
        if stale(&c.progress, generation) {
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
    let mut info = match client.server_info() {
        Ok(i) => i,
        Err(e) if brolink_stream::nvhttp::is_pin_mismatch(&e) => {
            // The engine was reinstalled; the saved cert is the old one.
            // Forget it and pair again so the user does not have to edit
            // client.toml.
            tracing::warn!("{}: {e:#}; pairing again", t.name);
            if let Err(save_error) = ClientConfig::update(|cfg| {
                cfg.forget_pin_on_mismatch(&t.node_id, &e);
            }) {
                tracing::warn!("could not forget the PC's old certificate: {save_error:#}");
            }
            client = Client::new(&identity, IpAddr::V4(t.ip), None)?;
            retry(8, || client.server_info())?
        }
        Err(_) => retry(8, || client.server_info())?,
    };
    if !info.paired || client.server_cert().is_none() {
        let pin = format!("{:04}", rand::random::<u16>() % 10_000);
        report(Step::Pairing { pin: pin.clone() }, String::new());
        let der = client.pair_cancellable(&pin, &brolink_core::config::machine_name(), || {
            submit_pin(t.ip, &pin, &c.progress);
            stale(&c.progress, generation)
        })?;
        remember_cert(&t.node_id, &t.name, &der);
        info = client.server_info()?;
        if !info.paired {
            bail!("{} did not accept the pairing", t.name);
        }
    }
    if stale(&c.progress, generation) {
        bail!("cancelled");
    }

    // 4. Pick the app and launch or resume it, at a quality the path can
    //    carry.
    report(Step::Launching, "Measuring the path…".into());
    let path = measure_path(&t.node_id, t.ip, &t.path);
    let settings = path::effective(&c.settings, &path);
    tracing::info!(
        "path to {}: {} · {}",
        t.name,
        path.label(),
        settings.describe()
    );
    report(Step::Launching, "Starting the stream…".into());
    let apps = client.app_list()?;
    c.progress.lock().apps = apps.iter().map(|a| a.title.clone()).collect();
    let app = apps
        .iter()
        .find(|a| a.title.eq_ignore_ascii_case(&settings.app))
        .or_else(|| apps.iter().find(|a| a.title == "Desktop"))
        .or_else(|| apps.first())
        .ok_or_else(|| anyhow!("{} offers nothing to stream", t.name))?;
    if info.current_game != 0
        && (info.current_game != app.id
            || (c.restart_capture && app.title.eq_ignore_ascii_case("Desktop")))
    {
        client.quit()?;
        let stopped = Instant::now();
        loop {
            if stale(&c.progress, generation) {
                bail!("cancelled");
            }
            info = client.server_info()?;
            if info.current_game == 0 {
                break;
            }
            if stopped.elapsed() >= Duration::from_secs(10) {
                bail!(
                    "{} did not stop the previous stream. Check BroLink Host there.",
                    t.name
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    let (w, h) = settings.resolution.pixels(c.native);
    let fps = settings.fps;
    let ri_key: [u8; 16] = rand::random();
    let ri_id: u32 = rand::random();
    let mut ri_iv = [0u8; 16];
    ri_iv[..4].copy_from_slice(&ri_id.to_be_bytes());
    let rtsp = client.launch(
        app.id,
        w,
        h,
        fps,
        settings.bitrate_kbps,
        &ri_key,
        ri_id,
        info.current_game != 0,
    )?;

    // 5. Connect.
    if stale(&c.progress, generation) {
        let _ = client.quit();
        bail!("cancelled");
    }
    let hevc = settings.codec != Codec::H264 && info.codec_mode_support & 0x0F00 != 0;
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
            bitrate_kbps: settings.bitrate_kbps,
            hevc,
            remote: false,
        },
        ri_key,
        ri_iv,
        frames.clone(),
        tx,
        move || ctx.request_repaint(),
    );
    if stale(&c.progress, generation) {
        drop(session);
        let _ = client.quit();
        bail!("cancelled");
    }
    let input = session.input();
    *c.live.lock() = Some(Live {
        pc: t.name.clone(),
        node_id: t.node_id.clone(),
        ip: t.ip,
        session,
        input,
        frames,
        events: rx,
        started: Instant::now(),
        codec: if hevc { "HEVC" } else { "H.264" },
        requested: (w, h, fps),
        settings,
        path,
        clipboard: clipboard::Sync::spawn(t.ip, c.ctx.clone()),
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
        match http::post_json::<_, Ack>((ip, CONTROL_PORT), "/v1/pin", &req, Duration::from_secs(1))
        {
            Ok(a) if a.ok => return,
            Ok(a) => {
                let reason = a.error.unwrap_or_else(|| "PIN not accepted yet".into());
                tracing::info!("PIN not accepted yet: {reason}");
                progress.lock().detail = reason;
            }
            Err(e) => {
                tracing::info!("BroLink Host did not take the PIN: {e}");
                progress.lock().detail =
                    "BroLink Host is not answering on the PC. Open BroLink Host there and run setup, then try again.".into();
            }
        }
        std::thread::sleep(Duration::from_millis(800));
    }
}

fn remember_cert(node_id: &str, name: &str, der: &[u8]) {
    if let Err(e) = ClientConfig::update(|cfg| {
        let e = cfg.pcs.entry(node_id.to_string()).or_default();
        e.name = name.to_string();
        e.server_cert = Some(brolink_stream::nvhttp::hex(der));
    }) {
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
    fn update_reservation_blocks_connect_in_every_idle_state() {
        for step in [Step::Idle, Step::Ended { error: None }] {
            let mut p = Progress {
                step,
                ..Default::default()
            };
            assert!(!p.active());
            p.updating = true;
            assert!(p.active());
        }
        for step in [
            Step::Waking,
            Step::Waiting,
            Step::Pairing { pin: "1234".into() },
            Step::Connecting,
            Step::Streaming,
        ] {
            assert!(Progress {
                step,
                ..Default::default()
            }
            .active());
        }
    }

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
                ..Default::default()
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
