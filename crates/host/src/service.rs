//! The background control service: what a Mac on the tailnet talks to.
//!
//! One TCP listener on every interface, one rule for who gets an answer:
//! loopback (the host's own control panel) or a tailnet address that
//! `tailscale whois` attributes to the same account this PC is signed in
//! as. Everyone else gets a 403 and nothing more.

use crate::clipboard;
use crate::config::HostConfig;
use crate::streamer::{self, Api, Install};
use crate::update;
use crate::wake::{self, WakeInfo};
use anyhow::{Context, Result};
use brolink_core::api::{
    Ack, Clipboard, NatReport, PinRequest, PowerRequest, Status, Streamer, CLIPBOARD_PATH,
    UPDATE_PATH,
};
use brolink_core::http::{self, Request, Response};
use brolink_core::{tailscale, CONTROL_PORT};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LOG_LINES: usize = 80;
/// How long a `whois` answer is trusted before asking again.
const AUTH_TTL: Duration = Duration::from_secs(60);
/// Ticks (of 5 s) between runs of `tailscale netcheck`: it takes seconds
/// and the network does not move often.
const NETCHECK_TICKS: u64 = 120;

pub struct Service {
    cfg: Mutex<HostConfig>,
    tailscale: Mutex<Result<tailscale::Status, String>>,
    wake: Mutex<WakeInfo>,
    /// When a magic packet for this PC last arrived.
    wake_seen: Mutex<Option<Instant>>,
    install: Mutex<Option<Install>>,
    streamer: Mutex<Streamer>,
    log: Mutex<VecDeque<String>>,
    auth: Mutex<HashMap<IpAddr, (Instant, Option<u64>)>>,
    /// This PC's side of the NAT story, from `tailscale netcheck`.
    nat: Mutex<Option<NatReport>>,
    nat_running: AtomicBool,
}

impl Service {
    pub fn new() -> Self {
        Self {
            cfg: Mutex::new(HostConfig::load()),
            tailscale: Mutex::new(Err("not checked yet".into())),
            wake: Mutex::new(WakeInfo::default()),
            wake_seen: Mutex::new(None),
            install: Mutex::new(None),
            streamer: Mutex::new(Streamer::default()),
            log: Mutex::new(VecDeque::new()),
            auth: Mutex::new(HashMap::new()),
            nat: Mutex::new(None),
            nat_running: AtomicBool::new(false),
        }
    }

    pub fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::info!("{msg}");
        let mut log = self.log.lock();
        if log.len() >= LOG_LINES {
            log.pop_front();
        }
        log.push_back(msg);
    }

    /// Bind, then serve forever. Fails only when the port is taken, which
    /// means another copy is already running. `replacing` is the service an
    /// update just started: the old one is still answering its last request,
    /// so wait for the port rather than give up.
    pub fn run(self: Arc<Self>, replacing: bool) -> Result<()> {
        let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, CONTROL_PORT));
        let listener = if replacing {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                match TcpListener::bind(addr) {
                    Ok(l) => break l,
                    Err(e) if Instant::now() < deadline => {
                        tracing::info!("waiting for the old service to stop: {e}");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    Err(e) => return Err(e).context("bind after an update"),
                }
            }
        } else {
            TcpListener::bind(addr)?
        };
        self.log(format!(
            "BroLink Host {} listening on TCP {CONTROL_PORT}",
            env!("CARGO_PKG_VERSION")
        ));
        if let Ok(exe) = std::env::current_exe() {
            if replacing {
                self.log("updated: the previous version has been replaced");
                update::tidy(&exe);
            }
            self.ensure_autostart(&exe);
        }
        let refresher = self.clone();
        std::thread::spawn(move || refresher.refresh_loop());
        // (refresh_loop takes the Arc so netcheck can run on its own thread.)
        let svc = self.clone();
        if let Err(e) = wake::listen(move |mac, from| svc.wake_packet(mac, from)) {
            self.log(format!("not listening for wake packets: {e:#}"));
        }
        let handler = self.clone();
        http::serve(listener, move |peer, req| handler.handle(peer, req));
        Ok(())
    }

    /// A PC nobody can reach in person has to come back by itself after a
    /// restart, so the logon entry is kept unless the owner turned it off.
    fn ensure_autostart(&self, exe: &std::path::Path) {
        if !self.cfg.lock().start_with_windows {
            return;
        }
        // Rewrite even when a Run value already exists: install-host.ps1
        // may have moved the exe after the first setup, and the old path
        // would start nothing at logon.
        if let Err(e) = crate::setup::set_start_with_windows(true, exe) {
            self.log(format!("could not register start at logon: {e:#}"));
        }
    }

    fn refresh_loop(self: Arc<Self>) {
        let mut tick: u64 = 0;
        loop {
            self.refresh(tick);
            tick += 1;
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Cheap checks every tick; the slower probes on a longer cadence.
    fn refresh(self: &Arc<Self>, tick: u64) {
        *self.cfg.lock() = HostConfig::load();
        let stay_awake = self.cfg.lock().stay_awake;
        crate::power::keep_awake(stay_awake);
        if tick == 0 && stay_awake {
            self.log("keeping this PC awake so Tailscale stays reachable");
        }

        let ts = tailscale::status().map_err(|e| e.to_string());
        let came_up;
        {
            let mut cur = self.tailscale.lock();
            came_up = matches!((&*cur, &ts), (Err(_), Ok(_)));
            match (&*cur, &ts) {
                (Ok(_), Err(e)) => self.log(format!("Tailscale: {e}")),
                (Err(_), Ok(s)) => self.log(format!(
                    "Tailscale up as {} ({})",
                    s.self_login().unwrap_or("?"),
                    s.self_node
                        .ipv4()
                        .map(|ip| ip.to_string())
                        .unwrap_or_default()
                )),
                _ => {}
            }
            *cur = ts;
        }
        if self.tailscale.lock().is_ok() && (came_up || tick % NETCHECK_TICKS == 1) {
            self.spawn_netcheck();
        }

        let install = streamer::find();
        let running = install.is_some() && streamer::running();
        let cfg = self.cfg.lock().clone();
        let api_ok = if running
            && cfg.has_creds()
            && (tick.is_multiple_of(6) || !self.streamer.lock().api_ok)
        {
            Api {
                user: &cfg.sunshine_user,
                pass: &cfg.sunshine_pass,
            }
            .ok()
        } else {
            running && self.streamer.lock().api_ok
        };
        // The encoder Sunshine picked and whether it could capture audio,
        // from its log; re-read now and then since Sunshine restarts on
        // its own after setup and every session tries the audio device.
        let (encoder, audio_problem) =
            if tick.is_multiple_of(12) || self.streamer.lock().encoder.is_empty() {
                let log = install.as_ref().and_then(streamer::log_text);
                (
                    log.as_deref()
                        .and_then(streamer::encoder_in)
                        .unwrap_or_default(),
                    log.as_deref()
                        .and_then(streamer::audio_problem_in)
                        .unwrap_or_default(),
                )
            } else {
                let cur = self.streamer.lock();
                (cur.encoder.clone(), cur.audio_problem.clone())
            };
        let st = Streamer {
            kind: install
                .as_ref()
                .map(|i| i.kind.to_string())
                .unwrap_or_default(),
            installed: install.is_some(),
            running,
            api_ok,
            encoder,
            audio_problem,
        };
        {
            let mut cur = self.streamer.lock();
            if (cur.installed, cur.running, cur.api_ok) != (st.installed, st.running, st.api_ok) {
                self.log(match (&st.installed, &st.running, &st.api_ok) {
                    (false, _, _) => "Sunshine is not installed".to_string(),
                    (true, false, _) => format!("{} is installed but not running", st.kind),
                    (true, true, false) => {
                        format!("{} is running; BroLink cannot log in to it yet", st.kind)
                    }
                    (true, true, true) => {
                        format!("{} is running and BroLink is logged in", st.kind)
                    }
                });
            }
            if cur.encoder != st.encoder && !st.encoder.is_empty() {
                self.log(if st.encoder == "software" {
                    format!(
                        "{} encodes in software: no GPU encoder worked, so streams will be slow",
                        st.kind
                    )
                } else {
                    format!("{} encodes with {}", st.kind, st.encoder)
                });
            }
            if cur.audio_problem != st.audio_problem {
                self.log(if st.audio_problem.is_empty() {
                    format!("{} can capture audio again", st.kind)
                } else {
                    format!(
                        "{} has no sound to send: {}. A PC with no monitor or speakers needs a virtual audio device",
                        st.kind, st.audio_problem
                    )
                });
            }
            *cur = st;
        }
        *self.install.lock() = install;

        if tick.is_multiple_of(12) {
            let w = wake::probe();
            let mut cur = self.wake.lock();
            if *cur != w {
                match (&w.mac, w.magic_packet) {
                    (Some(mac), Some(true)) => self.log(format!("Wake-on-LAN ready on {} ({mac})", w.adapter)),
                    (Some(_), Some(false)) => self.log(format!("Wake-on-LAN is off on {}; run setup", w.adapter)),
                    (Some(_), None) => self.log(format!("Wake-on-LAN state on {} is unknown", w.adapter)),
                    (None, _) => self.log("no wired adapter with a MAC found; the Mac will not be able to wake this PC"),
                }
                *cur = w;
            }
        }
    }

    /// `tailscale netcheck` on its own thread, once at a time.
    fn spawn_netcheck(self: &Arc<Self>) {
        if self.nat_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let svc = self.clone();
        std::thread::spawn(move || {
            match tailscale::netcheck() {
                Ok(n) => {
                    let report = n.report();
                    let mut cur = svc.nat.lock();
                    if cur.as_ref() != Some(&report) {
                        svc.log(describe_nat(&report));
                    }
                    *cur = Some(report);
                }
                Err(e) => tracing::info!("netcheck: {e}"),
            }
            svc.nat_running.store(false, Ordering::Release);
        });
    }

    /// A magic packet arrived while awake: remember when, for the status.
    fn wake_packet(&self, mac: brolink_core::wake::MacAddr, from: SocketAddr) {
        let own = self.wake.lock().mac.clone();
        if own.is_some_and(|o| o != mac.to_string()) {
            return;
        }
        let mut seen = self.wake_seen.lock();
        if seen.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            self.log(format!("wake packet received from {from}"));
        }
        *seen = Some(Instant::now());
    }

    pub fn status(&self, with_log: bool) -> Status {
        let cfg = self.cfg.lock().clone();
        let ts = self.tailscale.lock();
        let wake = self.wake.lock().clone();
        let streamer = self.streamer.lock().clone();
        let mut setup = Vec::new();
        if let Err(e) = &*ts {
            setup.push(format!("Tailscale: {e}. Install it and sign in."));
        }
        if !streamer.installed {
            setup.push("Sunshine is not installed.".into());
        } else if !streamer.running {
            setup.push(format!("{} is installed but not running.", streamer.kind));
        } else if !streamer.api_ok {
            setup.push(format!(
                "BroLink has no working login for {}.",
                streamer.kind
            ));
        }
        if wake.magic_packet == Some(false) {
            setup.push(format!("Wake-on-LAN is off on {}.", wake.adapter));
        }
        if wake.fast_startup == Some(true) {
            setup.push("Fast Startup is on, so the PC cannot be woken after a shutdown.".into());
        }
        Status {
            app: "brolink".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            name: brolink_core::config::machine_name(),
            tailscale_ip: ts
                .as_ref()
                .ok()
                .and_then(|s| s.self_node.ipv4())
                .map(|ip| ip.to_string()),
            tailscale_login: ts
                .as_ref()
                .ok()
                .and_then(|s| s.self_login().map(str::to_string)),
            lan_ip: wake.lan_ip.map(|ip| ip.to_string()),
            mac: wake.mac.clone(),
            wake_ready: wake.magic_packet,
            wake_adapter: wake.adapter.clone(),
            wake_adapter_description: wake.description.clone(),
            wake_packet_age_secs: self.wake_seen.lock().map(|t| t.elapsed().as_secs()),
            fast_startup: wake.fast_startup,
            streamer,
            power_allowed: cfg.power_allowed,
            nat: self.nat.lock().clone(),
            setup,
            log: if with_log {
                self.log.lock().iter().cloned().collect()
            } else {
                Vec::new()
            },
        }
    }

    /// Loopback is the control panel; a tailnet peer must belong to the
    /// account this PC is signed in as.
    fn authorized(&self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return true;
        }
        let IpAddr::V4(v4) = ip else { return false };
        if !is_tailnet(v4) {
            return false;
        }
        let me = match &*self.tailscale.lock() {
            Ok(s) => s.self_node.user_id,
            Err(_) => return false,
        };
        let now = Instant::now();
        let cached = self
            .auth
            .lock()
            .get(&ip)
            .filter(|(at, _)| now - *at < AUTH_TTL)
            .map(|(_, u)| *u);
        let user = match cached {
            Some(u) => u,
            None => {
                let u = match tailscale::whois(ip) {
                    Ok(w) => {
                        self.log(format!(
                            "{} ({}) asked{}",
                            w.node.computed_name,
                            w.user_profile.login_name,
                            if w.user_profile.id == me {
                                ""
                            } else {
                                ": not this account, refused"
                            }
                        ));
                        Some(w.user_profile.id)
                    }
                    Err(e) => {
                        self.log(format!("whois {ip}: {e}"));
                        None
                    }
                };
                self.auth.lock().insert(ip, (now, u));
                u
            }
        };
        user == Some(me)
    }

    fn handle(&self, peer: SocketAddr, req: &Request) -> Response {
        if !self.authorized(peer.ip()) {
            return Response::json(
                403,
                &Ack::err("not a machine on this PC's Tailscale account"),
            );
        }
        // A web page open in a browser on an authorised machine (this PC,
        // or the Mac) can POST here without asking: a cross-origin form
        // post needs no preflight, and sleep, quit or a clipboard write
        // happen whether or not the page can read the reply. BroLink's own
        // callers send JSON or an executable, neither of which a browser
        // can send cross-origin without a preflight that nothing here
        // answers.
        if req.method == "POST" && !sent_by_brolink(req) {
            return Response::json(
                403,
                &Ack::err("a BroLink request carries JSON or an executable"),
            );
        }
        let local = peer.ip().is_loopback();
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/v1/status") => Response::json(200, &self.status(local)),
            ("POST", "/v1/pin") => self.pin(req),
            ("POST", "/v1/power") => self.power(req),
            ("POST", p) if p == UPDATE_PATH => self.update(req),
            ("GET", p) if p == CLIPBOARD_PATH => match clipboard::read() {
                Ok(c) => Response::json(200, &c),
                Err(e) => Response::json(500, &Ack::err(format!("clipboard: {e}"))),
            },
            ("POST", p) if p == CLIPBOARD_PATH => match req.json::<Clipboard>() {
                Ok(c) => match clipboard::write(&c.text) {
                    Ok(()) => Response::json(200, &Ack::ok()),
                    Err(e) => Response::json(500, &Ack::err(format!("clipboard: {e}"))),
                },
                Err(_) => Response::json(400, &Ack::err("expected {\"text\"}")),
            },
            ("POST", "/v1/quit") if local => {
                self.log("control panel asked the service to stop");
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(300));
                    std::process::exit(0);
                });
                Response::json(200, &Ack::ok())
            }
            _ => Response::json(404, &Ack::err("no such route")),
        }
    }

    /// A newer `brolink-host.exe` from the Mac: stage it, answer, then swap
    /// it in and hand over. See [`crate::update`].
    fn update(&self, req: &Request) -> Response {
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => return Response::json(500, &Ack::err(format!("own path unknown: {e}"))),
        };
        match update::stage(req, &exe) {
            Ok(version) => {
                self.log(format!(
                    "updating to {version}: the Mac sent the new BroLink Host"
                ));
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(500));
                    match update::apply(&exe) {
                        Ok(()) => {
                            tracing::info!("handing over to {version}");
                            std::process::exit(0);
                        }
                        Err(e) => tracing::error!("update to {version} failed: {e:#}"),
                    }
                });
                Response::json(200, &Ack::ok())
            }
            Err(r) => {
                self.log(format!("update refused: {}", r.message()));
                Response::json(r.status(), &Ack::err(r.message()))
            }
        }
    }

    fn pin(&self, req: &Request) -> Response {
        let Ok(p) = req.json::<PinRequest>() else {
            return Response::json(400, &Ack::err("expected {\"pin\",\"name\"}"));
        };
        if p.pin.len() != 4 || !p.pin.chars().all(|c| c.is_ascii_digit()) {
            return Response::json(400, &Ack::err("the PIN is four digits"));
        }
        let cfg = self.cfg.lock().clone();
        if !cfg.has_creds() || !self.streamer.lock().api_ok {
            return Response::json(
                502,
                &Ack::err("BroLink cannot log in to Sunshine on this PC; run setup there"),
            );
        }
        let api = Api {
            user: &cfg.sunshine_user,
            pass: &cfg.sunshine_pass,
        };
        match api.submit_pin(&p.pin, &p.name) {
            Ok(()) => {
                self.log(format!("paired \"{}\"", p.name));
                Response::json(200, &Ack::ok())
            }
            Err(e) => {
                self.log(format!("PIN from \"{}\" refused: {e:#}", p.name));
                Response::json(502, &Ack::err(e.to_string()))
            }
        }
    }

    fn power(&self, req: &Request) -> Response {
        let Ok(p) = req.json::<PowerRequest>() else {
            return Response::json(
                400,
                &Ack::err("expected {\"action\": sleep|restart|shutdown}"),
            );
        };
        let cfg = self.cfg.lock().clone();
        if !cfg.power_allowed {
            return Response::json(
                403,
                &Ack::err("the owner of this PC has turned remote power actions off"),
            );
        }
        if cfg.has_creds() {
            Api {
                user: &cfg.sunshine_user,
                pass: &cfg.sunshine_pass,
            }
            .close_app();
        }
        self.log(format!(
            "{} requested from the Mac",
            p.action.label().to_lowercase()
        ));
        // Reply first: sleep can suspend the machine before the bytes leave.
        let action = p.action;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            if let Err(e) = crate::power::perform(action) {
                tracing::error!("{}: {e:#}", action.label());
            }
        });
        Response::json(200, &Ack::ok())
    }
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

/// One log line about what `tailscale netcheck` found.
pub fn describe_nat(n: &NatReport) -> String {
    let city = tailscale::derp_city(&n.derp);
    if !n.udp {
        "network: UDP is blocked here, so a Mac can only reach this PC through a Tailscale relay"
            .into()
    } else if n.hard == Some(true) && !n.portmap {
        format!(
            "network: hard NAT with no UPnP; a Mac on another network reaches this PC through the {city} relay unless the router gets UPnP or a forwarded UDP port"
        )
    } else if n.hard == Some(true) {
        "network: hard NAT, but the router maps ports; direct connections should work".into()
    } else if n.hard == Some(false) {
        format!("network: easy NAT; direct connections should work (nearest relay {city})")
    } else {
        format!("network: NAT type unknown (nearest relay {city})")
    }
}

/// The `Content-Type` BroLink's own callers send: JSON, or the executable
/// on the update route. A browser cannot send either cross-origin without
/// a CORS preflight, so this keeps web pages from driving the service.
fn sent_by_brolink(req: &Request) -> bool {
    let ct = req
        .header("content-type")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    ct.starts_with("application/json") || ct.starts_with("application/octet-stream")
}

/// Tailscale hands out addresses from 100.64.0.0/10.
fn is_tailnet(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_range_is_recognised() {
        assert!(is_tailnet("100.64.0.1".parse().unwrap()));
        assert!(is_tailnet("100.127.255.254".parse().unwrap()));
        assert!(is_tailnet("100.64.0.10".parse().unwrap()));
        assert!(!is_tailnet("100.128.0.1".parse().unwrap()));
        assert!(!is_tailnet("192.168.1.2".parse().unwrap()));
    }

    #[test]
    fn strangers_get_403_and_loopback_gets_status() {
        let svc = Service::new();
        let req = Request {
            method: "GET".into(),
            path: "/v1/status".into(),
            ..Default::default()
        };
        let r = svc.handle("192.168.1.11:5".parse().unwrap(), &req);
        assert_eq!(r.status, 403);
        // A tailnet address with Tailscale down is refused too: nobody can vouch for it.
        let r = svc.handle("100.64.0.30:5".parse().unwrap(), &req);
        assert_eq!(r.status, 403);
        let r = svc.handle("127.0.0.1:5".parse().unwrap(), &req);
        assert_eq!(r.status, 200);
        let st: Status = r.parse().unwrap();
        assert_eq!(st.app, "brolink");
        assert!(!st.name.is_empty());
    }

    #[test]
    fn pin_and_power_validate_their_bodies() {
        let svc = Service::new();
        let local: SocketAddr = "127.0.0.1:5".parse().unwrap();
        let post = |path: &str, body: &str| Request {
            method: "POST".into(),
            path: path.into(),
            headers: json_header(),
            body: body.into(),
        };
        assert_eq!(svc.handle(local, &post("/v1/pin", "{}")).status, 400);
        assert_eq!(
            svc.handle(local, &post("/v1/pin", r#"{"pin":"12","name":"m"}"#))
                .status,
            400
        );
        // Valid shape, but no Sunshine login on this box: a 502, not a crash.
        assert_eq!(
            svc.handle(local, &post("/v1/pin", r#"{"pin":"1234","name":"m"}"#))
                .status,
            502
        );
        assert_eq!(
            svc.handle(local, &post("/v1/power", r#"{"action":"nap"}"#))
                .status,
            400
        );
        assert_eq!(svc.handle(local, &post("/v1/nope", "")).status, 404);
        // An update without its headers is refused before anything is written.
        let r = svc.handle(local, &post(UPDATE_PATH, "MZ"));
        assert_eq!(r.status, 400, "{}", r.body);
        let r = svc.handle(
            local,
            &Request {
                method: "POST".into(),
                path: UPDATE_PATH.into(),
                headers: vec![
                    ("x-brolink-version".into(), "0.0.1".into()),
                    ("content-type".into(), "application/octet-stream".into()),
                ],
                body: b"MZ".to_vec(),
            },
        );
        assert_eq!(r.status, 409, "{}", r.body);
    }

    fn json_header() -> Vec<(String, String)> {
        vec![("content-type".into(), "application/json".into())]
    }

    #[test]
    fn a_web_page_cannot_post_from_an_authorised_machine() {
        // A cross-origin form post from a browser on this PC arrives from
        // loopback with a form or text content type and no preflight. It
        // must not sleep the PC, stop the service or touch the clipboard.
        let svc = Service::new();
        let local: SocketAddr = "127.0.0.1:5".parse().unwrap();
        for ct in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
            Some("multipart/form-data; boundary=x"),
        ] {
            for path in ["/v1/power", "/v1/quit", CLIPBOARD_PATH, UPDATE_PATH] {
                let mut headers = Vec::new();
                if let Some(ct) = ct {
                    headers.push(("content-type".to_string(), ct.to_string()));
                }
                let r = svc.handle(
                    local,
                    &Request {
                        method: "POST".into(),
                        path: path.into(),
                        headers,
                        body: br#"{"action":"sleep","text":"x"}"#.to_vec(),
                    },
                );
                assert_eq!(r.status, 403, "{path} with {ct:?}: {}", r.body);
            }
        }
        // BroLink's own callers are unaffected, whatever the case of the header.
        let r = svc.handle(
            local,
            &Request {
                method: "POST".into(),
                path: "/v1/power".into(),
                headers: vec![(
                    "content-type".into(),
                    "Application/JSON; charset=utf-8".into(),
                )],
                body: br#"{"action":"nap"}"#.to_vec(),
            },
        );
        assert_eq!(r.status, 400, "{}", r.body);
        // GETs carry no side effect and stay open to the panel.
        let r = svc.handle(
            local,
            &Request {
                method: "GET".into(),
                path: "/v1/status".into(),
                ..Default::default()
            },
        );
        assert_eq!(r.status, 200);
    }

    #[test]
    fn the_nat_line_names_the_problem() {
        let hard = NatReport {
            udp: true,
            hard: Some(true),
            portmap: false,
            derp: "tok".into(),
            ..Default::default()
        };
        let t = describe_nat(&hard);
        assert!(
            t.contains("hard NAT with no UPnP") && t.contains("Tokyo"),
            "{t}"
        );
        let mapped = NatReport {
            portmap: true,
            ..hard.clone()
        };
        assert!(describe_nat(&mapped).contains("maps ports"));
        let easy = NatReport {
            udp: true,
            hard: Some(false),
            ..Default::default()
        };
        assert!(describe_nat(&easy).contains("easy NAT"));
        let blocked = NatReport::default();
        assert!(describe_nat(&blocked).contains("UDP is blocked"));
        // The clipboard routes exist and answer JSON, whatever the platform
        // says about the clipboard itself.
        let svc = Service::new();
        let local: SocketAddr = "127.0.0.1:5".parse().unwrap();
        let r = svc.handle(
            local,
            &Request {
                method: "POST".into(),
                path: CLIPBOARD_PATH.into(),
                headers: json_header(),
                body: b"not json".to_vec(),
            },
        );
        assert_eq!(r.status, 400, "{}", r.body);
        let r = svc.handle(
            local,
            &Request {
                method: "GET".into(),
                path: CLIPBOARD_PATH.into(),
                ..Default::default()
            },
        );
        assert!(r.status == 200 || r.status == 500, "{}", r.body);
    }

    #[test]
    fn power_is_refused_when_the_owner_says_so() {
        let svc = Service::new();
        svc.cfg.lock().power_allowed = false;
        let r = svc.handle(
            "127.0.0.1:5".parse().unwrap(),
            &Request {
                method: "POST".into(),
                path: "/v1/power".into(),
                headers: json_header(),
                body: r#"{"action":"sleep"}"#.into(),
            },
        );
        assert_eq!(r.status, 403);
    }
}
